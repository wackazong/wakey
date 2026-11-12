//! The `config.json` model. Unknown fields are ignored and missing ones get
//! defaults, so the file can grow ahead of the firmware; [`Config::load`]
//! also returns the file's text with the missing ones added, so they show
//! up for editing.
//!
//! ```json
//! {
//!   "wifi": { "ssid": "MeinNetz", "password": "geheim" },
//!   "sync_interval_h": 24,
//!   "alarms": [ { "id": 1, "enabled": true, "hour": 6, "minute": 30, … } ],
//!   "ringing": { "snooze_min": 9, "max_snoozes": 3, "ring_timeout_min": 10 },
//!   "display": { "night_pixels_percent": 35 },
//!   "power": { "sleep_after_s": 60, "sleep_when": "dark" },
//!   "tidal": { "client_id": "…", "client_secret": "…", "playlist": "Wecker" }
//! }
//! ```
//!
//! Files from before several alarms have one `alarm` section; it becomes
//! the first entry of `alarms` (and its snooze settings `ringing`).

use crate::alarm::{self, AlarmConfig, RingConfig};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wifi: Option<WifiConfig>,
    /// Hours between clock syncs over Wi-Fi (1–168); Wi-Fi is off in
    /// between.
    pub sync_interval_h: u16,
    /// See [`crate::alarm`].
    pub alarms: Vec<AlarmConfig>,
    pub ringing: RingConfig,
    pub display: DisplayConfig,
    pub power: PowerConfig,
    pub tidal: TidalConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            wifi: None,
            sync_interval_h: 24,
            alarms: Vec::new(),
            ringing: RingConfig::default(),
            display: DisplayConfig::default(),
            power: PowerConfig::default(),
            tidal: TidalConfig::default(),
        }
    }
}

/// The `display` section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayConfig {
    /// Pixel brightness in a dark room (night mode, with the lowest
    /// backlight), 10–100 %.
    pub night_pixels_percent: u8,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            night_pixels_percent: 35,
        }
    }
}

/// The `power` section: when the clock goes to light sleep (display off,
/// woken by a touch or an alarm).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PowerConfig {
    /// Without a touch for this long (10–3600 s), and [`Self::sleep_when`].
    pub sleep_after_s: u16,
    pub sleep_when: SleepWhen,
}

impl Default for PowerConfig {
    fn default() -> Self {
        Self {
            sleep_after_s: 60,
            sleep_when: SleepWhen::Dark,
        }
    }
}

/// When light sleep is allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SleepWhen {
    /// In a dark room (night mode); on battery always.
    Dark,
    Always,
    Never,
    /// Anything else in the file: treated as [`SleepWhen::Dark`].
    #[serde(other)]
    Unknown,
}

impl DisplayConfig {
    fn sanitize(&mut self) {
        self.night_pixels_percent = self.night_pixels_percent.clamp(10, 100);
    }
}

impl PowerConfig {
    fn sanitize(&mut self) {
        self.sleep_after_s = self.sleep_after_s.clamp(10, 3600);
        if self.sleep_when == SleepWhen::Unknown {
            self.sleep_when = SleepWhen::Dark;
        }
    }
}

/// The `tidal` section: the app credentials the device login uses (copied
/// in by hand, see `spikes/tidal-download`), the playlist the alarms take
/// their songs from, and the session the clock writes after logging in.
/// Anyone with the SD card can use the session: it is the account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TidalConfig {
    pub client_id: String,
    pub client_secret: String,
    /// The playlist's name in the user's TIDAL collection.
    pub playlist: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<TidalSession>,
}

impl Default for TidalConfig {
    fn default() -> Self {
        Self {
            client_id: String::new(),
            client_secret: String::new(),
            playlist: "Wecker".into(),
            session: None,
        }
    }
}

impl TidalConfig {
    /// Credentials are set (a login can start).
    pub fn has_credentials(&self) -> bool {
        !self.client_id.is_empty() && !self.client_secret.is_empty()
    }
}

/// A logged-in TIDAL session (see [`crate::tidal::auth`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TidalSession {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix time the access token expires.
    pub expires_at: i64,
    pub user_id: u64,
    /// For the API's `countryCode` parameter.
    pub country_code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WifiConfig {
    pub ssid: String,
    /// Empty for an open network.
    #[serde(default)]
    pub password: String,
}

/// Fields of the old `alarm` section that moved to `ringing`.
const RINGING_FIELDS: [&str; 3] = ["snooze_min", "max_snoozes", "ring_timeout_min"];

impl Config {
    /// Parses (migrating an old `alarm` section) and brings out-of-range
    /// values into range.
    pub fn parse(json: &str) -> Result<Self, serde_json::Error> {
        let mut value: Value = serde_json::from_str(json)?;
        migrate(&mut value);
        Self::from_value(value)
    }

    fn from_value(value: Value) -> Result<Self, serde_json::Error> {
        let mut c: Self = serde_json::from_value(value)?;
        alarm::sanitize_list(&mut c.alarms);
        c.ringing.sanitize();
        c.sync_interval_h = c.sync_interval_h.clamp(1, 168);
        c.display.sanitize();
        c.power.sanitize();
        Ok(c)
    }

    /// Parses the file's text. Also returns the text to write back when it
    /// changed: an old `alarm` section migrated, missing settings added at
    /// their defaults (values already there, unknown fields included, stay).
    pub fn load(json: &str) -> Result<(Self, Option<String>), serde_json::Error> {
        let mut value: Value = serde_json::from_str(json)?;
        let mut changed = migrate(&mut value);
        if value.is_object() {
            let defaults = serde_json::to_value(Self::default()).expect("config JSON");
            changed |= merge(&mut value, defaults);
            if let Some(Value::Array(list)) = value.get_mut("alarms") {
                let defaults = serde_json::to_value(AlarmConfig::default()).expect("alarm JSON");
                for a in list {
                    changed |= merge(a, defaults.clone());
                }
            }
        }
        let text = changed.then(|| pretty(&value));
        Ok((Self::from_value(value)?, text))
    }

    /// The file's text with its `alarms` list replaced by `alarms`, every
    /// other field kept. `json` is the current text, `None` if there is no
    /// file; text that is not a JSON object is replaced by the defaults
    /// plus `alarms`.
    pub fn with_alarms(json: Option<&str>, alarms: &[AlarmConfig]) -> String {
        let mut value = json
            .and_then(|t| serde_json::from_str::<Value>(t).ok())
            .filter(Value::is_object)
            .unwrap_or_else(|| serde_json::to_value(Self::default()).expect("config JSON"));
        migrate(&mut value);
        value["alarms"] = serde_json::to_value(alarms).expect("alarms JSON");
        pretty(&value)
    }
}

fn pretty(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("JSON value");
    text.push('\n');
    text
}

/// Adds the fields of `defaults` missing in `into`, recursively for
/// objects. True if anything was added.
fn merge(into: &mut Value, defaults: Value) -> bool {
    let (Value::Object(into), Value::Object(defaults)) = (into, defaults) else {
        return false;
    };
    let mut changed = false;
    for (key, default) in defaults {
        match into.get_mut(&key) {
            Some(v) => changed |= merge(v, default),
            None => {
                into.insert(key, default);
                changed = true;
            }
        }
    }
    changed
}

/// Turns an old `alarm` section into `alarms` + `ringing`. True if it did.
fn migrate(value: &mut Value) -> bool {
    let Some(obj) = value.as_object_mut() else {
        return false;
    };
    let Some(Value::Object(mut old)) = obj.remove("alarm") else {
        return false;
    };
    if obj.contains_key("alarms") {
        return true; // both: the list wins, the old section goes
    }
    let mut ringing = Map::new();
    for key in RINGING_FIELDS {
        if let Some(v) = old.remove(key) {
            ringing.insert(key.into(), v);
        }
    }
    old.insert("id".into(), 1.into());
    // The old section defaulted to off.
    old.entry("enabled").or_insert(false.into());
    if old.get("sound").is_some_and(Value::is_null) {
        old.remove("sound");
    }
    obj.insert("alarms".into(), Value::Array(vec![Value::Object(old)]));
    obj.entry("ringing").or_insert(Value::Object(ringing));
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alarm::Sound;

    #[test]
    fn display_and_power_sections() {
        // Missing: defaults, written back.
        let (c, text) = Config::load(r#"{ "alarms": [] }"#).unwrap();
        assert_eq!(c.display, DisplayConfig::default());
        assert_eq!(c.power, PowerConfig::default());
        let text = text.unwrap();
        assert!(text.contains("\"night_pixels_percent\": 35"), "{text}");
        assert!(text.contains("\"sleep_when\": \"dark\""), "{text}");
        // Out of range or unknown: into range.
        let c = Config::parse(
            r#"{ "display": { "night_pixels_percent": 0 },
                 "power": { "sleep_after_s": 1, "sleep_when": "sometimes" } }"#,
        )
        .unwrap();
        assert_eq!(c.display.night_pixels_percent, 10);
        assert_eq!(c.power.sleep_after_s, 10);
        assert_eq!(c.power.sleep_when, SleepWhen::Dark);
        let c = Config::parse(r#"{ "power": { "sleep_when": "never" } }"#).unwrap();
        assert_eq!(c.power.sleep_when, SleepWhen::Never);
        let c = Config::parse(r#"{ "sync_interval_h": 0 }"#).unwrap();
        assert_eq!(c.sync_interval_h, 1);
        assert_eq!(Config::parse("{}").unwrap().sync_interval_h, 24);
    }

    #[test]
    fn wifi() {
        let c = Config::parse(r#"{ "wifi": { "ssid": "Netz", "password": "geheim" } }"#).unwrap();
        let w = c.wifi.unwrap();
        assert_eq!((w.ssid.as_str(), w.password.as_str()), ("Netz", "geheim"));
    }

    #[test]
    fn open_network_and_unknown_fields() {
        let c = Config::parse(r#"{ "wifi": { "ssid": "Gast" }, "future": { "x": 6 } }"#).unwrap();
        assert_eq!(c.wifi.unwrap().password, "");
    }

    #[test]
    fn alarms_and_ringing() {
        let c = Config::parse(
            r#"{ "alarms": [ { "id": 3, "hour": 7, "minute": 99 }, { "hour": 5 } ],
                 "ringing": { "max_snoozes": 1, "snooze_min": 0 } }"#,
        )
        .unwrap();
        assert_eq!(c.alarms.len(), 2);
        assert_eq!(
            (c.alarms[0].id, c.alarms[0].hour, c.alarms[0].minute),
            (3, 7, 59)
        );
        assert_eq!(c.alarms[1].id, 4);
        assert!(c.alarms[1].enabled);
        assert_eq!(
            (
                c.ringing.max_snoozes,
                c.ringing.snooze_min,
                c.ringing.ring_timeout_min
            ),
            (1, 1, 10)
        );
    }

    #[test]
    fn old_alarm_section_is_migrated() {
        let text = r#"{ "wifi": { "ssid": "Netz" },
            "alarm": { "enabled": true, "hour": 6, "minute": 45, "weekdays": [1,1,1,1,1,0,0],
                       "sound": "sounds/a.wav", "volume": 40, "fade_in_s": 20,
                       "snooze_min": 5, "max_snoozes": 2, "ring_timeout_min": 15 },
            "future": 1 }"#;
        let (c, written) = Config::load(text).unwrap();
        let a = &c.alarms[0];
        assert_eq!(
            (a.id, a.enabled, a.hour, a.minute, a.volume),
            (1, true, 6, 45, 40)
        );
        assert_eq!(a.sound, Sound::File("sounds/a.wav".into()));
        assert_eq!(
            c.ringing,
            RingConfig {
                snooze_min: 5,
                max_snoozes: 2,
                ring_timeout_min: 15
            }
        );
        let written = written.unwrap();
        let v: Value = serde_json::from_str(&written).unwrap();
        assert!(v.get("alarm").is_none());
        assert_eq!(v["future"], 1);
        assert_eq!(Config::parse(&written).unwrap(), c);
        assert_eq!(
            Config::load(&written).unwrap().1,
            None,
            "stable once written"
        );

        // An old section without "enabled" or with "sound": null.
        let c = Config::parse(r#"{ "alarm": { "hour": 6, "sound": null } }"#).unwrap();
        assert!(!c.alarms[0].enabled);
        assert_eq!(c.alarms[0].sound, Sound::Beep);
    }

    #[test]
    fn defaults_written_back() {
        let text =
            r#"{ "wifi": { "ssid": "Netz" }, "alarms": [ { "id": 1, "hour": 7 } ], "future": 1 }"#;
        let (c, filled) = Config::load(text).unwrap();
        let filled = filled.unwrap();
        let v: Value = serde_json::from_str(&filled).unwrap();
        assert_eq!(v["alarms"][0]["hour"], 7);
        assert_eq!(v["alarms"][0]["minute"], 0);
        assert_eq!(v["alarms"][0]["sound"], "beep");
        assert_eq!(v["ringing"]["max_snoozes"], 3);
        assert_eq!(v["future"], 1);
        assert_eq!(v["wifi"]["ssid"], "Netz");
        assert_eq!(Config::parse(&filled).unwrap(), c);
        // Complete files stay untouched.
        assert_eq!(Config::load(&filled).unwrap().1, None);
        assert!(Config::load("not json").is_err());
    }

    #[test]
    fn alarms_written_into_the_file() {
        let alarms = vec![AlarmConfig {
            id: 1,
            hour: 5,
            minute: 45,
            weekdays: [0, 0, 0, 0, 0, 1, 1],
            ..AlarmConfig::default()
        }];
        let text = r#"{ "wifi": { "ssid": "Netz" }, "alarm": { "snooze_min": 4 }, "x": [1] }"#;
        let out = Config::with_alarms(Some(text), &alarms);
        let c = Config::parse(&out).unwrap();
        assert_eq!(c.alarms, alarms);
        assert_eq!(c.ringing.snooze_min, 4);
        assert_eq!(c.wifi.unwrap().ssid, "Netz");
        assert_eq!(serde_json::from_str::<Value>(&out).unwrap()["x"][0], 1);
        // No file or a broken one: defaults plus the alarms.
        for json in [None, Some("kaputt")] {
            assert_eq!(
                Config::parse(&Config::with_alarms(json, &alarms))
                    .unwrap()
                    .alarms,
                alarms
            );
        }
    }

    #[test]
    fn empty() {
        assert_eq!(Config::parse("{}").unwrap(), Config::default());
        assert!(Config::parse("{ \"wifi\": 3 }").is_err());
    }
}
