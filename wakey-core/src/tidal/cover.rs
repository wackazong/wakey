//! Album covers: TIDAL's JPEG ([`super::api::cover_url`], [`SOURCE_PX`]),
//! decoded once and shrunk to the square the list shows ([`COVER_PX`]),
//! stored raw as RGB888 rows (`tidal/covers/<cover id>.rgb`, see
//! [`super::sync`]). The device only reads the file into an image.

use super::sync::COVER_PX;
use super::{Result, ensure, err};
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

/// The size requested from TIDAL (it offers 80, 160, 320, …): the next
/// one above [`COVER_PX`], shrunk with an area average.
pub const SOURCE_PX: u32 = 160;

/// Bytes of a stored cover.
pub const COVER_BYTES: usize = (COVER_PX * COVER_PX * 3) as usize;

/// Decodes `jpeg`, crops it to a centred square and shrinks it to
/// [`COVER_PX`] × [`COVER_PX`]; RGB888, row by row.
pub fn decode(jpeg: &[u8]) -> Result<Vec<u8>> {
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(jpeg), options);
    let pixels = decoder.decode().map_err(|e| err(format!("cover: {e:?}")))?;
    let (w, h) = decoder.dimensions().ok_or_else(|| err("cover: no size"))?;
    ensure!(
        w > 0 && h > 0 && pixels.len() >= w * h * 3,
        "cover: empty image"
    );
    Ok(shrink(&pixels, w, h, COVER_PX as usize))
}

/// The centred square of an RGB888 image (`w` × `h`), area-averaged down
/// (or up) to `px` × `px`.
fn shrink(rgb: &[u8], w: usize, h: usize, px: usize) -> Vec<u8> {
    let side = w.min(h);
    let (x_off, y_off) = ((w - side) / 2, (h - side) / 2);
    let mut out = Vec::with_capacity(px * px * 3);
    for oy in 0..px {
        let y0 = y_off + oy * side / px;
        let y1 = (y_off + (oy + 1) * side / px).max(y0 + 1);
        for ox in 0..px {
            let x0 = x_off + ox * side / px;
            let x1 = (x_off + (ox + 1) * side / px).max(x0 + 1);
            let mut sum = [0u32; 3];
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y * w + x) * 3;
                    for (s, &v) in sum.iter_mut().zip(&rgb[i..i + 3]) {
                        *s += u32::from(v);
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            out.extend(sum.iter().map(|s| ((s + n / 2) / n) as u8));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const JPEG: &[u8] = include_bytes!("../../testdata/cover-160x200.jpg");

    fn pixel(rgb: &[u8], x: usize, y: usize) -> [u8; 3] {
        let i = (y * COVER_PX as usize + x) * 3;
        [rgb[i], rgb[i + 1], rgb[i + 2]]
    }

    fn near(a: [u8; 3], b: [u8; 3]) -> bool {
        a.iter().zip(b).all(|(&x, y)| x.abs_diff(y) <= 12)
    }

    #[test]
    fn decodes_crops_and_shrinks() {
        // 160 × 200, left half red, right half blue: the centred square
        // keeps both halves.
        let rgb = decode(JPEG).unwrap();
        assert_eq!(rgb.len(), COVER_BYTES);
        let last = COVER_PX as usize - 1;
        assert!(
            near(pixel(&rgb, 2, 2), [0xd0, 0x20, 0x20]),
            "{:?}",
            pixel(&rgb, 2, 2)
        );
        assert!(near(pixel(&rgb, last - 2, last - 2), [0x20, 0x40, 0xd0]));
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode(b"not a jpeg").is_err());
    }

    #[test]
    fn shrink_averages() {
        // 2 × 2 → 1 × 1: the mean.
        let rgb = [0, 0, 0, 100, 100, 100, 200, 200, 200, 100, 100, 100];
        assert_eq!(shrink(&rgb, 2, 2, 1), vec![100, 100, 100]);
        // 4 × 2 → the centred 2 × 2 square.
        let wide = [
            9, 9, 9, 1, 1, 1, 3, 3, 3, 9, 9, 9, //
            9, 9, 9, 1, 1, 1, 3, 3, 3, 9, 9, 9,
        ];
        assert_eq!(
            shrink(&wide, 4, 2, 2),
            vec![1, 1, 1, 3, 3, 3, 1, 1, 1, 3, 3, 3]
        );
    }
}
