//! The alarm on the device: [`wakey_core::alarm::Alarm`] driven by the
//! clock, its state kept in NVS, and the speaker following it.
//!
//! [`AlarmClock::tick`] runs once a second (from the clock screen's timer)
//! and [`AlarmClock::input`] on the ringing screen's buttons, both on the UI
//! thread. While the state machine says "ringing", the USB stream runs and
//! the player rings the configured sound (or the built-in tone) with its
//! fade-in; otherwise both are stopped.

use crate::audio::player::{Player, Sound};
use crate::audio::uac::UsbAudio;
use crate::board::sd;
use crate::{clock, config};
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use std::path::Path;
use wakey_core::alarm::{Alarm, AlarmConfig, Input, Phase, State};
use wakey_core::datetime::DateTime;
use wakey_core::tone;

const NVS_NAMESPACE: &str = "wakey";
const NVS_KEY: &str = "alarm";

pub struct AlarmClock {
    cfg: AlarmConfig,
    alarm: Alarm,
    nvs: Option<EspNvs<NvsDefault>>,
    saved: State,
    player: Player,
    speaker: UsbAudio,
    /// Whether the speaker is ringing right now.
    sounding: bool,
}

/// What the screen shows of the alarm.
pub enum View {
    /// The next alarm, if one is set.
    Idle {
        next: Option<DateTime>,
    },
    Ringing {
        snoozes_left: u8,
    },
    Snoozed {
        until: DateTime,
        snoozes_left: u8,
    },
}

impl AlarmClock {
    /// Resumes from the state saved in NVS.
    pub fn new(
        cfg: AlarmConfig,
        nvs: EspDefaultNvsPartition,
        player: Player,
        speaker: UsbAudio,
    ) -> Self {
        let nvs = EspNvs::new(nvs, NVS_NAMESPACE, true)
            .inspect_err(|e| log::error!("alarm: NVS: {e}; the state will not survive a reboot"))
            .ok();
        let saved = nvs.as_ref().map(load).unwrap_or_default();
        log::info!("alarm: {cfg:?}, saved {saved:?}");
        Self {
            cfg,
            alarm: Alarm::new(saved.clone()),
            nvs,
            saved,
            player,
            speaker,
            sounding: false,
        }
    }

    pub fn config(&self) -> &AlarmConfig {
        &self.cfg
    }

    /// New settings: written to `config.json`, then in effect. On a write
    /// error nothing changes.
    pub fn set_config(&mut self, mut cfg: AlarmConfig) -> std::io::Result<()> {
        cfg.sanitize();
        config::save_alarm(&cfg)?;
        log::info!("alarm: new settings {cfg:?}");
        self.alarm.reconfigured(&cfg, clock::now());
        self.cfg = cfg;
        self.follow();
        Ok(())
    }

    pub fn tick(&mut self) {
        self.alarm.tick(&self.cfg, clock::moment());
        self.follow();
    }

    pub fn input(&mut self, input: Input) {
        log::info!("alarm: {input:?}");
        self.alarm.input(&self.cfg, clock::moment(), input);
        self.follow();
    }

    pub fn view(&self) -> View {
        let snoozes_left = self.alarm.snoozes_left(&self.cfg).unwrap_or(0);
        match self.alarm.state().phase {
            Phase::Idle => View::Idle {
                next: self.alarm.next(&self.cfg, clock::now()),
            },
            Phase::Ringing { .. } => View::Ringing { snoozes_left },
            Phase::Snoozed { until, .. } => View::Snoozed {
                until: clock::local(until),
                snoozes_left,
            },
        }
    }

    /// Saves a changed state and makes the speaker follow it.
    fn follow(&mut self) {
        let state = self.alarm.state();
        if *state != self.saved {
            log::info!("alarm: {:?}", state.phase);
            if let Some(nvs) = &self.nvs {
                save(nvs, state);
            }
            self.saved = state.clone();
        }
        let ring = self.alarm.is_ringing();
        if ring == self.sounding {
            return;
        }
        self.sounding = ring;
        if ring {
            let sound = match &self.cfg.sound {
                Some(file) => Sound::File(Path::new(sd::MOUNT_POINT).join(file)),
                None => Sound::Tone,
            };
            self.speaker.set_streaming(true);
            self.player.ring(
                sound,
                tone::volume_gain(self.cfg.volume),
                self.cfg.fade_in_s,
            );
            if !self.speaker.connected() {
                log::warn!("alarm: ringing, but no speaker is connected");
            }
        } else {
            self.player.stop();
            self.speaker.set_streaming(false);
        }
    }
}

fn load(nvs: &EspNvs<NvsDefault>) -> State {
    let mut buf = [0u8; 256];
    match nvs.get_str(NVS_KEY, &mut buf) {
        Ok(Some(json)) => State::from_json(json).unwrap_or_else(|e| {
            log::warn!("alarm: saved state {json:?}: {e}");
            State::default()
        }),
        Ok(None) => State::default(),
        Err(e) => {
            log::error!("alarm: reading the saved state: {e}");
            State::default()
        }
    }
}

fn save(nvs: &EspNvs<NvsDefault>, state: &State) {
    if let Err(e) = nvs.set_str(NVS_KEY, &state.to_json()) {
        log::error!("alarm: saving the state: {e}");
    }
}
