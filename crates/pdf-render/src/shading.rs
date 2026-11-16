//! Axial and radial shadings (ISO 32000-2, 8.7.4.5.3 and 8.7.4.5.4;
//! tranche 3.16): the parameter of a point, and the shading drawn into an
//! image of the device area it may cover. Colours come from a table of
//! `STEPS` entries over the parameter, made once per shading by the caller
//! (function and colour space evaluated there).

use crate::geometry::{Matrix, Point};

/// Entries of the colour table over the parameter, 0 to 1: as many as
/// the levels of an 8-bit channel, enough for a smooth shading.
pub(crate) const STEPS: usize = 256;
/// Pixels of the image a shading is drawn into at most; a larger device
/// area gets a smaller image, smoothed when drawn (shadings are smooth).
pub(crate) const MAX_PIXELS: u64 = 1 << 20;

/// Where the colour of a point comes from.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Geometry {
    /// Along the axis from (x0, y0) to (x1, y1).
    Axial { x0: f64, y0: f64, x1: f64, y1: f64 },
    /// Between the circle (x0, y0, r0) and the circle (x1, y1, r1).
    Radial {
        x0: f64,
        y0: f64,
        r0: f64,
        x1: f64,
        y1: f64,
        r1: f64,
    },
}

/// A shading ready to draw: its geometry, whether it extends beyond its
/// start and end, and its colours (RGBA, alpha 0 for none) over the
/// parameter.
pub(crate) struct Shading {
    pub geometry: Geometry,
    pub extend: [bool; 2],
    pub colors: Vec<[u8; 4]>,
    /// `/Background` as RGBA: what a shading pattern paints under the
    /// shading (`sh` ignores it, 8.7.4.3).
    pub background: Option<[u8; 4]>,
}

impl Geometry {
    /// The parameter s of `p` (0 at the start, 1 at the end), clamped when
    /// the shading extends there; `None` where nothing is painted.
    pub(crate) fn parameter(&self, p: Point, extend: [bool; 2]) -> Option<f64> {
        let within = |s: f64| -> Option<f64> {
            if s < 0.0 {
                extend[0].then_some(0.0)
            } else if s > 1.0 {
                extend[1].then_some(1.0)
            } else {
                Some(s)
            }
        };
        match *self {
            Geometry::Axial { x0, y0, x1, y1 } => {
                let (dx, dy) = (x1 - x0, y1 - y0);
                let length = dx * dx + dy * dy;
                if length == 0.0 || !length.is_finite() {
                    return None;
                }
                within(((p.x - x0) * dx + (p.y - y0) * dy) / length)
            }
            Geometry::Radial {
                x0,
                y0,
                r0,
                x1,
                y1,
                r1,
            } => {
                // The circles of centre c(s) = c0 + s (c1 - c0) and radius
                // r(s) = r0 + s (r1 - r0) through p: a s^2 - 2 b s + c = 0.
                let (cx, cy, dr) = (x1 - x0, y1 - y0, r1 - r0);
                let (px, py) = (p.x - x0, p.y - y0);
                let a = cx * cx + cy * cy - dr * dr;
                let b = px * cx + py * cy + r0 * dr;
                let c = px * px + py * py - r0 * r0;
                // `a` is 0 for circles of the same size difference as their
                // distance (tangent ones): a linear equation then. The
                // threshold is relative to the terms `a` is made of.
                let scale = cx * cx + cy * cy + dr * dr;
                let roots = if a.abs() <= 1e-12 * scale {
                    if b == 0.0 {
                        return None;
                    }
                    let s = c / (2.0 * b);
                    [s, s]
                } else {
                    let discriminant = b * b - a * c;
                    if discriminant < 0.0 {
                        return None;
                    }
                    // Without cancellation: q = b + sign(b) sqrt(d), then
                    // the roots q / a and c / q.
                    let q = b + discriminant.sqrt().copysign(b);
                    let (s1, s2) = if q == 0.0 { (0.0, 0.0) } else { (q / a, c / q) };
                    [s1.max(s2), s1.min(s2)]
                };
                // The larger s whose circle has a radius and is painted.
                roots
                    .into_iter()
                    .filter(|s| s.is_finite() && r0 + s * dr >= 0.0)
                    .find_map(within)
            }
        }
    }
}

impl Shading {
    /// The colour at parameter `s` (0 to 1).
    fn color(&self, s: f64) -> [u8; 4] {
        let last = self.colors.len().saturating_sub(1);
        let index = (s * last as f64).round().clamp(0.0, last as f64) as usize;
        self.colors.get(index).copied().unwrap_or([0; 4])
    }

    /// RGBA pixels of a `width` x `height` image whose pixel (i, j) covers
    /// the device square of side `step` at (x0 + i step, y0 + j step);
    /// `to_shading` maps device points to the shading's space.
    pub(crate) fn render(
        &self,
        to_shading: &Matrix,
        x0: f64,
        y0: f64,
        step: f64,
        width: u16,
        height: u16,
    ) -> Vec<u8> {
        let mut rgba = vec![0u8; usize::from(width) * usize::from(height) * 4];
        for (j, row) in rgba.chunks_exact_mut(usize::from(width) * 4).enumerate() {
            let y = y0 + (j as f64 + 0.5) * step;
            for (i, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let x = x0 + (i as f64 + 0.5) * step;
                let p = to_shading.apply(Point { x, y });
                if let Some(s) = self.geometry.parameter(p, self.extend) {
                    *pixel = self.color(s);
                }
            }
        }
        rgba
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f64, y: f64) -> Point {
        Point { x, y }
    }

    #[test]
    fn axial_parameters_follow_the_axis() {
        let axial = Geometry::Axial {
            x0: 0.0,
            y0: 0.0,
            x1: 10.0,
            y1: 0.0,
        };
        assert_eq!(axial.parameter(p(5.0, 7.0), [false; 2]), Some(0.5));
        assert_eq!(axial.parameter(p(-1.0, 0.0), [false; 2]), None);
        assert_eq!(axial.parameter(p(-1.0, 0.0), [true, false]), Some(0.0));
        assert_eq!(axial.parameter(p(11.0, 0.0), [false, true]), Some(1.0));
        let degenerate = Geometry::Axial {
            x0: 1.0,
            y0: 1.0,
            x1: 1.0,
            y1: 1.0,
        };
        assert_eq!(degenerate.parameter(p(1.0, 1.0), [true; 2]), None);
    }

    #[test]
    fn radial_parameters_take_the_larger_circle() {
        // Concentric circles of radius 0 and 10: s is the distance / 10.
        let radial = Geometry::Radial {
            x0: 0.0,
            y0: 0.0,
            r0: 0.0,
            x1: 0.0,
            y1: 0.0,
            r1: 10.0,
        };
        let s = radial.parameter(p(3.0, 4.0), [false; 2]).expect("inside");
        assert!((s - 0.5).abs() < 1e-12);
        assert_eq!(radial.parameter(p(20.0, 0.0), [false; 2]), None);
        assert_eq!(radial.parameter(p(20.0, 0.0), [false, true]), Some(1.0));
        // A cone from a point to a circle beside it: outside the cone,
        // nothing (no circle passes there with a radius of at least 0).
        let cone = Geometry::Radial {
            x0: 0.0,
            y0: 0.0,
            r0: 0.0,
            x1: 10.0,
            y1: 0.0,
            r1: 2.0,
        };
        assert!(cone.parameter(p(10.0, 1.0), [false; 2]).is_some());
        assert_eq!(cone.parameter(p(-5.0, 5.0), [false; 2]), None);
    }
}
