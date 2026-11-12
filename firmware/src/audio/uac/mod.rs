//! USB Audio Class 1.0 output on the Tab5's USB-A port (a high-speed host
//! port; the Creative Pebble V3 is a full-speed device). Rust on top of
//! ESP-IDF's USB host library; no C class driver.
//!
//! [`UsbAudio::start`] installs the host library and runs two FreeRTOS
//! threads (`std::thread`; the library is blocking):
//! - `usb-lib`: the library's daemon ([`host`]).
//! - `uac`: our client ([`device`]). It powers the port, waits for a
//!   device, picks a 48 kHz / 16 bit / stereo alternate setting, sets the
//!   sample rate and unmutes. While streaming it keeps [`NUM_XFERS`]
//!   isochronous transfers of [`PACKETS`] × 1 ms in flight; transfer
//!   callbacks run in this thread and refill a transfer from the sample
//!   [`Source`] before resubmitting it. When the device goes away
//!   everything is torn down and the thread waits for the next one; a
//!   re-plugged device resumes a wanted stream.
//!
//! The [`UsbAudio`] handle talks to the client thread through a channel.

mod device;
mod host;

use crate::board::ioexp::Switch;
use esp_idf_svc::sys::EspError;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, mpsc};

/// Stream format the alarm clock uses (the Pebble supports only this).
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
/// Isochronous transfers in flight, and 1 ms packets per transfer: 3 × 8 ms
/// of buffering.
const NUM_XFERS: usize = 3;
const PACKETS: usize = 8;
/// Bytes per 1 ms packet: 48 frames × 2 channels × 2 bytes.
const PACKET_BYTES: usize = (SAMPLE_RATE as usize / 1000) * CHANNELS * 2;

/// Fills interleaved stereo i16 samples (`buf.len()` = frames × 2).
pub type Source = Box<dyn FnMut(&mut [i16]) + Send>;

/// Packets sent and packets that failed, for logs.
pub static PACKETS_OK: AtomicU32 = AtomicU32::new(0);
pub static PACKETS_ERR: AtomicU32 = AtomicU32::new(0);

enum Cmd {
    Stream(bool),
    /// Volume in dB (0 = device maximum), clamped to the device range.
    Volume(f32),
    Mute(bool),
    /// USB-A 5 V and the root port on or off (off: the device goes away).
    Power(bool),
}

struct Shared {
    connected: AtomicBool,
    streaming: AtomicBool,
    /// Milliseconds since boot when the port was last powered on, 0 while
    /// it is off.
    powered_at_ms: AtomicU32,
}

fn now_ms() -> u32 {
    (unsafe { esp_idf_svc::sys::esp_timer_get_time() } / 1000) as u32
}

/// Grace after power on before a missing device counts as missing.
const ENUMERATE_GRACE_MS: u32 = 10_000;

/// The USB speaker. Cheap to clone.
#[derive(Clone)]
pub struct UsbAudio {
    shared: Arc<Shared>,
    cmds: mpsc::Sender<Cmd>,
}

impl UsbAudio {
    /// Installs the host and starts the client thread. `vbus` switches the
    /// USB-A 5 V (expander pin USB5V_EN); `source` feeds the stream.
    pub fn start(vbus: Switch, source: Source) -> Result<Self, EspError> {
        host::install()?;
        let shared = Arc::new(Shared {
            connected: AtomicBool::new(false),
            streaming: AtomicBool::new(false),
            powered_at_ms: AtomicU32::new(now_ms().max(1)),
        });
        let (tx, rx) = mpsc::channel();
        let thread_shared = shared.clone();
        std::thread::Builder::new()
            .name("uac".into())
            .stack_size(8192)
            .spawn(crate::taskstats::named("uac", move || {
                device::client_thread(thread_shared, rx, vbus, source)
            }))
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

    /// Powers the USB-A port (5 V and root port) on or off. The switched-on
    /// Pebble draws ~80 mA at 5 V even when silent. After power on, the
    /// device needs a few seconds to boot and enumerate.
    pub fn set_power(&self, on: bool) {
        let at = if on { now_ms().max(1) } else { 0 };
        self.shared.powered_at_ms.store(at, Ordering::Release);
        let _ = self.cmds.send(Cmd::Power(on));
    }

    /// True if the port has been on for a while and still no speaker is
    /// set up (the clock face shows "no speaker"). False while it is off.
    pub fn missing(&self) -> bool {
        let at = self.shared.powered_at_ms.load(Ordering::Acquire);
        at != 0 && now_ms().wrapping_sub(at) > ENUMERATE_GRACE_MS && !self.connected()
    }
}
