//! Spike: what does each part of the Tab5 draw while the display sleeps
//! (backlight off, everything else running)? Read off a USB power meter at
//! the USB-C input (the on-board INA226 only sees a battery, and this Tab5
//! has none).
//!
//! Seven steps, cumulative, each announced on screen for 6 s, then 25 s
//! dark for the reading (`METER_STEPS`): screen on (30 %), display sleep as
//! in the app (66 Hz stream, I2S and ES8388 running, USB audio and USB-A
//! 5 V on), codec off, USB-A 5 V off, 33 Hz, 22 Hz, display link down. A
//! refresh change rebuilds the whole link (a new stream alone leaves the
//! panel black); below 66 Hz the panel shows nothing but touch still
//! scans (5 and 10 taps seen at 33 and 22 Hz).
//!
//! Round 1 (USB meter, Pebble connected but switched off): screen on 242
//! mA, display sleep 157, codec off 148, USB-A 5 V off 148, 33 Hz 127,
//! 22 Hz 127, link down 118 (at ~5 V). In every step the current dipped
//! ~20 mA about 10 s after its start and ramped back.
//!
//! Round 2 (this code): USB-A on, off, on again in display sleep, with the
//! Pebble switched on.
//!
//! Not covered here: the camera light meter, CPU wake-ups, the unused
//! chips (ES7210, BMI270).
//!
//! `cargo run --release --bin power_probe`

use esp_idf_svc::sys;
use std::time::Duration;
use wakey::board::Board;
use wakey::board::i2c;
use wakey::board::ioexp::Pin;
use wakey::display::{Display, HEIGHT, WIDTH};

const GREEN: u16 = 0x0300;
const BLUE: u16 = 0x0010;
const WHITE: u16 = 0xffff;

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

/// Pixels per font dot: 25×35 px letters, 30 px apart, 24 per line.
const DOT: usize = 5;
const LINE: usize = 54;

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


/// Round 2: USB-A 5 V with the Pebble switched on, in display sleep.
const METER_STEPS: [&str; 3] = ["SLEEP, USB-A ON", "SLEEP, USB-A OFF", "SLEEP, USB-A ON AGAIN"];

fn main() {
    sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    let Board {
        mut backlight,
        mut display,
        i2c: bus,
        ioexp,
        modem: _,
        codec,
        ..
    } = Board::init().expect("board");
    backlight.set_level(0.3).unwrap();

    // As in the app: built-in speaker path and USB audio running.
    let player = wakey::audio::player::Player::new();
    let _speaker = wakey::audio::builtin::BuiltinSpeaker::start(codec, bus.clone(), ioexp.clone(), player.source());
    let _usb = wakey::audio::uac::UsbAudio::start(ioexp.switch(Pin::Usb5vEn), player.source()).ok();

    screen(&mut display, GREEN, &["POWER TEST", "", "3 STEPS. NOTE THE", "METER IN EACH.", "", "TAP TO START."]);
    while !matches!(wakey::board::touch::read_point(&mut i2c::lock(&bus)), Ok(Some(_))) {
        std::thread::sleep(Duration::from_millis(20));
    }

    for (i, step) in METER_STEPS.iter().enumerate() {
        let t = format!("STEP {} OF {}", i + 1, METER_STEPS.len());
        backlight.set_level(0.3).unwrap();
        countdown(
            &mut display,
            BLUE,
            &[&t, "", step, "", "THE SCREEN GOES", "DARK. WAIT 5 S,", "THEN NOTE THE", "METER."],
            6,
            "DARK",
        );
        backlight.set_level(0.0).unwrap();
        let _ = ioexp.set(Pin::Usb5vEn, i != 1);
        log::info!("probe: step {}: {step}", i + 1);
        std::thread::sleep(Duration::from_secs(30));
    }
    // (Round 1's steps, codec to link down, are in the module docs.)
    let _ = display.link_down();
    display.link_up().unwrap();
    backlight.set_level(0.3).unwrap();
    loop {
        log::info!("probe: done");
        screen(&mut display, GREEN, &["DONE.", "", "NOTE THE", "3 READINGS."]);
        std::thread::sleep(Duration::from_secs(10));
    }
}
