//! The alarms on the device: [`wakey_core::alarm::Alarms`] driven by the
//! clock, its state kept in NVS, and the speaker following it.
//!
//! [`AlarmClock::tick`] runs once a second (from the clock screen's timer)
//! and [`AlarmClock::input`] on the ringing screen's buttons, both on the UI
//! thread. While an alarm rings, the player rings that alarm's sound (or the
//! built-in tone) with its fade-in on the USB speaker, or on the built-in
//! one if none is connected ([`Output`]); otherwise both are stopped.

use crate::audio::output::Output;
use crate::audio::player::{Player, Sound as PlayerSound};
use crate::{clock, config, sounds};
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use wakey_core::alarm::{self, AlarmConfig, Alarms, Input, Phase, RingConfig, State};
use wakey_core::datetime::DateTime;
use wakey_core::tone;

const NVS_NAMESPACE: &str = "wakey";
const NVS_KEY: &str = "alarms";

pub struct AlarmClock {
    alarms: Vec<AlarmConfig>,
    ring: RingConfig,
    machine: Alarms,
    nvs: Option<EspNvs<NvsDefault>>,
    saved: State,
    player: Player,
    output: Output,
    /// The alarm the speaker is ringing for right now.
    sounding: Option<u32>,
    /// A sound page preview is playing.
    previewing: bool,
    /// Set to end a preview's stream (a track still downloading): its
    /// reader stops waiting for data.
    stream_cancel: Option<Arc<AtomicBool>>,
}

/// What the screens show of the alarms.
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
        alarms: Vec<AlarmConfig>,
        ring: RingConfig,
        nvs: EspDefaultNvsPartition,
        player: Player,
        output: Output,
    ) -> Self {
        let nvs = EspNvs::new(nvs, NVS_NAMESPACE, true)
            .inspect_err(|e| log::error!("alarm: NVS: {e}; the state will not survive a reboot"))
            .ok();
        let saved = nvs.as_ref().map(load_state).unwrap_or_default();
        log::info!("alarm: {} alarms, {ring:?}, saved {saved:?}", alarms.len());
        for a in &alarms {
            log::info!("alarm: {a:?}");
        }
        Self {
            alarms,
            ring,
            machine: Alarms::new(saved.clone()),
            nvs,
            saved,
            player,
            output,
            sounding: None,
            previewing: false,
            stream_cancel: None,
        }
    }

    pub fn alarms(&self) -> &[AlarmConfig] {
        &self.alarms
    }

    pub fn get(&self, id: u32) -> Option<&AlarmConfig> {
        self.alarms.iter().find(|a| a.id == id)
    }

    /// A new list: written to `config.json`, then in effect. On a write
    /// error nothing changes.
    pub fn set_alarms(&mut self, mut alarms: Vec<AlarmConfig>) -> std::io::Result<()> {
        alarm::sanitize_list(&mut alarms);
        config::save_alarms(&alarms)?;
        self.machine.reconfigured(&alarms, clock::now());
        self.alarms = alarms;
        self.follow();
        Ok(())
    }

    /// Adds `cfg` (id 0: a new alarm) or replaces the alarm with its id.
    pub fn save(&mut self, mut cfg: AlarmConfig) -> std::io::Result<()> {
        let mut list = self.alarms.clone();
        match list.iter_mut().find(|a| a.id == cfg.id && cfg.id != 0) {
            Some(a) => *a = cfg,
            None => {
                cfg.id = alarm::new_id(&list);
                list.push(cfg);
            }
        }
        log::info!("alarm: saving {list:?}");
        self.set_alarms(list)
    }

    pub fn delete(&mut self, id: u32) -> std::io::Result<()> {
        log::info!("alarm: deleting {id}");
        let list = self.alarms.iter().filter(|a| a.id != id).cloned().collect();
        self.set_alarms(list)
    }

    pub fn set_enabled(&mut self, id: u32, enabled: bool) -> std::io::Result<()> {
        let mut list = self.alarms.clone();
        if let Some(a) = list.iter_mut().find(|a| a.id == id) {
            a.enabled = enabled;
        }
        self.set_alarms(list)
    }

    pub fn tick(&mut self) {
        self.machine.tick(&self.alarms, &self.ring, clock::moment());
        self.follow();
        // The USB speaker may have come or gone.
        self.output.update();
    }

    pub fn input(&mut self, input: Input) {
        log::info!("alarm: {input:?}");
        self.machine.input(&self.ring, clock::moment(), input);
        self.follow();
    }

    /// Plays `cfg`'s sound at its volume, without fade-in, until
    /// [`stop_preview`](Self::stop_preview). Not while an alarm rings.
    ///
    /// Never the built-in tone in place of the sound (only if the tone is
    /// the sound): a TIDAL track still downloading plays from what has
    /// arrived ([`crate::tidal::Tidal::stream`]); a sound that cannot be
    /// played (not on the card, download failed) is silent. A real alarm
    /// still rings the tone then.
    pub fn preview(&mut self, cfg: &AlarmConfig, tidal: &crate::tidal::Tidal) {
        if self.sounding.is_some() {
            return;
        }
        self.cancel_stream();
        let sound = match (
            sounds::resolve(&cfg.sound, cfg.id, clock::now().date()),
            &cfg.sound,
        ) {
            (Some(path), _) => Some(PlayerSound::File(path)),
            (None, alarm::Sound::Beep) => Some(PlayerSound::Tone),
            (None, alarm::Sound::TidalTrack(id)) => id.parse().ok().and_then(|id| {
                let cancel = Arc::new(AtomicBool::new(false));
                let stream = tidal.stream(id, cancel.clone())?;
                self.stream_cancel = Some(cancel);
                Some(PlayerSound::Stream(
                    format!("TIDAL {id} (downloading)"),
                    Box::new(stream),
                ))
            }),
            (None, _) => None,
        };
        self.previewing = true;
        match sound {
            Some(sound) => {
                log::info!("alarm: preview {:?} at {} %", cfg.sound, cfg.volume);
                self.output.set_streaming(true);
                self.player.preview(sound, tone::volume_gain(cfg.volume));
            }
            None => {
                // Silent; a sound previewed before stops.
                log::info!("alarm: preview {:?}: cannot be played, silent", cfg.sound);
                self.player.stop();
                self.output.set_streaming(false);
            }
        }
    }

    /// Ends a preview's stream: its reader stops waiting, so the player's
    /// decoder ends at once (the player waits for it before the next sound).
    fn cancel_stream(&mut self) {
        if let Some(cancel) = self.stream_cancel.take() {
            cancel.store(true, Ordering::Release);
        }
    }

    /// The volume slider moved during a preview.
    pub fn preview_volume(&mut self, volume: u8) {
        if self.previewing {
            self.player.set_gain(tone::volume_gain(volume));
        }
    }

    pub fn stop_preview(&mut self) {
        self.cancel_stream();
        if self.previewing {
            log::info!(
                "alarm: preview stopped (underruns so far: {})",
                crate::audio::player::UNDERRUNS.load(Ordering::Relaxed)
            );
            self.previewing = false;
            if self.sounding.is_none() {
                self.player.stop();
                self.output.set_streaming(false);
            }
        }
    }

    pub fn is_previewing(&self) -> bool {
        self.previewing
    }

    /// When `cfg` (possibly being edited, not saved) rings next.
    pub fn next_of(&self, cfg: &AlarmConfig) -> Option<DateTime> {
        self.machine.next_of(cfg, clock::now())
    }

    /// The alarm that rings next.
    pub fn next_id(&self) -> Option<u32> {
        self.machine
            .next(&self.alarms, clock::now())
            .map(|(id, _)| id)
    }

    /// Seconds until something rings: the next alarm, or a snoozed one
    /// again (0 while ringing).
    pub fn secs_to_ring(&self) -> Option<i64> {
        let m = clock::moment();
        match self.machine.state().phase {
            Phase::Ringing { .. } => Some(0),
            Phase::Snoozed { until, .. } => Some(until - m.unix),
            Phase::Idle => self
                .machine
                .next(&self.alarms, m.local)
                .map(|(_, t)| t.to_unix() - m.local.to_unix()),
        }
    }

    pub fn view(&self) -> View {
        let snoozes_left = self.machine.snoozes_left(&self.ring).unwrap_or(0);
        match self.machine.state().phase {
            Phase::Idle => View::Idle {
                next: self
                    .machine
                    .next(&self.alarms, clock::now())
                    .map(|(_, t)| t),
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
        let state = self.machine.state();
        if *state != self.saved {
            log::info!("alarm: {:?}", state.phase);
            if let Some(nvs) = &self.nvs {
                save_state(nvs, state);
            }
            self.saved = state.clone();
        }
        let ringing = self.machine.ringing();
        if ringing == self.sounding {
            return;
        }
        self.sounding = ringing;
        // An alarm takes over from a preview.
        self.previewing = false;
        self.cancel_stream();
        match ringing.and_then(|id| self.get(id)).cloned() {
            Some(cfg) => {
                let sound = match sounds::resolve(&cfg.sound, cfg.id, clock::now().date()) {
                    Some(path) => PlayerSound::File(path),
                    None => PlayerSound::Tone,
                };
                self.output.set_streaming(true);
                self.player
                    .ring(sound, tone::volume_gain(cfg.volume), cfg.fade_in_s);
                if self.output.on_builtin() {
                    log::info!("alarm: no USB speaker, ringing on the built-in one");
                }
            }
            None => {
                self.player.stop();
                self.output.set_streaming(false);
            }
        }
    }
}

fn load_state(nvs: &EspNvs<NvsDefault>) -> State {
    let mut buf = [0u8; 1024];
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

fn save_state(nvs: &EspNvs<NvsDefault>, state: &State) {
    if let Err(e) = nvs.set_str(NVS_KEY, &state.to_json()) {
        log::error!("alarm: saving the state: {e}");
    }
}
