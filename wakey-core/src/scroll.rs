//! Touch scrolling physics for one axis: drag, fling with exponential
//! deceleration, rubber-band resistance past either end and a critically
//! damped spring back. Positions are in pixels of content, times in
//! microseconds of any monotonic clock.

/// Fling: velocity decays by e every `FLING_TAU` seconds and stops below
/// `FLING_STOP` px/s.
const FLING_TAU: f32 = 0.35;
const FLING_STOP: f32 = 20.0;
/// A touch that moves less than this (px) is a tap.
const TAP_SLOP: f32 = 12.0;
/// A finger that rested this long before lifting does not fling.
const REST_US: u64 = 80_000;
/// Rubber-band constant (as on iOS).
const RUBBER: f32 = 0.55;
/// Farthest the content can be pulled past an end, px.
const MAX_OVERSCROLL: f32 = 360.0;
/// Spring back: critically damped with this angular frequency (1/s);
/// settles in about 4/SPRING_W seconds.
const SPRING_W: f32 = 12.0;
/// Integration step, s (the spring is stiff).
const STEP: f32 = 0.004;

#[derive(Debug, Clone)]
struct Drag {
    /// Finger position and the content position it grabbed, the latter in
    /// "raw" units (before the rubber band).
    start_y: f32,
    start_raw: f32,
    last_y: f32,
    last_at: u64,
    moved: bool,
}

/// One scrollable axis. Feed it the touch ([`Self::press`],
/// [`Self::drag_to`], [`Self::release`]) and call [`Self::tick`] every
/// frame while [`Self::is_moving`]; [`Self::offset`] is where to draw.
#[derive(Debug, Clone)]
pub struct Scroller {
    view: f32,
    content: f32,
    /// Scroll position (negative or past the end while overscrolled).
    offset: f32,
    /// px/s, positive = towards the end.
    velocity: f32,
    /// Direction of the last movement: +1 towards the end, -1 back.
    direction: i32,
    drag: Option<Drag>,
    last_tick: u64,
}

impl Scroller {
    /// A view of `view` px over `content` px of content, at the start.
    pub fn new(view: f32, content: f32) -> Self {
        Self {
            view,
            content,
            offset: 0.0,
            velocity: 0.0,
            direction: 1,
            drag: None,
            last_tick: 0,
        }
    }

    /// Puts the content at `offset` (clamped to `0..=max_offset`) and stops
    /// any drag or motion: the list was scrolled elsewhere.
    pub fn set_offset(&mut self, offset: f32) {
        self.offset = offset.clamp(0.0, self.max_offset());
        self.velocity = 0.0;
        self.drag = None;
    }

    /// The content got longer or shorter. A position past the new end
    /// springs back on the next ticks.
    pub fn set_content(&mut self, content: f32) {
        self.content = content;
    }

    /// Content position at the top of the view, px (outside
    /// `0..=max_offset` while overscrolled).
    pub fn offset(&self) -> f32 {
        self.offset
    }

    /// +1 if the content last moved towards its end, -1 if back.
    pub fn direction(&self) -> i32 {
        self.direction
    }

    /// Dragged, flinging or springing back: the position changes over time.
    pub fn is_moving(&self) -> bool {
        self.drag.is_some() || self.velocity != 0.0 || self.overshoot() != 0.0
    }

    pub fn max_offset(&self) -> f32 {
        (self.content - self.view).max(0.0)
    }

    /// Distance past the nearest end (negative above the start), 0 in range.
    fn overshoot(&self) -> f32 {
        let max = self.max_offset();
        if self.offset < 0.0 {
            self.offset
        } else if self.offset > max {
            self.offset - max
        } else {
            0.0
        }
    }

    /// Finger down at view position `y`. Catches a running fling or spring.
    pub fn press(&mut self, y: f32, at: u64) {
        self.velocity = 0.0;
        self.drag = Some(Drag {
            start_y: y,
            start_raw: self.unrubber(self.offset),
            last_y: y,
            last_at: at,
            moved: false,
        });
    }

    /// Finger moved to `y`. Returns false if no drag is in progress.
    pub fn drag_to(&mut self, y: f32, at: u64) -> bool {
        let Some(d) = self.drag.as_mut() else {
            return false;
        };
        if (y - d.start_y).abs() > TAP_SLOP {
            d.moved = true;
        }
        let dt = at.saturating_sub(d.last_at) as f32 / 1e6;
        if dt > 0.0 {
            // Finger velocity, smoothed; the content moves the other way.
            let v = -(y - d.last_y) / dt;
            self.velocity = 0.6 * v + 0.4 * self.velocity;
        }
        d.last_y = y;
        d.last_at = at;
        let raw = d.start_raw - (y - d.start_y);
        self.offset = self.rubber(raw);
        if self.velocity != 0.0 {
            self.direction = self.velocity.signum() as i32;
        }
        true
    }

    /// Finger lifted. Returns `Some(content y)` if the touch was a tap
    /// (no fling then), `None` otherwise (also without a drag).
    pub fn release(&mut self, y: f32, at: u64) -> Option<f32> {
        let d = self.drag.take()?;
        self.last_tick = at;
        if at.saturating_sub(d.last_at) > REST_US {
            self.velocity = 0.0;
        }
        // Released while stretched: the spring returns it; velocity into
        // the stretch would only overshoot further.
        if self.overshoot() * self.velocity > 0.0 {
            self.velocity = 0.0;
        }
        if d.moved {
            return None;
        }
        self.velocity = 0.0;
        Some((self.offset + y).max(0.0))
    }

    /// Advances fling and spring to time `now`.
    pub fn tick(&mut self, now: u64) {
        let mut left = now.saturating_sub(self.last_tick) as f32 / 1e6;
        self.last_tick = now;
        if self.drag.is_some() {
            return;
        }
        let max = self.max_offset();
        while left > 0.0 && (self.velocity != 0.0 || self.overshoot() != 0.0) {
            let dt = left.min(STEP);
            left -= dt;
            let x = self.overshoot();
            if x != 0.0 {
                let a = -SPRING_W * SPRING_W * x - 2.0 * SPRING_W * self.velocity;
                self.velocity += a * dt;
                self.offset += self.velocity * dt;
                let x_new = self.overshoot();
                let settled = x_new.abs() < 0.5 && self.velocity.abs() < 10.0;
                if settled || x_new * x < 0.0 || x_new == 0.0 {
                    // Back at (or across) the end: stop exactly there.
                    self.offset = if x < 0.0 { 0.0 } else { max };
                    self.velocity = 0.0;
                }
            } else {
                // Free fling; the spring takes over past an end.
                self.offset += self.velocity * dt;
                self.velocity *= (-dt / FLING_TAU).exp();
                if self.velocity.abs() < FLING_STOP {
                    self.velocity = 0.0;
                }
            }
            self.offset = self.offset.clamp(-MAX_OVERSCROLL, max + MAX_OVERSCROLL);
        }
    }

    /// Visible stretch for a pull of `d` px past an end.
    fn band(&self, d: f32) -> f32 {
        ((1.0 - 1.0 / (d * RUBBER / self.view + 1.0)) * self.view).min(MAX_OVERSCROLL)
    }

    /// Inverse of [`Self::band`].
    fn unband(&self, x: f32) -> f32 {
        let x = x.min(self.view * 0.99);
        self.view / RUBBER * (1.0 / (1.0 - x / self.view) - 1.0)
    }

    /// Visible offset for a raw (finger) position.
    fn rubber(&self, raw: f32) -> f32 {
        let max = self.max_offset();
        if raw < 0.0 {
            -self.band(-raw)
        } else if raw > max {
            max + self.band(raw - max)
        } else {
            raw
        }
    }

    fn unrubber(&self, offset: f32) -> f32 {
        let max = self.max_offset();
        if offset < 0.0 {
            -self.unband(-offset)
        } else if offset > max {
            max + self.unband(offset - max)
        } else {
            offset
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1000;

    fn settle(s: &mut Scroller, mut t: u64) -> u64 {
        for _ in 0..1000 {
            if !s.is_moving() {
                break;
            }
            t += 16 * MS;
            s.tick(t);
        }
        t
    }

    #[test]
    fn tap() {
        let mut s = Scroller::new(720.0, 9600.0);
        s.press(300.0, 0);
        // A small wiggle at the start pulls the content slightly past it.
        s.drag_to(305.0, 50 * MS);
        let pulled = s.offset();
        assert!((-5.0..0.0).contains(&pulled));
        assert_eq!(s.release(305.0, 100 * MS), Some(305.0 + pulled));
        assert_eq!(s.velocity, 0.0);
    }

    #[test]
    fn fling_moves_on_and_stops() {
        let mut s = Scroller::new(720.0, 9600.0);
        // Swipe up 300 px in 100 ms: 3000 px/s towards the end.
        s.press(600.0, 0);
        for k in 1..=10 {
            s.drag_to(600.0 - 30.0 * k as f32, k * 10 * MS);
        }
        assert_eq!(s.release(300.0, 100 * MS), None);
        assert!(s.velocity > 2000.0);
        let released_at = s.offset();
        settle(&mut s, 100 * MS);
        assert_eq!(s.velocity, 0.0);
        // v·τ ≈ 3000 · 0.35 ≈ 1050 px further.
        let glide = s.offset() - released_at;
        assert!((800.0..1300.0).contains(&glide), "{glide}");
        assert_eq!(s.direction(), 1);
    }

    #[test]
    fn rested_finger_does_not_fling() {
        let mut s = Scroller::new(720.0, 9600.0);
        s.press(600.0, 0);
        s.drag_to(300.0, 50 * MS);
        assert_eq!(s.release(300.0, 300 * MS), None);
        assert_eq!(s.velocity, 0.0);
    }

    #[test]
    fn overscroll_is_damped_and_springs_back() {
        let mut s = Scroller::new(720.0, 9600.0);
        // Pull 600 px down at the start.
        s.press(100.0, 0);
        s.drag_to(700.0, 100 * MS);
        let stretch = -s.offset();
        assert!(stretch > 0.0 && stretch < 300.0, "{stretch}");
        s.release(700.0, 300 * MS);
        settle(&mut s, 300 * MS);
        assert_eq!(s.offset(), 0.0);
    }

    #[test]
    fn fling_into_the_end_springs_back_to_it() {
        let mut s = Scroller::new(720.0, 1000.0);
        s.press(600.0, 0);
        for k in 1..=10 {
            s.drag_to(600.0 - 40.0 * k as f32, k * 10 * MS);
        }
        s.release(200.0, 100 * MS);
        settle(&mut s, 100 * MS);
        assert_eq!(s.offset(), s.max_offset());
        assert!(!s.is_moving());
    }

    #[test]
    fn set_offset_clamps_and_stops() {
        let mut s = Scroller::new(720.0, 1000.0);
        s.press(600.0, 0);
        s.drag_to(300.0, 10 * MS);
        s.set_offset(500.0);
        assert_eq!(s.offset(), 280.0);
        assert!(!s.is_moving());
        s.set_offset(-20.0);
        assert_eq!(s.offset(), 0.0);
    }

    #[test]
    fn shorter_content_springs_back_to_the_new_end() {
        let mut s = Scroller::new(720.0, 2000.0);
        s.set_offset(1280.0);
        s.set_content(1500.0);
        assert!(s.is_moving());
        settle(&mut s, 0);
        assert_eq!(s.offset(), 780.0);
    }

    #[test]
    fn grabbing_while_overscrolled_keeps_the_position() {
        let mut s = Scroller::new(720.0, 9600.0);
        s.press(100.0, 0);
        s.drag_to(500.0, 100 * MS);
        let o = s.offset();
        s.release(500.0, 300 * MS);
        s.press(500.0, 300 * MS);
        s.drag_to(500.0, 310 * MS);
        assert!((s.offset() - o).abs() < 0.5, "{} vs {o}", s.offset());
    }
}
