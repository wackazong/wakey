//! The wheel click on the Tab5's built-in speaker (ES8388 codec, NS4150B
//! amplifier; see [`crate::board::codec`]).
//!
//! The I2S stream runs all the time and plays silence by itself
//! (`auto_clear`); while the amplifier is on, a thread streams an
//! inaudible keep-alive signal instead (see [`keepalive`]) and writes a
//! click when asked, quieter the closer it follows the previous one
//! ([`tone::click_gain`]). The amplifier is switched on when clicks may
//! come ([`Clicker::wake`], the alarm page) or with the first click, and off
//! after a few seconds without either, so the speaker does not hiss and
//! draw current while unused.

use crate::board::codec::{self, CodecPins};
use crate::board::i2c::SharedI2c;
use crate::board::ioexp::{IoExpanders, Pin};
use esp_idf_svc::hal::delay::TickType;
use esp_idf_svc::hal::i2s::config::{
    Config, DataBitWidth, MclkMultiple, SlotMode, StdClkConfig, StdConfig, StdGpioConfig,
    StdSlotConfig,
};
use esp_idf_svc::hal::i2s::{I2sDriver, I2sTx};
use std::sync::mpsc::{self, SyncSender, TryRecvError};
use std::time::{Duration, Instant};
use wakey_core::tone;

const SAMPLE_RATE: u32 = 48_000;
/// Peak loudness of a single click, 0–1 of full scale.
const AMPLITUDE: f32 = 0.5;
/// The amplifier goes off after this long without a click or a wake.
const AMP_OFF_AFTER: Duration = Duration::from_secs(8);
/// Time the amplifier gets to start before the first click.
const AMP_START: Duration = Duration::from_millis(50);
/// DMA: 3 × 2 ms. The keep-alive keeps the buffers full, so a click
/// waits behind them: small buffers keep that wait short and even (5 ms
/// buffers made fast clicks uneven).
const DMA_BUFFERS: u32 = 3;
const DMA_FRAMES: u32 = 96;

#[derive(Default)]
pub struct Clicker {
    tx: Option<SyncSender<Msg>>,
}

impl Clicker {
    /// Starts the I2S stream, sets up the codec and the click thread. On
    /// an error the clicker stays silent (logged).
    pub fn start(pins: CodecPins, bus: SharedI2c, ioexp: IoExpanders) -> Self {
        match Self::try_start(pins, bus, ioexp) {
            Ok(c) => c,
            Err(e) => {
                log::error!("click: built-in speaker unavailable: {e}");
                Self::default()
            }
        }
    }

    fn try_start(pins: CodecPins, bus: SharedI2c, ioexp: IoExpanders) -> anyhow::Result<Self> {
        let config = StdConfig::new(
            Config::default()
                .dma_buffer_count(DMA_BUFFERS)
                .frames_per_buffer(DMA_FRAMES)
                .auto_clear(true),
            StdClkConfig::from_sample_rate_hz(SAMPLE_RATE).mclk_multiple(MclkMultiple::M256),
            StdSlotConfig::philips_slot_default(DataBitWidth::Bits16, SlotMode::Stereo),
            StdGpioConfig::default(),
        );
        let mut i2s = I2sDriver::<I2sTx>::new_std_tx(
            pins.i2s,
            &config,
            pins.bclk,
            pins.dout,
            Some(pins.mclk),
            pins.ws,
        )?;
        i2s.tx_enable()?;
        codec::init(&bus)?;
        log::info!("click: built-in speaker ready");

        let (tx, rx) = mpsc::sync_channel::<Msg>(4);
        let mut thread = ClickThread {
            i2s,
            ioexp,
            click: tone::click(SAMPLE_RATE, AMPLITUDE),
            samples: Vec::new(),
            keepalive: keepalive(),
            amp_on: false,
            last_click: None,
            last_activity: Instant::now(),
        };
        std::thread::Builder::new()
            .name("click".into())
            .stack_size(4096)
            .spawn(move || thread.run(&rx))?;
        Ok(Self { tx: Some(tx) })
    }

    /// One tick; returns at once (dropped if clicks pile up).
    pub fn click(&self) {
        self.send(Msg::Click(Instant::now()));
    }

    /// Clicks may follow soon (the alarm page is open): switch the
    /// amplifier on now, so the first click is not cut off, and keep it on
    /// for [`AMP_OFF_AFTER`].
    pub fn wake(&self) {
        self.send(Msg::Wake);
    }

    fn send(&self, msg: Msg) {
        if let Some(tx) = &self.tx {
            let _ = tx.try_send(msg);
        }
    }
}

enum Msg {
    /// A tick at this time: the gap between ticks sets the loudness, not
    /// when the thread gets to play them.
    Click(Instant),
    Wake,
}

/// One DMA buffer of near-silence: ±2 (of 32767) alternating at half the
/// sample rate, inaudible and filtered out by the codec, but not the exact
/// zeros the stream plays by itself. On exact zeros a lone click came out
/// cut off and faint (something in the chain mutes on digital silence and
/// takes a moment to open again); a fast series of clicks did not.
fn keepalive() -> Vec<u8> {
    (0..DMA_FRAMES)
        .flat_map(|i| {
            let b = if i % 2 == 0 { 2i16 } else { -2 }.to_le_bytes();
            [b[0], b[1], b[0], b[1]]
        })
        .collect()
}

struct ClickThread {
    i2s: I2sDriver<'static, I2sTx>,
    ioexp: IoExpanders,
    /// The click at full gain, mono.
    click: Vec<i16>,
    /// The click at the current gain, interleaved stereo bytes.
    samples: Vec<u8>,
    keepalive: Vec<u8>,
    amp_on: bool,
    last_click: Option<Instant>,
    /// Last click or wake: the amplifier goes off [`AMP_OFF_AFTER`] later.
    last_activity: Instant,
}

impl ClickThread {
    fn run(&mut self, rx: &mpsc::Receiver<Msg>) {
        loop {
            let msg = if self.amp_on {
                match rx.try_recv() {
                    Ok(msg) => Some(msg),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => return,
                }
            } else {
                // Off: nothing to stream, wait for work.
                match rx.recv() {
                    Ok(msg) => Some(msg),
                    Err(_) => return,
                }
            };
            match msg {
                Some(Msg::Click(at)) => {
                    self.amp(true);
                    self.play(at);
                }
                Some(Msg::Wake) => self.amp(true),
                None if self.last_activity.elapsed() > AMP_OFF_AFTER => self.amp(false),
                // Keeps the stream from going digitally silent; blocks
                // until the DMA has room, so this paces itself.
                None => write(&mut self.i2s, &self.keepalive),
            }
        }
    }

    /// Switches the amplifier; on: lets it start up with the stream
    /// running, so the first click is whole.
    fn amp(&mut self, on: bool) {
        self.last_activity = Instant::now();
        if on == self.amp_on {
            return;
        }
        if self.ioexp.set(Pin::SpkEn, on).is_err() {
            return;
        }
        self.amp_on = on;
        log::info!("click: amplifier {}", if on { "on" } else { "off" });
        if on {
            let started = Instant::now();
            while started.elapsed() < AMP_START {
                write(&mut self.i2s, &self.keepalive);
            }
        }
    }

    fn play(&mut self, at: Instant) {
        let gap = self
            .last_click
            .map(|l| at.saturating_duration_since(l).as_millis() as u32);
        self.last_click = Some(at);
        let gain = tone::click_gain(gap);
        self.samples.clear();
        self.samples.extend(self.click.iter().flat_map(|&s| {
            let b = ((f32::from(s) * gain) as i16).to_le_bytes();
            [b[0], b[1], b[0], b[1]]
        }));
        write(&mut self.i2s, &self.samples);
    }
}

/// Queues `bytes` for the DMA; blocks while its buffers are full.
fn write(i2s: &mut I2sDriver<'static, I2sTx>, bytes: &[u8]) {
    let timeout = TickType::from(Duration::from_millis(100)).ticks();
    if let Err(e) = i2s.write_all(bytes, timeout) {
        log::warn!("click: {e}");
    }
}
