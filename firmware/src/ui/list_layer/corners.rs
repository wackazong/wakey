//! Rounded corners of a card around the list ([`Layout::corners`]): Slint
//! clips rectangles only, so the page draws caps in the colour outside the
//! card over the corners, and the layer blends the same caps into what it
//! copied.

use super::Layout;
use crate::display::{HEIGHT, WIDTH, cache};
use crate::ui::dim;

/// How much of each pixel of a `radius` × `radius` top-left corner lies
/// outside the rounding, 0..=16 (4 × 4 samples), row by row (landscape y,
/// then x). The page's images and the layer's blend use the same values.
pub fn corner_alpha(radius: usize) -> Vec<u8> {
    let r = radius as f32;
    let mut out = Vec::with_capacity(radius * radius);
    for y in 0..radius {
        for x in 0..radius {
            let mut n = 0u8;
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = x as f32 + (sx as f32 + 0.5) / 4.0;
                    let py = y as f32 + (sy as f32 + 0.5) / 4.0;
                    if (r - px).powi(2) + (r - py).powi(2) > r * r {
                        n += 1;
                    }
                }
            }
            out.push(n);
        }
    }
    out
}

/// `bg` over `px` (RGB565) with weight `a` of 16.
fn blend565(px: u16, bg: u16, a: u8) -> u16 {
    let a = u32::from(a);
    let mix = |shift: u32, mask: u32| {
        let p = (u32::from(px) >> shift) & mask;
        let b = (u32::from(bg) >> shift) & mask;
        ((b * a + p * (16 - a) + 8) / 16) << shift
    };
    (mix(11, 0x1f) | mix(5, 0x3f) | mix(0, 0x1f)) as u16
}

/// The caps over the four corners of a list's area, as runs in the frame
/// buffer.
pub(super) struct CornerCaps {
    /// The colour outside the corners (RGB565, undimmed).
    outside: u16,
    /// Start pixel in the frame buffer and the weights of the run's pixels.
    runs: Vec<(usize, Vec<u8>)>,
}

impl CornerCaps {
    /// The caps of `layout`'s area, if it has rounded corners. Each
    /// landscape column of a corner is one run: landscape x is panel row
    /// `HEIGHT - 1 - x`, landscape y the panel column.
    pub(super) fn new(layout: &Layout) -> Option<Self> {
        let (radius, outside) = layout.corners?;
        let alpha = corner_alpha(radius);
        let (x0, y0, w, h) = (layout.x, layout.y, layout.width, layout.height);
        let mut runs = Vec::with_capacity(4 * radius);
        for (left, top) in [(true, true), (false, true), (true, false), (false, false)] {
            // Column `i` of the corner, counted from the card's side edge.
            for i in 0..radius {
                let lx = if left { x0 + i } else { x0 + w - 1 - i };
                let ly = if top { y0 } else { y0 + h - radius };
                // Weights along y in the corner, top to bottom on screen.
                let weights = (0..radius)
                    .map(|k| {
                        let j = if top { k } else { radius - 1 - k };
                        alpha[j * radius + i]
                    })
                    .collect();
                runs.push(((HEIGHT - 1 - lx) * WIDTH + ly, weights));
            }
        }
        Some(Self { outside, runs })
    }

    /// Blends the caps into the frame buffer `fb`.
    ///
    /// # Safety
    ///
    /// `fb` must be a whole frame buffer that holds the frame's copies and
    /// that no DMA engine writes meanwhile.
    pub(super) unsafe fn apply(&self, fb: *mut u16) {
        let bg = dim::scale(self.outside);
        for (start, weights) in &self.runs {
            let n = weights.len();
            // SAFETY: the runs lie inside the frame buffer (`new`; the
            // layout is inside the screen), which no DMA writes now (the
            // caller's contract). The CPU writes it only here and writes
            // each run back at once, so the invalidated lines hold no other
            // CPU writes.
            let run = unsafe {
                let px = fb.add(*start);
                cache::invalidate(px, n);
                core::slice::from_raw_parts_mut(px, n)
            };
            for (p, &a) in run.iter_mut().zip(weights) {
                if a > 0 {
                    *p = blend565(*p, bg, a);
                }
            }
            // SAFETY: the same run.
            unsafe { cache::writeback(run.as_mut_ptr(), n) };
        }
    }
}
