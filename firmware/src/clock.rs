//! Wall-clock time. The RX8130 RTC (UTC) seeds the system clock at boot;
//! newlib applies the time zone (Europe/Zurich as a POSIX rule, no tz
//! database); [`now`] is local time. NTP (`crate::net`) corrects the system
//! clock, and [`store_system_time`] writes it back to the RTC.
//!
//! If the RTC holds no valid time (power lost, never set), the firmware's
//! build time is used and written back, so a fresh board shows a plausible
//! clock until the first NTP sync.

use crate::board::rtc::{DateTime, Rx8130};
use esp_idf_svc::sys;
use std::time::Duration;
use wakey_core::alarm::Now;

/// Europe/Zurich: CET, CEST from the last Sunday of March to the last
/// Sunday of October (03:00).
const TZ: &core::ffi::CStr = c"CET-1CEST,M3.5.0,M10.5.0/3";

/// Unix time when this firmware was built (`build.rs`).
const BUILD_UNIX_TIME: i64 = match i64::from_str_radix(env!("BUILD_UNIX_TIME"), 10) {
    Ok(t) => t,
    Err(_) => 0,
};

/// Sets the time zone and the system time from the RTC.
pub fn init(rtc: &Rx8130) {
    unsafe {
        sys::setenv(c"TZ".as_ptr(), TZ.as_ptr(), 1);
        sys::tzset();
    }
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
    unsafe { sys::settimeofday(&tv, core::ptr::null()) };
    log::info!("clock: {} UTC, local {}", t.hms(), now().hms());
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
    let mut tm: sys::tm = unsafe { core::mem::zeroed() };
    unsafe { sys::localtime_r(&unix, &mut tm) };
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

/// Time until the system clock's next full second.
pub fn until_next_second() -> Duration {
    Duration::from_micros(1_000_000 - system_time().tv_usec as u64)
}
