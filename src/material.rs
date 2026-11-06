//! Wex Material 3 Expressive foundation: semantic roles and interruptible spring motion.
//! Spatial springs may overshoot; effect values are clamped. Reduced motion snaps to target.
#[derive(Clone, Debug)]
pub struct Spring {
    pub value: f32,
    pub velocity: f32,
}
impl Default for Spring {
    fn default() -> Self {
        Self {
            value: 0.,
            velocity: 0.,
        }
    }
}
impl Spring {
    pub fn step(&mut self, target: f32, dt: f32, reduced: bool) -> f32 {
        if reduced {
            self.value = target;
            self.velocity = 0.;
            return target;
        }
        self.step_with(target, dt, reduced, MotionToken::FastSpatial)
    }
    pub fn step_with(&mut self, target: f32, dt: f32, reduced: bool, token: MotionToken) -> f32 {
        if reduced {
            self.value = target;
            self.velocity = 0.;
            return target;
        }
        let (stiffness, damping) = token.spec();
        let dt = if dt.is_finite() {
            dt.clamp(0., 0.05)
        } else {
            0.
        };
        let steps = (dt / 0.004).ceil().max(1.) as usize;
        let h = dt / steps as f32;
        // Wex fast spatial token, using M3's damping/stiffness model.
        for _ in 0..steps {
            let acceleration =
                stiffness * (target - self.value) - 2. * damping * stiffness.sqrt() * self.velocity;
            self.velocity += acceleration * h;
            self.value += self.velocity * h;
        }
        if (self.value - target).abs() < 0.001 && self.velocity.abs() < 0.001 {
            self.value = target;
            self.velocity = 0.;
        }
        self.value
    }
    pub fn moving(&self, target: f32) -> bool {
        (self.value - target).abs() > 0.001 || self.velocity.abs() > 0.001
    }
}
#[derive(Clone, Copy, Debug)]
pub enum MotionToken {
    FastSpatial,
    DefaultSpatial,
    Effects,
}
impl MotionToken {
    fn spec(self) -> (f32, f32) {
        match self {
            Self::FastSpatial => (650., 0.74),
            Self::DefaultSpatial => (420., 0.82),
            Self::Effects => (800., 1.),
        }
    }
}
#[cfg(feature = "desktop")]
#[path = "material_ui.rs"]
mod desktop;
#[cfg(feature = "desktop")]
pub use desktop::*;
#[cfg(feature = "desktop")]
#[path = "material_fonts.rs"]
mod fonts;
#[cfg(feature = "desktop")]
pub use fonts::FontFallback;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spring_settles_and_handles_interruptions() {
        let mut s = Spring::default();
        for _ in 0..20 {
            s.step(1., 1. / 60., false);
        }
        assert!(s.value > 0.9);
        let before = s.value;
        s.step(0., 1. / 60., false);
        assert!((s.value - before).abs() < 0.2);
        for _ in 0..180 {
            s.step(0., 1. / 60., false);
        }
        assert_eq!(s.value, 0.);
    }
    #[test]
    fn reduced_motion_snaps_and_resets_velocity() {
        let mut s = Spring {
            value: 0.5,
            velocity: 20.,
        };
        assert_eq!(s.step(1., 1., true), 1.);
        assert_eq!(s.velocity, 0.);
        assert!(!s.moving(1.));
    }
    #[test]
    fn frame_stalls_do_not_explode() {
        let mut s = Spring::default();
        for _ in 0..100 {
            s.step(1., 10., false);
        }
        assert!(s.value.is_finite() && (s.value - 1.).abs() < 0.001);
    }
}
