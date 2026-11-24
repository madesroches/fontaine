//! Touch-drag scrolling with fling momentum. It holds no ImGui types: the caller feeds it the
//! pointer's y position and the time, and applies the whole-pixel deltas it returns.

/// Friction of a fling: its speed decays as `exp(-FRICTION * t)`.
pub const FRICTION: f32 = 4.0;
/// Slowest release speed that still starts a fling, in dp/s.
pub const MIN_FLING_DP: f32 = 50.0;
/// A fling slower than this stops, in dp/s.
pub const STOP_SPEED_DP: f32 = 20.0;
/// Fastest fling, in dp/s.
pub const MAX_SPEED_DP: f32 = 8000.0;
/// The release velocity comes from the movement within this long before lift-off.
const VELOCITY_WINDOW: f32 = 0.1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Idle,
    Dragging,
    Flinging,
}

#[derive(Debug, Default)]
pub struct DragScroll {
    phase: Phase,
    /// A touch that began over the transcript is still held, whether or not it passed the slop.
    pressed: bool,
    start_y: f32,
    last_y: f32,
    /// Pointer samples `(time, y)` within the velocity window.
    samples: Vec<(f32, f32)>,
    /// Fling speed in scroll pixels per second; positive scrolls down.
    velocity: f32,
    /// The part of a delta below one pixel, carried to the next call because ImGui rounds the
    /// scroll offset to whole pixels.
    carry: f32,
}

impl DragScroll {
    /// A press began over the transcript. Returns true when it interrupted a running fling, so
    /// that touch must not activate a row.
    pub fn press(&mut self, y: f32, now: f32) -> bool {
        let interrupted = self.phase == Phase::Flinging;
        self.phase = Phase::Idle;
        self.pressed = true;
        self.start_y = y;
        self.last_y = y;
        self.velocity = 0.0;
        self.carry = 0.0;
        self.samples.clear();
        self.samples.push((now, y));
        interrupted
    }

    /// A press the transcript did not take: drops a touch left held by a release that was never
    /// delivered, so it cannot drive a later drag.
    pub fn abandon_press(&mut self) {
        self.pressed = false;
        self.samples.clear();
        if self.phase == Phase::Dragging {
            self.stop();
        }
    }

    /// The pointer moved while held. Returns the pixels to scroll by: nothing until the movement
    /// passes `slop`, then everything accumulated so far so the content does not jump.
    pub fn drag(&mut self, y: f32, now: f32, slop: f32) -> f32 {
        if !self.pressed {
            return 0.0;
        }
        self.samples.push((now, y));
        self.samples.retain(|&(t, _)| now - t <= VELOCITY_WINDOW);
        if self.phase != Phase::Dragging {
            if (y - self.start_y).abs() <= slop {
                return 0.0;
            }
            self.phase = Phase::Dragging;
        }
        // The content follows the finger, so the scroll offset moves the opposite way.
        let moved = -(y - self.last_y);
        self.last_y = y;
        self.whole_pixels(moved)
    }

    /// The finger lifted. Starts a fling when it was moving fast enough just before.
    pub fn release(&mut self, now: f32, scale: f32) {
        if !self.pressed {
            return;
        }
        self.pressed = false;
        if self.phase != Phase::Dragging {
            self.phase = Phase::Idle;
            return;
        }
        self.samples.retain(|&(t, _)| now - t <= VELOCITY_WINDOW);
        let speed = match (self.samples.first(), self.samples.last()) {
            (Some(&(t0, y0)), Some(&(t1, y1))) if t1 > t0 => -(y1 - y0) / (t1 - t0),
            _ => 0.0,
        };
        self.samples.clear();
        let max = MAX_SPEED_DP * scale;
        if speed.abs() >= MIN_FLING_DP * scale {
            self.velocity = speed.clamp(-max, max);
            self.phase = Phase::Flinging;
        } else {
            self.stop();
        }
    }

    /// Advances a fling by `dt` seconds and returns the whole pixels to scroll by.
    pub fn step(&mut self, dt: f32, scale: f32) -> f32 {
        if self.phase != Phase::Flinging {
            return 0.0;
        }
        let decay = (-FRICTION * dt).exp();
        let moved = self.velocity / FRICTION * (1.0 - decay);
        self.velocity *= decay;
        let whole = self.whole_pixels(moved);
        if self.velocity.abs() < STOP_SPEED_DP * scale {
            self.stop();
        }
        whole
    }

    /// The view is at an edge in the direction the fling moves: stops it.
    pub fn hit_edge(&mut self) {
        self.cancel();
    }

    /// Stops a fling.
    pub fn cancel(&mut self) {
        if self.phase == Phase::Flinging {
            self.stop();
        }
    }

    pub fn is_dragging(&self) -> bool {
        self.phase == Phase::Dragging
    }

    /// True while dragging or flinging.
    pub fn is_active(&self) -> bool {
        self.phase != Phase::Idle
    }

    pub fn is_flinging(&self) -> bool {
        self.phase == Phase::Flinging
    }

    /// The fling's speed in scroll pixels per second; positive scrolls down, zero when not
    /// flinging.
    pub fn velocity(&self) -> f32 {
        if self.phase == Phase::Flinging {
            self.velocity
        } else {
            0.0
        }
    }

    fn stop(&mut self) {
        self.phase = Phase::Idle;
        self.velocity = 0.0;
        self.carry = 0.0;
    }

    fn whole_pixels(&mut self, delta: f32) -> f32 {
        let total = self.carry + delta;
        let whole = total.trunc();
        self.carry = total - whole;
        whole
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SLOP: f32 = 10.0;

    fn flick(scroll: &mut DragScroll) {
        scroll.press(500.0, 0.0);
        scroll.drag(450.0, 0.016, SLOP);
        scroll.drag(400.0, 0.032, SLOP);
        scroll.drag(350.0, 0.048, SLOP);
        scroll.release(0.05, 1.0);
    }

    #[test]
    fn drag_below_slop_scrolls_nothing() {
        let mut scroll = DragScroll::default();
        scroll.press(100.0, 0.0);
        assert_eq!(scroll.drag(95.0, 0.01, SLOP), 0.0);
        assert_eq!(scroll.drag(91.0, 0.02, SLOP), 0.0);
        assert!(!scroll.is_dragging());
        assert!(!scroll.is_active());
    }

    #[test]
    fn crossing_slop_returns_everything_once_then_deltas() {
        let mut scroll = DragScroll::default();
        scroll.press(100.0, 0.0);
        assert_eq!(scroll.drag(95.0, 0.01, SLOP), 0.0);
        // Finger up 20 px scrolls the content down by 20.
        assert_eq!(scroll.drag(80.0, 0.02, SLOP), 20.0);
        assert!(scroll.is_dragging());
        assert_eq!(scroll.drag(70.0, 0.03, SLOP), 10.0);
        assert_eq!(scroll.drag(75.0, 0.04, SLOP), -5.0);
    }

    #[test]
    fn drag_without_press_is_ignored() {
        let mut scroll = DragScroll::default();
        assert_eq!(scroll.drag(0.0, 0.0, SLOP), 0.0);
        assert_eq!(scroll.drag(500.0, 0.1, SLOP), 0.0);
        assert!(!scroll.is_active());
    }

    #[test]
    fn fast_flick_flings_with_decaying_travel_until_idle() {
        let mut scroll = DragScroll::default();
        flick(&mut scroll);
        assert!(scroll.is_flinging());
        assert!(scroll.velocity() > 0.0);
        let mut steps = 0;
        let mut previous = f32::MAX;
        let mut travelled = 0.0;
        while scroll.is_active() {
            let delta = scroll.step(0.016, 1.0);
            assert!(delta >= 0.0);
            // Whole-pixel rounding lets a step differ by one pixel from the exact decay.
            assert!(delta <= previous + 1.0);
            previous = delta;
            travelled += delta;
            steps += 1;
            assert!(steps < 1000, "the fling never stopped");
        }
        assert!(travelled > 0.0);
        assert_eq!(scroll.step(0.016, 1.0), 0.0);
        assert_eq!(scroll.phase, Phase::Idle);
    }

    #[test]
    fn upward_scroll_flings_negative() {
        let mut scroll = DragScroll::default();
        scroll.press(100.0, 0.0);
        scroll.drag(150.0, 0.016, SLOP);
        scroll.drag(200.0, 0.032, SLOP);
        scroll.release(0.033, 1.0);
        assert!(scroll.velocity() < 0.0);
        assert!(scroll.step(0.016, 1.0) < 0.0);
    }

    #[test]
    fn pause_before_lift_off_starts_no_fling() {
        let mut scroll = DragScroll::default();
        scroll.press(500.0, 0.0);
        scroll.drag(450.0, 0.016, SLOP);
        scroll.drag(400.0, 0.032, SLOP);
        scroll.release(0.2, 1.0);
        assert!(!scroll.is_active());
        assert_eq!(scroll.step(0.016, 1.0), 0.0);
    }

    #[test]
    fn slow_release_starts_no_fling() {
        let mut scroll = DragScroll::default();
        scroll.press(500.0, 0.0);
        scroll.drag(498.0, 0.05, 1.0);
        scroll.drag(497.0, 0.1, 1.0);
        scroll.release(0.1, 1.0);
        assert!(!scroll.is_active());
    }

    #[test]
    fn velocity_is_clamped() {
        let mut scroll = DragScroll::default();
        scroll.press(100_000.0, 0.0);
        scroll.drag(0.0, 0.001, SLOP);
        scroll.release(0.002, 1.0);
        assert_eq!(scroll.velocity(), MAX_SPEED_DP);
        let mut scaled = DragScroll::default();
        scaled.press(100_000.0, 0.0);
        scaled.drag(0.0, 0.001, SLOP);
        scaled.release(0.002, 3.0);
        assert_eq!(scaled.velocity(), MAX_SPEED_DP * 3.0);
    }

    #[test]
    fn press_stops_a_fling_and_reports_it() {
        let mut scroll = DragScroll::default();
        flick(&mut scroll);
        assert!(scroll.press(300.0, 1.0));
        assert!(!scroll.is_active());
        assert_eq!(scroll.step(0.016, 1.0), 0.0);
    }

    #[test]
    fn press_when_idle_reports_no_interruption() {
        let mut scroll = DragScroll::default();
        assert!(!scroll.press(300.0, 0.0));
        scroll.release(0.01, 1.0);
        assert!(!scroll.press(300.0, 1.0));
    }

    #[test]
    fn sub_pixel_drags_carry_and_sum_to_whole_pixels() {
        let mut scroll = DragScroll::default();
        scroll.press(1000.0, 0.0);
        // Get past the slop, then move 0.3 px per call.
        let first = scroll.drag(985.0, 0.001, SLOP);
        assert_eq!(first, 15.0);
        let mut total = 0.0;
        let mut y = 985.0;
        for i in 0..100 {
            y -= 0.3;
            let delta = scroll.drag(y, 0.002 + i as f32 * 0.001, SLOP);
            assert_eq!(delta, delta.trunc());
            total += delta;
        }
        // 30 px of movement, less the remainder still carried.
        assert!((29.0..=30.0).contains(&total), "total {total}");
    }

    #[test]
    fn sub_pixel_steps_carry_and_return_whole_numbers() {
        let mut scroll = DragScroll::default();
        // 60 dp/s is just above the fling minimum: about 0.9 px per 16 ms step at first.
        scroll.press(100.0, 0.0);
        scroll.drag(94.0, 0.05, 1.0);
        scroll.drag(88.0, 0.1, 1.0);
        scroll.release(0.1, 1.0);
        assert!(scroll.is_flinging());
        let mut total = 0.0;
        let mut zero_steps = 0;
        while scroll.is_active() {
            let delta = scroll.step(0.016, 1.0);
            assert_eq!(delta, delta.trunc());
            if delta == 0.0 {
                zero_steps += 1;
            }
            total += delta;
        }
        assert!(zero_steps > 0, "expected some steps under one pixel");
        assert!(total > 0.0);
    }

    #[test]
    fn abandoned_press_no_longer_drags_or_releases() {
        let mut scroll = DragScroll::default();
        scroll.press(500.0, 0.0);
        scroll.drag(450.0, 0.016, SLOP);
        scroll.abandon_press();
        assert!(!scroll.is_active());
        assert_eq!(scroll.drag(300.0, 0.032, SLOP), 0.0);
        scroll.release(0.033, 1.0);
        assert!(!scroll.is_active());
    }

    #[test]
    fn hit_edge_stops_a_fling() {
        let mut scroll = DragScroll::default();
        flick(&mut scroll);
        scroll.hit_edge();
        assert!(!scroll.is_active());
        assert_eq!(scroll.step(0.016, 1.0), 0.0);
    }

    #[test]
    fn a_tiny_step_does_not_stop_a_fling() {
        let mut scroll = DragScroll::default();
        flick(&mut scroll);
        assert_eq!(scroll.step(0.000_01, 1.0), 0.0);
        assert!(scroll.is_flinging());
    }

    #[test]
    fn constants_are_pinned() {
        assert_eq!(FRICTION, 4.0);
        assert_eq!(MIN_FLING_DP, 50.0);
        assert_eq!(STOP_SPEED_DP, 20.0);
        assert_eq!(MAX_SPEED_DP, 8000.0);
    }

    #[test]
    fn scale_scales_the_thresholds() {
        // 80 px/s: a fling at scale 1 (min 50), none at scale 2 (min 100).
        let release_at = |scale: f32| {
            let mut scroll = DragScroll::default();
            scroll.press(100.0, 0.0);
            scroll.drag(96.0, 0.05, 1.0);
            scroll.drag(92.0, 0.1, 1.0);
            scroll.release(0.1, scale);
            scroll.is_flinging()
        };
        assert!(release_at(1.0));
        assert!(!release_at(2.0));

        // A fling below stop speed at scale 2 but above it at scale 1.
        let mut slow = DragScroll::default();
        slow.press(100.0, 0.0);
        slow.drag(95.0, 0.05, 1.0);
        slow.drag(90.0, 0.1, 1.0);
        slow.release(0.1, 1.0);
        assert!(slow.is_flinging());
        slow.step(0.1, 4.0);
        assert!(
            !slow.is_active(),
            "stop speed 20 dp/s is 80 px/s at scale 4"
        );
    }

    #[test]
    fn is_active_follows_the_phase() {
        let mut scroll = DragScroll::default();
        assert!(!scroll.is_active());
        scroll.press(500.0, 0.0);
        assert!(!scroll.is_active());
        scroll.drag(450.0, 0.016, SLOP);
        assert!(scroll.is_active());
        scroll.drag(400.0, 0.032, SLOP);
        scroll.drag(350.0, 0.048, SLOP);
        scroll.release(0.05, 1.0);
        assert!(scroll.is_active());
        assert!(!scroll.is_dragging());
        scroll.cancel();
        assert!(!scroll.is_active());
    }

    #[test]
    fn step_size_does_not_change_the_travel() {
        let travel = |steps: u32, dt: f32| {
            let mut scroll = DragScroll::default();
            flick(&mut scroll);
            (0..steps).map(|_| scroll.step(dt, 1.0)).sum::<f32>()
        };
        let fine = travel(60, 0.016);
        let coarse = travel(30, 0.032);
        assert!(fine > 100.0);
        assert!((fine - coarse).abs() <= 2.0, "{fine} vs {coarse}");
    }
}
