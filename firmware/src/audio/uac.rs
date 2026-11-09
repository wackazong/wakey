//! USB Audio Class 1.0 output on the Tab5's USB-A port (high-speed host
//! port; the Pebble V3 is a full-speed device).
//!
//! Threads (plain FreeRTOS tasks via `std::thread`, the USB host library is
//! blocking):
//! - `usb-lib`: the host library's daemon (`usb_host_lib_handle_events`).
//! - `uac`: our client. It handles device events, parses the configuration
//!   descriptor, selects a 48 kHz / 16 bit / stereo alternate setting and
//!   keeps [`NUM_XFERS`] isochronous transfers of [`PACKETS`] × 1 ms in
//!   flight. Transfer callbacks run inside this thread and refill a
//!   transfer from the sample source before resubmitting it.
//!
//! References: USB Audio Class 1.0 spec; ESP-IDF `usb/usb_host.h`;
//! Espressif's `usb_host_uac` component (C, used only as a reference).

use core::ffi::c_void;
use core::ptr;
use esp_idf_svc::sys::{self as sys, EspError, esp};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Stream format the alarm clock uses (the Pebble supports only this).
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
/// Isochronous transfers in flight, and 1 ms packets per transfer: 3 × 8 ms
/// of buffering.
const NUM_XFERS: usize = 3;
const PACKETS: usize = 8;
/// Bytes per 1 ms packet: 48 frames × 2 channels × 2 bytes.
const PACKET_BYTES: usize = (SAMPLE_RATE as usize / 1000) * CHANNELS * 2;

/// Device volume after connecting: this many dB below the maximum.
const VOLUME_BELOW_MAX_DB: i16 = 20;

/// Fills interleaved stereo i16 samples (`buf.len()` = frames × 2).
pub type Source = Box<dyn FnMut(&mut [i16]) + Send>;

/// Packets sent and packets that failed, for logs.
pub static PACKETS_OK: AtomicU32 = AtomicU32::new(0);
pub static PACKETS_ERR: AtomicU32 = AtomicU32::new(0);
/// Set once a device has enumerated (the client got NEW_DEV).
pub static DEVICE_FOUND: AtomicBool = AtomicBool::new(false);

// ── Descriptor parsing ──────────────────────────────────────────────────────

const DESC_INTERFACE: u8 = 0x04;
const DESC_ENDPOINT: u8 = 0x05;
const CS_INTERFACE: u8 = 0x24;
const CS_ENDPOINT: u8 = 0x25;
const CLASS_AUDIO: u8 = 0x01;
const SUBCLASS_CONTROL: u8 = 0x01;
const SUBCLASS_STREAMING: u8 = 0x02;
const AC_FEATURE_UNIT: u8 = 0x06;
const AS_FORMAT_TYPE: u8 = 0x02;

/// A playable alternate setting of an Audio Streaming interface.
#[derive(Debug, Clone)]
pub struct StreamAlt {
    pub interface: u8,
    pub alt: u8,
    pub endpoint: u8,
    pub max_packet: u16,
    pub interval: u8,
    pub channels: u8,
    pub subframe_bytes: u8,
    pub bits: u8,
    pub rates: Vec<u32>,
    pub freq_control: bool,
}

#[derive(Debug, Default)]
pub struct AudioFunction {
    pub control_interface: Option<u8>,
    pub feature_units: Vec<u8>,
    pub outputs: Vec<StreamAlt>,
}

/// Parses a configuration descriptor for UAC 1.0 output streams.
pub fn parse(config: &[u8]) -> AudioFunction {
    let mut f = AudioFunction::default();
    let mut cur: Option<(u8, u8, u8)> = None; // (interface, alt, subclass)
    let mut alt: Option<StreamAlt> = None;
    let mut i = 0;
    let finish = |alt: &mut Option<StreamAlt>, f: &mut AudioFunction| {
        if let Some(a) = alt.take()
            && a.endpoint != 0
            && a.endpoint & 0x80 == 0
        {
            f.outputs.push(a);
        }
    };
    while i + 2 <= config.len() {
        let len = config[i] as usize;
        if len < 2 || i + len > config.len() {
            break;
        }
        let d = &config[i..i + len];
        match d[1] {
            DESC_INTERFACE if len >= 9 => {
                finish(&mut alt, &mut f);
                let (num, alt_n, class, sub) = (d[2], d[3], d[5], d[6]);
                cur = (class == CLASS_AUDIO).then_some((num, alt_n, sub));
                if class == CLASS_AUDIO && sub == SUBCLASS_CONTROL {
                    f.control_interface = Some(num);
                }
                if class == CLASS_AUDIO && sub == SUBCLASS_STREAMING && alt_n > 0 {
                    alt = Some(StreamAlt {
                        interface: num,
                        alt: alt_n,
                        endpoint: 0,
                        max_packet: 0,
                        interval: 0,
                        channels: 0,
                        subframe_bytes: 0,
                        bits: 0,
                        rates: Vec::new(),
                        freq_control: false,
                    });
                }
            }
            CS_INTERFACE if len >= 3 => match cur {
                Some((_, _, SUBCLASS_CONTROL)) if d[2] == AC_FEATURE_UNIT && len >= 4 => {
                    f.feature_units.push(d[3]);
                }
                Some((_, _, SUBCLASS_STREAMING)) if d[2] == AS_FORMAT_TYPE && len >= 8 => {
                    if let Some(a) = alt.as_mut() {
                        a.channels = d[4];
                        a.subframe_bytes = d[5];
                        a.bits = d[6];
                        let n = d[7] as usize;
                        if n == 0 && len >= 14 {
                            // Continuous range: lower and upper bound.
                            a.rates.push(u32::from_le_bytes([d[8], d[9], d[10], 0]));
                            a.rates.push(u32::from_le_bytes([d[11], d[12], d[13], 0]));
                        }
                        for k in 0..n {
                            let o = 8 + 3 * k;
                            if o + 3 <= len {
                                a.rates
                                    .push(u32::from_le_bytes([d[o], d[o + 1], d[o + 2], 0]));
                            }
                        }
                    }
                }
                _ => {}
            },
            DESC_ENDPOINT if len >= 7 => {
                if let Some(a) = alt.as_mut()
                    && d[3] & 0x03 == 0x01
                {
                    a.endpoint = d[2];
                    a.max_packet = u16::from_le_bytes([d[4], d[5]]) & 0x7FF;
                    a.interval = d[6];
                }
            }
            CS_ENDPOINT if len >= 4 => {
                if let Some(a) = alt.as_mut() {
                    a.freq_control = d[3] & 0x01 != 0;
                }
            }
            _ => {}
        }
        i += len;
    }
    finish(&mut alt, &mut f);
    f
}

// ── Host library ────────────────────────────────────────────────────────────

/// Installs the USB host library on the high-speed port, with the root port
/// unpowered, and starts its daemon thread. Switch on VBUS (USB5V_EN), let
/// the device boot, then call [`set_port_power`].
pub fn install_host() -> Result<(), EspError> {
    unsafe {
        let mut cfg: sys::usb_host_config_t = core::mem::zeroed();
        cfg.intr_flags = sys::ESP_INTR_FLAG_LOWMED as i32;
        cfg.root_port_unpowered = true;
        // peripheral_map 0: the default, the high-speed port (USB-A).
        esp!(sys::usb_host_install(&cfg))?;
    }
    set_full_speed_only();
    std::thread::Builder::new()
        .name("usb-lib".into())
        .stack_size(4096)
        .spawn(|| {
            loop {
                let mut flags = 0u32;
                unsafe { sys::usb_host_lib_handle_events(u32::MAX, &mut flags) };
                if flags != 0 {
                    log::debug!("usb: host library events {flags:#x}");
                }
            }
        })
        .expect("usb-lib thread");
    log::info!("usb: host installed");
    Ok(())
}

unsafe extern "C" {
    /// Register block of the high-speed USB-DWC controller (linker symbol).
    static mut USB_DWC_HS: u32;
}

/// Makes the high-speed controller a full/low-speed-only host: sets
/// `HCFG.FSLSSupp` (offset 0x400, bit 2), so it does not answer a device's
/// high-speed chirp during the port reset. Full-speed devices (the Pebble)
/// failed the root-port reset on the Tab5's USB-A port without this. Newer
/// releases of Espressif's USB host stack expose this as
/// `usb_host_config_t::fsls_only`; ESP-IDF 5.5.5's HAL cannot. Must run
/// after `usb_host_install` (which resets the core) and before the root port
/// is powered.
fn set_full_speed_only() {
    unsafe {
        let hcfg = (&raw mut USB_DWC_HS).cast::<u8>().add(0x400).cast::<u32>();
        let v = hcfg.read_volatile();
        hcfg.write_volatile(v | 1 << 2);
        log::info!(
            "usb: HCFG {v:#010x} → {:#010x} (full/low-speed only)",
            hcfg.read_volatile()
        );
    }
}

/// Powers the root port (starts connection detection, reset and
/// enumeration) or powers it down. After a failed port reset ESP-IDF leaves
/// the port disabled; cycling the power retries.
pub fn set_port_power(on: bool) -> Result<(), EspError> {
    unsafe { esp!(sys::usb_host_lib_set_root_port_power(on)) }
}

// ── Client: device handling and streaming ───────────────────────────────────

struct Client {
    handle: sys::usb_host_client_handle_t,
    new_device: Option<u8>,
    device_gone: bool,
    device: sys::usb_device_handle_t,
    control_done: bool,
    source: Source,
    /// Scratch for one transfer's samples.
    samples: Vec<i16>,
}

unsafe extern "C" fn on_client_event(
    msg: *const sys::usb_host_client_event_msg_t,
    arg: *mut c_void,
) {
    let client = unsafe { &mut *(arg as *mut Client) };
    let msg = unsafe { &*msg };
    match msg.event {
        sys::usb_host_client_event_t_USB_HOST_CLIENT_EVENT_NEW_DEV => {
            client.new_device = Some(unsafe { msg.__bindgen_anon_1.new_dev.address });
            DEVICE_FOUND.store(true, Ordering::Release);
        }
        sys::usb_host_client_event_t_USB_HOST_CLIENT_EVENT_DEV_GONE => client.device_gone = true,
        _ => {}
    }
}

unsafe extern "C" fn on_control_done(xfer: *mut sys::usb_transfer_t) {
    let client = unsafe { &mut *((*xfer).context as *mut Client) };
    client.control_done = true;
}

/// Refills a finished isochronous transfer from the source and resubmits it.
unsafe extern "C" fn on_iso_done(xfer: *mut sys::usb_transfer_t) {
    let x = unsafe { &mut *xfer };
    let client = unsafe { &mut *(x.context as *mut Client) };
    let packets = unsafe { x.isoc_packet_desc.as_mut_slice(PACKETS) };
    for p in packets.iter_mut() {
        if p.status == sys::usb_transfer_status_t_USB_TRANSFER_STATUS_COMPLETED {
            PACKETS_OK.fetch_add(1, Ordering::Relaxed);
        } else {
            PACKETS_ERR.fetch_add(1, Ordering::Relaxed);
        }
        p.num_bytes = PACKET_BYTES as i32;
    }
    if x.status != sys::usb_transfer_status_t_USB_TRANSFER_STATUS_COMPLETED || client.device_gone {
        // Device unplugged or stream stopped: let the transfer rest.
        return;
    }
    fill(client, x);
    unsafe { sys::usb_host_transfer_submit(xfer) };
}

fn fill(client: &mut Client, x: &mut sys::usb_transfer_t) {
    (client.source)(&mut client.samples);
    let bytes = unsafe { core::slice::from_raw_parts_mut(x.data_buffer, PACKET_BYTES * PACKETS) };
    for (dst, s) in bytes
        .as_chunks_mut::<2>()
        .0
        .iter_mut()
        .zip(client.samples.iter())
    {
        dst.copy_from_slice(&s.to_le_bytes());
    }
    x.num_bytes = (PACKET_BYTES * PACKETS) as i32;
}

impl Client {
    fn pump(&mut self, ticks: u32) {
        unsafe { sys::usb_host_client_handle_events(self.handle, ticks) };
    }

    /// Class request IN, reads `len` bytes, waits for completion.
    fn control_in(
        &mut self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        len: usize,
    ) -> Result<Vec<u8>, EspError> {
        unsafe {
            let mut xfer: *mut sys::usb_transfer_t = ptr::null_mut();
            // Setup packet + data stage rounded up to the 64-byte max packet.
            esp!(sys::usb_host_transfer_alloc(
                8 + len.next_multiple_of(64),
                0,
                &mut xfer
            ))?;
            let x = &mut *xfer;
            let setup = [
                request_type,
                request,
                value as u8,
                (value >> 8) as u8,
                index as u8,
                (index >> 8) as u8,
                len as u8,
                (len >> 8) as u8,
            ];
            core::slice::from_raw_parts_mut(x.data_buffer, 8).copy_from_slice(&setup);
            // IN data stages must be a multiple of the endpoint's packet size.
            x.num_bytes = (8 + len.next_multiple_of(64)) as i32;
            x.device_handle = self.device;
            x.bEndpointAddress = 0x80;
            x.callback = Some(on_control_done);
            x.context = (self as *mut Self).cast();
            x.timeout_ms = 1000;
            self.control_done = false;
            let r = esp!(sys::usb_host_transfer_submit_control(self.handle, xfer));
            if r.is_ok() {
                while !self.control_done {
                    self.pump(10);
                }
            }
            let got = (x.actual_num_bytes as usize).saturating_sub(8).min(len);
            let data = core::slice::from_raw_parts(x.data_buffer.add(8), got).to_vec();
            let status = x.status;
            sys::usb_host_transfer_free(xfer);
            r?;
            if status != sys::usb_transfer_status_t_USB_TRANSFER_STATUS_COMPLETED {
                log::warn!("uac: control request {request:#x} status {status}");
            }
            Ok(data)
        }
    }

    /// Feature unit: logs mute and the volume range, unmutes, and sets each
    /// channel `below_max_db` dB below the maximum (clamped to the range).
    fn set_volume(&mut self, unit: u8, interface: u8, below_max_db: i16) -> Result<(), EspError> {
        const GET_CUR: u8 = 0x81;
        const GET_MIN: u8 = 0x82;
        const GET_MAX: u8 = 0x83;
        const GET_RES: u8 = 0x84;
        const SET_CUR: u8 = 0x01;
        const MUTE: u16 = 0x01;
        const VOLUME: u16 = 0x02;
        let index = u16::from(unit) << 8 | u16::from(interface);
        let i16_of = |d: &[u8]| (d.len() >= 2).then(|| i16::from_le_bytes([d[0], d[1]]));
        let db = |v: i16| v as f32 / 256.0;

        let mute = self.control_in(0xA1, GET_CUR, MUTE << 8, index, 1)?;
        log::info!("uac: unit {unit} mute {:?}", mute.first());
        self.control_out(0x21, SET_CUR, MUTE << 8, index, &[0])?;

        for ch in 1..=CHANNELS as u16 {
            let value = VOLUME << 8 | ch;
            let get =
                |c: &mut Self, req| c.control_in(0xA1, req, value, index, 2).map(|d| i16_of(&d));
            let (cur, min, max, res) = (
                get(self, GET_CUR)?,
                get(self, GET_MIN)?,
                get(self, GET_MAX)?,
                get(self, GET_RES)?,
            );
            log::info!(
                "uac: unit {unit} ch {ch} volume {:?} dB, range {:?}..{:?} dB, step {:?} dB",
                cur.map(db),
                min.map(db),
                max.map(db),
                res.map(db)
            );
            let (Some(min), Some(max)) = (min, max) else {
                continue;
            };
            let target =
                (max as i32 - i32::from(below_max_db) * 256).clamp(min as i32, max as i32) as i16;
            self.control_out(0x21, SET_CUR, value, index, &target.to_le_bytes())?;
            let now = self.control_in(0xA1, GET_CUR, value, index, 2)?;
            log::info!(
                "uac: unit {unit} ch {ch} volume set to {:?} dB",
                i16_of(&now).map(db)
            );
        }
        Ok(())
    }

    /// Class request OUT with `data`, waits for completion.
    fn control_out(
        &mut self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        data: &[u8],
    ) -> Result<(), EspError> {
        unsafe {
            let mut xfer: *mut sys::usb_transfer_t = ptr::null_mut();
            esp!(sys::usb_host_transfer_alloc(64, 0, &mut xfer))?;
            let x = &mut *xfer;
            let setup = [
                request_type,
                request,
                value as u8,
                (value >> 8) as u8,
                index as u8,
                (index >> 8) as u8,
                data.len() as u8,
                (data.len() >> 8) as u8,
            ];
            let buf = core::slice::from_raw_parts_mut(x.data_buffer, 8 + data.len());
            buf[..8].copy_from_slice(&setup);
            buf[8..].copy_from_slice(data);
            x.num_bytes = (8 + data.len()) as i32;
            x.device_handle = self.device;
            x.bEndpointAddress = 0;
            x.callback = Some(on_control_done);
            x.context = (self as *mut Self).cast();
            x.timeout_ms = 1000;
            self.control_done = false;
            let r = esp!(sys::usb_host_transfer_submit_control(self.handle, xfer));
            if r.is_ok() {
                while !self.control_done {
                    self.pump(10);
                }
            }
            let status = x.status;
            sys::usb_host_transfer_free(xfer);
            r?;
            if status != sys::usb_transfer_status_t_USB_TRANSFER_STATUS_COMPLETED {
                log::warn!("uac: control request {request:#x} status {status}");
            }
            Ok(())
        }
    }

    fn start(&mut self, address: u8) -> Result<(), EspError> {
        unsafe {
            esp!(sys::usb_host_device_open(
                self.handle,
                address,
                &mut self.device
            ))?;
            let mut info: sys::usb_device_info_t = core::mem::zeroed();
            esp!(sys::usb_host_device_info(self.device, &mut info))?;
            let mut dev_desc: *const sys::usb_device_desc_t = ptr::null();
            esp!(sys::usb_host_get_device_descriptor(
                self.device,
                &mut dev_desc
            ))?;
            let dd = &(*dev_desc).__bindgen_anon_1;
            log::info!(
                "uac: device {address}: VID {:04x} PID {:04x}, speed {} (0 low, 1 full, 2 high)",
                { dd.idVendor },
                { dd.idProduct },
                info.speed
            );
            let mut cfg: *const sys::usb_config_desc_t = ptr::null();
            esp!(sys::usb_host_get_active_config_descriptor(
                self.device,
                &mut cfg
            ))?;
            let total = (*cfg).__bindgen_anon_1.wTotalLength as usize;
            let raw = core::slice::from_raw_parts(cfg.cast::<u8>(), total);
            let f = parse(raw);
            log::info!(
                "uac: control interface {:?}, feature units {:?}",
                f.control_interface,
                f.feature_units
            );
            for o in &f.outputs {
                log::info!("uac: output {o:?}");
            }
            let Some(alt) = f
                .outputs
                .iter()
                .find(|o| {
                    o.channels as usize == CHANNELS
                        && o.subframe_bytes == 2
                        && (o.rates.contains(&SAMPLE_RATE) || o.rates.is_empty())
                })
                .cloned()
            else {
                log::error!("uac: no 48 kHz / 16 bit / stereo output alternate setting");
                return Ok(());
            };
            assert!(
                alt.max_packet as usize >= PACKET_BYTES,
                "endpoint packet size too small"
            );
            esp!(sys::usb_host_interface_claim(
                self.handle,
                self.device,
                alt.interface,
                alt.alt
            ))?;
            // Claiming only reserves the endpoint in ESP-IDF; the device
            // switches its streaming interface (and starts its audio path)
            // on the standard SET_INTERFACE request.
            self.control_out(
                0x01,
                0x0B,
                u16::from(alt.alt),
                u16::from(alt.interface),
                &[],
            )?;
            if alt.freq_control {
                // SET_CUR SAMPLING_FREQ to the endpoint.
                let r = SAMPLE_RATE.to_le_bytes();
                self.control_out(0x22, 0x01, 0x0100, alt.endpoint as u16, &r[..3])?;
            }
            // Unmute and set a moderate volume (hosts like macOS do this on
            // connect; the device may start muted or at minimum).
            if let (Some(&unit), Some(ac)) = (f.feature_units.first(), f.control_interface) {
                self.set_volume(unit, ac, VOLUME_BELOW_MAX_DB)?;
            }
            log::info!(
                "uac: streaming {SAMPLE_RATE} Hz stereo to endpoint {:#04x} (interface {} alt {})",
                alt.endpoint,
                alt.interface,
                alt.alt
            );
            for _ in 0..NUM_XFERS {
                let mut xfer: *mut sys::usb_transfer_t = ptr::null_mut();
                esp!(sys::usb_host_transfer_alloc(
                    PACKET_BYTES * PACKETS,
                    PACKETS as i32,
                    &mut xfer
                ))?;
                let x = &mut *xfer;
                x.device_handle = self.device;
                x.bEndpointAddress = alt.endpoint;
                x.callback = Some(on_iso_done);
                x.context = (self as *mut Self).cast();
                x.timeout_ms = 100;
                for p in x.isoc_packet_desc.as_mut_slice(PACKETS) {
                    p.num_bytes = PACKET_BYTES as i32;
                }
                fill(self, x);
                esp!(sys::usb_host_transfer_submit(xfer))?;
            }
        }
        Ok(())
    }
}

/// Starts the `uac` client thread: waits for a USB audio device and plays
/// `source` on it. Call after [`install_host`].
pub fn play(source: Source) {
    std::thread::Builder::new()
        .name("uac".into())
        .stack_size(8192)
        .spawn(move || {
            // The client lives on this thread's stack for good; callbacks get
            // a pointer to it.
            let mut client = Client {
                handle: ptr::null_mut(),
                new_device: None,
                device_gone: false,
                device: ptr::null_mut(),
                control_done: false,
                source,
                samples: vec![0; PACKET_BYTES * PACKETS / 2],
            };
            unsafe {
                let mut cfg: sys::usb_host_client_config_t = core::mem::zeroed();
                cfg.is_synchronous = false;
                cfg.max_num_event_msg = 5;
                cfg.__bindgen_anon_1.async_.client_event_callback = Some(on_client_event);
                cfg.__bindgen_anon_1.async_.callback_arg = (&raw mut client).cast();
                esp!(sys::usb_host_client_register(&cfg, &mut client.handle)).expect("usb client");
            }
            log::info!("uac: waiting for a USB audio device");
            loop {
                client.pump(u32::MAX);
                if let Some(addr) = client.new_device.take()
                    && let Err(e) = client.start(addr)
                {
                    log::error!("uac: starting device {addr} failed: {e}");
                }
                if client.device_gone {
                    log::warn!("uac: device gone");
                    // Proof of concept: no clean-up / re-attach yet.
                }
            }
        })
        .expect("uac thread");
}
