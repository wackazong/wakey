//! The built-in alarm tone (used when no sound file is set or it cannot be
//! played) and the volume curve with fade-in.

use std::f32::consts::TAU;

/// Beep pitch.
const HZ: f32 = 880.0;
/// One period of the pattern: beep, gap, beep, pause (seconds).
const PATTERN: [(bool, f32); 4] = [(true, 0.15), (false, 0.1), (true, 0.15), (false, 0.6)];
/// Edge ramp against clicks (seconds).
const RAMP: f32 = 0.005;
/// Peak amplitude (of i16 full scale).
const LEVEL: f32 = 0.5;

/// Endless double-beep pattern as interleaved stereo i16.
pub struct Beeper {
    rate: f32,
    /// Frames since the start.
    pos: u64,
}

impl Beeper {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            rate: sample_rate as f32,
            pos: 0,
        }
    }

    fn period(&self) -> f32 {
        PATTERN.iter().map(|(_, s)| s).sum()
    }

    /// Fills `out` (frames × 2).
    pub fn fill(&mut self, out: &mut [i16]) {
        let period = self.period();
        for frame in out.chunks_exact_mut(2) {
            let t = self.pos as f32 / self.rate;
            let mut at = t % period;
            let mut level = 0.0;
            for (on, len) in PATTERN {
                if at < len {
                    if on {
                        let edge = at.min(len - at);
                        level = LEVEL * (edge / RAMP).min(1.0);
                    }
                    break;
                }
                at -= len;
            }
            // Phase from the frame count modulo one second keeps f32 exact.
            let phase = (self.pos % self.rate as u64) as f32 / self.rate * HZ;
            let s = (level * (TAU * phase).sin() * f32::from(i16::MAX)) as i16;
            frame[0] = s;
            frame[1] = s;
            self.pos += 1;
        }
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
}
