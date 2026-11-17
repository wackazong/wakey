//! Sample-rate conversion for interleaved stereo i16, on top of rubato's
//! FFT resampler (`FftFixedInOut`: a windowed-sinc anti-aliasing filter,
//! exact for fixed ratios such as 44.1 → 48 kHz).

use rubato::{FftFixedInOut, Resampler};

/// A stream from one sample rate to another, fed in pieces of any size.
pub struct Converter {
    resampler: FftFixedInOut<f32>,
    /// Input frames rubato takes per block.
    in_frames: usize,
    /// Input frames not converted yet (interleaved).
    pending: Vec<i16>,
    planar_in: Vec<Vec<f32>>,
}

impl Converter {
    pub fn new(from_hz: u32, to_hz: u32) -> Result<Self, String> {
        // About 1024 input frames per block; rubato rounds to the ratio.
        let resampler = FftFixedInOut::<f32>::new(from_hz as usize, to_hz as usize, 1024, 2)
            .map_err(|e| e.to_string())?;
        let in_frames = resampler.input_frames_next();
        Ok(Self {
            resampler,
            in_frames,
            pending: Vec::with_capacity(in_frames * 4),
            planar_in: vec![vec![0.0; in_frames]; 2],
        })
    }

    /// Input frames per conversion block.
    pub fn block_frames(&self) -> usize {
        self.in_frames
    }

    /// Converts `input` (interleaved stereo) and appends the result to
    /// `out`. Input that does not fill a whole block is kept for the next
    /// call, so output lags the input by up to one block.
    pub fn process(&mut self, input: &[i16], out: &mut Vec<i16>) {
        self.pending.extend_from_slice(input);
        let mut blocks = self.pending.chunks_exact(self.in_frames * 2);
        for block in &mut blocks {
            for (i, frame) in block.as_chunks::<2>().0.iter().enumerate() {
                self.planar_in[0][i] = f32::from(frame[0]) / 32768.0;
                self.planar_in[1][i] = f32::from(frame[1]) / 32768.0;
            }
            let res = self
                .resampler
                .process(&self.planar_in, None)
                .expect("block has the size rubato asked for");
            for (l, r) in res[0].iter().zip(res[1].iter()) {
                out.push((l * 32767.0).clamp(-32768.0, 32767.0) as i16);
                out.push((r * 32767.0).clamp(-32768.0, 32767.0) as i16);
            }
        }
        let converted = self.pending.len() - blocks.remainder().len();
        self.pending.drain(..converted);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratio_44100_to_48000() {
        let mut c = Converter::new(44_100, 48_000).unwrap();
        assert_eq!(c.block_frames(), 1029); // 44100/48000 = 147/160
        let seconds = 2;
        let input: Vec<i16> = (0..44_100 * seconds)
            .flat_map(|i| {
                let s =
                    ((i as f32 * 440.0 / 44_100.0 * std::f32::consts::TAU).sin() * 8000.0) as i16;
                [s, -s]
            })
            .collect();
        let mut out = Vec::new();
        c.process(&input, &mut out);
        let frames = out.len() / 2;
        // Whole blocks only: within one block of the exact length.
        let exact = 48_000 * seconds;
        assert!(frames <= exact && exact - frames <= 1120, "{frames}");
        // A sine stays a sine of the same level (after the filter delay).
        let peak = out[4000..].iter().map(|s| s.unsigned_abs()).max().unwrap();
        assert!((7500..=8500).contains(&peak), "peak {peak}");
    }

    #[test]
    fn pieces_of_any_size_give_the_same_output() {
        let input: Vec<i16> = (0..3000i16).flat_map(|i| [i * 7, -i]).collect();
        let mut whole = Vec::new();
        Converter::new(44_100, 48_000)
            .unwrap()
            .process(&input, &mut whole);
        assert!(!whole.is_empty());
        let mut c = Converter::new(44_100, 48_000).unwrap();
        let mut pieces = Vec::new();
        for piece in input.chunks(2 * 700) {
            c.process(piece, &mut pieces);
        }
        assert_eq!(pieces, whole);
    }
}
