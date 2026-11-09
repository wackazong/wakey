//! USB Audio Class 1.0 output on the Tab5's USB-A port (high-speed host
//! port; the Pebble V3 is a full-speed device).
//!
//! [`UsbAudio::start`] installs ESP-IDF's USB host library and runs two
//! FreeRTOS threads (`std::thread`; the library is blocking):
//! - `usb-lib`: the library's daemon (`usb_host_lib_handle_events`).
//! - `uac`: our client. It powers the port, waits for a device, parses the
//!   configuration descriptor, picks a 48 kHz / 16 bit / stereo alternate
//!   setting, sets the sample rate and unmutes. While streaming
//!   ([`UsbAudio::set_streaming`]) it keeps [`NUM_XFERS`] isochronous
//!   transfers of [`PACKETS`] × 1 ms in flight; transfer callbacks run in
//!   this thread and refill a transfer from the sample [`Source`] before
//!   resubmitting it. When the device goes away everything is torn down and
//!   the thread waits for the next one; a re-plugged device resumes a wanted
//!   stream.
//!
//! Tab5 specifics, each of them needed (see README): the controller is put
//! into full/low-speed-only mode before the root port is powered; the root
//! port is powered after VBUS plus a boot delay, and power-cycled if nothing
//! enumerates; `SET_INTERFACE` is sent explicitly.
//!
//! References: USB Audio Class 1.0 spec; ESP-IDF `usb/usb_host.h`;
//! Espressif's `usb_host_uac` component (C, used only as a reference).

use core::ffi::c_void;
use core::ptr;
use esp_idf_svc::sys::{self as sys, EspError, esp};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

/// Stream format the alarm clock uses (the Pebble supports only this).
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
/// Isochronous transfers in flight, and 1 ms packets per transfer: 3 × 8 ms
/// of buffering.
const NUM_XFERS: usize = 3;
const PACKETS: usize = 8;
/// Bytes per 1 ms packet: 48 frames × 2 channels × 2 bytes.
const PACKET_BYTES: usize = (SAMPLE_RATE as usize / 1000) * CHANNELS * 2;
/// Device volume after connecting, dB below the device's maximum.
const DEFAULT_VOLUME_BELOW_MAX_DB: f32 = 20.0;
/// Attempts to get a device enumerated by power-cycling the root port.
const PORT_ATTEMPTS: u32 = 5;

/// Fills interleaved stereo i16 samples (`buf.len()` = frames × 2).
pub type Source = Box<dyn FnMut(&mut [i16]) + Send>;
/// Switches the 5 V supply of the USB-A socket.
pub type VbusSwitch = Box<dyn Fn(bool) -> Result<(), EspError> + Send>;

/// Packets sent and packets that failed, for logs.
pub static PACKETS_OK: AtomicU32 = AtomicU32::new(0);
pub static PACKETS_ERR: AtomicU32 = AtomicU32::new(0);

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
    }
}

/// Installs the host library on the high-speed port with the root port
/// unpowered and starts its daemon thread.
fn install_host() -> Result<(), EspError> {
    unsafe {
        let mut cfg: sys::usb_host_config_t = core::mem::zeroed();
        cfg.intr_flags = sys::ESP_INTR_FLAG_LOWMED as i32;
        // peripheral_map 0: the default, the high-speed port (USB-A).
        cfg.root_port_unpowered = true;
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
                if flags & sys::USB_HOST_LIB_EVENT_FLAGS_NO_CLIENTS != 0 {
                    unsafe { sys::usb_host_device_free_all() };
                }
            }
        })
        .expect("usb-lib thread");
    Ok(())
}

/// Powers the root port (connection detection, reset, enumeration) or
/// powers it down. After a failed port reset ESP-IDF leaves the port
/// disabled; cycling the power retries.
fn set_port_power(on: bool) -> Result<(), EspError> {
    unsafe { esp!(sys::usb_host_lib_set_root_port_power(on)) }
}

// ── Public handle ───────────────────────────────────────────────────────────

enum Cmd {
    Stream(bool),
    /// Volume in dB (0 = device maximum), clamped to the device range.
    Volume(f32),
    Mute(bool),
}

struct Shared {
    connected: AtomicBool,
    streaming: AtomicBool,
}

/// The USB speaker. Cheap to clone.
#[derive(Clone)]
pub struct UsbAudio {
    shared: Arc<Shared>,
    cmds: mpsc::Sender<Cmd>,
}

impl UsbAudio {
    /// Installs the host and starts the client thread. `vbus` switches the
    /// USB-A 5 V (expander pin USB5V_EN); `source` feeds the stream.
    pub fn start(vbus: VbusSwitch, source: Source) -> Result<Self, EspError> {
        install_host()?;
        let shared = Arc::new(Shared {
            connected: AtomicBool::new(false),
            streaming: AtomicBool::new(false),
        });
        let (tx, rx) = mpsc::channel();
        let thread_shared = shared.clone();
        std::thread::Builder::new()
            .name("uac".into())
            .stack_size(8192)
            .spawn(move || client_thread(thread_shared, rx, vbus, source))
            .expect("uac thread");
        Ok(Self { shared, cmds: tx })
    }

    /// True while a USB audio device is set up.
    pub fn connected(&self) -> bool {
        self.shared.connected.load(Ordering::Acquire)
    }

    /// Starts or stops the isochronous stream. Remembered: a device that
    /// connects later starts streaming right away if wanted.
    pub fn set_streaming(&self, on: bool) {
        self.shared.streaming.store(on, Ordering::Release);
        let _ = self.cmds.send(Cmd::Stream(on));
    }

    /// Device volume in dB (0 = device maximum; the Pebble takes −60..0).
    pub fn set_volume_db(&self, db: f32) {
        let _ = self.cmds.send(Cmd::Volume(db));
    }

    pub fn set_mute(&self, mute: bool) {
        let _ = self.cmds.send(Cmd::Mute(mute));
    }
}

// ── Client thread ───────────────────────────────────────────────────────────

/// The connected device.
struct Device {
    handle: sys::usb_device_handle_t,
    alt: StreamAlt,
    control_interface: Option<u8>,
    feature_unit: Option<u8>,
    volume_range_db: Option<(f32, f32)>,
    transfers: Vec<*mut sys::usb_transfer_t>,
    /// Transfers submitted and not yet returned by a callback.
    in_flight: usize,
    stream_on: bool,
}

struct Client {
    handle: sys::usb_host_client_handle_t,
    shared: Arc<Shared>,
    source: Source,
    /// Scratch for one transfer's samples.
    samples: Vec<i16>,
    new_device: Option<u8>,
    device_gone: bool,
    control_done: bool,
    device: Option<Device>,
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
        }
        sys::usb_host_client_event_t_USB_HOST_CLIENT_EVENT_DEV_GONE => client.device_gone = true,
        _ => {}
    }
}

unsafe extern "C" fn on_control_done(xfer: *mut sys::usb_transfer_t) {
    let client = unsafe { &mut *((*xfer).context as *mut Client) };
    client.control_done = true;
}

/// Refills a finished isochronous transfer from the source and resubmits it,
/// unless the stream is stopping or the device is gone.
unsafe extern "C" fn on_iso_done(xfer: *mut sys::usb_transfer_t) {
    let x = unsafe { &mut *xfer };
    let client = unsafe { &mut *(x.context as *mut Client) };
    for p in unsafe { x.isoc_packet_desc.as_mut_slice(PACKETS) } {
        if p.status == sys::usb_transfer_status_t_USB_TRANSFER_STATUS_COMPLETED {
            PACKETS_OK.fetch_add(1, Ordering::Relaxed);
        } else {
            PACKETS_ERR.fetch_add(1, Ordering::Relaxed);
        }
        p.num_bytes = PACKET_BYTES as i32;
    }
    let Some(dev) = client.device.as_mut() else {
        return;
    };
    dev.in_flight = dev.in_flight.saturating_sub(1);
    let ok = x.status == sys::usb_transfer_status_t_USB_TRANSFER_STATUS_COMPLETED;
    if !ok || !dev.stream_on || client.device_gone {
        return;
    }
    fill(&mut client.source, &mut client.samples, x);
    if unsafe { sys::usb_host_transfer_submit(xfer) } == sys::ESP_OK {
        dev.in_flight += 1;
    }
}

fn fill(source: &mut Source, samples: &mut [i16], x: &mut sys::usb_transfer_t) {
    source(samples);
    let bytes = unsafe { core::slice::from_raw_parts_mut(x.data_buffer, PACKET_BYTES * PACKETS) };
    for (dst, s) in bytes.as_chunks_mut::<2>().0.iter_mut().zip(samples.iter()) {
        *dst = s.to_le_bytes();
    }
    x.num_bytes = (PACKET_BYTES * PACKETS) as i32;
}

fn client_thread(shared: Arc<Shared>, cmds: mpsc::Receiver<Cmd>, vbus: VbusSwitch, source: Source) {
    // The client lives on this thread's stack for good; callbacks get a
    // pointer to it.
    let mut client = Client {
        handle: ptr::null_mut(),
        shared,
        source,
        samples: vec![0; PACKET_BYTES * PACKETS / 2],
        new_device: None,
        device_gone: false,
        control_done: false,
        device: None,
    };
    unsafe {
        let mut cfg: sys::usb_host_client_config_t = core::mem::zeroed();
        cfg.is_synchronous = false;
        cfg.max_num_event_msg = 5;
        cfg.__bindgen_anon_1.async_.client_event_callback = Some(on_client_event);
        cfg.__bindgen_anon_1.async_.callback_arg = (&raw mut client).cast();
        esp!(sys::usb_host_client_register(&cfg, &mut client.handle)).expect("usb client");
    }

    // Power: VBUS, let the device boot, then the root port. If nothing
    // enumerates (the first port resets fail on this board), cycle the port.
    if let Err(e) = vbus(true) {
        log::error!("uac: switching USB-A 5 V on failed: {e}");
    }
    client.pump_for(Duration::from_secs(1));
    let mut attempt = 0;
    let mut port_on_at: Option<Instant> = None;

    log::info!("uac: waiting for a USB audio device");
    loop {
        client.pump(10);

        if client.device.is_none() && client.new_device.is_none() {
            match port_on_at {
                None if attempt < PORT_ATTEMPTS => {
                    attempt += 1;
                    if let Err(e) = set_port_power(true) {
                        log::error!("uac: root port power failed: {e}");
                    }
                    port_on_at = Some(Instant::now());
                }
                Some(t) if t.elapsed() > Duration::from_secs(3) && attempt < PORT_ATTEMPTS => {
                    log::warn!("uac: no device after attempt {attempt}, cycling the root port");
                    let _ = set_port_power(false);
                    client.pump_for(Duration::from_millis(500));
                    port_on_at = None;
                }
                _ => {}
            }
        }

        if let Some(addr) = client.new_device.take() {
            match client.open(addr) {
                Ok(true) => {
                    client.shared.connected.store(true, Ordering::Release);
                    if client.shared.streaming.load(Ordering::Acquire) {
                        client.stream(true);
                    }
                }
                Ok(false) => {}
                Err(e) => log::error!("uac: setting up device {addr} failed: {e}"),
            }
        }
        if client.device_gone {
            client.teardown();
            client.device_gone = false;
            client.shared.connected.store(false, Ordering::Release);
            log::warn!("uac: device gone, waiting for the next one");
        }
        while let Ok(cmd) = cmds.try_recv() {
            match cmd {
                Cmd::Stream(on) => client.stream(on),
                Cmd::Volume(db) => client.set_volume_db(db),
                Cmd::Mute(m) => client.set_mute(m),
            }
        }
    }
}

impl Client {
    fn pump(&mut self, ticks: u32) {
        unsafe { sys::usb_host_client_handle_events(self.handle, ticks) };
    }

    fn pump_for(&mut self, d: Duration) {
        let end = Instant::now() + d;
        while Instant::now() < end {
            self.pump(10);
        }
    }

    /// Control request; `data` is written (OUT) or read into (IN, when
    /// `request_type` has bit 7 set). Waits for completion.
    fn control(
        &mut self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        data: &mut [u8],
    ) -> Result<(), EspError> {
        let Some(dev) = self.device.as_ref().map(|d| d.handle) else {
            return Err(EspError::from_infallible::<{ sys::ESP_ERR_INVALID_STATE }>());
        };
        let is_in = request_type & 0x80 != 0;
        // Setup packet + data stage rounded up to the 64-byte max packet (IN
        // data stages must be a multiple of the endpoint's packet size).
        let data_stage = data.len().next_multiple_of(64).max(64);
        unsafe {
            let mut xfer: *mut sys::usb_transfer_t = ptr::null_mut();
            esp!(sys::usb_host_transfer_alloc(8 + data_stage, 0, &mut xfer))?;
            let x = &mut *xfer;
            let len = data.len() as u16;
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
            let buf = core::slice::from_raw_parts_mut(x.data_buffer, 8 + data_stage);
            buf[..8].copy_from_slice(&setup);
            if !is_in {
                buf[8..8 + data.len()].copy_from_slice(data);
            }
            x.num_bytes = if is_in {
                8 + data_stage
            } else {
                8 + data.len()
            } as i32;
            x.device_handle = dev;
            x.bEndpointAddress = if is_in { 0x80 } else { 0x00 };
            x.callback = Some(on_control_done);
            x.context = (self as *mut Self).cast();
            x.timeout_ms = 1000;
            self.control_done = false;
            let r = esp!(sys::usb_host_transfer_submit_control(self.handle, xfer));
            if r.is_ok() {
                let t = Instant::now();
                while !self.control_done && t.elapsed() < Duration::from_secs(2) {
                    self.pump(10);
                }
            }
            let status = x.status;
            if is_in && self.control_done {
                let got = (x.actual_num_bytes as usize)
                    .saturating_sub(8)
                    .min(data.len());
                data[..got].copy_from_slice(&buf[8..8 + got]);
            }
            if self.control_done || r.is_err() {
                sys::usb_host_transfer_free(xfer);
            }
            r?;
            if !self.control_done
                || status != sys::usb_transfer_status_t_USB_TRANSFER_STATUS_COMPLETED
            {
                log::warn!(
                    "uac: control request {request:#x} value {value:#x} failed (status {status})"
                );
                return Err(EspError::from_infallible::<{ sys::ESP_FAIL }>());
            }
        }
        Ok(())
    }

    /// Opens and sets up a new device. `Ok(false)`: not an audio device we
    /// can use (closed again).
    fn open(&mut self, address: u8) -> Result<bool, EspError> {
        let mut handle: sys::usb_device_handle_t = ptr::null_mut();
        unsafe { esp!(sys::usb_host_device_open(self.handle, address, &mut handle))? };
        let f = unsafe {
            let mut dev_desc: *const sys::usb_device_desc_t = ptr::null();
            esp!(sys::usb_host_get_device_descriptor(handle, &mut dev_desc))?;
            let dd = &(*dev_desc).__bindgen_anon_1;
            log::info!(
                "uac: device {address}: VID {:04x} PID {:04x}",
                { dd.idVendor },
                { dd.idProduct }
            );
            let mut cfg: *const sys::usb_config_desc_t = ptr::null();
            esp!(sys::usb_host_get_active_config_descriptor(handle, &mut cfg))?;
            let total = (*cfg).__bindgen_anon_1.wTotalLength as usize;
            parse(core::slice::from_raw_parts(cfg.cast::<u8>(), total))
        };
        log::debug!(
            "uac: control interface {:?}, feature units {:?}, outputs {:?}",
            f.control_interface,
            f.feature_units,
            f.outputs
        );
        let Some(alt) = f
            .outputs
            .iter()
            .find(|o| {
                o.channels as usize == CHANNELS
                    && o.subframe_bytes == 2
                    && (o.rates.contains(&SAMPLE_RATE) || o.rates.is_empty())
                    && o.max_packet as usize >= PACKET_BYTES
            })
            .cloned()
        else {
            log::warn!("uac: device {address} has no 48 kHz / 16 bit / stereo output; ignored");
            unsafe { sys::usb_host_device_close(self.handle, handle) };
            return Ok(false);
        };
        unsafe {
            esp!(sys::usb_host_interface_claim(
                self.handle,
                handle,
                alt.interface,
                alt.alt
            ))?
        };
        self.device = Some(Device {
            handle,
            alt: alt.clone(),
            control_interface: f.control_interface,
            feature_unit: f.feature_units.first().copied(),
            volume_range_db: None,
            transfers: Vec::new(),
            in_flight: 0,
            stream_on: false,
        });
        // Claiming only reserves the endpoint in ESP-IDF; the device switches
        // its streaming interface on the standard SET_INTERFACE request. Idle
        // (alt 0) until the stream starts.
        self.set_interface(0)?;
        if alt.freq_control {
            // SET_CUR SAMPLING_FREQ to the endpoint (3 bytes).
            let mut r = SAMPLE_RATE.to_le_bytes();
            self.control(0x22, 0x01, 0x0100, u16::from(alt.endpoint), &mut r[..3])?;
        }
        self.read_volume_range();
        self.set_mute(false);
        if let Some((_, max)) = self.device.as_ref().and_then(|d| d.volume_range_db) {
            self.set_volume_db(max - DEFAULT_VOLUME_BELOW_MAX_DB);
        }
        log::info!(
            "uac: ready: {SAMPLE_RATE} Hz stereo, endpoint {:#04x} (interface {} alt {})",
            alt.endpoint,
            alt.interface,
            alt.alt
        );
        Ok(true)
    }

    fn set_interface(&mut self, alt: u8) -> Result<(), EspError> {
        let Some(iface) = self.device.as_ref().map(|d| u16::from(d.alt.interface)) else {
            return Ok(());
        };
        self.control(0x01, 0x0B, u16::from(alt), iface, &mut [])
    }

    /// wIndex for feature unit requests: unit in the high byte, control
    /// interface in the low byte.
    fn feature_index(&self) -> Option<u16> {
        let d = self.device.as_ref()?;
        Some(u16::from(d.feature_unit?) << 8 | u16::from(d.control_interface?))
    }

    fn read_volume_range(&mut self) {
        const GET_MIN: u8 = 0x82;
        const GET_MAX: u8 = 0x83;
        let Some(index) = self.feature_index() else {
            return;
        };
        let mut get = |req: u8| -> Option<f32> {
            let mut b = [0u8; 2];
            self.control(0xA1, req, 0x0200 | 1, index, &mut b).ok()?;
            Some(f32::from(i16::from_le_bytes(b)) / 256.0)
        };
        let range = (get(GET_MIN), get(GET_MAX));
        if let (Some(min), Some(max)) = range {
            log::info!("uac: volume range {min:.1}..{max:.1} dB");
            if let Some(d) = self.device.as_mut() {
                d.volume_range_db = Some((min, max));
            }
        }
    }

    fn set_volume_db(&mut self, db: f32) {
        let Some(index) = self.feature_index() else {
            return;
        };
        let (min, max) = self
            .device
            .as_ref()
            .and_then(|d| d.volume_range_db)
            .unwrap_or((-60.0, 0.0));
        let v = (db.clamp(min, max) * 256.0) as i16;
        for ch in 1..=CHANNELS as u16 {
            let _ = self.control(0x21, 0x01, 0x0200 | ch, index, &mut v.to_le_bytes());
        }
        log::info!("uac: volume {:.1} dB", f32::from(v) / 256.0);
    }

    fn set_mute(&mut self, mute: bool) {
        let Some(index) = self.feature_index() else {
            return;
        };
        let _ = self.control(0x21, 0x01, 0x0100, index, &mut [u8::from(mute)]);
    }

    /// Starts or stops the isochronous stream.
    fn stream(&mut self, on: bool) {
        let Some(d) = self.device.as_ref() else {
            return;
        };
        if d.stream_on == on {
            return;
        }
        if !on {
            self.device.as_mut().unwrap().stream_on = false;
            self.wait_in_flight();
            let _ = self.set_interface(0);
            log::info!("uac: stream off");
            return;
        }
        let alt = d.alt.alt;
        if let Err(e) = self.set_interface(alt) {
            log::error!("uac: SET_INTERFACE failed: {e}");
            return;
        }
        let d = self.device.as_mut().unwrap();
        d.stream_on = true;
        let (handle, endpoint) = (d.handle, d.alt.endpoint);
        while d.transfers.len() < NUM_XFERS {
            let mut xfer: *mut sys::usb_transfer_t = ptr::null_mut();
            let r = unsafe {
                esp!(sys::usb_host_transfer_alloc(
                    PACKET_BYTES * PACKETS,
                    PACKETS as i32,
                    &mut xfer
                ))
            };
            if r.is_err() {
                log::error!("uac: transfer alloc failed");
                break;
            }
            d.transfers.push(xfer);
        }
        let transfers = d.transfers.clone();
        for xfer in transfers {
            let x = unsafe { &mut *xfer };
            x.device_handle = handle;
            x.bEndpointAddress = endpoint;
            x.callback = Some(on_iso_done);
            x.context = (self as *mut Self).cast();
            x.timeout_ms = 100;
            for p in unsafe { x.isoc_packet_desc.as_mut_slice(PACKETS) } {
                p.num_bytes = PACKET_BYTES as i32;
            }
            fill(&mut self.source, &mut self.samples, x);
            if unsafe { sys::usb_host_transfer_submit(xfer) } == sys::ESP_OK {
                self.device.as_mut().unwrap().in_flight += 1;
            }
        }
        log::info!("uac: stream on");
    }

    /// Pumps events until all submitted transfers have returned.
    fn wait_in_flight(&mut self) {
        let t = Instant::now();
        while self.device.as_ref().is_some_and(|d| d.in_flight > 0)
            && t.elapsed() < Duration::from_secs(1)
        {
            self.pump(10);
        }
    }

    /// The device is gone: let the transfers return, free them, close.
    fn teardown(&mut self) {
        if let Some(d) = self.device.as_mut() {
            d.stream_on = false;
        }
        self.wait_in_flight();
        if let Some(d) = self.device.take() {
            for xfer in d.transfers {
                unsafe { sys::usb_host_transfer_free(xfer) };
            }
            unsafe {
                sys::usb_host_interface_release(self.handle, d.handle, d.alt.interface);
                sys::usb_host_device_close(self.handle, d.handle);
            }
        }
    }
}
