//! The sound files on the SD card: `sounds/*.wav|aif|aiff`, for the sound
//! page and for "Zufällig" ([`wakey_core::alarm::Sound::RandomFile`]).

use crate::board::sd;
use std::path::{Path, PathBuf};
use wakey_core::alarm::{self, Sound};
use wakey_core::datetime::Date;

/// Directory below the SD card root.
pub const DIR: &str = "sounds";

/// A playable file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoundFile {
    /// Below the SD card root, as stored in `config.json` ("sounds/a.wav").
    pub path: String,
    /// For the list: the file name without extension, `_` as spaces.
    pub title: String,
}

/// Files the player can read, sorted by title.
pub fn list() -> Vec<SoundFile> {
    let dir = Path::new(sd::MOUNT_POINT).join(DIR);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<SoundFile> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let (stem, ext) = name.rsplit_once('.')?;
            let playable = ["wav", "aif", "aiff"].contains(&ext.to_ascii_lowercase().as_str());
            (playable && !stem.starts_with('.')).then(|| SoundFile {
                path: format!("{DIR}/{name}"),
                title: stem.replace('_', " "),
            })
        })
        .collect();
    files.sort_by_key(|f| f.title.to_lowercase());
    files
}

/// The title shown for `sound` (the alarm page's "Klang" row).
pub fn title(sound: &Sound) -> String {
    match sound {
        Sound::Beep => "Eingebauter Ton".into(),
        Sound::RandomFile | Sound::TidalRandom => "Zufällig".into(),
        Sound::File(path) => Path::new(path)
            .file_stem()
            .map(|s| s.to_string_lossy().replace('_', " "))
            .unwrap_or_else(|| path.clone()),
        Sound::TidalTrack(_) => "TIDAL-Titel".into(),
    }
}

/// The file to play for alarm `id` on `date`; `None` for the built-in tone
/// (also when TIDAL is chosen: not available yet).
pub fn resolve(sound: &Sound, id: u32, date: Date) -> Option<PathBuf> {
    let root = Path::new(sd::MOUNT_POINT);
    match sound {
        Sound::Beep => None,
        Sound::File(path) => Some(root.join(path)),
        Sound::RandomFile => {
            let files = list();
            let file = files.get(alarm::pick_daily(files.len(), date, id))?;
            log::info!("sounds: today's random pick: {}", file.path);
            Some(root.join(&file.path))
        }
        Sound::TidalTrack(_) | Sound::TidalRandom => {
            log::warn!("sounds: TIDAL is not available yet, playing the built-in tone");
            None
        }
    }
}
