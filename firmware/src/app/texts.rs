//! The screen's texts in the configured language ([`wakey_core::lang`]):
//! the pages' fixed labels, and what the services report, in words. The
//! services report types; details are in the log.

use super::screens::{AppWindow, Labels, Lang};
use crate::net::Unavailable;
use crate::tidal::Problem;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::sync::OnceLock;
use wakey_core::alarm::Sound;
use wakey_core::lang::{Language, Texts, fill};

static TEXTS: OnceLock<Texts> = OnceLock::new();

/// Chooses the language; without it (`config.json` could not be read) the
/// texts are the default language's.
pub fn init(language: Language) {
    if TEXTS.set(language.texts()).is_err() {
        log::error!("texts: the language is already set");
    }
}

/// The texts of the configured language.
pub fn get() -> &'static Texts {
    TEXTS.get_or_init(|| Language::default().texts())
}

/// Gives `window` the fixed texts (each window has its own globals).
pub fn apply(window: &AppWindow) {
    fn strings(texts: &[String]) -> ModelRc<SharedString> {
        let texts: Vec<SharedString> = texts.iter().map(SharedString::from).collect();
        ModelRc::new(VecModel::from(texts))
    }
    fn rows(rows: &[Vec<String>]) -> ModelRc<ModelRc<SharedString>> {
        let rows: Vec<_> = rows.iter().map(|row| strings(row)).collect();
        ModelRc::new(VecModel::from(rows))
    }
    let texts = get();
    let lang = window.global::<Lang>();
    lang.set_labels(wakey_core::slint_labels!(Labels, texts.labels));
    lang.set_weekdays(strings(&texts.weekdays_short));
    let (lower, upper) = texts.keyboard_rows();
    lang.set_keys_lower(rows(&lower));
    lang.set_keys_upper(rows(&upper));
}

/// The title shown for `sound` (the alarm page's sound row).
pub fn sound_title(sound: &Sound) -> String {
    let texts = &get().sound;
    match sound {
        Sound::Beep => texts.built_in.clone(),
        Sound::RandomFile | Sound::TidalRandom => texts.random.clone(),
        Sound::File(path) => crate::sounds::file_title(path),
        Sound::TidalTrack(id) => id
            .parse()
            .ok()
            .and_then(crate::tidal::track_title)
            .map_or_else(|| texts.tidal_track.clone(), |(title, _)| title),
    }
}

/// Why the TIDAL tab shows no playlist (a sentence).
pub fn tidal_problem(problem: &Problem) -> String {
    let texts = &get().tidal;
    match problem {
        Problem::Network(why) => no_network(why),
        Problem::Session => texts.session.clone(),
        Problem::NoPlaylist(name) => fill(&texts.no_playlist, &[("name", name)]),
        Problem::CodeExpired => texts.code_expired.clone(),
        Problem::LoginRefused => texts.login_refused.clone(),
        Problem::Unreachable => texts.unreachable.clone(),
        Problem::Server => texts.server.clone(),
        Problem::Storage => texts.storage.clone(),
    }
}

/// Why there is no network (a sentence).
fn no_network(why: &Unavailable) -> String {
    let texts = &get().network;
    match why {
        Unavailable::NotConfigured => texts.not_configured.clone(),
        Unavailable::NoConnection { ssid } => fill(&texts.no_connection, &[("ssid", ssid)]),
        Unavailable::Failed => texts.failed.clone(),
    }
}
