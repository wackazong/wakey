//! Wall-clock time. The RX8130 RTC (UTC) seeds the system clock at boot;
//! newlib applies the time zone (`time_zone` in `config.json`: a POSIX
//! rule, as newlib has no zone database; UTC until [`set_time_zone`]);
//! [`now`] is local time. NTP (`crate::net`) corrects the system
//! clock, and [`store_system_time`] writes it back to the RTC.
//!
//! If the RTC holds no valid time (power lost, never set), the firmware's
//! build time is used and written back, so a fresh board shows a plausible
//! clock until the first NTP sync.

use crate::board::rtc::{DateTime, Rx8130};
use esp_idf_svc::sys;
use std::time::Duration;
use wakey_core::alarm::Now;

/// Unix time when this firmware was built (`build.rs`).
const BUILD_UNIX_TIME: i64 = match i64::from_str_radix(env!("BUILD_UNIX_TIME"), 10) {
    Ok(t) => t,
    Err(_) => 0,
};

/// Sets the time zone from a POSIX `TZ` rule ("CET-1CEST,M3.5.0,M10.5.0/3").
/// Called once at start, before any other thread reads the time.
pub fn set_time_zone(rule: &str) {
    let Ok(rule) = std::ffi::CString::new(rule) else {
        log::error!("clock: time zone {rule:?} is not a C string; staying on UTC");
        return;
    };
    // SAFETY: C strings that live for the call; at start, before other
    // threads read the environment.
    unsafe {
        sys::setenv(c"TZ".as_ptr(), rule.as_ptr(), 1);
        sys::tzset();
    }
    log::info!("clock: time zone {rule:?}, local {}", now().hms());
}

/// Sets the system time from the RTC.
pub fn init(rtc: &Rx8130) {
    let from_rtc = match rtc.get_time() {
        // Earlier than the build: the chip holds nonsense (a day of slack
        // for a build machine whose clock is off).
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
    let tv = sys::timeval {
        tv_sec: t.to_unix(),
        tv_usec: 0,
    };
    // SAFETY: plain call; `tv` lives for it.
    unsafe { sys::settimeofday(&tv, core::ptr::null()) };
    log::info!("clock: {} UTC", t.hms());
}

/// Writes the system time to the RTC (after an NTP sync).
pub fn store_system_time(rtc: &Rx8130) {
    let t = DateTime::from_unix(system_time().tv_sec);
    match rtc.set_time(&t) {
        Ok(()) => log::info!("clock: RTC set to {t:?} UTC, local {}", now().hms()),
        Err(e) => log::error!("clock: RTC write failed: {e}"),
    }
}

fn system_time() -> sys::timeval {
    let mut tv = sys::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    // SAFETY: plain call; `tv` is an out-parameter.
    unsafe { sys::gettimeofday(&mut tv, core::ptr::null_mut()) };
    tv
}

/// The current local date and time.
pub fn now() -> DateTime {
    local(system_time().tv_sec)
}

/// Local and Unix time of this instant (for the alarm).
pub fn moment() -> Now {
    let unix = system_time().tv_sec;
    Now {
        local: local(unix),
        unix,
    }
}

/// The local date and time of a Unix time.
pub fn local(unix: i64) -> DateTime {
    // SAFETY: `tm` is plain data (all zeros is valid) and an out-parameter
    // of the reentrant `localtime_r`.
    let tm = unsafe {
        let mut tm: sys::tm = core::mem::zeroed();
        sys::localtime_r(&unix, &mut tm);
        tm
    };
    DateTime {
        year: (tm.tm_year + 1900) as u16,
        month: (tm.tm_mon + 1) as u8,
        day: tm.tm_mday as u8,
        hour: tm.tm_hour as u8,
        minute: tm.tm_min as u8,
        second: tm.tm_sec as u8,
        weekday: tm.tm_wday as u8,
    }
}

/// The Unix time of a local date and time (the inverse of [`local`]; the
/// C library decides whether summer time applies).
pub fn unix(local: &DateTime) -> i64 {
    // SAFETY: `tm` is plain data (all zeros is valid).
    let mut tm: sys::tm = unsafe { core::mem::zeroed() };
    tm.tm_year = i32::from(local.year) - 1900;
    tm.tm_mon = i32::from(local.month) - 1;
    tm.tm_mday = local.day.into();
    tm.tm_hour = local.hour.into();
    tm.tm_min = local.minute.into();
    tm.tm_sec = local.second.into();
    tm.tm_isdst = -1;
    // SAFETY: plain call; `tm` lives for it.
    unsafe { sys::mktime(&mut tm) }
}

/// Time until the system clock's next full second.
pub fn until_next_second() -> Duration {
    Duration::from_micros(1_000_000 - system_time().tv_usec as u64)
}
