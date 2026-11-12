//! Spike: can a touch wake the Tab5 from chip light sleep?
//!
//! Earlier rounds of this spike (2026-11-12) found: in normal mode the
//! ST7123 holds `TP_INT` (GPIO23) low from a touch until its report is
//! read; a held finger keeps it low for seconds. But with the chip in light
//! sleep no touch ever woke it (6 of 6 sleeps ended by the 20 s timer,
//! with the frame buffer stream, with the DSI colour bars and with the
//! crystal kept on), and with the video stream deleted the awake chip saw
//! no `TP_INT` edge at all: the TDDI's touch scanning follows the video
//! stream, which light sleep stops.
//!
//! This round tries the controller's "Smart Wake Up" mode (Sitronix ST7123
//! TDDI Interface Protocol V01.11, sections 6.2, 6.6, 6.8, 6.9): in power
//! down with Smart Wake Up enabled it keeps sensing at low power and pulls
//! `TP_INT` low on a gesture (ID in 0x0012: 0xB0 double tap, 0xB1 tap,
//! 0xB2 long press, 0xC0.. slides).
//!
//! Round 1 (Smart Wake Up enabled, then power down, panel left on): the
//! controller reports state "smart wake up", but no gesture ever pulled
//! `TP_INT` low: awake with the picture running, in 3 light sleeps (all
//! ended by the timer), awake with the video stream deleted. Its scan
//! counter stands still in that mode.
//!
//! Round 2, the phone sequence: Smart Wake Up enabled, panel display off
//! and sleep in (DCS 0x28, 0x10), then touch power down. Scans continue
//! (~80/s). Awake with the video stream running (into the sleeping panel):
//! no gesture, no `TP_INT` edge (ISR). In light sleep: a double tap is
//! recognised (0xB0 in the gesture register after waking, twice in two
//! runs) but `TP_INT` does not wake the chip (timer after 20 s); single
//! tap and swipe leave no gesture. With the frame buffer stream during
//! sleep the chip once hung in sleep 2 (watchdog reset); with the DSI
//! colour bars during sleep it did not, but the panel stayed black after
//! sleep 2.
//!
//! Round 3: with the stream deleted and the chip awake, each double tap
//! gave one `TP_INT` low pulse of 0.26–6 ms (10 taps, 10 pulses).
//!
//! Round 4: the pin's sleep configuration switched input off. With
//! `gpio_sleep_sel_dis(23)` a double tap wakes the chip from light sleep
//! (3 of 3 in one run, 4–5 s after the sleep started). After waking:
//! colour bars off, Smart Wake Up off, full panel init (software reset,
//! vendor init, sleep out, display on); the picture came back 3 of 3.
//!
//! Round 5, the gesture bits in 0x00F1 (undocumented; the firmware sets
//! 0x04): bit 2 = double tap; bit 0 adds long press (0xB2, woke the chip);
//! bit 1 woke once on a tap reported as 0xC5 (arrow right) but not again,
//! and the panel stayed black after every wake with it; 0xBC recognised
//! 0xC4 without waking; 0xFF nothing. Single tap (0xB1) never reported.
//! Also: deleting the stream before sleep and creating a new one after
//! left the panel black (frames running, DCS power mode 0x9c = on), and a
//! DCS read during video mode is under suspicion of the same.
//!
//! Round 6: single taps with 0x00F1 = 0x84, 0x8C, 0x94, 0xA4, 0xC4 (bits
//! 3–6): no touch point ever appeared in the report (polled every 20 ms),
//! so waking by timer and polling cannot find a single tap either.
//!
//! Round 7, the black picture after waking: light sleep stops the DSI
//! PHY's clock; afterwards DCS commands stay in the DSI command FIFO (with
//! the stream deleted the main task hung in
//! `mipi_dsi_hal_host_gen_write_dcs_command`). Taking the whole link down
//! before sleep (stream, DBI IO, DSI bus; `Display::link_down`) and
//! rebuilding it after waking as at boot (`Display::link_up`: bus, panel
//! init, stream) brought the picture back 4 of 4 times, promptly.
//!
//! The code below is round 7 (part 2 of round 6 removed):
//!
//! 1. Registers in normal mode: firmware, status (0x0001 low nibble: 0
//!    normal, 3 smart wake-up, 4 idle, 5 power down, 6 pre smart wake-up),
//!    Smart Wake Up support (0x00F0 bit 7), scans per second (0x000A/B).
//! 2. Chip awake, touch controller as in sleep, 5 × 10 s of single taps
//!    with 0x00F1 = 0x84, 0x8C, 0x94, 0xA4, 0xC4 (bits 3–6 one at a time):
//!    `TP_INT` edges, gestures, and touch points in the report (polled
//!    every 20 ms). A visible touch point would allow waking by timer a
//!    few times a second and polling.
//! 3. Four 5 s light sleeps (timer, 0x85), no touching: does the picture
//!    come back each time?
//!
//! The USB log drops during sleep: sleeps are stored in NVS and printed at
//! the end (repeatedly) and at the next start. Listen without resetting the
//! chip: `espflash monitor -c esp32p4 --non-interactive --no-reset
//! --before no-reset-no-sync` (the default `--before` puts it into download
//! mode).
//!
//! `cargo run --release --bin chip_sleep`

// Helpers from earlier rounds stay for the next ones.
#![allow(dead_code)]

use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, Ordering};
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use esp_idf_svc::sys;
use std::time::{Duration, Instant};
use wakey::board::Board;
use wakey::board::i2c::{self, SharedI2c};
use wakey::display::{Display, HEIGHT, WIDTH};

const TP_INT: i32 = 23;
const TOUCH: u8 = wakey::board::touch::ADDR;
const SLEEP_US: u64 = 5_000_000;

const GREEN: u16 = 0x0300;
const BLUE: u16 = 0x0010;
const ORANGE: u16 = 0xa200;
const WHITE: u16 = 0xffff;

// ── TP_INT edges ────────────────────────────────────────────────────────────

static FALLS: AtomicU32 = AtomicU32::new(0);
static LOW_AT: AtomicU32 = AtomicU32::new(0);
static MAX_LOW_US: AtomicU32 = AtomicU32::new(0);
static MIN_LOW_US: AtomicU32 = AtomicU32::new(u32::MAX);

unsafe extern "C" fn on_tp_int(_: *mut c_void) {
    let now = unsafe { sys::esp_timer_get_time() } as u32;
    if unsafe { sys::gpio_get_level(TP_INT) } == 0 {
        FALLS.fetch_add(1, Ordering::Relaxed);
        LOW_AT.store(now, Ordering::Relaxed);
    } else {
        let low = now.wrapping_sub(LOW_AT.load(Ordering::Relaxed));
        MAX_LOW_US.fetch_max(low, Ordering::Relaxed);
        MIN_LOW_US.fetch_min(low, Ordering::Relaxed);
    }
}

/// Counts `TP_INT` edges until [`edges_stop`] (an ISR: catches pulses
/// that polling misses).
fn edges_start() {
    FALLS.store(0, Ordering::Relaxed);
    MAX_LOW_US.store(0, Ordering::Relaxed);
    MIN_LOW_US.store(u32::MAX, Ordering::Relaxed);
    unsafe {
        sys::gpio_install_isr_service(0);
        sys::gpio_isr_handler_add(TP_INT, Some(on_tp_int), core::ptr::null_mut());
        sys::gpio_set_intr_type(TP_INT, sys::gpio_int_type_t_GPIO_INTR_ANYEDGE);
        sys::gpio_intr_enable(TP_INT);
    }
}

/// Removes the ISR (none during sleep); returns (falls, shortest and
/// longest low in µs).
fn edges_stop() -> (u32, u32, u32) {
    unsafe {
        sys::gpio_intr_disable(TP_INT);
        sys::gpio_isr_handler_remove(TP_INT);
        sys::gpio_uninstall_isr_service();
    }
    let min = MIN_LOW_US.load(Ordering::Relaxed);
    (
        FALLS.load(Ordering::Relaxed),
        if min == u32::MAX { 0 } else { min },
        MAX_LOW_US.load(Ordering::Relaxed),
    )
}

// ── Touch controller registers ──────────────────────────────────────────────

fn rd(bus: &SharedI2c, reg: u16, buf: &mut [u8]) -> bool {
    i2c::lock(bus).write_read(TOUCH, &reg.to_be_bytes(), buf).is_ok()
}

fn rd8(bus: &SharedI2c, reg: u16) -> Option<u8> {
    let mut b = [0u8];
    rd(bus, reg, &mut b).then_some(b[0])
}

fn wr8(bus: &SharedI2c, reg: u16, value: u8) -> bool {
    let [h, l] = reg.to_be_bytes();
    i2c::lock(bus).write(TOUCH, &[h, l, value]).is_ok()
}

fn scans(bus: &SharedI2c) -> Option<u16> {
    let mut b = [0u8; 2];
    rd(bus, 0x000A, &mut b).then(|| u16::from_be_bytes(b))
}

/// Scans per second over one second (None: counter unreadable).
fn scan_rate(bus: &SharedI2c) -> Option<u16> {
    let a = scans(bus)?;
    std::thread::sleep(Duration::from_secs(1));
    Some(scans(bus)?.wrapping_sub(a))
}

fn status_name(s: Option<u8>) -> &'static str {
    match s.map(|s| s & 0x0f) {
        Some(0) => "NORMAL",
        Some(1) => "STARTING",
        Some(2) => "ERROR",
        Some(3) => "SMART WAKE UP",
        Some(4) => "IDLE",
        Some(5) => "POWER DOWN",
        Some(6) => "PRE-WAKE",
        Some(_) => "OTHER",
        None => "NO ANSWER",
    }
}

/// Reads the report (0x0010 up to the last coordinate) so the controller
/// releases `TP_INT`; returns the gesture ID (0x0012).
fn read_report(bus: &SharedI2c) -> Option<u8> {
    let max = rd8(bus, 0x0009).unwrap_or(10).clamp(1, 10) as usize;
    let mut buf = [0u8; 4 + 7 * 10];
    rd(bus, 0x0010, &mut buf[..4 + 7 * max]).then_some(buf[2])
}

fn gesture_name(g: u8) -> String {
    match g {
        0 => "NONE".into(),
        0xB0 => "DOUBLE TAP".into(),
        0xB1 => "TAP".into(),
        0xB2 => "LONG PRESS".into(),
        0xC0 => "SWIPE RIGHT".into(),
        0xC1 => "SWIPE LEFT".into(),
        0xC2 => "SWIPE DOWN".into(),
        0xC3 => "SWIPE UP".into(),
        0xFF => "UNKNOWN".into(),
        g => format!("CODE {g}"),
    }
}

fn int_low() -> bool {
    unsafe { sys::gpio_get_level(TP_INT) == 0 }
}

/// Logs the controller's registers and returns screen lines about them.
fn registers(bus: &SharedI2c, when: &str) -> Vec<String> {
    let status = rd8(bus, 0x0001);
    let misc = rd8(bus, 0x00F0);
    let rate = scan_rate(bus);
    log::info!(
        "spike: {when}: firmware {:?}, status {:?} ({}), misc 0x00F0 {:?}, control 0x0002 {:?}, misc control 0x00F1 {:?}, {:?} scans/s, TP_INT {}",
        rd8(bus, 0x0000),
        status,
        status_name(status),
        misc,
        rd8(bus, 0x0002),
        rd8(bus, 0x00F1),
        rate,
        unsafe { sys::gpio_get_level(TP_INT) }
    );
    vec![
        format!("STATE: {}", status_name(status)),
        format!(
            "SMART WAKE: {}",
            match misc {
                Some(m) if m & 0x80 != 0 => "YES",
                Some(_) => "NO",
                None => "?",
            }
        ),
        match rate {
            Some(r) => format!("SCANS/SECOND: {r}"),
            None => "SCANS: ?".into(),
        },
    ]
}

// ── Screen: 5×7 capitals, scaled ────────────────────────────────────────────

fn glyph(c: char) -> [u8; 7] {
    match c {
        'A' => [14, 17, 17, 31, 17, 17, 17],
        'B' => [30, 17, 17, 30, 17, 17, 30],
        'C' => [14, 17, 16, 16, 16, 17, 14],
        'D' => [30, 17, 17, 17, 17, 17, 30],
        'E' => [31, 16, 16, 30, 16, 16, 31],
        'F' => [31, 16, 16, 30, 16, 16, 16],
        'G' => [14, 17, 16, 23, 17, 17, 15],
        'H' => [17, 17, 17, 31, 17, 17, 17],
        'I' => [14, 4, 4, 4, 4, 4, 14],
        'J' => [7, 2, 2, 2, 2, 18, 12],
        'K' => [17, 18, 20, 24, 20, 18, 17],
        'L' => [16, 16, 16, 16, 16, 16, 31],
        'M' => [17, 27, 21, 21, 17, 17, 17],
        'N' => [17, 17, 25, 21, 19, 17, 17],
        'O' => [14, 17, 17, 17, 17, 17, 14],
        'P' => [30, 17, 17, 30, 16, 16, 16],
        'Q' => [14, 17, 17, 17, 21, 18, 13],
        'R' => [30, 17, 17, 30, 20, 18, 17],
        'S' => [15, 16, 16, 14, 1, 1, 30],
        'T' => [31, 4, 4, 4, 4, 4, 4],
        'U' => [17, 17, 17, 17, 17, 17, 14],
        'V' => [17, 17, 17, 17, 17, 10, 4],
        'W' => [17, 17, 17, 21, 21, 21, 10],
        'X' => [17, 17, 10, 4, 10, 17, 17],
        'Y' => [17, 17, 17, 10, 4, 4, 4],
        'Z' => [31, 1, 2, 4, 8, 16, 31],
        '0' => [14, 17, 19, 21, 25, 17, 14],
        '1' => [4, 12, 4, 4, 4, 4, 14],
        '2' => [14, 17, 1, 2, 4, 8, 31],
        '3' => [31, 2, 4, 2, 1, 17, 14],
        '4' => [2, 6, 10, 18, 31, 2, 2],
        '5' => [31, 16, 30, 1, 1, 17, 14],
        '6' => [6, 8, 16, 30, 17, 17, 14],
        '7' => [31, 1, 2, 4, 8, 8, 8],
        '8' => [14, 17, 17, 14, 17, 17, 14],
        '9' => [14, 17, 17, 15, 1, 2, 12],
        '.' => [0, 0, 0, 0, 0, 12, 12],
        ',' => [0, 0, 0, 0, 12, 4, 8],
        ':' => [0, 12, 12, 0, 12, 12, 0],
        '!' => [4, 4, 4, 4, 4, 0, 4],
        '?' => [14, 17, 1, 2, 4, 0, 4],
        '-' => [0, 0, 0, 31, 0, 0, 0],
        _ => [0; 7],
    }
}

/// Pixels per font dot: 30×42 px letters, 36 px apart, 20 per line.
const DOT: usize = 6;
const LINE: usize = 64;

/// Fills the screen with `bg` and writes `lines` centred, in white.
fn screen(display: &mut Display, bg: u16, lines: &[&str]) {
    // The stream may not come back from sleep: then no frame ends.
    let mut tries = 0;
    let fb = loop {
        if let Some(fb) = display.try_back_buffer() {
            break fb;
        }
        tries += 1;
        if tries > 100 {
            log::warn!("spike: no frame finished in 500 ms, picture not updated");
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    fb.fill(bg);
    let top = (HEIGHT - lines.len() * LINE) / 2;
    for (row, text) in lines.iter().enumerate() {
        let width = text.chars().count() * 6 * DOT;
        let left = WIDTH.saturating_sub(width) / 2;
        for (col, ch) in text.chars().enumerate() {
            let x0 = left + col * 6 * DOT;
            for (gy, bits) in glyph(ch).iter().enumerate() {
                for gx in 0..5 {
                    if bits >> (4 - gx) & 1 == 0 {
                        continue;
                    }
                    let x = x0 + gx * DOT;
                    for y in top + row * LINE + gy * DOT..top + row * LINE + (gy + 1) * DOT {
                        if y < HEIGHT && x + DOT <= WIDTH {
                            fb[y * WIDTH + x..y * WIDTH + x + DOT].fill(WHITE);
                        }
                    }
                }
            }
        }
    }
    display.present(0..HEIGHT).expect("present");
}


fn countdown(display: &mut Display, bg: u16, head: &[&str], secs: u32, what: &str) {
    for left in (1..=secs).rev() {
        let count = format!("{what} IN {left} SECONDS");
        let mut lines = head.to_vec();
        lines.extend_from_slice(&["", &count]);
        screen(display, bg, &lines);
        std::thread::sleep(Duration::from_secs(1));
    }
}

// ── Smart Wake Up with the panel asleep ─────────────────────────────────────

/// The phone sequence: Smart Wake Up enabled, display off and sleep in,
/// touch power down. The screen is black afterwards.
/// 0x00F1 as the firmware set it, restored by [`leave_swk`].
static ORIG_F1: AtomicU32 = AtomicU32::new(0x04);

/// `misc`: the value for 0x00F1 (Smart Wake Up enable is bit 7); `None`
/// keeps the firmware's other bits (it sets 0x04 by itself).
fn enter_swk(display: &Display, bus: &SharedI2c, misc: Option<u8>) {
    let orig = rd8(bus, 0x00F1).unwrap_or(0x04);
    ORIG_F1.store(orig.into(), Ordering::Relaxed);
    let value = misc.unwrap_or(orig | 0x80);
    let a = wr8(bus, 0x00F1, value);
    log::info!("spike: 0x00F1 was 0x{orig:02x}, wrote 0x{value:02x}, reads 0x{:02x}", rd8(bus, 0x00F1).unwrap_or(0));
    display.set_panel_sleep(true).unwrap();
    let b = wr8(bus, 0x0002, 0x02);
    std::thread::sleep(Duration::from_millis(300));
    log::info!("spike: smart wake up written {a}, power down written {b}");
}

/// The touch half of [`leave_swk`] (I2C only).
fn leave_touch_swk(bus: &SharedI2c) {
    let a = wr8(bus, 0x0002, 0x00);
    let b = wr8(bus, 0x00F1, ORIG_F1.load(Ordering::Relaxed) as u8 & !0x80);
    log::info!("spike: power down cleared {a}, smart wake up cleared {b}");
}

fn leave_swk(display: &Display, bus: &SharedI2c) {
    let a = wr8(bus, 0x0002, 0x00);
    let b = wr8(bus, 0x00F1, ORIG_F1.load(Ordering::Relaxed) as u8 & !0x80);
    // Sleep out alone left the panel black after a wake: full init.
    let r = display.reinit_panel();
    log::info!("spike: power down cleared {a}, smart wake up cleared {b}, panel init {r:?}");
}

/// Answers every `TP_INT` low by reading the report for `secs`, and reads
/// the gesture register every 100 ms regardless; returns the number of
/// lows seen by polling and the gestures (marked by how they were found).
fn collect(bus: &SharedI2c, secs: u64) -> (u32, Vec<String>) {
    let end = Instant::now() + Duration::from_secs(secs);
    let mut seen = Vec::new();
    let mut lows = 0;
    let mut next_poll = Instant::now();
    while Instant::now() < end {
        let low = int_low();
        let polled = Instant::now() >= next_poll;
        if low || polled {
            if low {
                lows += 1;
            }
            if polled {
                next_poll = Instant::now() + Duration::from_millis(100);
            }
            if rd8(bus, 0x0012).unwrap_or(0) != 0 || low {
                if let Some(g) = read_report(bus).filter(|g| *g != 0) {
                    let how = if low { "INT" } else { "READ" };
                    log::info!("spike: gesture 0x{g:02x} {} ({how})", gesture_name(g));
                    seen.push(format!("{} ({how})", gesture_name(g)));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    (lows, seen)
}

/// Chip awake, touch controller as in sleep (Smart Wake Up with `misc` in
/// 0x00F1, panel asleep, colour bars), 10 s of single taps. Polls the
/// report every 20 ms: does a single tap show up as a gesture or as a
/// touch point? Returns (TP_INT falls, touches seen, gestures).
fn tap_variant(display: &mut Display, bus: &SharedI2c, misc: u8) -> (u32, u32, Vec<String>) {
    enter_swk(display, bus, Some(misc));
    display.set_test_pattern(true).unwrap();
    edges_start();
    let max = rd8(bus, 0x0009).unwrap_or(10).clamp(1, 10) as usize;
    let mut buf = [0u8; 4 + 7 * 10];
    let mut touches = 0;
    let mut finger = false;
    let mut gestures = Vec::new();
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if rd(bus, 0x0010, &mut buf[..4 + 7 * max]) {
            let any = buf[4..4 + 7 * max].chunks(7).any(|r| r[0] & 0x80 != 0);
            if any && !finger {
                touches += 1;
                log::info!("spike: 0x{misc:02x}: touch point, report {:02x?}", &buf[..11]);
            }
            finger = any;
            if buf[2] != 0 {
                log::info!("spike: 0x{misc:02x}: gesture 0x{:02x}", buf[2]);
                gestures.push(gesture_name(buf[2]));
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let (falls, min_us, max_us) = edges_stop();
    display.set_test_pattern(false).unwrap();
    leave_swk(display, bus);
    log::info!(
        "spike: 0x{misc:02x}: {falls} TP_INT falls ({min_us}..{max_us} us), {touches} touches seen, gestures {gestures:?}"
    );
    (falls, touches, gestures)
}

fn sleep_round(display: &mut Display, bus: &SharedI2c, nvs: &EspNvs<NvsDefault>, round: u32) {
    // Which bits of 0x00F1 choose the wake gestures? (Not documented.)
    // Found so far: 0x84 (firmware) double tap; bit 0 (0x85) adds long
    // press; bit 1 (0x86) woke on a single tap, reported as 0xC5 (arrow
    // right): handwriting? 0xBC recognised 0xC4 without waking; 0xFF
    // nothing. Bit 1 again: a tap did not wake, a double tap did, and the
    // panel stayed black after every wake with bit 1 (stream running,
    // power mode 0x9c = on). Single tap (0xB1) was never reported.
    // Now: no touching, timer only: does the picture come back?
    let misc = 0x85;
    let title = format!("PART 3, SLEEP {round} OF 4");
    let head = vec![
        title.as_str(),
        "",
        "DO NOT TOUCH.",
        "THE SCREEN GOES",
        "BLACK FOR 5 SECONDS.",
        "",
        "DOES THIS SCREEN",
        "COME BACK AFTER?",
    ];
    countdown(display, BLUE, &head, 3, "SLEEP");
    enter_swk(display, bus, Some(misc));
    let _ = nvs.set_u32(&format!("m{}", round), misc.into());
    // The whole display link down during sleep: light sleep stops the DSI
    // PHY clock, and afterwards commands stuck in the DSI command FIFO
    // (task watchdog in mipi_dsi_hal_host_gen_write_dcs_command). Rebuilt
    // after waking as at boot.
    display.link_down().unwrap();
    // TP_INT must be high, or the sleep ends at once.
    for _ in 0..200 {
        if !int_low() {
            break;
        }
        let _ = read_report(bus);
        std::thread::sleep(Duration::from_millis(10));
    }
    let n = nvs.get_u32("n").ok().flatten().unwrap_or(0) + 1;
    let _ = nvs.set_u32("n", n);
    let _ = nvs.set_u32(&format!("p{n}"), round);
    let _ = nvs.set_u32(&format!("c{n}"), 9); // 9: did not come back
    for k in ["t", "r", "g", "f", "w"] {
        let _ = nvs.set_u32(&format!("{k}{n}"), 0);
    }
    log::info!("spike: sleep {round}: sleeping, TP_INT low {}", int_low());
    std::thread::sleep(Duration::from_millis(100));
    let (cause, ms, ret) = unsafe {
        // Keep the pin's awake configuration (input, pull-up) in sleep:
        // the sleep configuration may switch the input off.
        sys::gpio_sleep_sel_dis(TP_INT);
        sys::gpio_wakeup_enable(TP_INT, sys::gpio_int_type_t_GPIO_INTR_LOW_LEVEL);
        sys::esp_sleep_enable_gpio_wakeup();
        sys::esp_sleep_enable_timer_wakeup(SLEEP_US);
        let t0 = sys::esp_timer_get_time();
        let ret = sys::esp_light_sleep_start();
        let ms = ((sys::esp_timer_get_time() - t0) / 1000) as u32;
        let cause = sys::esp_sleep_get_wakeup_cause();
        sys::gpio_wakeup_disable(TP_INT);
        sys::esp_sleep_disable_wakeup_source(sys::esp_sleep_source_t_ESP_SLEEP_WAKEUP_ALL);
        (cause, ms, ret)
    };
    let code = match cause {
        sys::esp_sleep_source_t_ESP_SLEEP_WAKEUP_GPIO => 1,
        sys::esp_sleep_source_t_ESP_SLEEP_WAKEUP_TIMER => 2,
        _ => 0,
    };
    let gesture = read_report(bus).unwrap_or(0);
    let _ = nvs.set_u32(&format!("c{n}"), code);
    let _ = nvs.set_u32(&format!("t{n}"), ms);
    let _ = nvs.set_u32(&format!("r{n}"), ret as u32);
    let _ = nvs.set_u32(&format!("g{n}"), gesture.into());
    log::info!(
        "spike: sleep {round}: woke by {code} (cause {cause}) after {ms} ms, returned {ret}, gesture 0x{gesture:02x}"
    );
    registers(bus, "after the sleep");
    leave_touch_swk(bus);
    let up = display.link_up();
    log::info!("spike: sleep {round}: display link up {up:?}");
    // Is the stream running, is the panel on?
    let f0 = Display::frames_scanned();
    std::thread::sleep(Duration::from_millis(200));
    let frames = Display::frames_scanned().wrapping_sub(f0);
    // Not read: a DCS read (bus turnaround) during video mode may be what
    // left the panel black after waking (it reported 0x9c = on).
    let mode = 0u32;
    log::info!("spike: sleep {round}: after wake {frames} frames in 200 ms, power mode 0x{mode:02x}");
    let _ = nvs.set_u32(&format!("f{n}"), frames);
    let _ = nvs.set_u32(&format!("w{n}"), mode);
    let by = match code {
        1 => "BY YOUR TOUCH.",
        2 => "BY THE TIMER.",
        _ => "AT ONCE.",
    };
    let slept = format!("SLEPT {} SECONDS.", ms / 1000);
    let g = format!("GESTURE: {}", gesture_name(gesture));
    countdown(display, ORANGE, &[&title, "", "AWAKE AGAIN,", by, &slept, &g], 6, "NEXT");
}

/// Logs the stored sleeps; returns one short line per sleep for the screen.
fn report(nvs: &EspNvs<NvsDefault>) -> Vec<String> {
    let mut lines = Vec::new();
    let get = |k: &str| nvs.get_u32(k).ok().flatten().unwrap_or(0);
    let n = get("n");
    log::info!("spike: {n} sleeps recorded (woken by 1 touch, 2 timer, 0 other, 9 never)");
    for i in 1..=n {
        log::info!(
            "spike: sleep {} (0x00F1 = 0x{:02x}): woken by {}, {} ms, returned {}, gesture 0x{:02x}; then {} frames in 200 ms, power mode 0x{:02x}",
            get(&format!("p{i}")),
            get(&format!("m{}", get(&format!("p{i}")))),
            get(&format!("c{i}")),
            get(&format!("t{i}")),
            get(&format!("r{i}")),
            get(&format!("g{i}")),
            get(&format!("f{i}")),
            get(&format!("w{i}"))
        );
        let by = match get(&format!("c{i}")) {
            1 => "TOUCH",
            2 => "TIMER",
            9 => "NEVER WOKE",
            _ => "OTHER",
        };
        let pic = if get(&format!("w{i}")) & 0x04 != 0 { "ON" } else { "OFF" };
        lines.push(format!("{i}: {by}, PANEL {pic}"));
    }
    lines
}

fn main() {
    sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    let nvs = EspNvs::new(EspDefaultNvsPartition::take().unwrap(), "chipsleep", true).unwrap();
    let previous = report(&nvs);

    let Board {
        mut backlight,
        mut display,
        i2c: bus,
        ..
    } = Board::init().expect("board");
    backlight.set_level(0.3).unwrap();
    unsafe {
        sys::gpio_reset_pin(TP_INT);
        sys::gpio_set_direction(TP_INT, sys::gpio_mode_t_GPIO_MODE_INPUT);
        sys::gpio_set_pull_mode(TP_INT, sys::gpio_pull_mode_t_GPIO_PULLUP_ONLY);
    }
    // The last run's results first (the USB log is often not connected
    // yet at boot): on screen and in the log, 15 s.
    for left in (1..=15).rev() {
        log::info!("spike: previous run:");
        report(&nvs);
        let count = format!("START IN {left} SECONDS");
        let mut lines: Vec<&str> = vec!["LAST RUN:", ""];
        if previous.is_empty() {
            lines.push("NO SLEEPS");
        }
        lines.extend(previous.iter().map(String::as_str));
        lines.extend_from_slice(&["", &count]);
        screen(&mut display, GREEN, &lines);
        std::thread::sleep(Duration::from_secs(1));
    }
    let _ = nvs.set_u32("n", 0);
    countdown(
        &mut display,
        GREEN,
        &["SLEEP TEST", "", "DO NOT TOUCH THE", "SCREEN UNTIL IT", "ASKS YOU TO."],
        5,
        "START",
    );

    // Part 1: normal mode.
    screen(&mut display, GREEN, &["PART 1 OF 3", "", "READING THE TOUCH", "CHIP. DO NOT TOUCH."]);
    registers(&bus, "normal mode");

    // Part 2 (round 6): single taps with 0x00F1 = 0x84, 0x8C, 0x94, 0xA4,
    // 0xC4: the report never showed a touch point ("touches seen" 0 in
    // every round), so polling by timer cannot find a single tap either.

    // Part 3: does the picture come back after light sleep? No touching.
    for round in 1..=4 {
        sleep_round(&mut display, &bus, &nvs, round);
    }
    screen(&mut display, GREEN, &["DONE.", "", "THANK YOU!"]);
    loop {
        log::info!("spike: done");
        report(&nvs);
        std::thread::sleep(Duration::from_secs(10));
    }
}
