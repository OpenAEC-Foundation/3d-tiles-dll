//! Quantized-mesh terrain tiles (`.terrain`): a triangulated height field over one tile of the
//! geographic tiling scheme, with vertex positions as 15-bit fractions of the tile.

use crate::geo;
use crate::math::Vec3;

#[derive(Debug, Clone)]
pub struct QuantizedMesh {
    /// The tile's centre, earth-centred (metres).
    pub center: Vec3,
    pub min_height: f32,
    pub max_height: f32,
    /// Per vertex: east, north and up fractions of the tile, 0..=32767.
    pub u: Vec<u16>,
    pub v: Vec<u16>,
    pub height: Vec<u16>,
    pub indices: Vec<u32>,
    /// Earth-centred unit normals, when the tile has the oct-encoded normals extension.
    pub normals: Option<Vec<[f32; 3]>>,
}

const MAX: f64 = 32767.0;

fn zigzag(v: u16) -> i32 {
    ((v >> 1) as i32) ^ -((v & 1) as i32)
}

pub fn decode(bytes: &[u8]) -> Result<QuantizedMesh, String> {
    let short = || "quantized mesh cut short".to_owned();
    let f64_at = |at: usize| bytes.get(at..at + 8).map(|b| f64::from_le_bytes(b.try_into().unwrap_or([0; 8]))).ok_or_else(short);
    let f32_at = |at: usize| bytes.get(at..at + 4).map(|b| f32::from_le_bytes(b.try_into().unwrap_or([0; 4]))).ok_or_else(short);
    let u32_at = |at: usize| bytes.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap_or([0; 4]))).ok_or_else(short);
    let u16_at = |at: usize| bytes.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).ok_or_else(short);
    let center = [f64_at(0)?, f64_at(8)?, f64_at(16)?];
    let (min_height, max_height) = (f32_at(24)?, f32_at(28)?);
    let mut at = 88;
    let count = u32_at(at)? as usize;
    at += 4;
    if count > 64 << 20 || at + count * 6 > bytes.len() {
        return Err(short());
    }
    let mut channels = [Vec::with_capacity(count), Vec::with_capacity(count), Vec::with_capacity(count)];
    for channel in &mut channels {
        let mut value = 0i32;
        for _ in 0..count {
            value += zigzag(u16_at(at)?);
            channel.push(value.clamp(0, 32767) as u16);
            at += 2;
        }
    }
    let wide = count > 65_536;
    let size = if wide { 4 } else { 2 };
    at = at.div_ceil(size) * size;
    let read = |at: usize| -> Result<u32, String> { if wide { u32_at(at) } else { u16_at(at).map(u32::from) } };
    let triangles = u32_at(at)? as usize;
    at += 4;
    if at + triangles * 3 * size > bytes.len() {
        return Err(short());
    }
    let mut indices = Vec::with_capacity(triangles * 3);
    let mut highest = 0u32;
    for _ in 0..triangles * 3 {
        let code = read(at)?;
        at += size;
        indices.push(highest.wrapping_sub(code));
        if code == 0 {
            highest += 1;
        }
    }
    if indices.iter().any(|&i| i as usize >= count) {
        return Err("quantized mesh: index out of range".into());
    }
    // edge vertex lists (west, south, east, north): skipped
    for _ in 0..4 {
        let n = u32_at(at)? as usize;
        at += 4 + n * size;
    }
    let mut normals = None;
    while at + 5 <= bytes.len() {
        let id = bytes[at];
        let len = u32_at(at + 1)? as usize;
        let data = bytes.get(at + 5..at + 5 + len).ok_or_else(short)?;
        if id == 1 && len >= count * 2 {
            normals = Some(data.chunks_exact(2).take(count).map(|c| crate::formats::oct_decode(c[0] as f64, c[1] as f64, 255.0).map(|v| v as f32)).collect());
        }
        at += 5 + len;
    }
    let [u, v, height] = channels;
    Ok(QuantizedMesh { center, min_height, max_height, u, v, height, indices, normals })
}

impl QuantizedMesh {
    /// The vertices, earth-centred, for a tile covering `rect` (west, south, east, north, in
    /// radians).
    pub fn positions(&self, rect: [f64; 4]) -> Vec<Vec3> {
        let [w, s, e, n] = rect;
        let dh = (self.max_height - self.min_height) as f64;
        (0..self.u.len())
            .map(|i| {
                let lon = w + (e - w) * self.u[i] as f64 / MAX;
                let lat = s + (n - s) * self.v[i] as f64 / MAX;
                geo::to_ecef(lon, lat, self.min_height as f64 + dh * self.height[i] as f64 / MAX)
            })
            .collect()
    }
}

/// The rectangle (west, south, east, north, radians) of tile `x`, `y` at `level` in the
/// geographic tiling scheme (EPSG:4326, two tiles at level 0, rows counted from the south as
/// TMS does).
pub fn geographic_rect(level: u32, x: u64, y: u64) -> [f64; 4] {
    let size = std::f64::consts::PI / (1u64 << level) as f64;
    let w = -std::f64::consts::PI + x as f64 * size;
    let s = -std::f64::consts::FRAC_PI_2 + y as f64 * size;
    [w, s, w + size, s + size]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_zigzag_and_high_water_mark() {
        let mut b = vec![0u8; 88];
        b[24..28].copy_from_slice(&0f32.to_le_bytes());
        b[28..32].copy_from_slice(&100f32.to_le_bytes());
        b.extend_from_slice(&3u32.to_le_bytes());
        let zz = |v: i32| ((v << 1) ^ (v >> 31)) as u16;
        // u: 0, 32767, 0 ; v: 0, 0, 32767 ; h: 0, 32767, 16383 (as deltas)
        for d in [0, 32767, -32767, 0, 0, 32767, 0, 32767, -16384] {
            b.extend_from_slice(&zz(d).to_le_bytes());
        }
        b.extend_from_slice(&1u32.to_le_bytes());
        for code in [0u16, 0, 0] {
            b.extend_from_slice(&code.to_le_bytes());
        }
        b.extend_from_slice(&[0u8; 16]);
        let m = decode(&b).unwrap();
        assert_eq!(m.u, vec![0, 32767, 0]);
        assert_eq!(m.v, vec![0, 0, 32767]);
        assert_eq!(m.height, vec![0, 32767, 16383]);
        assert_eq!(m.indices, vec![0, 1, 2]);
        assert_eq!(geographic_rect(0, 1, 0), [0.0, -std::f64::consts::FRAC_PI_2, std::f64::consts::PI, std::f64::consts::FRAC_PI_2]);
    }
}
