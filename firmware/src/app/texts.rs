//! What the services report, in the UI's words (German). The services
//! report types; details are in the log.

use crate::net::Unavailable;
use crate::tidal::Problem;

/// Why the TIDAL tab shows no playlist (a sentence).
pub fn tidal_problem(problem: &Problem) -> String {
    match problem {
        Problem::Network(why) => no_network(why),
        Problem::Session => "Die Anmeldung ist ungültig.".into(),
        Problem::NoPlaylist(name) => format!("Keine Playlist „{name}“ bei TIDAL."),
        Problem::CodeExpired => "Der Code ist abgelaufen.".into(),
        Problem::LoginRefused => "Die Anmeldung wurde abgelehnt.".into(),
        Problem::Unreachable => "TIDAL ist nicht erreichbar.".into(),
        Problem::Server => "TIDAL hat unerwartet geantwortet.".into(),
        Problem::Storage => "Die Speicherkarte ist nicht beschreibbar.".into(),
    }
}

/// Why there is no network (a sentence).
fn no_network(why: &Unavailable) -> String {
    match why {
        Unavailable::NotConfigured => "Kein WLAN eingerichtet (config.json).".into(),
        Unavailable::NoConnection { ssid } => format!("Keine Verbindung zu „{ssid}“."),
        Unavailable::Failed => "WLAN-Fehler.".into(),
    }
}
