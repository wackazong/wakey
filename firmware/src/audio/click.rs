//! The wheel click on the Tab5's built-in speaker (ES8388 codec, NS4150B
//! amplifier; see [`crate::board::codec`]).
//!
//! The I2S stream runs all the time and plays silence by itself
//! (`auto_clear`); a thread writes a click when asked. The amplifier is
//! switched on with the first click and off after a few seconds without
//! one, so the speaker does not hiss and draw current while unused.

use crate::board::codec::{self, CodecPins};
use crate::board::i2c::SharedI2c;
use crate::board::ioexp::{IoExpanders, Pin};
use esp_idf_svc::hal::delay::TickType;
use esp_idf_svc::hal::i2s::config::{
    Config, DataBitWidth, MclkMultiple, SlotMode, StdClkConfig, StdConfig, StdGpioConfig,
    StdSlotConfig,
};
use esp_idf_svc::hal::i2s::{I2sDriver, I2sTx};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::time::Duration;
use wakey_core::tone;

const SAMPLE_RATE: u32 = 48_000;
/// Click loudness, 0–1 of full scale (0.8 was a bit loud; at 0.4, with
/// 128-frame DMA buffers, the click was gone).
const AMPLITUDE: f32 = 0.6;
/// The amplifier goes off after this long without a click.
const AMP_OFF_AFTER: Duration = Duration::from_secs(3);
/// DMA: 3 × 5 ms; a click starts within ~15 ms. (240 frames is what was
/// heard to work; 120 frames, which the driver rounds to 128, was not.)
const DMA_BUFFERS: u32 = 3;
const DMA_FRAMES: u32 = 240;

#[derive(Default)]
pub struct Clicker {
    tx: Option<SyncSender<()>>,
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

        let (tx, rx) = mpsc::sync_channel::<()>(4);
        let samples: Vec<u8> = tone::click(SAMPLE_RATE, AMPLITUDE)
            .iter()
            .flat_map(|&s| {
                let b = s.to_le_bytes();
                [b[0], b[1], b[0], b[1]]
            })
            .collect();
        std::thread::Builder::new()
            .name("click".into())
            .stack_size(4096)
            .spawn(move || {
                let timeout = TickType::from(Duration::from_millis(100)).ticks();
                let mut amp_on = false;
                loop {
                    let next = if amp_on {
                        rx.recv_timeout(AMP_OFF_AFTER)
                    } else {
                        rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
                    };
                    match next {
                        Ok(()) => {
                            if !amp_on {
                                amp_on = ioexp.set(Pin::SpkEn, true).is_ok();
                                log::info!("click: amplifier on");
                                // Let the amplifier start; the first click
                                // would be cut off.
                                std::thread::sleep(Duration::from_millis(30));
                            }
                            if let Err(e) = i2s.write_all(&samples, timeout) {
                                log::warn!("click: {e}");
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {
                            if amp_on {
                                let _ = ioexp.set(Pin::SpkEn, false);
                                amp_on = false;
                                log::info!("click: amplifier off");
                            }
                        }
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
            })?;
        Ok(Self { tx: Some(tx) })
    }

    /// One tick; returns at once (dropped if clicks pile up).
    pub fn click(&self) {
        if let Some(tx) = &self.tx {
            let _ = tx.try_send(());
        }
    }
}
