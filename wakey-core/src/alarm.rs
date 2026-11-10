//! The alarm: its settings ([`AlarmConfig`], part of `config.json`) and a
//! state machine ([`Alarm`]) that decides when it rings, snoozes and stops.
//!
//! The firmware calls [`Alarm::tick`] about once a second and
//! [`Alarm::input`] on a button press, then makes the speaker follow
//! [`Alarm::is_ringing`]. [`State`] is small and serialisable; the firmware
//! keeps it in NVS so a reboot neither rings a finished alarm again nor
//! forgets a snooze.
//!
//! Time comes in as [`Now`]: local wall-clock time decides *when* the alarm
//! is due (06:30 is 06:30 in summer and winter), Unix time measures
//! *durations* (snooze, ring timeout), so daylight saving changes do not
//! stretch them. Daylight saving days:
//! - Spring (02:00 → 03:00): an alarm inside the skipped hour rings at
//!   03:00, because the alarm time was crossed between two ticks.
//! - Autumn (03:00 → 02:00): an alarm inside the repeated hour rings once;
//!   the day is marked done.
//!
//! Rules:
//! - Rings on the configured weekdays at the alarm time, at most once per
//!   day.
//! - Snooze (at most `max_snoozes` times) pauses for `snooze_min` minutes.
//! - Stop ends the alarm for the day, also while snoozed.
//! - Unanswered, it stops after `ring_timeout_min` and does not come back
//!   (counts as done).
//! - After a boot it still rings if the alarm time is less than the ring
//!   timeout ago (power cut, crash, wake from power-off a bit late). An
//!   alarm time crossed by a clock correction (NTP jumping more than that)
//!   does not ring.

use crate::datetime::{Date, DateTime};
use serde::{Deserialize, Serialize};

/// The `alarm` section of `config.json`.
///
/// ```json
/// "alarm": {
///   "enabled": true, "hour": 6, "minute": 30,
///   "weekdays": [1, 1, 1, 1, 1, 0, 0],
///   "sound": "sounds/morgen.wav",
///   "volume": 60, "fade_in_s": 10,
///   "snooze_min": 9, "max_snoozes": 3, "ring_timeout_min": 10
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AlarmConfig {
    pub enabled: bool,
    pub hour: u8,
    pub minute: u8,
    /// Monday to Sunday; non-zero rings.
    pub weekdays: [u8; 7],
    /// Path below the SD card root; `None` plays the built-in tone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sound: Option<String>,
    /// 0–100 % of the volume cap.
    pub volume: u8,
    pub fade_in_s: u16,
    pub snooze_min: u16,
    pub max_snoozes: u8,
    pub ring_timeout_min: u16,
}

impl Default for AlarmConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            hour: 6,
            minute: 30,
            weekdays: [1, 1, 1, 1, 1, 0, 0],
            sound: None,
            volume: 60,
            fade_in_s: 10,
            snooze_min: 9,
            max_snoozes: 3,
            ring_timeout_min: 10,
        }
    }
}

impl AlarmConfig {
    /// Brings out-of-range values into range (a hand-edited file should not
    /// stop the clock).
    pub fn sanitize(&mut self) {
        self.hour = self.hour.min(23);
        self.minute = self.minute.min(59);
        self.volume = self.volume.min(100);
        self.fade_in_s = self.fade_in_s.min(600);
        self.snooze_min = self.snooze_min.clamp(1, 60);
        self.max_snoozes = self.max_snoozes.min(10);
        self.ring_timeout_min = self.ring_timeout_min.clamp(1, 60);
    }

    /// `weekday`: 0 = Sunday, as in [`DateTime`].
    pub fn rings_on(&self, weekday: u8) -> bool {
        self.weekdays[(usize::from(weekday) + 6) % 7] != 0
    }

    fn second_of_day(&self) -> u32 {
        u32::from(self.hour) * 3600 + u32::from(self.minute) * 60
    }

    fn ring_timeout_s(&self) -> i64 {
        i64::from(self.ring_timeout_min) * 60
    }

    fn snooze_s(&self) -> i64 {
        i64::from(self.snooze_min) * 60
    }
}

/// The current time, both ways.
#[derive(Debug, Clone, Copy)]
pub struct Now {
    pub local: DateTime,
    pub unix: i64,
}

/// What survives a reboot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// The last day whose alarm has finished; it does not ring again that
    /// day.
    pub done_on: Option<Date>,
    pub phase: Phase,
}

impl State {
    /// For storage (NVS).
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("alarm state JSON")
    }

    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    #[default]
    Idle,
    /// Ringing for `day`'s alarm until `until` (Unix time), then silenced.
    Ringing { day: Date, snoozes: u8, until: i64 },
    /// Snoozed `snoozes` times; rings again at `until`.
    Snoozed { day: Date, snoozes: u8, until: i64 },
}

/// Buttons on the ringing screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Snooze,
    Stop,
}

pub struct Alarm {
    state: State,
    /// The previous tick, to see the alarm time being crossed.
    last: Option<Now>,
}

impl Alarm {
    /// Resumes from a saved state (`State::default()` the first time).
    pub fn new(state: State) -> Self {
        Self { state, last: None }
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn is_ringing(&self) -> bool {
        matches!(self.state.phase, Phase::Ringing { .. })
    }

    /// While ringing or snoozed: how many snoozes are left.
    pub fn snoozes_left(&self, cfg: &AlarmConfig) -> Option<u8> {
        match self.state.phase {
            Phase::Ringing { snoozes, .. } | Phase::Snoozed { snoozes, .. } => {
                Some(cfg.max_snoozes.saturating_sub(snoozes))
            }
            Phase::Idle => None,
        }
    }

    /// When snoozed: the Unix time it rings again.
    pub fn snoozed_until(&self) -> Option<i64> {
        match self.state.phase {
            Phase::Snoozed { until, .. } => Some(until),
            _ => None,
        }
    }

    /// Advances the state to `now`. Call about once a second.
    pub fn tick(&mut self, cfg: &AlarmConfig, now: Now) {
        let s = &mut self.state;
        s.phase = match s.phase {
            Phase::Ringing { day, .. } | Phase::Snoozed { day, .. } if s.done_on == Some(day) => {
                Phase::Idle
            }
            Phase::Ringing {
                day,
                snoozes,
                until,
            } => {
                if now.unix >= until {
                    s.done_on = Some(day);
                    Phase::Idle
                } else {
                    // A clock set back must not stretch the ringing.
                    let until = until.min(now.unix + cfg.ring_timeout_s());
                    Phase::Ringing {
                        day,
                        snoozes,
                        until,
                    }
                }
            }
            Phase::Snoozed {
                day,
                snoozes,
                until,
            } => {
                if now.unix >= until {
                    Phase::Ringing {
                        day,
                        snoozes,
                        until: now.unix + cfg.ring_timeout_s(),
                    }
                } else {
                    let until = until.min(now.unix + cfg.snooze_s());
                    Phase::Snoozed {
                        day,
                        snoozes,
                        until,
                    }
                }
            }
            Phase::Idle if due(cfg, s.done_on, self.last, now) => Phase::Ringing {
                day: now.local.date(),
                snoozes: 0,
                until: now.unix + cfg.ring_timeout_s(),
            },
            Phase::Idle => Phase::Idle,
        };
        self.last = Some(now);
    }

    /// A button press. Snooze beyond the limit is ignored.
    pub fn input(&mut self, cfg: &AlarmConfig, now: Now, input: Input) {
        let s = &mut self.state;
        match (s.phase, input) {
            (Phase::Ringing { day, snoozes, .. }, Input::Snooze) if snoozes < cfg.max_snoozes => {
                s.phase = Phase::Snoozed {
                    day,
                    snoozes: snoozes + 1,
                    until: now.unix + cfg.snooze_s(),
                };
            }
            (Phase::Ringing { day, .. } | Phase::Snoozed { day, .. }, Input::Stop) => {
                s.done_on = Some(day);
                s.phase = Phase::Idle;
            }
            _ => {}
        }
    }

    /// The next regular alarm after `now` (local time), not counting a
    /// snooze; `None` if the alarm is off.
    pub fn next(&self, cfg: &AlarmConfig, now: DateTime) -> Option<DateTime> {
        if !cfg.enabled {
            return None;
        }
        let today_open = self.state.phase == Phase::Idle
            && self.state.done_on != Some(now.date())
            && cfg.second_of_day() > now.second_of_day();
        (0..=7)
            .map(|d| now.add_days(d))
            .filter(|day| cfg.rings_on(day.weekday))
            .find(|day| day.date() != now.date() || today_open)
            .map(|day| DateTime {
                hour: cfg.hour,
                minute: cfg.minute,
                second: 0,
                ..day
            })
    }
}

/// Whether an idle alarm starts ringing at `now`.
fn due(cfg: &AlarmConfig, done_on: Option<Date>, last: Option<Now>, now: Now) -> bool {
    let today = now.local.date();
    if !cfg.enabled || !cfg.rings_on(now.local.weekday) || done_on == Some(today) {
        return false;
    }
    let alarm = cfg.second_of_day();
    let t = now.local.second_of_day();
    if t < alarm {
        return false;
    }
    let late = i64::from(t - alarm) <= cfg.ring_timeout_s();
    match last {
        // Boot: catch up within the ring timeout.
        None => late,
        // Crossed since the previous tick, and either recently or within
        // one tick (the skipped spring hour), not by a big clock jump.
        Some(last) => {
            let crossed = now.unix >= last.unix
                && (last.local.date() != today || last.local.second_of_day() < alarm);
            crossed && (late || now.unix - last.unix <= cfg.ring_timeout_s())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Europe/Zurich in 2026: CEST (UTC+2) from 29 March 01:00 UTC to
    /// 25 October 01:00 UTC.
    fn at_unix(unix: i64) -> Now {
        let dst = (1_774_746_000..1_792_890_000).contains(&unix);
        let offset = if dst { 7200 } else { 3600 };
        Now {
            local: DateTime::from_unix(unix + offset),
            unix,
        }
    }

    /// Local winter time (UTC+1) on a 2026 date.
    fn winter(month: u8, day: u8, hour: u8, minute: u8, second: u8) -> Now {
        let local = DateTime {
            year: 2026,
            month,
            day,
            hour,
            minute,
            second,
            weekday: 0,
        };
        at_unix(local.to_unix() - 3600)
    }

    fn cfg() -> AlarmConfig {
        AlarmConfig {
            enabled: true,
            weekdays: [1; 7],
            ..AlarmConfig::default()
        }
    }

    /// Ticks once a second from `from` to `to` (Unix times, inclusive).
    fn run(a: &mut Alarm, cfg: &AlarmConfig, from: i64, to: i64) {
        for t in from..=to {
            a.tick(cfg, at_unix(t));
        }
    }

    // Monday 2026-11-09, 06:30 local = 05:30 UTC.
    const ALARM: i64 = 1_794_202_200;

    #[test]
    fn rings_at_the_alarm_time() {
        let c = cfg();
        let mut a = Alarm::new(State::default());
        run(&mut a, &c, ALARM - 5, ALARM - 1);
        assert!(!a.is_ringing());
        a.tick(&c, at_unix(ALARM));
        assert!(a.is_ringing());
        assert_eq!(a.snoozes_left(&c), Some(3));
    }

    #[test]
    fn disabled_and_weekdays() {
        let mut c = cfg();
        c.enabled = false;
        let mut a = Alarm::new(State::default());
        run(&mut a, &c, ALARM - 1, ALARM + 1);
        assert!(!a.is_ringing());

        let mut c = cfg();
        c.weekdays = [0, 1, 1, 1, 1, 1, 1]; // not on Mondays
        let mut a = Alarm::new(State::default());
        run(&mut a, &c, ALARM - 1, ALARM + 1);
        assert!(!a.is_ringing());
        assert!(!c.rings_on(1) && c.rings_on(0) && c.rings_on(2));
    }

    #[test]
    fn stop_ends_the_day() {
        let c = cfg();
        let mut a = Alarm::new(State::default());
        run(&mut a, &c, ALARM - 1, ALARM);
        a.input(&c, at_unix(ALARM + 5), Input::Stop);
        assert!(!a.is_ringing());
        run(&mut a, &c, ALARM + 5, ALARM + 3600);
        assert!(!a.is_ringing());
        // Tomorrow it rings again.
        run(&mut a, &c, ALARM + 86_400 - 1, ALARM + 86_400);
        assert!(a.is_ringing());
    }

    #[test]
    fn snooze_three_times_then_only_stop() {
        let c = cfg();
        let mut a = Alarm::new(State::default());
        run(&mut a, &c, ALARM - 1, ALARM);
        let mut t = ALARM;
        for left in [2, 1, 0] {
            a.input(&c, at_unix(t + 10), Input::Snooze);
            assert!(!a.is_ringing());
            assert_eq!(a.snoozes_left(&c), Some(left));
            assert_eq!(a.snoozed_until(), Some(t + 10 + 9 * 60));
            run(&mut a, &c, t + 10, t + 10 + 9 * 60 - 1);
            assert!(!a.is_ringing());
            t += 10 + 9 * 60;
            a.tick(&c, at_unix(t));
            assert!(a.is_ringing());
        }
        a.input(&c, at_unix(t + 1), Input::Snooze);
        assert!(a.is_ringing(), "a fourth snooze is ignored");
        a.input(&c, at_unix(t + 2), Input::Stop);
        assert!(!a.is_ringing());
        assert_eq!(a.state().done_on, Some(at_unix(ALARM).local.date()));
    }

    #[test]
    fn stop_while_snoozed() {
        let c = cfg();
        let mut a = Alarm::new(State::default());
        run(&mut a, &c, ALARM - 1, ALARM);
        a.input(&c, at_unix(ALARM + 1), Input::Snooze);
        a.input(&c, at_unix(ALARM + 2), Input::Stop);
        run(&mut a, &c, ALARM + 2, ALARM + 3600);
        assert!(!a.is_ringing());
        assert_eq!(a.state().phase, Phase::Idle);
    }

    #[test]
    fn unanswered_alarm_is_silenced_for_good() {
        let c = cfg();
        let mut a = Alarm::new(State::default());
        run(&mut a, &c, ALARM - 1, ALARM + 10 * 60 - 1);
        assert!(a.is_ringing());
        a.tick(&c, at_unix(ALARM + 10 * 60));
        assert!(!a.is_ringing());
        run(&mut a, &c, ALARM + 10 * 60, ALARM + 7200);
        assert!(!a.is_ringing());
    }

    #[test]
    fn timeout_restarts_after_a_snooze() {
        let c = cfg();
        let mut a = Alarm::new(State::default());
        run(&mut a, &c, ALARM - 1, ALARM);
        a.input(&c, at_unix(ALARM + 9 * 60), Input::Snooze);
        let again = ALARM + 18 * 60;
        run(&mut a, &c, ALARM + 9 * 60, again);
        assert!(a.is_ringing());
        run(&mut a, &c, again, again + 10 * 60 - 1);
        assert!(a.is_ringing());
        a.tick(&c, at_unix(again + 10 * 60));
        assert!(!a.is_ringing());
    }

    #[test]
    fn boot_catches_up_within_the_ring_timeout() {
        let c = cfg();
        let mut a = Alarm::new(State::default());
        a.tick(&c, at_unix(ALARM + 5 * 60));
        assert!(a.is_ringing());

        let mut a = Alarm::new(State::default());
        a.tick(&c, at_unix(ALARM + 11 * 60));
        assert!(!a.is_ringing());
    }

    #[test]
    fn reboot_resumes_saved_state() {
        let c = cfg();
        let mut a = Alarm::new(State::default());
        run(&mut a, &c, ALARM - 1, ALARM);
        a.input(&c, at_unix(ALARM + 1), Input::Stop);
        let saved = State::from_json(&a.state().to_json()).unwrap();
        // Rebooted a minute later: does not ring again.
        let mut b = Alarm::new(saved);
        b.tick(&c, at_unix(ALARM + 60));
        assert!(!b.is_ringing());

        // Rebooted while snoozed: rings when the snooze ends.
        let mut a = Alarm::new(State::default());
        run(&mut a, &c, ALARM - 1, ALARM);
        a.input(&c, at_unix(ALARM + 1), Input::Snooze);
        let mut b = Alarm::new(a.state().clone());
        run(&mut b, &c, ALARM + 120, ALARM + 1 + 9 * 60);
        assert!(b.is_ringing());
        assert_eq!(b.snoozes_left(&c), Some(2));
    }

    #[test]
    fn alarm_set_just_after_its_time_waits_for_tomorrow() {
        let c = cfg();
        let mut a = Alarm::new(State::default());
        let mut off = c.clone();
        off.enabled = false;
        run(&mut a, &off, ALARM - 1, ALARM + 60);
        // Enabled a minute late: not now, tomorrow.
        run(&mut a, &c, ALARM + 61, ALARM + 120);
        assert!(!a.is_ringing());
        let next = a.next(&c, at_unix(ALARM + 120).local).unwrap();
        assert_eq!((next.day, next.hour, next.minute), (10, 6, 30));
    }

    #[test]
    fn clock_jump_across_the_alarm_does_not_ring() {
        let c = cfg();
        let mut a = Alarm::new(State::default());
        a.tick(&c, at_unix(ALARM - 3 * 3600));
        a.tick(&c, at_unix(ALARM + 3600)); // NTP corrects by four hours
        assert!(!a.is_ringing());
        // A small correction does.
        let mut a = Alarm::new(State::default());
        a.tick(&c, at_unix(ALARM - 30));
        a.tick(&c, at_unix(ALARM + 30));
        assert!(a.is_ringing());
    }

    #[test]
    fn clock_set_back_does_not_stretch_ringing() {
        let c = cfg();
        let mut a = Alarm::new(State::default());
        run(&mut a, &c, ALARM - 1, ALARM);
        a.tick(&c, at_unix(ALARM - 3600));
        run(&mut a, &c, ALARM - 3600, ALARM - 3600 + 10 * 60);
        assert!(!a.is_ringing());
    }

    #[test]
    fn spring_forward_skipped_hour() {
        // Sunday 2026-03-29: 02:00 CET → 03:00 CEST at 01:00 UTC.
        let mut c = cfg();
        (c.hour, c.minute) = (2, 30);
        let jump = 1_774_746_000;
        let mut a = Alarm::new(State::default());
        run(&mut a, &c, jump - 5, jump - 1);
        assert_eq!(at_unix(jump - 1).local.hms(), "01:59:59");
        assert!(!a.is_ringing());
        a.tick(&c, at_unix(jump));
        assert_eq!(at_unix(jump).local.hms(), "03:00:00");
        assert!(a.is_ringing());
    }

    #[test]
    fn autumn_repeated_hour_rings_once() {
        // Sunday 2026-10-25: 03:00 CEST → 02:00 CET at 01:00 UTC.
        let mut c = cfg();
        (c.hour, c.minute) = (2, 30);
        let back = 1_792_890_000;
        let first = back - 1800; // 02:30 CEST
        let mut a = Alarm::new(State::default());
        run(&mut a, &c, first - 1, first);
        assert_eq!(at_unix(first).local.hms(), "02:30:00");
        assert!(a.is_ringing());
        // Unanswered: silenced after 10 minutes.
        run(&mut a, &c, first, back + 3600);
        assert_eq!(at_unix(back + 1800).local.hms(), "02:30:00");
        assert!(!a.is_ringing());
        assert_eq!(a.state().done_on, Some(at_unix(first).local.date()));
    }

    #[test]
    fn rings_at_midnight() {
        let mut c = cfg();
        (c.hour, c.minute) = (0, 0);
        let midnight = winter(11, 10, 0, 0, 0).unix;
        let mut a = Alarm::new(State::default());
        run(&mut a, &c, midnight - 3, midnight);
        assert!(a.is_ringing());
    }

    #[test]
    fn next_alarm() {
        let mut c = cfg();
        c.weekdays = [1, 1, 1, 1, 1, 0, 0];
        let a = Alarm::new(State::default());
        // Monday 05:00 → today 06:30.
        let n = a.next(&c, winter(11, 9, 5, 0, 0).local).unwrap();
        assert_eq!(
            (n.day, n.weekday, n.hour, n.minute, n.second),
            (9, 1, 6, 30, 0)
        );
        // Friday 07:00 → Monday.
        let n = a.next(&c, winter(11, 13, 7, 0, 0).local).unwrap();
        assert_eq!((n.month, n.day, n.weekday), (11, 16, 1));
        // Done today → tomorrow.
        let a = Alarm::new(State {
            done_on: Some(winter(11, 9, 5, 0, 0).local.date()),
            phase: Phase::Idle,
        });
        let n = a.next(&c, winter(11, 9, 5, 0, 0).local).unwrap();
        assert_eq!(n.day, 10);
        // No weekdays or disabled → none.
        c.weekdays = [0; 7];
        assert!(a.next(&c, winter(11, 9, 5, 0, 0).local).is_none());
        c.weekdays = [1; 7];
        c.enabled = false;
        assert!(a.next(&c, winter(11, 9, 5, 0, 0).local).is_none());
    }

    #[test]
    fn config_json_and_sanitize() {
        let mut c: AlarmConfig =
            serde_json::from_str(r#"{ "enabled": true, "hour": 25, "minute": 5 }"#).unwrap();
        c.sanitize();
        assert_eq!(
            (c.enabled, c.hour, c.minute, c.max_snoozes),
            (true, 23, 5, 3)
        );
        assert_eq!(c.weekdays, [1, 1, 1, 1, 1, 0, 0]);
    }
}
