//! The WGS 84 ellipsoid: geodetic to earth-centred coordinates and back, and the east-north-up
//! frame at a point.

use crate::math::{self, Mat4, Vec3};

pub const A: f64 = 6_378_137.0;
pub const F: f64 = 1.0 / 298.257_223_563;
pub const E2: f64 = F * (2.0 - F);

/// Longitude and latitude in radians and height in metres to earth-centred coordinates.
pub fn to_ecef(lon: f64, lat: f64, height: f64) -> Vec3 {
    let n = A / (1.0 - E2 * lat.sin().powi(2)).sqrt();
    [(n + height) * lat.cos() * lon.cos(), (n + height) * lat.cos() * lon.sin(), (n * (1.0 - E2) + height) * lat.sin()]
}

/// Earth-centred coordinates to longitude, latitude (radians) and height (metres).
pub fn to_geodetic(p: Vec3) -> (f64, f64, f64) {
    let [x, y, z] = p;
    let lon = y.atan2(x);
    let r = (x * x + y * y).sqrt();
    let mut lat = z.atan2(r * (1.0 - E2));
    let mut height = 0.0;
    for _ in 0..8 {
        let n = A / (1.0 - E2 * lat.sin().powi(2)).sqrt();
        height = if lat.cos().abs() > 1e-9 { r / lat.cos() - n } else { z.abs() - n * (1.0 - E2) };
        lat = z.atan2(r * (1.0 - E2 * n / (n + height)));
    }
    (lon, lat, height)
}

/// Whether a point is plausibly earth-centred: between the earth's centre region and orbit.
pub fn is_earth_centred(p: Vec3) -> bool {
    let r = math::length(p);
    (6_000_000.0..7_000_000.0).contains(&r)
}

/// The east-north-up frame at an earth-centred point: from local (east, north, up) metres to
/// earth-centred coordinates.
pub fn enu_frame(origin: Vec3) -> Mat4 {
    let (lon, lat, _) = to_geodetic(origin);
    let east = [-lon.sin(), lon.cos(), 0.0];
    let north = [-lat.sin() * lon.cos(), -lat.sin() * lon.sin(), lat.cos()];
    let up = [lat.cos() * lon.cos(), lat.cos() * lon.sin(), lat.sin()];
    math::from_axes(east, north, up, origin)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geodetic_round_trip() {
        let (lon, lat, h) = (5.3_f64.to_radians(), 52.1_f64.to_radians(), 12.5);
        let p = to_ecef(lon, lat, h);
        let (lon2, lat2, h2) = to_geodetic(p);
        assert!((lon - lon2).abs() < 1e-12 && (lat - lat2).abs() < 1e-12 && (h - h2).abs() < 1e-6);
        // on the equator at Greenwich: x = a
        assert!((to_ecef(0.0, 0.0, 0.0)[0] - A).abs() < 1e-9);
    }

    #[test]
    fn up_of_the_frame_points_away_from_the_centre() {
        let p = to_ecef(0.3, 0.8, 0.0);
        let frame = enu_frame(p);
        let up = [frame[8], frame[9], frame[10]];
        assert!(math::dot(up, math::normalize(p)) > 0.99);
    }
}
