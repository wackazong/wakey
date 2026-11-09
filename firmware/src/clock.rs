//! Wall-clock time: the RX8130 RTC (UTC) seeds the system clock at boot, the
//! C library applies the time zone (Europe/Zurich; POSIX rule, no tz
//! database needed), and [`now`] returns the local time for the UI.
//!
//! If the RTC lost power (or was never set) the build time of the firmware
//! is used and written back, so a fresh board shows a plausible clock until
//! NTP (later) corrects it.

use crate::board::rtc::{DateTime, Rx8130};
use esp_idf_svc::sys;

/// Europe/Zurich: CET, CEST from the last Sunday of March to the last
/// Sunday of October (03:00).
pub const TZ: &core::ffi::CStr = c"CET-1CEST,M3.5.0,M10.5.0/3";

/// Unix time when this firmware was built (`build.rs`).
const BUILD_UNIX_TIME: i64 = match i64::from_str_radix(env!("BUILD_UNIX_TIME"), 10) {
    Ok(t) => t,
    Err(_) => 0,
};

/// Sets the time zone and the system time from the RTC. Returns the UTC
/// time used.
pub fn init(rtc: &Rx8130) -> DateTime {
    unsafe {
        sys::setenv(c"TZ".as_ptr(), TZ.as_ptr(), 1);
        sys::tzset();
    }
    let from_rtc = match rtc.get_time() {
        // Earlier than the build: the chip was set to nonsense (a day of
        // slack for a build machine whose clock is off).
        Ok(Some(t)) if t.to_unix() >= BUILD_UNIX_TIME - 86_400 => Some(t),
        Ok(Some(t)) => {
            log::warn!("clock: RTC says {t:?}, before the firmware was built; ignoring");
            None
        }
        Ok(None) => {
            log::warn!("clock: RTC has no valid time");
            None
        }
        Err(e) => {
            log::error!("clock: RTC read failed: {e}");
            None
        }
    };
    let t = from_rtc.unwrap_or_else(|| {
        let t = DateTime::from_unix(BUILD_UNIX_TIME);
        log::warn!("clock: setting the RTC to the build time {t:?}");
        if let Err(e) = rtc.set_time(&t) {
            log::error!("clock: RTC write failed: {e}");
        }
        t
    });
    set_system_time(t.to_unix());
    log::info!("clock: {t:?} UTC, local {}", now().hms());
    t
}

/// Writes the system time (after an NTP sync) to the RTC.
pub fn store_system_time(rtc: &Rx8130) {
    let mut tv = sys::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    unsafe { sys::gettimeofday(&mut tv, core::ptr::null_mut()) };
    let t = DateTime::from_unix(tv.tv_sec);
    match rtc.set_time(&t) {
        Ok(()) => log::info!(
            "clock: NTP time {t:?} UTC written to the RTC, local {}",
            now().hms()
        ),
        Err(e) => log::error!("clock: RTC write failed: {e}"),
    }
}

fn set_system_time(unix: i64) {
    let tv = sys::timeval {
        tv_sec: unix,
        tv_usec: 0,
    };
    unsafe { sys::settimeofday(&tv, core::ptr::null()) };
}

/// Local date and time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Local {
    pub year: u16,
    /// 1–12
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    /// 0 = Sunday.
    pub weekday: u8,
    /// Microseconds within the second.
    pub micros: u32,
}

/// The current local time from the system clock.
pub fn now() -> Local {
    let mut tv = sys::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    let mut tm: sys::tm = unsafe { core::mem::zeroed() };
    unsafe {
        sys::gettimeofday(&mut tv, core::ptr::null_mut());
        sys::localtime_r(&tv.tv_sec, &mut tm);
    }
    Local {
        year: (tm.tm_year + 1900) as u16,
        month: (tm.tm_mon + 1) as u8,
        day: tm.tm_mday as u8,
        hour: tm.tm_hour as u8,
        minute: tm.tm_min as u8,
        second: tm.tm_sec as u8,
        weekday: tm.tm_wday as u8,
        micros: tv.tv_usec as u32,
    }
}

pub const WEEKDAYS_DE: [&str; 7] = [
    "Sonntag",
    "Montag",
    "Dienstag",
    "Mittwoch",
    "Donnerstag",
    "Freitag",
    "Samstag",
];

pub const MONTHS_DE: [&str; 12] = [
    "Januar",
    "Februar",
    "März",
    "April",
    "Mai",
    "Juni",
    "Juli",
    "August",
    "September",
    "Oktober",
    "November",
    "Dezember",
];

impl Local {
    pub fn hms(&self) -> String {
        format!("{:02}:{:02}:{:02}", self.hour, self.minute, self.second)
    }

    /// "Montag, 9. November 2026"
    pub fn date_de(&self) -> String {
        format!(
            "{}, {}. {} {}",
            WEEKDAYS_DE[usize::from(self.weekday) % 7],
            self.day,
            MONTHS_DE[usize::from(self.month.clamp(1, 12)) - 1],
            self.year
        )
    }
}
