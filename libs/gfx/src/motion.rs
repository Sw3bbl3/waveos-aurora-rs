//! Shared, interruptible motion in logical time. No timers are owned here.
pub const FEEDBACK: u64 = 120;
pub const REVEAL: u64 = 180;
pub const TRANSITION: u64 = 240;

pub fn ease(elapsed: u64, duration: u64) -> i32 {
    let t = (elapsed.saturating_mul(1000) / duration.max(1)).min(1000) as i64;
    let inv = 1000 - t;
    (1000 - inv * inv * inv / 1_000_000) as i32
}

#[derive(Clone, Copy, Default)]
pub struct Fade {
    from: i32,
    to: i32,
    start: u64,
}
impl Fade {
    pub fn value(&self, now: u64, reduced: bool) -> i32 {
        if reduced {
            return self.to;
        }
        self.from + (self.to - self.from) * ease(now.saturating_sub(self.start), REVEAL) / 1000
    }
    pub fn set(&mut self, visible: bool, now: u64, reduced: bool) {
        let to = if visible { 1000 } else { 0 };
        if to != self.to {
            self.from = self.value(now, reduced);
            self.to = to;
            self.start = now;
        }
    }
    pub fn moving(&self, now: u64, reduced: bool) -> bool {
        !reduced && self.from != self.to && now.saturating_sub(self.start) < REVEAL
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reversing_reveal_keeps_position_and_finishes() {
        let mut f = Fade::default();
        f.set(true, 100, false);
        let halfway = f.value(190, false);
        f.set(false, 190, false);
        assert_eq!(f.value(190, false), halfway);
        assert_eq!(f.value(370, false), 0);
        assert!(!f.moving(370, false));
        f.set(true, 400, true);
        assert_eq!(f.value(400, true), 1000);
        assert!(!f.moving(400, true));
    }
}
