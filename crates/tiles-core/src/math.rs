//! Column-major 4x4 matrices and 3-vectors in f64, as 3D Tiles writes them.

pub type Mat4 = [f64; 16];
pub type Vec3 = [f64; 3];

pub const IDENTITY: Mat4 = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];

pub fn mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut out = [0.0; 16];
    for c in 0..4 {
        for r in 0..4 {
            out[c * 4 + r] = (0..4).map(|k| a[k * 4 + r] * b[c * 4 + k]).sum();
        }
    }
    out
}

pub fn point(m: &Mat4, p: Vec3) -> Vec3 {
    [0, 1, 2].map(|r| m[r] * p[0] + m[4 + r] * p[1] + m[8 + r] * p[2] + m[12 + r])
}

pub fn vector(m: &Mat4, v: Vec3) -> Vec3 {
    [0, 1, 2].map(|r| m[r] * v[0] + m[4 + r] * v[1] + m[8 + r] * v[2])
}

pub fn translation(t: Vec3) -> Mat4 {
    let mut m = IDENTITY;
    m[12..15].copy_from_slice(&t);
    m
}

/// A matrix with the given columns (x, y, z axes) and origin.
pub fn from_axes(x: Vec3, y: Vec3, z: Vec3, origin: Vec3) -> Mat4 {
    [x[0], x[1], x[2], 0.0, y[0], y[1], y[2], 0.0, z[0], z[1], z[2], 0.0, origin[0], origin[1], origin[2], 1.0]
}

/// The inverse of a rigid transform (rotation and translation, no scale).
pub fn rigid_inverse(m: &Mat4) -> Mat4 {
    let r = |c: usize, k: usize| m[c * 4 + k];
    let t = [m[12], m[13], m[14]];
    let mut out = IDENTITY;
    for c in 0..3 {
        for k in 0..3 {
            out[c * 4 + k] = r(k, c);
        }
    }
    for k in 0..3 {
        out[12 + k] = -(0..3).map(|c| r(k, c) * t[c]).sum::<f64>();
    }
    out
}

pub fn from_slice(values: &[serde_json::Value]) -> Option<Mat4> {
    if values.len() != 16 {
        return None;
    }
    let mut m = [0.0; 16];
    for (slot, v) in m.iter_mut().zip(values) {
        *slot = v.as_f64()?;
    }
    Some(m)
}

pub fn add(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
pub fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
pub fn scale(a: Vec3, s: f64) -> Vec3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
pub fn dot(a: Vec3, b: Vec3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
pub fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
pub fn length(a: Vec3) -> f64 {
    dot(a, a).sqrt()
}
pub fn normalize(a: Vec3) -> Vec3 {
    let l = length(a);
    if l > 0.0 {
        scale(a, 1.0 / l)
    } else {
        a
    }
}

/// The largest scale factor of a matrix's axes (to scale a radius).
pub fn max_scale(m: &Mat4) -> f64 {
    (0..3).map(|c| length([m[c * 4], m[c * 4 + 1], m[c * 4 + 2]])).fold(0.0, f64::max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rigid_inverse_undoes_a_turn_and_move() {
        let m = from_axes([0.0, 1.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [10.0, 20.0, 30.0]);
        let p = point(&m, [1.0, 2.0, 3.0]);
        assert_eq!(p, [8.0, 21.0, 33.0]);
        assert_eq!(point(&rigid_inverse(&m), p), [1.0, 2.0, 3.0]);
        assert_eq!(mul(&m, &rigid_inverse(&m)), IDENTITY);
    }
}
