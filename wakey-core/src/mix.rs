//! Sample operations for the built-in speaker's mixer: interleaved stereo
//! i16 at 48 kHz, as the player and the I2S stream use.

/// Both channels of each frame set to their mean: the built-in speaker's
/// amplifier takes the left channel only, and a stereo file should not
/// lose its right half.
pub fn to_mono(frames: &mut [i16]) {
    for f in frames.as_chunks_mut::<2>().0 {
        let m = ((i32::from(f[0]) + i32::from(f[1])) / 2) as i16;
        f[0] = m;
        f[1] = m;
    }
}

/// `dst += src` sample by sample, clipped to the i16 range.
pub fn add(dst: &mut [i16], src: &[i16]) {
    for (d, &s) in dst.iter_mut().zip(src) {
        *d = d.saturating_add(s);
    }
}

/// `dst += mono · gain` for interleaved stereo `dst`, both channels.
pub fn add_mono(dst: &mut [i16], mono: &[i16], gain: f32) {
    for (f, &s) in dst.as_chunks_mut::<2>().0.iter_mut().zip(mono) {
        let v = (f32::from(s) * gain) as i16;
        f[0] = f[0].saturating_add(v);
        f[1] = f[1].saturating_add(v);
    }
}

/// Inaudible signal for `frames` stereo frames: ±2 (of 32767) alternating
/// at half the sample rate, which the codec filters out. Mixed into
/// everything the built-in speaker plays, so it never gets exact digital
/// silence: on silence something in the chain mutes and a following sound
/// loses its start.
pub fn keepalive(frames: usize) -> Vec<i16> {
    (0..frames)
        .flat_map(|i| {
            let v = if i % 2 == 0 { 2 } else { -2 };
            [v, v]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_is_the_mean() {
        let mut b = [100, 300, -32768, 32767, 7, 7];
        to_mono(&mut b);
        assert_eq!(b, [200, 200, 0, 0, 7, 7]);
    }

    #[test]
    fn add_clips() {
        let mut b = [32000, -32000, 5];
        add(&mut b, &[1000, -1000, 5]);
        assert_eq!(b, [32767, -32768, 10]);
    }

    #[test]
    fn add_mono_to_both_channels() {
        let mut b = [0i16; 6];
        add_mono(&mut b, &[1000, -2000, 32767], 0.5);
        assert_eq!(b, [500, 500, -1000, -1000, 16383, 16383]);
    }

    #[test]
    fn keepalive_is_tiny_and_never_zero() {
        let k = keepalive(4);
        assert_eq!(k, [2, 2, -2, -2, 2, 2, -2, -2]);
        // Added to silence, nothing is zero any more.
        let mut b = [0i16; 8];
        add(&mut b, &k);
        assert!(b.iter().all(|&s| s != 0 && s.abs() <= 2));
    }
}
