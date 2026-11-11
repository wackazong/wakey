//! The alarms: their settings ([`AlarmConfig`], the `alarms` list in
//! `config.json`, plus [`RingConfig`] shared by all) and a state machine
//! ([`Alarms`]) that decides which one rings, snoozes and stops.
//!
//! The firmware calls [`Alarms::tick`] about once a second and
//! [`Alarms::input`] on a button press, then makes the speaker follow
//! [`Alarms::ringing`]. [`State`] is small and serialisable; the firmware
//! keeps it in NVS so a reboot neither rings a finished alarm again nor
//! forgets a snooze.
//!
//! Time comes in as [`Now`]: local wall-clock time decides *when* an alarm
//! is due (06:30 is 06:30 in summer and winter), Unix time measures
//! *durations* (snooze, ring timeout), so daylight saving changes do not
//! stretch them. Daylight saving days:
//! - Spring (02:00 → 03:00): an alarm inside the skipped hour rings at
//!   03:00, because the alarm time was crossed between two ticks.
//! - Autumn (03:00 → 02:00): an alarm inside the repeated hour rings once;
//!   the day is marked done.
//!
//! Rules:
//! - Each alarm rings on its weekdays at its time, at most once per day.
//! - One alarm is active (ringing or snoozed) at a time. An alarm that
//!   becomes due while another is snoozed takes over (the snoozed one is
//!   done for the day); one that becomes due while another rings is folded
//!   into that ringing (done, no second ring).
//! - Snooze (at most `max_snoozes` times) pauses for `snooze_min` minutes.
//! - Stop ends the active alarm for the day, also while snoozed.
//! - Unanswered, it stops after `ring_timeout_min` and does not come back
//!   (counts as done).
//! - After a boot an alarm still rings if its time is less than the ring
//!   timeout ago (power cut, crash, wake from power-off a bit late). An
//!   alarm time crossed by a clock correction (NTP jumping more than that)
//!   does not ring.
//! - Deleting or switching off the active alarm ends it.

use crate::datetime::{Date, DateTime};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// What an alarm plays. In `config.json` a string: `"beep"`, `"random"`,
/// `"tidal:random"`, `"tidal:<track id>"`, or a file path below the SD
/// card root (`"sounds/morgen.wav"`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum Sound {
    /// The built-in tone.
    #[default]
    Beep,
    File(String),
    /// A different file from `sounds/` each day ([`pick_daily`]).
    RandomFile,
    /// A track of the synced TIDAL playlist (V2).
    TidalTrack(String),
    /// A different track of the TIDAL playlist each day (V2).
    TidalRandom,
}

impl From<String> for Sound {
    fn from(s: String) -> Self {
        match s.as_str() {
            "" | "beep" => Sound::Beep,
            "random" => Sound::RandomFile,
            "tidal:random" => Sound::TidalRandom,
            _ => match s.strip_prefix("tidal:") {
                Some(id) => Sound::TidalTrack(id.into()),
                None => Sound::File(s),
            },
        }
    }
}

impl From<Sound> for String {
    fn from(s: Sound) -> Self {
        match s {
            Sound::Beep => "beep".into(),
            Sound::File(path) => path,
            Sound::RandomFile => "random".into(),
            Sound::TidalTrack(id) => format!("tidal:{id}"),
            Sound::TidalRandom => "tidal:random".into(),
        }
    }
}

/// One entry of the `alarms` list in `config.json`.
///
/// ```json
/// { "id": 1, "name": "Arbeit", "enabled": true, "hour": 6, "minute": 30,
///   "weekdays": [1, 1, 1, 1, 1, 0, 0], "sound": "random",
///   "volume": 60, "fade_in_s": 10 }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AlarmConfig {
    /// Unique within the list; keeps the alarm's state across edits.
    pub id: u32,
    /// Optional, shown in the list.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub enabled: bool,
    pub hour: u8,
    pub minute: u8,
    /// Monday to Sunday; non-zero rings.
    pub weekdays: [u8; 7],
    pub sound: Sound,
    /// 0–100 % of the volume cap.
    pub volume: u8,
    pub fade_in_s: u16,
}

/// A new alarm: on, 07:00, Monday to Friday, the built-in tone.
impl Default for AlarmConfig {
    fn default() -> Self {
        Self {
            id: 0,
            name: String::new(),
            enabled: true,
            hour: 7,
            minute: 0,
            weekdays: [1, 1, 1, 1, 1, 0, 0],
            sound: Sound::Beep,
            volume: 60,
            fade_in_s: 10,
        }
    }
}

/// Longest name kept, in characters.
pub const NAME_MAX: usize = 24;

impl AlarmConfig {
    /// Brings out-of-range values into range (a hand-edited file should not
    /// stop the clock).
    pub fn sanitize(&mut self) {
        self.hour = self.hour.min(23);
        self.minute = self.minute.min(59);
        self.volume = self.volume.min(100);
        self.fade_in_s = self.fade_in_s.min(600);
        self.name = self.name.trim().chars().take(NAME_MAX).collect();
    }

    /// `weekday`: 0 = Sunday, as in [`DateTime`].
    pub fn rings_on(&self, weekday: u8) -> bool {
        self.weekdays[(usize::from(weekday) + 6) % 7] != 0
    }

    pub fn any_day(&self) -> bool {
        self.weekdays.iter().any(|&d| d != 0)
    }

    /// The weekdays as bits: Monday = 1, Tuesday = 2, … Sunday = 64.
    pub fn weekday_bits(&self) -> u8 {
        self.weekdays
            .iter()
            .enumerate()
            .filter(|&(_, &d)| d != 0)
            .fold(0, |bits, (i, _)| bits | 1 << i)
    }

    fn second_of_day(&self) -> u32 {
        u32::from(self.hour) * 3600 + u32::from(self.minute) * 60
    }
}

/// The `ringing` section of `config.json`, the same for every alarm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RingConfig {
    pub snooze_min: u16,
    pub max_snoozes: u8,
    pub ring_timeout_min: u16,
}

impl Default for RingConfig {
    fn default() -> Self {
        Self {
            snooze_min: 9,
            max_snoozes: 3,
            ring_timeout_min: 10,
        }
    }
}

impl RingConfig {
    pub fn sanitize(&mut self) {
        self.snooze_min = self.snooze_min.clamp(1, 60);
        self.max_snoozes = self.max_snoozes.min(10);
        self.ring_timeout_min = self.ring_timeout_min.clamp(1, 60);
    }

    fn timeout_s(&self) -> i64 {
        i64::from(self.ring_timeout_min) * 60
    }

    fn snooze_s(&self) -> i64 {
        i64::from(self.snooze_min) * 60
    }
}

/// Sanitizes every alarm and gives each a unique, non-zero id (keeping the
/// existing ones), so a hand-written list works.
pub fn sanitize_list(alarms: &mut [AlarmConfig]) {
    let mut seen = std::collections::BTreeSet::new();
    let mut next = alarms.iter().map(|a| a.id).max().unwrap_or(0);
    for a in alarms.iter_mut() {
        a.sanitize();
        if a.id == 0 || !seen.insert(a.id) {
            next += 1;
            a.id = next;
            seen.insert(next);
        }
    }
}

/// An id for a new alarm.
pub fn new_id(alarms: &[AlarmConfig]) -> u32 {
    alarms.iter().map(|a| a.id).max().unwrap_or(0) + 1
}

/// The list sorted by time of day (then id), as the list page shows it.
pub fn sorted(alarms: &[AlarmConfig]) -> Vec<AlarmConfig> {
    let mut v = alarms.to_vec();
    v.sort_by_key(|a| (a.hour, a.minute, a.id));
    v
}

/// Index into `n` choices for alarm `id` on `date`: fixed for the day (a
/// reboot while ringing plays the same sound), usually different from one
/// day to the next.
pub fn pick_daily(n: usize, date: Date, id: u32) -> usize {
    if n == 0 {
        return 0;
    }
    // SplitMix64 over the date and the id.
    let mut x = (u64::from(date.year) << 40)
        ^ (u64::from(date.month) << 32)
        ^ (u64::from(date.day) << 24)
        ^ u64::from(id);
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    (x % n as u64) as usize
}

/// The current time, both ways.
#[derive(Debug, Clone, Copy)]
pub struct Now {
    pub local: DateTime,
    pub unix: i64,
}

/// What survives a reboot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// Per alarm id: the last day it finished; it does not ring again that
    /// day.
    pub done: BTreeMap<u32, Date>,
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
    /// Alarm `id` rings for `day` until `until` (Unix time), then silenced.
    Ringing {
        id: u32,
        day: Date,
        snoozes: u8,
        until: i64,
    },
    /// Alarm `id`, snoozed `snoozes` times; rings again at `until`.
    Snoozed {
        id: u32,
        day: Date,
        snoozes: u8,
        until: i64,
    },
}

impl Phase {
    /// The alarm ringing or snoozed.
    pub fn active(&self) -> Option<u32> {
        match *self {
            Phase::Ringing { id, .. } | Phase::Snoozed { id, .. } => Some(id),
            Phase::Idle => None,
        }
    }
}

/// Buttons on the ringing screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Snooze,
    Stop,
}

pub struct Alarms {
    state: State,
    /// The previous tick, to see alarm times being crossed.
    last: Option<Now>,
}

impl Alarms {
    /// Resumes from a saved state (`State::default()` the first time).
    pub fn new(state: State) -> Self {
        Self { state, last: None }
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    /// The alarm ringing right now.
    pub fn ringing(&self) -> Option<u32> {
        match self.state.phase {
            Phase::Ringing { id, .. } => Some(id),
            _ => None,
        }
    }

    pub fn is_ringing(&self) -> bool {
        self.ringing().is_some()
    }

    /// While ringing or snoozed: how many snoozes are left.
    pub fn snoozes_left(&self, ring: &RingConfig) -> Option<u8> {
        match self.state.phase {
            Phase::Ringing { snoozes, .. } | Phase::Snoozed { snoozes, .. } => {
                Some(ring.max_snoozes.saturating_sub(snoozes))
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
    pub fn tick(&mut self, alarms: &[AlarmConfig], ring: &RingConfig, now: Now) {
        let s = &mut self.state;
        s.phase = match s.phase {
            Phase::Ringing { id, day, .. } | Phase::Snoozed { id, day, .. }
                if s.done.get(&id) == Some(&day) =>
            {
                Phase::Idle
            }
            Phase::Ringing {
                id,
                day,
                snoozes,
                until,
            } => {
                if now.unix >= until {
                    s.done.insert(id, day);
                    Phase::Idle
                } else {
                    // A clock set back must not stretch the ringing.
                    let until = until.min(now.unix + ring.timeout_s());
                    Phase::Ringing {
                        id,
                        day,
                        snoozes,
                        until,
                    }
                }
            }
            Phase::Snoozed {
                id,
                day,
                snoozes,
                until,
            } => {
                if now.unix >= until {
                    Phase::Ringing {
                        id,
                        day,
                        snoozes,
                        until: now.unix + ring.timeout_s(),
                    }
                } else {
                    let until = until.min(now.unix + ring.snooze_s());
                    Phase::Snoozed {
                        id,
                        day,
                        snoozes,
                        until,
                    }
                }
            }
            Phase::Idle => Phase::Idle,
        };

        let active = s.phase.active();
        let today = now.local.date();
        let due: Vec<u32> = alarms
            .iter()
            .filter(|a| Some(a.id) != active)
            .filter(|a| due(a, ring, s.done.get(&a.id).copied(), self.last, now))
            .map(|a| a.id)
            .collect();
        if let Some(&first) = due.first() {
            match s.phase {
                Phase::Ringing { .. } => {
                    // Folded into the one already ringing.
                    for id in due {
                        s.done.insert(id, today);
                    }
                }
                Phase::Snoozed { id, day, .. } => {
                    s.done.insert(id, day);
                    s.phase = ringing(first, today, ring, now);
                    for &id in &due[1..] {
                        s.done.insert(id, today);
                    }
                }
                Phase::Idle => {
                    s.phase = ringing(first, today, ring, now);
                    for &id in &due[1..] {
                        s.done.insert(id, today);
                    }
                }
            }
        }
        self.last = Some(now);
    }

    /// A button press. Snooze beyond the limit is ignored.
    pub fn input(&mut self, ring: &RingConfig, now: Now, input: Input) {
        let s = &mut self.state;
        match (s.phase, input) {
            (
                Phase::Ringing {
                    id, day, snoozes, ..
                },
                Input::Snooze,
            ) if snoozes < ring.max_snoozes => {
                s.phase = Phase::Snoozed {
                    id,
                    day,
                    snoozes: snoozes + 1,
                    until: now.unix + ring.snooze_s(),
                };
            }
            (Phase::Ringing { id, day, .. } | Phase::Snoozed { id, day, .. }, Input::Stop) => {
                s.done.insert(id, day);
                s.phase = Phase::Idle;
            }
            _ => {}
        }
    }

    /// Call after the list changed (`alarms` is the new one).
    /// - An alarm already finished today rings again today if its new time
    ///   is still ahead; one moved to a time already past stays done, so a
    ///   reboot does not catch it up.
    /// - The active alarm ends if it was deleted or switched off.
    /// - Days of deleted alarms are forgotten.
    pub fn reconfigured(&mut self, alarms: &[AlarmConfig], now: DateTime) {
        let s = &mut self.state;
        s.done.retain(|id, _| alarms.iter().any(|a| a.id == *id));
        for a in alarms {
            if s.phase.active() != Some(a.id)
                && s.done.get(&a.id) == Some(&now.date())
                && a.second_of_day() > now.second_of_day()
            {
                s.done.remove(&a.id);
            }
        }
        if let Some(id) = s.phase.active()
            && !alarms.iter().any(|a| a.id == id && a.enabled)
        {
            s.phase = Phase::Idle;
        }
    }

    /// The next regular ringing of `cfg` after `now` (local time), not
    /// counting a snooze; `None` if it is off or has no weekday.
    pub fn next_of(&self, cfg: &AlarmConfig, now: DateTime) -> Option<DateTime> {
        if !cfg.enabled {
            return None;
        }
        let today_open = self.state.phase.active() != Some(cfg.id)
            && self.state.done.get(&cfg.id) != Some(&now.date())
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

    /// The alarm that rings next after `now`, and when.
    pub fn next(&self, alarms: &[AlarmConfig], now: DateTime) -> Option<(u32, DateTime)> {
        alarms
            .iter()
            .filter_map(|a| Some((a.id, self.next_of(a, now)?)))
            .min_by_key(|(id, t)| (t.to_unix(), *id))
    }
}

fn ringing(id: u32, day: Date, ring: &RingConfig, now: Now) -> Phase {
    Phase::Ringing {
        id,
        day,
        snoozes: 0,
        until: now.unix + ring.timeout_s(),
    }
}

/// German short form of a weekday selection (Monday first), with runs of
/// three or more days joined: "Mo–Fr", "Sa, So", "Mo, Mi–Fr", "täglich",
/// "" for none.
pub fn weekdays_de(days: &[u8; 7]) -> String {
    const SHORT: [&str; 7] = ["Mo", "Di", "Mi", "Do", "Fr", "Sa", "So"];
    if days.iter().all(|&d| d != 0) {
        return "täglich".into();
    }
    let mut parts = Vec::new();
    let mut i = 0;
    while i < 7 {
        if days[i] == 0 {
            i += 1;
            continue;
        }
        let start = i;
        while i + 1 < 7 && days[i + 1] != 0 {
            i += 1;
        }
        match i - start {
            0 => parts.push(SHORT[start].to_string()),
            1 => {
                parts.push(SHORT[start].to_string());
                parts.push(SHORT[i].to_string());
            }
            _ => parts.push(format!("{}–{}", SHORT[start], SHORT[i])),
        }
        i += 1;
    }
    parts.join(", ")
}

/// Whether alarm `cfg` starts ringing at `now`.
fn due(
    cfg: &AlarmConfig,
    ring: &RingConfig,
    done_on: Option<Date>,
    last: Option<Now>,
    now: Now,
) -> bool {
    let today = now.local.date();
    if !cfg.enabled || !cfg.rings_on(now.local.weekday) || done_on == Some(today) {
        return false;
    }
    let alarm = cfg.second_of_day();
    let t = now.local.second_of_day();
    if t < alarm {
        return false;
    }
    let late = i64::from(t - alarm) <= ring.timeout_s();
    match last {
        // Boot: catch up within the ring timeout.
        None => late,
        // Crossed since the previous tick, and either recently or within
        // one tick (the skipped spring hour), not by a big clock jump.
        Some(last) => {
            let crossed = now.unix >= last.unix
                && (last.local.date() != today || last.local.second_of_day() < alarm);
            crossed && (late || now.unix - last.unix <= ring.timeout_s())
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

    /// One alarm, id 1, 06:30 every day.
    fn cfg() -> Vec<AlarmConfig> {
        vec![AlarmConfig {
            id: 1,
            hour: 6,
            minute: 30,
            weekdays: [1; 7],
            ..AlarmConfig::default()
        }]
    }

    fn r() -> RingConfig {
        RingConfig::default()
    }

    /// Ticks once a second from `from` to `to` (Unix times, inclusive).
    fn run(a: &mut Alarms, cfg: &[AlarmConfig], from: i64, to: i64) {
        for t in from..=to {
            a.tick(cfg, &r(), at_unix(t));
        }
    }

    // Monday 2026-11-09, 06:30 local = 05:30 UTC.
    const ALARM: i64 = 1_794_202_200;

    #[test]
    fn rings_at_the_alarm_time() {
        let c = cfg();
        let mut a = Alarms::new(State::default());
        run(&mut a, &c, ALARM - 5, ALARM - 1);
        assert!(!a.is_ringing());
        a.tick(&c, &r(), at_unix(ALARM));
        assert!(a.is_ringing());
        assert_eq!(a.snoozes_left(&r()), Some(3));
    }

    #[test]
    fn disabled_and_weekdays() {
        let mut c = cfg();
        c[0].enabled = false;
        let mut a = Alarms::new(State::default());
        run(&mut a, &c, ALARM - 1, ALARM + 1);
        assert!(!a.is_ringing());

        let mut c = cfg();
        c[0].weekdays = [0, 1, 1, 1, 1, 1, 1]; // not on Mondays
        let mut a = Alarms::new(State::default());
        run(&mut a, &c, ALARM - 1, ALARM + 1);
        assert!(!a.is_ringing());
        assert!(!c[0].rings_on(1) && c[0].rings_on(0) && c[0].rings_on(2));
        assert_eq!(c[0].weekday_bits(), 0b111_1110);
    }

    #[test]
    fn stop_ends_the_day() {
        let c = cfg();
        let mut a = Alarms::new(State::default());
        run(&mut a, &c, ALARM - 1, ALARM);
        a.input(&r(), at_unix(ALARM + 5), Input::Stop);
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
        let mut a = Alarms::new(State::default());
        run(&mut a, &c, ALARM - 1, ALARM);
        let mut t = ALARM;
        for left in [2, 1, 0] {
            a.input(&r(), at_unix(t + 10), Input::Snooze);
            assert!(!a.is_ringing());
            assert_eq!(a.snoozes_left(&r()), Some(left));
            assert_eq!(a.snoozed_until(), Some(t + 10 + 9 * 60));
            run(&mut a, &c, t + 10, t + 10 + 9 * 60 - 1);
            assert!(!a.is_ringing());
            t += 10 + 9 * 60;
            a.tick(&c, &r(), at_unix(t));
            assert!(a.is_ringing());
        }
        a.input(&r(), at_unix(t + 1), Input::Snooze);
        assert!(a.is_ringing(), "a fourth snooze is ignored");
        a.input(&r(), at_unix(t + 2), Input::Stop);
        assert!(!a.is_ringing());
        assert_eq!(
            a.state().done.get(&1).copied(),
            Some(at_unix(ALARM).local.date())
        );
    }

    #[test]
    fn stop_while_snoozed() {
        let c = cfg();
        let mut a = Alarms::new(State::default());
        run(&mut a, &c, ALARM - 1, ALARM);
        a.input(&r(), at_unix(ALARM + 1), Input::Snooze);
        a.input(&r(), at_unix(ALARM + 2), Input::Stop);
        run(&mut a, &c, ALARM + 2, ALARM + 3600);
        assert!(!a.is_ringing());
        assert_eq!(a.state().phase, Phase::Idle);
    }

    #[test]
    fn unanswered_alarm_is_silenced_for_good() {
        let c = cfg();
        let mut a = Alarms::new(State::default());
        run(&mut a, &c, ALARM - 1, ALARM + 10 * 60 - 1);
        assert!(a.is_ringing());
        a.tick(&c, &r(), at_unix(ALARM + 10 * 60));
        assert!(!a.is_ringing());
        run(&mut a, &c, ALARM + 10 * 60, ALARM + 7200);
        assert!(!a.is_ringing());
    }

    #[test]
    fn timeout_restarts_after_a_snooze() {
        let c = cfg();
        let mut a = Alarms::new(State::default());
        run(&mut a, &c, ALARM - 1, ALARM);
        a.input(&r(), at_unix(ALARM + 9 * 60), Input::Snooze);
        let again = ALARM + 18 * 60;
        run(&mut a, &c, ALARM + 9 * 60, again);
        assert!(a.is_ringing());
        run(&mut a, &c, again, again + 10 * 60 - 1);
        assert!(a.is_ringing());
        a.tick(&c, &r(), at_unix(again + 10 * 60));
        assert!(!a.is_ringing());
    }

    #[test]
    fn boot_catches_up_within_the_ring_timeout() {
        let c = cfg();
        let mut a = Alarms::new(State::default());
        a.tick(&c, &r(), at_unix(ALARM + 5 * 60));
        assert!(a.is_ringing());

        let mut a = Alarms::new(State::default());
        a.tick(&c, &r(), at_unix(ALARM + 11 * 60));
        assert!(!a.is_ringing());
    }

    #[test]
    fn reboot_resumes_saved_state() {
        let c = cfg();
        let mut a = Alarms::new(State::default());
        run(&mut a, &c, ALARM - 1, ALARM);
        a.input(&r(), at_unix(ALARM + 1), Input::Stop);
        let saved = State::from_json(&a.state().to_json()).unwrap();
        // Rebooted a minute later: does not ring again.
        let mut b = Alarms::new(saved);
        b.tick(&c, &r(), at_unix(ALARM + 60));
        assert!(!b.is_ringing());

        // Rebooted while snoozed: rings when the snooze ends.
        let mut a = Alarms::new(State::default());
        run(&mut a, &c, ALARM - 1, ALARM);
        a.input(&r(), at_unix(ALARM + 1), Input::Snooze);
        let mut b = Alarms::new(a.state().clone());
        run(&mut b, &c, ALARM + 120, ALARM + 1 + 9 * 60);
        assert!(b.is_ringing());
        assert_eq!(b.snoozes_left(&r()), Some(2));
    }

    #[test]
    fn alarm_set_just_after_its_time_waits_for_tomorrow() {
        let c = cfg();
        let mut a = Alarms::new(State::default());
        let mut off = c.clone();
        off[0].enabled = false;
        run(&mut a, &off, ALARM - 1, ALARM + 60);
        // Enabled a minute late: not now, tomorrow.
        run(&mut a, &c, ALARM + 61, ALARM + 120);
        assert!(!a.is_ringing());
        let next = a.next_of(&c[0], at_unix(ALARM + 120).local).unwrap();
        assert_eq!((next.day, next.hour, next.minute), (10, 6, 30));
    }

    #[test]
    fn clock_jump_across_the_alarm_does_not_ring() {
        let c = cfg();
        let mut a = Alarms::new(State::default());
        a.tick(&c, &r(), at_unix(ALARM - 3 * 3600));
        a.tick(&c, &r(), at_unix(ALARM + 3600)); // NTP corrects by four hours
        assert!(!a.is_ringing());
        // A small correction does.
        let mut a = Alarms::new(State::default());
        a.tick(&c, &r(), at_unix(ALARM - 30));
        a.tick(&c, &r(), at_unix(ALARM + 30));
        assert!(a.is_ringing());
    }

    #[test]
    fn clock_set_back_does_not_stretch_ringing() {
        let c = cfg();
        let mut a = Alarms::new(State::default());
        run(&mut a, &c, ALARM - 1, ALARM);
        a.tick(&c, &r(), at_unix(ALARM - 3600));
        run(&mut a, &c, ALARM - 3600, ALARM - 3600 + 10 * 60);
        assert!(!a.is_ringing());
    }

    #[test]
    fn spring_forward_skipped_hour() {
        // Sunday 2026-03-29: 02:00 CET → 03:00 CEST at 01:00 UTC.
        let mut c = cfg();
        (c[0].hour, c[0].minute) = (2, 30);
        let jump = 1_774_746_000;
        let mut a = Alarms::new(State::default());
        run(&mut a, &c, jump - 5, jump - 1);
        assert_eq!(at_unix(jump - 1).local.hms(), "01:59:59");
        assert!(!a.is_ringing());
        a.tick(&c, &r(), at_unix(jump));
        assert_eq!(at_unix(jump).local.hms(), "03:00:00");
        assert!(a.is_ringing());
    }

    #[test]
    fn autumn_repeated_hour_rings_once() {
        // Sunday 2026-10-25: 03:00 CEST → 02:00 CET at 01:00 UTC.
        let mut c = cfg();
        (c[0].hour, c[0].minute) = (2, 30);
        let back = 1_792_890_000;
        let first = back - 1800; // 02:30 CEST
        let mut a = Alarms::new(State::default());
        run(&mut a, &c, first - 1, first);
        assert_eq!(at_unix(first).local.hms(), "02:30:00");
        assert!(a.is_ringing());
        // Unanswered: silenced after 10 minutes.
        run(&mut a, &c, first, back + 3600);
        assert_eq!(at_unix(back + 1800).local.hms(), "02:30:00");
        assert!(!a.is_ringing());
        assert_eq!(
            a.state().done.get(&1).copied(),
            Some(at_unix(first).local.date())
        );
    }

    #[test]
    fn rings_at_midnight() {
        let mut c = cfg();
        (c[0].hour, c[0].minute) = (0, 0);
        let midnight = winter(11, 10, 0, 0, 0).unix;
        let mut a = Alarms::new(State::default());
        run(&mut a, &c, midnight - 3, midnight);
        assert!(a.is_ringing());
    }

    #[test]
    fn next_alarm() {
        let mut c = cfg();
        c[0].weekdays = [1, 1, 1, 1, 1, 0, 0];
        let a = Alarms::new(State::default());
        // Monday 05:00 → today 06:30.
        let n = a.next_of(&c[0], winter(11, 9, 5, 0, 0).local).unwrap();
        assert_eq!(
            (n.day, n.weekday, n.hour, n.minute, n.second),
            (9, 1, 6, 30, 0)
        );
        // Friday 07:00 → Monday.
        let n = a.next_of(&c[0], winter(11, 13, 7, 0, 0).local).unwrap();
        assert_eq!((n.month, n.day, n.weekday), (11, 16, 1));
        // Done today → tomorrow.
        let a = Alarms::new(State {
            done: [(1, winter(11, 9, 5, 0, 0).local.date())].into(),
            phase: Phase::Idle,
        });
        let n = a.next_of(&c[0], winter(11, 9, 5, 0, 0).local).unwrap();
        assert_eq!(n.day, 10);
        // No weekdays or disabled → none.
        c[0].weekdays = [0; 7];
        assert!(a.next_of(&c[0], winter(11, 9, 5, 0, 0).local).is_none());
        c[0].weekdays = [1; 7];
        c[0].enabled = false;
        assert!(a.next_of(&c[0], winter(11, 9, 5, 0, 0).local).is_none());
    }

    #[test]
    fn reconfigured_to_later_today_rings_again() {
        let c = cfg();
        let mut a = Alarms::new(State::default());
        run(&mut a, &c, ALARM - 1, ALARM);
        a.input(&r(), at_unix(ALARM + 5), Input::Stop);
        // Moved to 06:20 (past): stays done, also across a reboot.
        let mut earlier = c.clone();
        earlier[0].minute = 20;
        a.reconfigured(&earlier, at_unix(ALARM + 60).local);
        let mut b = Alarms::new(a.state().clone());
        run(&mut b, &earlier, ALARM + 60, ALARM + 120);
        assert!(!b.is_ringing());
        // Moved to 07:00: rings at 07:00.
        let mut later = c.clone();
        later[0].hour = 7;
        later[0].minute = 0;
        a.reconfigured(&later, at_unix(ALARM + 60).local);
        assert_eq!(a.state().done.get(&1).copied(), None);
        run(&mut a, &later, ALARM + 60, ALARM + 1800);
        assert!(a.is_ringing());
    }

    #[test]
    fn weekday_summary() {
        assert_eq!(weekdays_de(&[1, 1, 1, 1, 1, 0, 0]), "Mo–Fr");
        assert_eq!(weekdays_de(&[0, 0, 0, 0, 0, 1, 1]), "Sa, So");
        assert_eq!(weekdays_de(&[1, 0, 1, 1, 1, 0, 0]), "Mo, Mi–Fr");
        assert_eq!(weekdays_de(&[0, 0, 1, 0, 0, 0, 0]), "Mi");
        assert_eq!(weekdays_de(&[1; 7]), "täglich");
        assert_eq!(weekdays_de(&[0; 7]), "");
    }

    #[test]
    fn config_json_and_sanitize() {
        let mut c: Vec<AlarmConfig> = serde_json::from_str(
            r#"[{ "hour": 25, "minute": 5, "name": "  Ein sehr, sehr langer Weckername  " },
                { "id": 1, "sound": "random" }, { "id": 1, "sound": "sounds/a.wav" }]"#,
        )
        .unwrap();
        sanitize_list(&mut c);
        assert_eq!((c[0].enabled, c[0].hour, c[0].minute), (true, 23, 5));
        assert_eq!(c[0].weekdays, [1, 1, 1, 1, 1, 0, 0]);
        assert_eq!(c[0].name, "Ein sehr, sehr langer We");
        let ids: Vec<u32> = c.iter().map(|a| a.id).collect();
        assert_eq!(ids, [2, 1, 3]);
        assert_eq!(c[1].sound, Sound::RandomFile);
        assert_eq!(c[2].sound, Sound::File("sounds/a.wav".into()));
        assert_eq!(new_id(&c), 4);
        let json = serde_json::to_string(&c[1]).unwrap();
        assert!(json.contains(r#""sound":"random""#) && !json.contains("name"));
    }

    #[test]
    fn sound_strings() {
        for (text, sound) in [
            ("beep", Sound::Beep),
            ("random", Sound::RandomFile),
            ("tidal:random", Sound::TidalRandom),
            ("tidal:12345", Sound::TidalTrack("12345".into())),
            ("sounds/x.flac", Sound::File("sounds/x.flac".into())),
        ] {
            assert_eq!(Sound::from(text.to_string()), sound);
            assert_eq!(String::from(sound), text);
        }
        assert_eq!(Sound::from(String::new()), Sound::Beep);
    }

    /// Two alarms, 06:30 (id 1) and `hour:minute` (id 2), every day.
    fn two(hour: u8, minute: u8) -> Vec<AlarmConfig> {
        let mut c = cfg();
        c.push(AlarmConfig {
            id: 2,
            hour,
            minute,
            weekdays: [1; 7],
            ..AlarmConfig::default()
        });
        c
    }

    #[test]
    fn second_alarm_rings_on_its_own() {
        let c = two(7, 0);
        let mut a = Alarms::new(State::default());
        run(&mut a, &c, ALARM - 1, ALARM);
        assert_eq!(a.ringing(), Some(1));
        a.input(&r(), at_unix(ALARM + 5), Input::Stop);
        run(&mut a, &c, ALARM + 5, ALARM + 1800);
        assert_eq!(a.ringing(), Some(2));
        let next = a.next(&c, at_unix(ALARM + 1800).local).unwrap();
        assert_eq!((next.0, next.1.day, next.1.hour), (1, 10, 6));
    }

    #[test]
    fn same_time_rings_once() {
        let c = two(6, 30);
        let mut a = Alarms::new(State::default());
        run(&mut a, &c, ALARM - 1, ALARM);
        assert_eq!(a.ringing(), Some(1));
        a.input(&r(), at_unix(ALARM + 5), Input::Stop);
        run(&mut a, &c, ALARM + 5, ALARM + 3600);
        assert!(!a.is_ringing());
    }

    #[test]
    fn due_alarm_takes_over_a_snooze() {
        let c = two(6, 35);
        let mut a = Alarms::new(State::default());
        run(&mut a, &c, ALARM - 1, ALARM);
        a.input(&r(), at_unix(ALARM + 1), Input::Snooze);
        run(&mut a, &c, ALARM + 1, ALARM + 300);
        assert_eq!(a.ringing(), Some(2));
        assert_eq!(a.snoozes_left(&r()), Some(3));
        assert_eq!(a.state().done.get(&1), Some(&at_unix(ALARM).local.date()));
        // The first one's snooze does not come back.
        a.input(&r(), at_unix(ALARM + 301), Input::Stop);
        run(&mut a, &c, ALARM + 301, ALARM + 3600);
        assert!(!a.is_ringing());
    }

    #[test]
    fn due_alarm_while_ringing_is_folded_in() {
        let c = two(6, 33);
        let mut a = Alarms::new(State::default());
        run(&mut a, &c, ALARM - 1, ALARM + 180);
        assert_eq!(a.ringing(), Some(1));
        a.input(&r(), at_unix(ALARM + 181), Input::Stop);
        run(&mut a, &c, ALARM + 181, ALARM + 3600);
        assert!(!a.is_ringing());
    }

    #[test]
    fn deleting_or_disabling_the_active_alarm_ends_it() {
        let c = two(7, 0);
        let mut a = Alarms::new(State::default());
        run(&mut a, &c, ALARM - 1, ALARM);
        let mut off = c.clone();
        off[0].enabled = false;
        a.reconfigured(&off, at_unix(ALARM + 1).local);
        assert!(!a.is_ringing());

        let mut a = Alarms::new(State::default());
        run(&mut a, &c, ALARM - 1, ALARM);
        a.input(&r(), at_unix(ALARM + 1), Input::Snooze);
        a.reconfigured(&c[1..], at_unix(ALARM + 2).local);
        assert_eq!(a.state().phase, Phase::Idle);
        assert!(a.state().done.is_empty());
        // Editing another alarm keeps it ringing.
        let mut a = Alarms::new(State::default());
        run(&mut a, &c, ALARM - 1, ALARM);
        a.reconfigured(&two(8, 0), at_unix(ALARM + 1).local);
        assert_eq!(a.ringing(), Some(1));
    }

    #[test]
    fn reboot_while_snoozed_does_not_restart_the_alarm() {
        let c = cfg();
        let mut a = Alarms::new(State::default());
        run(&mut a, &c, ALARM - 1, ALARM);
        a.input(&r(), at_unix(ALARM + 1), Input::Snooze);
        let mut b = Alarms::new(a.state().clone());
        b.tick(&c, &r(), at_unix(ALARM + 60));
        assert!(!b.is_ringing());
        assert_eq!(b.snoozes_left(&r()), Some(2));
    }

    #[test]
    fn daily_pick() {
        let day = |d| Date {
            year: 2026,
            month: 11,
            day: d,
        };
        assert_eq!(pick_daily(5, day(9), 1), pick_daily(5, day(9), 1));
        let picks: std::collections::BTreeSet<usize> =
            (1..=28).map(|d| pick_daily(5, day(d), 1)).collect();
        assert_eq!(picks.len(), 5, "every file comes up within four weeks");
        assert_eq!(pick_daily(0, day(9), 1), 0);
    }

    #[test]
    fn sorted_by_time() {
        let c = two(5, 0);
        let ids: Vec<u32> = sorted(&c).iter().map(|a| a.id).collect();
        assert_eq!(ids, [2, 1]);
    }
}
