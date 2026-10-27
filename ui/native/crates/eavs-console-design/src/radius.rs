//! Radius: single dial, proportional scale, sharp-at-0 (spec/radius.md).
//! The dial value is per-tool identity; this template defaults to a rounded
//! dial so the proportional tiers are visible, but sharp (`0`) is the oqto
//! identity and is a legitimate choice.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RadiusScale {
    pub dial: f32,
}

impl RadiusScale {
    pub const fn new(dial: f32) -> Self {
        Self { dial }
    }

    pub fn sm(self) -> f32 {
        self.dial * 0.5
    }
    pub fn md(self) -> f32 {
        self.dial * 0.75
    }
    pub fn lg(self) -> f32 {
        self.dial
    }
    pub fn xl(self) -> f32 {
        self.dial * 1.25
    }

    /// Exact concentric helpers (opt-in).
    pub fn child_radius(parent: f32, inset: f32) -> f32 {
        (parent - inset).max(0.0)
    }
    pub fn parent_radius(child: f32, inset: f32) -> f32 {
        child + inset
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sharp_at_zero_and_all_tiers_round_when_dialed() {
        let sharp = RadiusScale::new(0.0);
        assert_eq!(
            (sharp.sm(), sharp.md(), sharp.lg(), sharp.xl()),
            (0.0, 0.0, 0.0, 0.0)
        );
        let r = RadiusScale::new(8.0);
        assert!(r.sm() > 0.0 && r.sm() < r.md() && r.md() < r.lg() && r.lg() < r.xl());
    }
}
