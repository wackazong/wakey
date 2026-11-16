//! The `uac` client thread: device set-up, class requests, the
//! isochronous stream, teardown.
//!
//! Tab5 specifics, each needed (see `docs/pitfalls.md`): the root port is
//! powered once, after VBUS plus a boot delay, and stays powered; if
//! nothing enumerates, VBUS is cycled; `SET_INTERFACE` is sent explicitly
//! (ESP-IDF's interface claim does not send it, and the Pebble stays
//! silent without it).

use super::host::power_root_port;
use super::{
    CHANNELS, Cmd, NUM_XFERS, PACKET_BYTES, PACKETS, PACKETS_ERR, PACKETS_OK, SAMPLE_RATE, Shared,
    Source,
};
use crate::board::ioexp::Switch;
use core::ffi::c_void;
use core::ptr;
use esp_idf_svc::sys::{self as sys, EspError, esp};
use std::sync::atomic::Ordering;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};
use wakey_core::uac::{self as desc, StreamAlt};

/// Device volume after connecting, dB below the device's maximum.
const DEFAULT_VOLUME_BELOW_MAX_DB: f32 = 20.0;
/// Attempts to get a device enumerated by power-cycling the root port.
const PORT_ATTEMPTS: u32 = 5;

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
    // SAFETY: `arg` is the client registered with this callback, which
    // runs on the client's thread inside `usb_host_client_handle_events`
    // (see `client_thread` on the aliasing this implies); `msg` is valid
    // for the call.
    let client = unsafe { &mut *(arg as *mut Client) };
    let msg = unsafe { &*msg };
    match msg.event {
        sys::usb_host_client_event_t_USB_HOST_CLIENT_EVENT_NEW_DEV => {
            // SAFETY: the union member of this event.
            client.new_device = Some(unsafe { msg.__bindgen_anon_1.new_dev.address });
        }
        sys::usb_host_client_event_t_USB_HOST_CLIENT_EVENT_DEV_GONE => client.device_gone = true,
        _ => {}
    }
}

unsafe extern "C" fn on_control_done(xfer: *mut sys::usb_transfer_t) {
    // SAFETY: the transfer is valid for the call; its context is the
    // client (`control`), on whose thread this runs.
    let client = unsafe { &mut *((*xfer).context as *mut Client) };
    client.control_done = true;
}

/// Refills a finished isochronous transfer from the source and resubmits it,
/// unless the stream is stopping or the device is gone.
unsafe extern "C" fn on_iso_done(xfer: *mut sys::usb_transfer_t) {
    // SAFETY: the transfer is ours and back from the driver for the call;
    // its context is the client (`stream`), on whose thread this runs; it
    // was allocated with PACKETS packet descriptors.
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
    // SAFETY: the transfer is set up (`stream`) and not in flight.
    if unsafe { sys::usb_host_transfer_submit(xfer) } == sys::ESP_OK {
        dev.in_flight += 1;
    }
}

fn fill(source: &mut Source, samples: &mut [i16], x: &mut sys::usb_transfer_t) {
    source(samples);
    // SAFETY: the transfer's buffer holds PACKETS packets
    // (`usb_host_transfer_alloc` in `stream`).
    let bytes = unsafe { core::slice::from_raw_parts_mut(x.data_buffer, PACKET_BYTES * PACKETS) };
    for (dst, s) in bytes.as_chunks_mut::<2>().0.iter_mut().zip(samples.iter()) {
        *dst = s.to_le_bytes();
    }
    x.num_bytes = (PACKET_BYTES * PACKETS) as i32;
}

pub(super) fn client_thread(
    shared: Arc<Shared>,
    cmds: mpsc::Receiver<Cmd>,
    vbus: Switch,
    source: Source,
) {
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
    // SAFETY: `client` stays on this thread's stack for good (the function
    // never returns), so the pointer given to the callbacks stays valid.
    // The callbacks run only inside `usb_host_client_handle_events`
    // (`pump`), while a `&mut Client` is held by the caller: an aliasing
    // `&mut` that Rust's rules do not allow, though nothing else touches
    // the client meanwhile. Callback-written state in `Cell`s would remove
    // it.
    unsafe {
        let mut cfg: sys::usb_host_client_config_t = core::mem::zeroed();
        cfg.is_synchronous = false;
        cfg.max_num_event_msg = 5;
        cfg.__bindgen_anon_1.async_.client_event_callback = Some(on_client_event);
        cfg.__bindgen_anon_1.async_.callback_arg = (&raw mut client).cast();
        esp!(sys::usb_host_client_register(&cfg, &mut client.handle)).expect("usb client");
    }

    // Power: VBUS, let the device boot, then the root port, once: it stays
    // powered. If nothing enumerates (the first port resets fail on this
    // board), cycle the 5 V (the device restarts, the hub driver recovers
    // the port). Never the root port's power: switched off while a device
    // was still enumerating, the next disconnection made ESP-IDF's hub
    // driver abort (`hub.c`, `dev_tree_node_dev_gone` on a device it never
    // added; docs/pitfalls.md).
    if let Err(e) = vbus(true) {
        log::error!("uac: switching USB-A 5 V on failed: {e}");
    }
    client.pump_for(Duration::from_secs(1));
    let mut attempt = 0;
    let mut port_on_at: Option<Instant> = None;
    let mut root_port_on = false;
    // Off (`Cmd::Power(false)`): no port power attempts.
    let mut powered = true;

    log::info!("uac: waiting for a USB audio device");
    loop {
        client.pump(10);

        if powered && client.device.is_none() && client.new_device.is_none() {
            match port_on_at {
                None if attempt < PORT_ATTEMPTS => {
                    attempt += 1;
                    if !root_port_on {
                        match power_root_port() {
                            Ok(()) => root_port_on = true,
                            Err(e) => log::error!("uac: root port power failed: {e}"),
                        }
                    }
                    port_on_at = Some(Instant::now());
                }
                Some(t) if t.elapsed() > Duration::from_secs(3) && attempt < PORT_ATTEMPTS => {
                    log::warn!("uac: no device after attempt {attempt}, cycling USB-A's 5 V");
                    if let Err(e) = vbus(false) {
                        log::error!("uac: switching USB-A 5 V off failed: {e}");
                    }
                    client.pump_for(Duration::from_millis(500));
                    if let Err(e) = vbus(true) {
                        log::error!("uac: switching USB-A 5 V on failed: {e}");
                    }
                    // Let the device boot (it may enumerate meanwhile).
                    client.pump_for(Duration::from_secs(1));
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
                Cmd::Power(on) if on != powered => {
                    powered = on;
                    if on {
                        log::info!("uac: USB-A on");
                        if let Err(e) = vbus(true) {
                            log::error!("uac: switching USB-A 5 V on failed: {e}");
                        }
                        // Let the device boot; the loop then powers the port.
                        client.pump_for(Duration::from_secs(1));
                        attempt = 0;
                        port_on_at = None;
                    } else {
                        log::info!("uac: USB-A off");
                        if client.device.is_some() {
                            client.teardown();
                            client.shared.connected.store(false, Ordering::Release);
                        }
                        // Only the 5 V off (the root port stays powered: see
                        // above). The Pebble clicks (its amplifier losing
                        // power; muting or ramping the volume first did not
                        // help); with only the root port off it still drew
                        // ~80 mA. ESP-IDF 5.5.5 has no USB suspend.
                        if let Err(e) = vbus(false) {
                            log::error!("uac: switching USB-A 5 V off failed: {e}");
                        }
                        client.pump_for(Duration::from_millis(200));
                        // Events of the vanished device.
                        client.new_device = None;
                        client.device_gone = false;
                    }
                }
                Cmd::Power(_) => {}
            }
        }
    }
}

impl Client {
    fn pump(&mut self, ticks: u32) {
        // SAFETY: the client is registered; callbacks run in here.
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
        // SAFETY: the transfer is allocated here with room for the setup
        // packet and `data_stage`; it is freed once the driver returned it
        // (or never took it); the callback's context is this client.
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
    /// can use. Either way but `Ok(true)` the device is closed again.
    fn open(&mut self, address: u8) -> Result<bool, EspError> {
        let mut handle: sys::usb_device_handle_t = ptr::null_mut();
        // SAFETY: plain driver call; `handle` is an out-parameter.
        unsafe { esp!(sys::usb_host_device_open(self.handle, address, &mut handle))? };
        let result = self.set_up(address, handle);
        if !matches!(result, Ok(true)) {
            if self.device.is_some() {
                self.teardown();
            } else {
                // SAFETY: opened above, nothing claimed.
                unsafe { sys::usb_host_device_close(self.handle, handle) };
            }
        }
        result
    }

    /// [`open`](Self::open) for the opened `handle`: claims the stream
    /// interface (then `self.device` holds it) and sets the device up.
    fn set_up(&mut self, address: u8, handle: sys::usb_device_handle_t) -> Result<bool, EspError> {
        // SAFETY: the descriptors belong to the open device and stay valid
        // while it is open; `wTotalLength` is the configuration's size.
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
            desc::parse(core::slice::from_raw_parts(cfg.cast::<u8>(), total))
        };
        log::debug!(
            "uac: control interface {:?}, feature units {:?}, outputs {:?}",
            f.control_interface,
            f.feature_units,
            f.outputs
        );
        let Some(alt) = f
            .find_output(CHANNELS as u8, 2, SAMPLE_RATE, PACKET_BYTES)
            .cloned()
        else {
            log::warn!("uac: device {address} has no 48 kHz / 16 bit / stereo output; ignored");
            return Ok(false);
        };
        // SAFETY: plain call on the open device.
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
        // The master volume is kept in the device and may be anywhere
        // (too low is silence): 0 dB, and the channels carry the volume.
        if let Some(index) = self.feature_index() {
            let _ = self.control(0x21, 0x01, 0x0200, index, &mut 0i16.to_le_bytes());
        }
        if let Some((_, max)) = self.device.as_ref().and_then(|d| d.volume_range_db) {
            self.set_volume_db(max - DEFAULT_VOLUME_BELOW_MAX_DB);
        }
        self.log_controls();
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

    /// Logs what the device reports for mute and volume (master and both
    /// channels), to check that our settings took.
    fn log_controls(&mut self) {
        const GET_CUR: u8 = 0x81;
        let Some(index) = self.feature_index() else {
            return;
        };
        let mut m = [0u8; 1];
        let mute = self
            .control(0xA1, GET_CUR, 0x0100, index, &mut m)
            .ok()
            .map(|()| m[0]);
        let mut vol = [None; 3];
        for (ch, v) in vol.iter_mut().enumerate() {
            let mut b = [0u8; 2];
            if self
                .control(0xA1, GET_CUR, 0x0200 | ch as u16, index, &mut b)
                .is_ok()
            {
                *v = Some(f32::from(i16::from_le_bytes(b)) / 256.0);
            }
        }
        log::info!("uac: device reports mute {mute:?}, volume master/L/R {vol:?} dB");
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
            log::info!(
                "uac: stream off ({} packets sent, {} failed so far)",
                PACKETS_OK.load(Ordering::Relaxed),
                PACKETS_ERR.load(Ordering::Relaxed)
            );
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
            // SAFETY: plain allocation; `xfer` is an out-parameter.
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
            // SAFETY: allocated above with PACKETS packets and not in
            // flight (the stream was off); the context is this client,
            // which outlives its transfers (`teardown`).
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
            // SAFETY: the transfers are back: the stream is off, and
            // `wait_in_flight` waited for them (the driver completes every
            // transfer of a device that is gone). The interface and the
            // device are ours to release.
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
