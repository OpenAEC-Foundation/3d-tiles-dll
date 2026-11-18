//! Bounding volumes of tiles: box, region and sphere.

use crate::geo;
use crate::math::{self, Mat4, Vec3};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Volume {
    /// Centre and three half axes, in the tile's frame.
    Box { center: Vec3, half: [Vec3; 3] },
    /// West, south, east, north (radians) and lowest and highest height (metres), on WGS 84.
    Region([f64; 6]),
    /// Centre and radius, in the tile's frame.
    Sphere { center: Vec3, radius: f64 },
}

impl Volume {
    pub fn parse(v: &Value) -> Option<Volume> {
        let numbers = |key: &str| -> Option<Vec<f64>> { v[key].as_array()?.iter().map(Value::as_f64).collect() };
        if let Some(b) = numbers("box").filter(|b| b.len() == 12) {
            return Some(Volume::Box { center: [b[0], b[1], b[2]], half: [[b[3], b[4], b[5]], [b[6], b[7], b[8]], [b[9], b[10], b[11]]] });
        }
        if let Some(r) = numbers("region").filter(|r| r.len() == 6) {
            return Some(Volume::Region([r[0], r[1], r[2], r[3], r[4], r[5]]));
        }
        if let Some(s) = numbers("sphere").filter(|s| s.len() == 4) {
            return Some(Volume::Sphere { center: [s[0], s[1], s[2]], radius: s[3] });
        }
        None
    }

    /// The enclosing sphere in world coordinates (a region is on the earth whatever the
    /// transform; the others are moved by it).
    pub fn sphere(&self, world: &Mat4) -> (Vec3, f64) {
        match *self {
            Volume::Box { center, half } => {
                let radius = math::length(math::add(math::add(half[0].map(f64::abs), half[1].map(f64::abs)), half[2].map(f64::abs)));
                let r = math::length(half[0]).powi(2) + math::length(half[1]).powi(2) + math::length(half[2]).powi(2);
                (math::point(world, center), r.sqrt().min(radius) * math::max_scale(world))
            }
            Volume::Sphere { center, radius } => (math::point(world, center), radius * math::max_scale(world)),
            Volume::Region([w, s, e, n, lo, hi]) => {
                let mut corners = Vec::with_capacity(18);
                for lon in [w, (w + e) / 2.0, e] {
                    for lat in [s, (s + n) / 2.0, n] {
                        for h in [lo, hi] {
                            corners.push(geo::to_ecef(lon, lat, h));
                        }
                    }
                }
                let center = math::scale(corners.iter().fold([0.0; 3], |a, &c| math::add(a, c)), 1.0 / corners.len() as f64);
                let radius = corners.iter().map(|&c| math::length(math::sub(c, center))).fold(0.0, f64::max);
                (center, radius)
            }
        }
    }

    /// The part of this volume an implicit tile at `level` and `(x, y, z)` covers (quadtree:
    /// `z` is 0 and the third axis, or the height, is not split).
    pub fn subdivided(&self, level: u32, x: u64, y: u64, z: u64, octree: bool) -> Volume {
        let n = (1u64 << level) as f64;
        let part = |i: u64| (2.0 * i as f64 + 1.0) / n - 1.0; // centre in [-1, 1]
        match *self {
            Volume::Box { center, half } => {
                let (cx, cy) = (part(x), part(y));
                let cz = if octree { part(z) } else { 0.0 };
                let c = math::add(math::add(math::add(center, math::scale(half[0], cx)), math::scale(half[1], cy)), math::scale(half[2], cz));
                let hz = if octree { math::scale(half[2], 1.0 / n) } else { half[2] };
                Volume::Box { center: c, half: [math::scale(half[0], 1.0 / n), math::scale(half[1], 1.0 / n), hz] }
            }
            Volume::Region([w, s, e, nn, lo, hi]) => {
                let (dx, dy) = ((e - w) / n, (nn - s) / n);
                let (lo2, hi2) = if octree { let dz = (hi - lo) / n; (lo + dz * z as f64, lo + dz * (z + 1) as f64) } else { (lo, hi) };
                Volume::Region([w + dx * x as f64, s + dy * y as f64, w + dx * (x + 1) as f64, s + dy * (y + 1) as f64, lo2, hi2])
            }
            Volume::Sphere { .. } => *self,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subdivides_a_box() {
        let root = Volume::Box { center: [0.5, 0.5, 0.0], half: [[0.5, 0.0, 0.0], [0.0, 0.5, 0.0], [0.0, 0.0, 0.1]] };
        let Volume::Box { center, half } = root.subdivided(1, 1, 0, 0, false) else { panic!() };
        assert_eq!(center, [0.75, 0.25, 0.0]);
        assert_eq!(half[0], [0.25, 0.0, 0.0]);
        assert_eq!(half[2], [0.0, 0.0, 0.1]);
    }
}
