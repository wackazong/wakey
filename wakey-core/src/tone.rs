//! The built-in alarm tone (used when no sound file is set or it cannot be
//! played) and the volume curve with fade-in.

use std::f32::consts::TAU;

/// Beep pitch; a whole number of cycles per pattern period, so the table
/// loops without a seam.
const HZ: u64 = 880;
/// One period of the pattern: beep, gap, beep, pause (seconds).
const PATTERN: [(bool, f32); 4] = [(true, 0.15), (false, 0.1), (true, 0.15), (false, 0.6)];
/// Edge ramp against clicks (seconds).
const RAMP: f32 = 0.005;
/// Peak amplitude (of i16 full scale).
const LEVEL: f32 = 0.5;

/// Endless double-beep pattern as interleaved stereo i16.
///
/// One period of the pattern (1 s, a whole number of 880 Hz cycles) is
/// computed once and played from a table: per-sample `sin` and `%` cost a
/// whole core on the ESP32-P4 (single-precision FPU only, soft-float and
/// slow range reduction in the C library) and starved the UI.
pub struct Beeper {
    /// One period, mono.
    table: Vec<i16>,
    /// Frames since the start.
    pos: u64,
}

impl Beeper {
    pub fn new(sample_rate: u32) -> Self {
        let rate = sample_rate as f32;
        let mut table = Vec::new();
        for (on, len) in PATTERN {
            let n = (len * rate).round() as usize;
            for i in 0..n {
                let at = i as f32 / rate;
                let level = if on {
                    LEVEL * (at.min(len - at) / RAMP).min(1.0)
                } else {
                    0.0
                };
                // Phase within one cycle, exact in integers.
                let k = table.len() as u64;
                let phase = (k * HZ % u64::from(sample_rate)) as f32 / rate;
                table.push((level * (TAU * phase).sin() * f32::from(i16::MAX)) as i16);
            }
        }
        Self { table, pos: 0 }
    }

    /// Fills `out` (frames × 2).
    pub fn fill(&mut self, out: &mut [i16]) {
        let len = self.table.len() as u64;
        let mut i = (self.pos % len) as usize;
        for frame in out.chunks_exact_mut(2) {
            let s = self.table[i];
            frame[0] = s;
            frame[1] = s;
            i += 1;
            if i == self.table.len() {
                i = 0;
            }
        }
        self.pos += (out.len() / 2) as u64;
    }
}

/// Linear gain for a volume of 0–100 %: roughly even loudness steps
/// (−40 dB at 1 %, 0 dB at 100 %), 0 % is silent.
pub fn volume_gain(percent: u8) -> f32 {
    match percent.min(100) {
        0 => 0.0,
        p => 10f32.powf((f32::from(p) / 100.0 - 1.0) * 2.0),
    }
}

/// Gain `frames` frames into a fade-in of `fade_frames` (squared, so the
/// start is gentle), times `gain`.
pub fn faded(gain: f32, frames: u64, fade_frames: u64) -> f32 {
    if frames >= fade_frames {
        return gain;
    }
    let x = frames as f32 / fade_frames as f32;
    gain * x * x
}

/// The wheel click: a short "tock" in the mids, mono, peaking at about
/// `amplitude` (0–1 of full scale). A 1.2 kHz tone (1.8 kHz was too
/// bright) with a 0.3 ms attack, dying away within 10 ms (2 ms decay): a
/// click rather than a tone, and over before the next one, which comes at
/// most once per frame (~15 ms; longer clicks ran together).
pub fn click(sample_rate: u32, amplitude: f32) -> Vec<i16> {
    const FREQ: f32 = 1200.0;
    const ATTACK_S: f32 = 0.0003;
    const DECAY_S: f32 = 0.002;
    const LEN_S: f32 = 0.01;
    let rate = sample_rate as f32;
    let n = (rate * LEN_S) as usize;
    (0..n)
        .map(|i| {
            let t = i as f32 / rate;
            let attack = if t < ATTACK_S {
                0.5 - 0.5 * (core::f32::consts::PI * t / ATTACK_S).cos()
            } else {
                1.0
            };
            let decay = (-(t - ATTACK_S).max(0.0) / DECAY_S).exp();
            let v = amplitude * attack * decay * (TAU * FREQ * t).sin();
            (v * f32::from(i16::MAX)) as i16
        })
        .collect()
}

/// Loudness of a click that follows the previous one after `gap_ms`
/// (`None`: the first): full from 100 ms apart, down to 0.6 at 20 ms or
/// less. Clicks in quick succession add up in the ear, so a fast-turning
/// wheel would sound louder than a slow one.
pub fn click_gain(gap_ms: Option<u32>) -> f32 {
    const FULL_MS: f32 = 100.0;
    const FAST_MS: f32 = 20.0;
    const FAST_GAIN: f32 = 0.6;
    let Some(gap) = gap_ms else { return 1.0 };
    let x = ((gap as f32 - FAST_MS) / (FULL_MS - FAST_MS)).clamp(0.0, 1.0);
    FAST_GAIN + (1.0 - FAST_GAIN) * x
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beeps_and_pauses() {
        let mut b = Beeper::new(48_000);
        let mut out = vec![0i16; 48_000 * 2]; // one period
        b.fill(&mut out);
        let peak = |from: f32, to: f32| {
            out[(from * 48_000.0) as usize * 2..(to * 48_000.0) as usize * 2]
                .iter()
                .map(|s| s.unsigned_abs())
                .max()
                .unwrap()
        };
        assert!(peak(0.01, 0.14) > 16_000);
        assert_eq!(peak(0.16, 0.24), 0);
        assert!(peak(0.26, 0.39) > 16_000);
        assert_eq!(peak(0.41, 0.99), 0);
        // Starts without a click.
        assert!(out[0].unsigned_abs() < 100 && out[2].unsigned_abs() < 500);
        assert!(out.chunks(2).all(|f| f[0] == f[1]));
    }

    #[test]
    fn volume_curve() {
        assert_eq!(volume_gain(0), 0.0);
        assert!((volume_gain(100) - 1.0).abs() < 1e-6);
        assert!((volume_gain(50) - 0.1).abs() < 1e-3);
        assert!(volume_gain(1) > 0.0 && volume_gain(1) < 0.011);
        assert_eq!(volume_gain(200), volume_gain(100));
    }

    #[test]
    fn fade_in() {
        assert_eq!(faded(0.8, 0, 100), 0.0);
        assert!((faded(0.8, 50, 100) - 0.2).abs() < 1e-6);
        assert_eq!(faded(0.8, 100, 100), 0.8);
        assert_eq!(faded(0.8, 5, 0), 0.8);
    }

    #[test]
    fn beeper_phase_stays_within_one_cycle() {
        // Same samples as the plain formula, at any position.
        let mut b = Beeper::new(48_000);
        b.pos = 48_000 * 3600 + 1234;
        let mut out = [0i16; 2 * 64];
        b.fill(&mut out);
        let t0 = 1234.0_f64 / 48_000.0;
        let expect = (std::f64::consts::TAU * 880.0 * t0).sin();
        // The first frame is inside a beep (0.0257 s into the pattern).
        assert!((f64::from(out[0]) / (0.5 * 32767.0) - expect).abs() < 0.01);
    }

    #[test]
    fn click_is_a_soft_short_tock() {
        let c = click(48_000, 0.5);
        assert_eq!(c.len(), 480);
        let peak = c.iter().map(|s| s.unsigned_abs()).max().unwrap();
        assert!(peak > 9_000 && peak <= 16_384, "peak {peak}");
        // Soft start: the first samples are small.
        assert!(
            c[..3].iter().all(|s| s.unsigned_abs() < 3000),
            "{:?}",
            &c[..3]
        );
        // Dies away by the end.
        let tail = c[440..].iter().map(|s| s.unsigned_abs()).max().unwrap();
        assert!(tail < 500, "tail {tail}");
    }

    #[test]
    fn fast_clicks_are_quieter() {
        assert_eq!(click_gain(None), 1.0);
        assert_eq!(click_gain(Some(500)), 1.0);
        assert_eq!(click_gain(Some(100)), 1.0);
        assert!((click_gain(Some(10)) - 0.6).abs() < 1e-6);
        let mid = click_gain(Some(60));
        assert!(mid > 0.6 && mid < 1.0);
    }
}
