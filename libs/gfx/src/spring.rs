//! Damped springs in fixed point, for motion that can be interrupted.
//!
//! A [`Spring`] moves a value towards a target the way a mass on a damped
//! spring would. Changing the target mid-flight keeps the current position
//! *and velocity*, so a reversed transition turns around smoothly instead of
//! jumping or restarting its curve. Time is the caller's milliseconds; the
//! spring integrates in 1 ms steps (semi-implicit Euler), so the same inputs
//! always give the same frames, at any frame rate.
//!
//! Parameters follow the familiar "response and damping" form: `response`
//! is the period of the undamped oscillation in milliseconds (how quickly it
//! gets there) and `damping` is the damping ratio in thousandths (1000 is
//! critically damped: the fastest approach without overshoot; a little less
//! gives one soft overshoot). Values are plain integers (points, thousandths
//! of progress, …); positions and velocities are kept with 16 fractional bits.
//!
//! No floating point: the kernel compositor uses this too.

/// 2π × 65536.
const TAU_Q16: i64 = 411_775;
/// Longest stretch simulated at once; a stalled caller lands on the target.
const MAX_STEP_MS: u64 = 2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spring {
    /// Position and target, 16.16.
    pos: i64,
    target: i64,
    /// Velocity, 16.16 units per second.
    vel: i64,
    /// Stiffness ω² (1/s², 16.16) and damping 2ζω (1/s, 16.16).
    k: i64,
    c: i64,
    /// When the state was last brought up to date (ms).
    last: u64,
}

/// A gentle, critically damped settle (panels, overlays).
pub const SMOOTH: (u32, u32) = (420, 1000);
/// Quick and slightly lively (focus, tiles): one small overshoot.
pub const SNAPPY: (u32, u32) = (300, 820);
/// The big scene changes: unhurried, landing with a hint of bounce.
pub const SCENE: (u32, u32) = (560, 880);

impl Spring {
    /// A spring resting at `value`. `response` in ms, `damping` in thousandths.
    pub fn new(value: i32, (response, damping): (u32, u32)) -> Spring {
        let response = response.max(40) as i64;
        let omega = TAU_Q16 * 1000 / response; // 16.16 rad/s
        let k = (omega * omega) >> 16;
        let c = 2 * omega * damping.min(4000) as i64 / 1000;
        let v = (value as i64) << 16;
        Spring { pos: v, target: v, vel: 0, k, c, last: 0 }
    }

    /// The position now (rounded to the nearest unit).
    pub fn value(&self) -> i32 {
        ((self.pos + 0x8000) >> 16) as i32
    }

    /// The position in 1/256 units (for sub-pixel drawing).
    pub fn value_q8(&self) -> i32 {
        (self.pos >> 8) as i32
    }

    pub fn target(&self) -> i32 {
        (self.target >> 16) as i32
    }

    /// Units per second.
    pub fn velocity(&self) -> i32 {
        (self.vel >> 16) as i32
    }

    /// Whether it is at rest on its target.
    pub fn settled(&self) -> bool {
        self.pos == self.target && self.vel == 0
    }

    /// Aims at `target` from wherever it is, keeping its velocity. `now`
    /// brings the state up to date first, so time spent before the change
    /// still counts towards the old target.
    pub fn set_target(&mut self, target: i32, now: u64) {
        self.advance(now);
        self.target = (target as i64) << 16;
    }

    /// Jumps to `value` and stops there (reduced motion, layout changes).
    pub fn snap(&mut self, value: i32, now: u64) {
        let v = (value as i64) << 16;
        self.pos = v;
        self.target = v;
        self.vel = 0;
        self.last = now;
    }

    /// Adds velocity (units per second): a nudge, such as a bump against
    /// the end of a row, that the spring then absorbs.
    pub fn kick(&mut self, velocity: i32, now: u64) {
        self.advance(now);
        self.vel += (velocity as i64) << 16;
    }

    /// Brings the state up to `now`. Returns true while still moving.
    pub fn advance(&mut self, now: u64) -> bool {
        let elapsed = now.saturating_sub(self.last);
        self.last = now.max(self.last);
        if self.settled() {
            return false;
        }
        if elapsed > MAX_STEP_MS {
            self.pos = self.target;
            self.vel = 0;
            return false;
        }
        for _ in 0..elapsed {
            // a = -k (x - target) - c v; one millisecond at a time.
            let a = -((self.k * (self.pos - self.target)) >> 16) - ((self.c * self.vel) >> 16);
            self.vel += a / 1000;
            self.pos += self.vel / 1000;
        }
        // At rest: within 1/64 unit and slower than a unit per second.
        if (self.pos - self.target).abs() < 1 << 10 && self.vel.abs() < 1 << 16 {
            self.pos = self.target;
            self.vel = 0;
        }
        !self.settled()
    }
}

/// `t` (thousandths of the way from `start` to `end`) mapped to 0..=1000 of
/// a sub-interval: a staggered element's own progress within a shared
/// timeline. Values past either end pass through unclamped by up to 15%,
/// so a spring's overshoot reaches the elements too.
pub fn stage(t: i32, start: i32, end: i32) -> i32 {
    let span = (end - start).max(1);
    ((t - start) * 1000 / span).clamp(-150, 1150)
}

/// `a` to `b` by `t` thousandths.
pub fn lerp(a: i32, b: i32, t: i32) -> i32 {
    a + ((b - a) as i64 * t as i64 / 1000) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(s: &mut Spring, from: u64, to: u64) -> (i32, i32) {
        let (mut lo, mut hi) = (i32::MAX, i32::MIN);
        let mut t = from;
        while t < to {
            t += 16;
            s.advance(t);
            lo = lo.min(s.value());
            hi = hi.max(s.value());
        }
        (lo, hi)
    }

    #[test]
    fn critically_damped_arrives_without_overshoot() {
        let mut s = Spring::new(0, (400, 1000));
        s.set_target(1000, 0);
        let (_, hi) = run(&mut s, 0, 2000);
        assert!(hi <= 1000, "overshot to {hi}");
        assert!(s.settled());
        assert_eq!(s.value(), 1000);
    }

    #[test]
    fn response_sets_the_pace() {
        let mut fast = Spring::new(0, (250, 1000));
        let mut slow = Spring::new(0, (600, 1000));
        fast.set_target(1000, 0);
        slow.set_target(1000, 0);
        fast.advance(200);
        slow.advance(200);
        assert!(fast.value() > slow.value() + 200, "{} vs {}", fast.value(), slow.value());
        // Critically damped with a 400 ms response is ~95% there by 400 ms.
        let mut s = Spring::new(0, (400, 1000));
        s.set_target(1000, 0);
        s.advance(400);
        assert!((900..=1000).contains(&s.value()), "{}", s.value());
    }

    #[test]
    fn underdamped_overshoots_a_little_then_settles() {
        let mut s = Spring::new(0, SNAPPY);
        s.set_target(1000, 0);
        let (_, hi) = run(&mut s, 0, 3000);
        assert!(hi > 1000 && hi < 1100, "peak {hi}");
        assert!(s.settled());
    }

    #[test]
    fn reversing_keeps_position_and_velocity() {
        let mut s = Spring::new(0, SCENE);
        s.set_target(1000, 0);
        s.advance(150);
        let (p, v) = (s.value(), s.velocity());
        assert!(p > 0 && v > 0);
        s.set_target(0, 150);
        // No jump at the moment of reversal …
        assert_eq!((s.value(), s.velocity()), (p, v));
        // … it keeps moving forward briefly before turning back …
        s.advance(166);
        assert!(s.value() >= p, "{} < {p}", s.value());
        // … and comes home.
        run(&mut s, 166, 4000);
        assert_eq!(s.value(), 0);
        assert!(s.settled());
    }

    #[test]
    fn steps_are_independent_of_frame_rate() {
        let mut a = Spring::new(0, SNAPPY);
        let mut b = Spring::new(0, SNAPPY);
        a.set_target(500, 0);
        b.set_target(500, 0);
        for t in (1..=240).map(|i| i * 4) {
            a.advance(t);
        }
        for t in (1..=30).map(|i| i * 32) {
            b.advance(t);
        }
        assert_eq!(a, b);
    }

    #[test]
    fn stalls_land_on_target_and_kicks_are_absorbed() {
        let mut s = Spring::new(0, SMOOTH);
        s.set_target(300, 0);
        assert!(!s.advance(10_000));
        assert_eq!(s.value(), 300);
        s.kick(-800, 10_000);
        assert!(s.advance(10_050));
        assert!(s.value() < 300);
        run(&mut s, 10_050, 14_000);
        assert_eq!(s.value(), 300);
    }

    #[test]
    fn stages_stagger_a_timeline() {
        assert_eq!(stage(0, 200, 600), -150);
        assert_eq!(stage(400, 200, 600), 500);
        assert_eq!(stage(1000, 200, 600), 1150);
        assert_eq!(lerp(100, 200, 250), 125);
    }
}
