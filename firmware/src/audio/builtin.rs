//! The Tab5's built-in speaker (ES8388 codec, NS4150B amplifier; see
//! [`crate::board::codec`]): the wheel click, and the alarm sound when no
//! USB speaker is connected.
//!
//! One thread mixes what the speaker plays, 2 ms at a time, into the I2S
//! stream: the player's sound while [`BuiltinSpeaker::set_streaming`] is on
//! (mixed to mono, the amplifier takes the left channel only), clicks on
//! top, and always the inaudible [`mix::keepalive`] signal: on exact digital
//! silence something in the chain mutes, and a lone click or the first beep
//! after a pause lost its start.
//!
//! The amplifier is on while streaming, from a [`BuiltinSpeaker::wake`]
//! (the alarm page: its wheels click) or a click, until [`AMP_OFF_AFTER`]
//! without either. With it the codec and the I2S clocks go off (~9 mA at
//! 5 V), and the thread sleeps; before the next sound the I2S starts, the
//! codec is set up again (~3 ms) and the amplifier comes on.

use super::uac::Source;
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
use wakey_core::{mix, tone};

const SAMPLE_RATE: u32 = 48_000;
/// Peak loudness of a single click, 0–1 of full scale.
const CLICK_AMPLITUDE: f32 = 0.25;
/// The amplifier goes off after this long without a click, a wake or the
/// stream.
const AMP_OFF_AFTER: Duration = Duration::from_secs(8);
/// Time the amplifier gets to start before anything is played.
const AMP_START: Duration = Duration::from_millis(50);
/// DMA: 3 × 2 ms, and the mixer's block. The stream is kept full, so
/// whatever is mixed in waits behind it: small buffers keep that wait short
/// and even (5 ms buffers made fast clicks uneven).
const DMA_BUFFERS: u32 = 3;
const DMA_FRAMES: u32 = 96;
const BLOCK_SAMPLES: usize = DMA_FRAMES as usize * 2;

/// The built-in speaker. Cheap to clone; silent if it could not be set up.
#[derive(Clone, Default)]
pub struct BuiltinSpeaker {
    tx: Option<SyncSender<Msg>>,
}

enum Msg {
    /// A wheel tick at this time: the gap between ticks sets the loudness,
    /// not when the thread gets to play them.
    Click(Instant),
    Wake,
    Stream(bool),
}

impl BuiltinSpeaker {
    /// Starts the I2S stream, sets up the codec and the mixer thread, which
    /// plays `source` while streaming. On an error the speaker stays silent
    /// (logged).
    pub fn start(pins: CodecPins, bus: SharedI2c, ioexp: IoExpanders, source: Source) -> Self {
        match Self::try_start(pins, bus, ioexp, source) {
            Ok(s) => s,
            Err(e) => {
                log::error!("speaker: built-in speaker unavailable: {e}");
                Self::default()
            }
        }
    }

    fn try_start(
        pins: CodecPins,
        bus: SharedI2c,
        ioexp: IoExpanders,
        source: Source,
    ) -> anyhow::Result<Self> {
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
        // Set up once (it answers), then off until the first sound.
        i2s.tx_enable()?;
        codec::init(&bus)?;
        codec::power_down(&bus)?;
        i2s.tx_disable()?;
        log::info!("speaker: built-in speaker ready");

        let (tx, rx) = mpsc::sync_channel::<Msg>(8);
        let mut mixer = Mixer {
            i2s,
            bus,
            ioexp,
            source,
            click: tone::click(SAMPLE_RATE, CLICK_AMPLITUDE),
            click_at: None,
            click_gain: 1.0,
            last_click: None,
            keepalive: mix::keepalive(DMA_FRAMES as usize),
            block: vec![0; BLOCK_SAMPLES],
            bytes: vec![0; BLOCK_SAMPLES * 2],
            streaming: false,
            amp_on: false,
            last_activity: Instant::now(),
        };
        crate::runtime::Thread::new(c"speaker", 4096).spawn(move || mixer.run(&rx))?;
        Ok(Self { tx: Some(tx) })
    }

    /// One wheel tick; returns at once (dropped if ticks pile up).
    pub fn click(&self) {
        self.send(Msg::Click(Instant::now()));
    }

    /// Clicks may follow soon (the alarm page is open): switch the
    /// amplifier on now, so the first click is not cut off, and keep it on
    /// for [`AMP_OFF_AFTER`].
    pub fn wake(&self) {
        self.send(Msg::Wake);
    }

    /// Plays the source (on) or not. Only one output may take from the
    /// player at a time (see `audio::output`).
    pub fn set_streaming(&self, on: bool) {
        if let Some(tx) = &self.tx {
            // Must not be dropped, unlike a click.
            let _ = tx.send(Msg::Stream(on));
        }
    }

    fn send(&self, msg: Msg) {
        if let Some(tx) = &self.tx {
            let _ = tx.try_send(msg);
        }
    }
}

struct Mixer {
    i2s: I2sDriver<'static, I2sTx>,
    bus: SharedI2c,
    ioexp: IoExpanders,
    source: Source,
    /// The click at full gain, mono.
    click: Vec<i16>,
    /// Position in `click` of the click being played, if any, and its gain.
    click_at: Option<usize>,
    click_gain: f32,
    last_click: Option<Instant>,
    keepalive: Vec<i16>,
    /// The block being mixed, and as bytes for the driver.
    block: Vec<i16>,
    bytes: Vec<u8>,
    streaming: bool,
    amp_on: bool,
    /// Last click, wake or streaming: the amplifier goes off
    /// [`AMP_OFF_AFTER`] later.
    last_activity: Instant,
}

impl Mixer {
    fn run(&mut self, rx: &mpsc::Receiver<Msg>) {
        loop {
            if !self.amp_on {
                // Nothing to play: wait for work.
                match rx.recv() {
                    Ok(msg) => self.handle(msg),
                    Err(_) => return,
                }
            }
            loop {
                match rx.try_recv() {
                    Ok(msg) => self.handle(msg),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                }
            }
            if self.streaming {
                self.last_activity = Instant::now();
            }
            let busy = self.streaming || self.click_at.is_some();
            if !busy && self.last_activity.elapsed() > AMP_OFF_AFTER {
                self.amp(false);
                continue;
            }
            if self.amp_on {
                self.mix_block();
                self.write_block();
            }
        }
    }

    fn handle(&mut self, msg: Msg) {
        self.last_activity = Instant::now();
        match msg {
            Msg::Click(at) => {
                let gap = self
                    .last_click
                    .map(|l| at.saturating_duration_since(l).as_millis() as u32);
                self.last_click = Some(at);
                self.click_gain = tone::click_gain(gap);
                self.click_at = Some(0);
                self.amp(true);
            }
            Msg::Wake => self.amp(true),
            Msg::Stream(on) => {
                if on != self.streaming {
                    log::info!("speaker: built-in stream {}", if on { "on" } else { "off" });
                }
                self.streaming = on;
                if on {
                    self.amp(true);
                }
            }
        }
    }

    /// Switches the amplifier, and with it the codec and the I2S clocks
    /// (the amplifier off first, so the codec's power-down is not heard);
    /// on: lets it start up with the keep-alive running, so the first sound
    /// is whole.
    fn amp(&mut self, on: bool) {
        if on == self.amp_on {
            return;
        }
        if on {
            // MCLK first: the codec's setup needs it.
            if let Err(e) = self.i2s.tx_enable() {
                log::warn!("speaker: I2S start: {e}");
            }
            if let Err(e) = codec::init(&self.bus) {
                log::warn!("speaker: codec setup: {e}");
            }
        }
        if self.ioexp.set(Pin::SpkEn, on).is_err() {
            return;
        }
        self.amp_on = on;
        log::info!("speaker: amplifier {}", if on { "on" } else { "off" });
        if !on {
            if let Err(e) = codec::power_down(&self.bus) {
                log::warn!("speaker: codec power-down: {e}");
            }
            if let Err(e) = self.i2s.tx_disable() {
                log::warn!("speaker: I2S stop: {e}");
            }
        }
        if on {
            let started = Instant::now();
            self.block.copy_from_slice(&self.keepalive);
            while started.elapsed() < AMP_START {
                self.write_block();
            }
        }
    }

    /// The next 2 ms: the source (mono) or silence, the keep-alive, and
    /// the click being played.
    fn mix_block(&mut self) {
        if self.streaming {
            (self.source)(&mut self.block);
            mix::to_mono(&mut self.block);
        } else {
            self.block.fill(0);
        }
        mix::add(&mut self.block, &self.keepalive);
        if let Some(at) = self.click_at {
            let end = (at + DMA_FRAMES as usize).min(self.click.len());
            mix::add_mono(&mut self.block, &self.click[at..end], self.click_gain);
            self.click_at = (end < self.click.len()).then_some(end);
        }
    }

    /// Queues the block for the DMA; blocks while its buffers are full,
    /// which paces the mixer.
    fn write_block(&mut self) {
        for (s, b) in self.block.iter().zip(self.bytes.as_chunks_mut::<2>().0) {
            *b = s.to_le_bytes();
        }
        let timeout = TickType::from(Duration::from_millis(100)).ticks();
        if let Err(e) = self.i2s.write_all(&self.bytes, timeout) {
            log::warn!("speaker: {e}");
        }
    }
}
