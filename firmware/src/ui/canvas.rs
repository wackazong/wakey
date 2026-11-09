//! A landscape `embedded-graphics` draw target over a portrait frame buffer.
//!
//! Used by the Buoyant test and the raw display test. Solid fills (the bulk
//! of what UI toolkits draw) write whole panel rows with `slice::fill`.

use super::{UI_HEIGHT, UI_WIDTH};
use crate::display::{HEIGHT, WIDTH};
use core::convert::Infallible;
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::Rectangle;

pub struct Canvas<'a> {
    buf: &'a mut [u16],
}

impl<'a> Canvas<'a> {
    pub fn new(buf: &'a mut [u16]) -> Self {
        assert_eq!(buf.len(), WIDTH * HEIGHT);
        Self { buf }
    }

    /// Landscape `(x, y)` → index into the portrait buffer.
    #[inline(always)]
    fn index(x: u32, y: u32) -> usize {
        // panel row = HEIGHT - 1 - x, panel column = y
        (HEIGHT - 1 - x as usize) * WIDTH + y as usize
    }

    fn bounds() -> Rectangle {
        Rectangle::new(Point::zero(), Size::new(UI_WIDTH, UI_HEIGHT))
    }
}

impl OriginDimensions for Canvas<'_> {
    fn size(&self) -> Size {
        Size::new(UI_WIDTH, UI_HEIGHT)
    }
}

impl DrawTarget for Canvas<'_> {
    type Color = Rgb565;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(p, c) in pixels {
            if p.x >= 0 && p.y >= 0 && (p.x as u32) < UI_WIDTH && (p.y as u32) < UI_HEIGHT {
                self.buf[Self::index(p.x as u32, p.y as u32)] = c.into_storage();
            }
        }
        Ok(())
    }

    fn fill_contiguous<I>(&mut self, area: &Rectangle, colors: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Self::Color>,
    {
        let bounds = Self::bounds();
        for (p, c) in area.points().zip(colors) {
            if bounds.contains(p) {
                self.buf[Self::index(p.x as u32, p.y as u32)] = c.into_storage();
            }
        }
        Ok(())
    }

    fn fill_solid(&mut self, area: &Rectangle, color: Self::Color) -> Result<(), Self::Error> {
        let a = area.intersection(&Self::bounds());
        let Some(br) = a.bottom_right() else {
            return Ok(());
        };
        let raw = color.into_storage();
        let (x0, y0, x1, y1) = (
            a.top_left.x as usize,
            a.top_left.y as usize,
            br.x as usize,
            br.y as usize,
        );
        // Landscape column x is panel row HEIGHT-1-x; landscape rows y0..=y1
        // are the contiguous panel columns y0..=y1.
        let (c0, c1) = (y0, y1);
        for row in (HEIGHT - 1 - x1)..=(HEIGHT - 1 - x0) {
            self.buf[row * WIDTH + c0..=row * WIDTH + c1].fill(raw);
        }
        Ok(())
    }

    fn clear(&mut self, color: Self::Color) -> Result<(), Self::Error> {
        self.buf.fill(color.into_storage());
        Ok(())
    }
}
