//! Presentation timing shared by both themes. No navigation waits for motion.

pub const FOCUS: f32 = 0.24;
pub const PAGE: f32 = 0.24;
pub const SHEET: f32 = 0.22;
pub const CONTROL: f32 = 0.16;

pub fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

/// A content change starts a fresh entrance; focus changes keep it running.
#[derive(Clone)]
pub struct Entrance<K> {
    key: K,
    since: f64,
}

impl<K: PartialEq> Entrance<K> {
    pub fn settled(key: K, now: f64, seconds: f32) -> Self {
        Self {
            key,
            since: now - f64::from(seconds),
        }
    }

    pub fn sample(&mut self, key: K, now: f64, seconds: f32) -> f32 {
        if self.key != key {
            self.key = key;
            self.since = now;
        }
        ease_out(((now - self.since) / f64::from(seconds.max(0.001))) as f32)
    }
}

/// Preserve velocity when a held arrow changes the destination again.
#[derive(Clone, Copy)]
pub struct Spring {
    value: f32,
    velocity: f32,
    target: f32,
    at: f64,
}

impl Spring {
    pub fn new(value: f32, now: f64) -> Self {
        Self {
            value,
            velocity: 0.0,
            target: value,
            at: now,
        }
    }

    pub fn sample(
        &mut self,
        target: f32,
        now: f64,
        response: f32,
        damping: f32,
    ) -> (f32, f32, bool) {
        let gap = (now - self.at).max(0.0);
        self.at = now;
        let k = (std::f32::consts::TAU / response.max(0.05)).powi(2);
        let drag = 2.0 * damping * k.sqrt();
        // A hidden window resumes at rest, without replaying old movement.
        let mut dt = if gap > 0.25 {
            (self.value, self.velocity) = (self.target, 0.0);
            0.0
        } else {
            gap as f32
        };
        while dt > 0.0 {
            let h = dt.min(1.0 / 240.0);
            self.velocity += (-k * (self.value - self.target) - drag * self.velocity) * h;
            self.value += self.velocity * h;
            dt -= h;
        }
        self.target = target;
        let scale = target.abs().max(1.0);
        let moving =
            (self.value - target).abs() > 1e-3 * scale || self.velocity.abs() > 1e-2 * scale;
        if !moving {
            (self.value, self.velocity) = (target, 0.0);
        }
        (self.value, self.velocity, moving)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entrances_restart_for_content_and_settle() {
        let mut entry = Entrance::settled("home", 0.0, PAGE);
        assert_eq!(entry.sample("home", 0.0, PAGE), 1.0);
        assert_eq!(entry.sample("settings", 1.0, PAGE), 0.0);
        let halfway = entry.sample("settings", 1.12, PAGE);
        assert!(halfway > 0.5 && halfway < 1.0);
        assert_eq!(entry.sample("settings", 1.3, PAGE), 1.0);
        assert_eq!(entry.sample("home", 1.31, PAGE), 0.0);
    }

    #[test]
    fn rapid_retargeting_preserves_position_and_velocity() {
        let mut spring = Spring::new(0.0, 0.0);
        spring.sample(1.0, 0.0, FOCUS, 1.0);
        let (value, velocity, _) = spring.sample(1.0, 0.06, FOCUS, 1.0);
        assert!(value > 0.0 && value < 1.0 && velocity > 0.0);
        let (redirected, speed, _) = spring.sample(0.0, 0.06, FOCUS, 1.0);
        assert_eq!((value, velocity), (redirected, speed));
        for frame in 1..=60 {
            spring.sample(0.0, 0.06 + f64::from(frame) / 60.0, FOCUS, 1.0);
        }
        assert_eq!(spring.sample(0.0, 1.1, FOCUS, 1.0), (0.0, 0.0, false));
    }

    #[test]
    fn focus_does_not_overshoot_and_stops_requesting_frames() {
        for fps in [30, 60, 120] {
            let mut spring = Spring::new(0.0, 0.0);
            spring.sample(1.0, 0.0, FOCUS, 1.0);
            let mut previous = 0.0;
            for frame in 1..=fps {
                let (value, _, _) =
                    spring.sample(1.0, f64::from(frame) / f64::from(fps), FOCUS, 1.0);
                assert!(value >= previous && value <= 1.0);
                previous = value;
            }
            assert_eq!(spring.sample(1.0, 1.1, FOCUS, 1.0), (1.0, 0.0, false));
        }
    }

    #[test]
    fn waking_does_not_replay_motion_or_skip_the_next_change() {
        let mut spring = Spring::new(0.0, 0.0);
        spring.sample(1.0, 0.0, FOCUS, 1.0);
        spring.sample(1.0, 0.03, FOCUS, 1.0);
        assert_eq!(spring.sample(1.0, 20.0, FOCUS, 1.0), (1.0, 0.0, false));
        assert_eq!(spring.sample(0.0, 40.0, FOCUS, 1.0), (1.0, 0.0, true));
        let (value, _, _) = spring.sample(0.0, 40.016, FOCUS, 1.0);
        assert!(value > 0.0 && value < 1.0);
    }
}
