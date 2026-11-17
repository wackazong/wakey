//! Everything the screen says, per language: one resource file each
//! (`lang/<code>.json`, compiled in), chosen by `language` in
//! `config.json`.
//!
//! A new language is a new file with the same keys and a [`Language`]
//! variant; the tests check every file for completeness, for the same
//! `{placeholders}` as English, and that the UI's font has its characters.

use crate::datetime::DateTime;
use serde::{Deserialize, Serialize};
use std::fmt::{Display, Write};

/// The `language` setting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    En,
    De,
    /// Anything else in the file: treated as the default.
    #[serde(other)]
    Unknown,
}

impl Language {
    pub const ALL: [Self; 2] = [Self::En, Self::De];

    fn resource(self) -> &'static str {
        match self {
            Self::En | Self::Unknown => include_str!("../lang/en.json"),
            Self::De => include_str!("../lang/de.json"),
        }
    }

    /// The language with this code ("en").
    pub fn from_code(code: &str) -> Option<Self> {
        serde_json::from_value(code.into())
            .ok()
            .filter(|language| *language != Self::Unknown)
    }

    /// The language's texts.
    pub fn texts(self) -> Texts {
        serde_json::from_str(self.resource()).expect("language file")
    }
}

/// A language file. Texts with `{placeholders}` are templates for
/// [`fill`].
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Texts {
    /// Monday first, as the alarms' weekdays.
    pub weekdays: [String; 7],
    pub weekdays_short: [String; 7],
    pub months: [String; 12],
    /// `{weekday}`, `{day}`, `{month}`, `{year}`.
    date: String,
    /// The keyboard's three rows of letters, at most 11, 11 and 8.
    pub keyboard: [String; 3],
    pub labels: Labels,
    pub clock: ClockTexts,
    pub alarm: AlarmTexts,
    pub sound: SoundTexts,
    pub airdrop: AirdropTexts,
    pub tidal: TidalTexts,
    pub network: NetworkTexts,
}

/// Fixed texts the pages show as they are (the Slint struct `Labels`:
/// [`slint_labels!`](crate::slint_labels)).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Labels {
    pub cancel: String,
    pub save: String,
    pub done: String,
    pub delete: String,
    /// The way back to the clock page.
    pub clock: String,
    pub new_alarm: String,
    pub no_alarms: String,
    pub name: String,
    pub name_optional: String,
    pub name_hint: String,
    pub days: String,
    pub choose_a_day: String,
    pub sound: String,
    /// The way back to the alarm page.
    pub alarm: String,
    pub files: String,
    pub volume: String,
    pub fade_in: String,
    pub off: String,
    pub preview: String,
    pub stop: String,
    pub space: String,
    pub hold_to_stop: String,
    pub stop_hint: String,
    pub tidal_scan: String,
    pub config_error: String,
    pub config_error_help: String,
}

/// A Slint struct `$ty` with the fields of [`Labels`] (kebab-case there),
/// filled from `$labels`.
#[macro_export]
macro_rules! slint_labels {
    ($ty:ident, $labels:expr) => {
        $crate::slint_labels!(@fields $ty, $labels;
            cancel, save, done, delete, clock, new_alarm, no_alarms, name, name_optional,
            name_hint, days, choose_a_day, sound, alarm, files, volume, fade_in, off, preview,
            stop, space, hold_to_stop, stop_hint, tidal_scan, config_error, config_error_help)
    };
    (@fields $ty:ident, $labels:expr; $($field:ident),*) => {{
        let labels: &$crate::lang::Labels = &$labels;
        $ty { $($field: labels.$field.as_str().into(),)* }
    }};
}

/// The clock page's bottom lines.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClockTexts {
    pub no_alarm: String,
    pub today: String,
    pub tomorrow: String,
    /// `{day}`, `{time}`.
    pub next_alarm: String,
    /// `{n}`: snoozes left.
    pub snooze_hint: String,
    /// `{time}`, `{n}`.
    pub snoozed: String,
    /// `{time}`.
    pub snoozed_last: String,
}

/// The alarm page.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlarmTexts {
    /// `{time}`: [`Texts::duration`].
    pub rings_in: String,
    pub rings_never: String,
    /// `{n}`, as are `hours` and `minutes`.
    days: String,
    hours: String,
    minutes: String,
    pub save_failed: String,
}

/// Sounds that are not a file's or a track's title.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SoundTexts {
    pub built_in: String,
    pub random: String,
    pub random_note: String,
    /// A track whose title is not known.
    pub tidal_track: String,
}

/// The Files tab's AirDrop rows.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AirdropTexts {
    pub starting: String,
    pub ready: String,
    pub failed: String,
    pub receiving: String,
}

/// The TIDAL tab.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TidalTexts {
    /// `{code}`.
    pub code: String,
    /// `{artist}`, as are `queued` and `removed`.
    pub downloading: String,
    pub queued: String,
    pub removed: String,
    pub no_credentials: String,
    pub connecting: String,
    pub syncing: String,
    pub empty: String,
    /// `{problem}`: one of the sentences below, or of [`NetworkTexts`].
    pub retry: String,
    pub session: String,
    /// `{name}`.
    pub no_playlist: String,
    pub code_expired: String,
    pub login_refused: String,
    pub unreachable: String,
    pub server: String,
    pub storage: String,
}

/// Why there is no network (sentences).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkTexts {
    pub not_configured: String,
    /// `{ssid}`.
    pub no_connection: String,
    pub failed: String,
}

impl Texts {
    /// The weekday's name.
    pub fn weekday(&self, t: &DateTime) -> &str {
        // `DateTime::weekday` counts from Sunday.
        &self.weekdays[(usize::from(t.weekday) + 6) % 7]
    }

    /// "Monday, 9 November 2026"
    pub fn date(&self, t: &DateTime) -> String {
        fill(
            &self.date,
            &[
                ("weekday", &self.weekday(t)),
                ("day", &t.day),
                ("month", &self.months[usize::from(t.month.clamp(1, 12)) - 1]),
                ("year", &t.year),
            ],
        )
    }

    /// "9 h 14 min": days, hours and minutes, without the parts that are
    /// zero.
    pub fn duration(&self, minutes: i64) -> String {
        let alarm = &self.alarm;
        let parts = [
            (minutes / 1440, &alarm.days),
            (minutes % 1440 / 60, &alarm.hours),
            (minutes % 60, &alarm.minutes),
        ];
        let shown: Vec<_> = parts
            .iter()
            .filter(|(n, _)| *n > 0)
            .map(|(n, unit)| fill(unit, &[("n", n)]))
            .collect();
        if shown.is_empty() {
            fill(&alarm.minutes, &[("n", &0)])
        } else {
            shown.join(" ")
        }
    }

    /// The keyboard's rows as keys: (lower case, upper case).
    pub fn keyboard_rows(&self) -> (Vec<Vec<String>>, Vec<Vec<String>>) {
        let rows = |key: fn(char) -> String| {
            self.keyboard
                .iter()
                .map(|row| row.chars().map(key).collect())
                .collect()
        };
        (rows(|c| c.to_string()), rows(upper))
    }
}

/// The key for a capital `c`; `c` itself where the capital is not one
/// character (ß).
fn upper(c: char) -> String {
    let mut capital = c.to_uppercase();
    match (capital.next(), capital.next()) {
        (Some(u), None) => u.to_string(),
        _ => c.to_string(),
    }
}

/// `template` with each `{name}` replaced by its value (an unknown name
/// stays as written).
pub fn fill(template: &str, values: &[(&str, &dyn Display)]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let Some(len) = rest[open..].find('}') else {
            break;
        };
        out.push_str(&rest[..open]);
        let name = &rest[open + 1..open + len];
        match values.iter().find(|(n, _)| *n == name) {
            Some((_, value)) => write!(out, "{value}").expect("write to a String"),
            None => out.push_str(&rest[open..=open + len]),
        }
        rest = &rest[open + len + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::collections::BTreeSet;

    fn texts(language: Language) -> Texts {
        // Every file parses: all keys there, none unknown.
        language.texts()
    }

    /// Every string of a file with its path ("clock.snoozed").
    fn strings(value: &Value, path: &str, out: &mut Vec<(String, String)>) {
        match value {
            Value::String(s) => out.push((path.to_owned(), s.clone())),
            Value::Array(items) => {
                for (i, item) in items.iter().enumerate() {
                    strings(item, &format!("{path}[{i}]"), out);
                }
            }
            Value::Object(fields) => {
                for (key, item) in fields {
                    let path = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    strings(item, &path, out);
                }
            }
            _ => panic!("{path}: not a text"),
        }
    }

    fn all_strings(language: Language) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let value = serde_json::from_str(language.resource()).unwrap();
        strings(&value, "", &mut out);
        out
    }

    fn placeholders(text: &str) -> BTreeSet<&str> {
        text.split('{')
            .skip(1)
            .filter_map(|s| s.split_once('}'))
            .map(|(name, _)| name)
            .collect()
    }

    #[test]
    fn every_language_has_the_placeholders_of_english() {
        let english = all_strings(Language::En);
        for language in Language::ALL {
            let other = all_strings(language);
            assert_eq!(english.len(), other.len(), "{language:?}");
            for ((path, en), (other_path, text)) in english.iter().zip(&other) {
                assert_eq!(path, other_path, "{language:?}");
                assert!(!text.is_empty(), "{language:?}: {path} is empty");
                assert_eq!(placeholders(en), placeholders(text), "{language:?}: {path}");
            }
        }
    }

    #[test]
    fn the_font_has_every_character() {
        // Slint embeds ASCII and the characters of the UI's string
        // literals; anything else draws as a blob.
        let ui = include_str!("../../firmware/ui/wakey.slint");
        for language in Language::ALL {
            for (path, text) in all_strings(language) {
                for c in text.chars().filter(|c| !c.is_ascii()) {
                    assert!(
                        ui.contains(c),
                        "{language:?}: {path}: '{c}' is not in wakey.slint's glyph list"
                    );
                }
            }
        }
    }

    #[test]
    fn keyboard_rows_fit_the_page() {
        for language in Language::ALL {
            let (lower, upper) = texts(language).keyboard_rows();
            for rows in [&lower, &upper] {
                let keys: Vec<_> = rows.iter().map(Vec::len).collect();
                assert!(
                    keys[0] <= 11 && keys[1] <= 11 && keys[2] <= 8,
                    "{language:?}: {keys:?}"
                );
            }
            assert_eq!(lower[0].len(), upper[0].len());
        }
        let (_, upper) = texts(Language::De).keyboard_rows();
        assert_eq!(upper[0][10], "Ü");
        assert_eq!(upper[2][7], "ß");
    }

    #[test]
    fn dates() {
        let monday = DateTime::from_unix(1_794_265_200); // 2026-11-09
        assert_eq!(texts(Language::En).date(&monday), "Monday, 9 November 2026");
        assert_eq!(
            texts(Language::De).date(&monday),
            "Montag, 9. November 2026"
        );
        let leap_day = DateTime::from_unix(1_709_164_800); // 2024-02-29
        assert_eq!(
            texts(Language::De).date(&leap_day),
            "Donnerstag, 29. Februar 2024"
        );
    }

    #[test]
    fn durations() {
        let en = texts(Language::En);
        assert_eq!(en.duration(0), "0 min");
        assert_eq!(en.duration(1), "1 min");
        assert_eq!(en.duration(554), "9 h 14 min");
        assert_eq!(en.duration(1440), "1 d");
        assert_eq!(
            texts(Language::De).duration(6 * 1440 + 4 * 60 + 33),
            "6 Tg. 4 Std. 33 Min."
        );
    }

    #[test]
    fn fill_replaces_known_names_once() {
        assert_eq!(
            fill("{a} and {b}{a}", &[("a", &"{b}"), ("b", &2)]),
            "{b} and 2{b}"
        );
        assert_eq!(fill("{other} {", &[("a", &1)]), "{other} {");
        assert_eq!(fill("plain", &[]), "plain");
    }

    #[test]
    fn an_unknown_language_is_english() {
        let language: Language = serde_json::from_str("\"fr\"").unwrap();
        assert_eq!(language, Language::Unknown);
        assert_eq!(Language::from_code("fr"), None);
        assert_eq!(Language::from_code("de"), Some(Language::De));
        assert_eq!(language.texts().labels.save, "Save");
    }
}
