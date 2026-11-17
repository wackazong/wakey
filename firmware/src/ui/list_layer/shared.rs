//! Two list layers on one set of buffers.

use super::{ListLayer, ListSource};
use crate::display::ppa::{PpaRotator, Target};
use crate::ui::input::TouchEvent;
use crate::ui::render_loop::{FrameHook, Route};
use core::ops::Range;

/// Two list layers whose pages are never on screen together, sharing one
/// set of buffers ([`ListLayer::new_in`]): the one shown owns them (with
/// neither shown, `a`, so its rows are ready when its page opens). A new
/// owner renders its rows again.
pub struct SharedLayers<A: ListSource, B: ListSource> {
    a: ListLayer<A>,
    b: ListLayer<B>,
    /// `b` owns the buffers.
    b_owns: bool,
}

impl<A: ListSource, B: ListSource> SharedLayers<A, B> {
    pub fn new(a: ListLayer<A>, b: ListLayer<B>) -> Self {
        Self {
            a,
            b,
            b_owns: false,
        }
    }

    /// Hands the buffers to the layer shown (else `a`).
    fn claim(&mut self) {
        let b = self.b.is_shown();
        if b != self.b_owns {
            self.b_owns = b;
            if b {
                self.b.invalidate();
            } else {
                self.a.invalidate();
            }
        }
    }

    fn owner(&mut self) -> &mut dyn FrameHook {
        if self.b_owns {
            &mut self.b
        } else {
            &mut self.a
        }
    }
}

impl<A: ListSource, B: ListSource> FrameHook for SharedLayers<A, B> {
    fn active(&mut self) -> bool {
        // Both: one whose page went away mid-scroll hands back.
        let (a, b) = (self.a.active(), self.b.active());
        self.claim();
        if self.b_owns { b } else { a }
    }

    fn pointer(&mut self, ev: &TouchEvent) -> Route {
        self.claim();
        self.owner().pointer(ev)
    }

    fn start(&mut self, fb: Target, rot: &mut PpaRotator) {
        self.owner().start(fb, rot);
    }

    fn repeat(&mut self, fb: Target, rot: &mut PpaRotator) {
        self.owner().repeat(fb, rot);
    }

    fn finish(&mut self, rot: &mut PpaRotator) {
        self.owner().finish(rot);
    }

    fn overlaps(&mut self, x: Range<usize>, y: Range<usize>) -> bool {
        self.owner().overlaps(x, y)
    }

    fn idle_work(&mut self, rot: &mut PpaRotator) -> bool {
        self.claim();
        self.owner().idle_work(rot)
    }
}
