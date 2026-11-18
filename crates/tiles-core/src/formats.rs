//! The tile formats of 3D Tiles 1.0: batched (b3dm), instanced (i3dm), point cloud (pnts) and
//! composite (cmpt) tiles. Each starts with a 4-byte magic, a version and the byte length,
//! followed by a feature table and a batch table (JSON and binary each).

use serde_json::Value;

fn u32_at(b: &[u8], at: usize) -> Result<u32, String> {
    b.get(at..at + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]])).ok_or_else(|| "tile header cut short".to_owned())
}

/// The kind of a tile, by its magic.
pub fn magic(bytes: &[u8]) -> Option<&'static str> {
    match bytes.get(..4)? {
        b"b3dm" => Some("b3dm"),
        b"i3dm" => Some("i3dm"),
        b"pnts" => Some("pnts"),
        b"cmpt" => Some("cmpt"),
        b"glTF" => Some("glb"),
        _ => None,
    }
}

/// A feature table: its JSON and its binary body.
#[derive(Debug, Default, Clone)]
pub struct FeatureTable {
    pub json: Value,
    pub binary: Vec<u8>,
}

impl FeatureTable {
    fn parse(json: &[u8], binary: &[u8]) -> Result<FeatureTable, String> {
        let text = std::str::from_utf8(json).map_err(|e| format!("feature table: {e}"))?.trim_end_matches(['\0', ' ']);
        let json = if text.trim().is_empty() { Value::Object(Default::default()) } else { serde_json::from_str(text).map_err(|e| format!("feature table: {e}"))? };
        Ok(FeatureTable { json, binary: binary.to_vec() })
    }

    /// A global number (e.g. `POINTS_LENGTH`).
    pub fn number(&self, key: &str) -> Option<f64> {
        match &self.json[key] {
            Value::Number(n) => n.as_f64(),
            Value::Array(a) => a.first()?.as_f64(),
            v @ Value::Object(_) => self.values(v, 1, 1, "UNSIGNED_INT")?.first().copied(),
            _ => None,
        }
    }

    /// A global vector of `n` numbers (e.g. `RTC_CENTER`).
    pub fn vector(&self, key: &str, n: usize) -> Option<Vec<f64>> {
        match &self.json[key] {
            Value::Array(a) if a.len() >= n => a.iter().take(n).map(Value::as_f64).collect(),
            v @ Value::Object(_) => self.values(v, 1, n, "FLOAT"),
            _ => None,
        }
    }

    /// A per-feature property: `count` features of `n` components, from the binary body
    /// (`{"byteOffset": .., "componentType": ..}`) or written out in the JSON.
    pub fn property(&self, key: &str, count: usize, n: usize, default_type: &str) -> Option<Vec<f64>> {
        match &self.json[key] {
            Value::Array(a) => a.iter().map(Value::as_f64).collect(),
            v @ Value::Object(_) => self.values(v, count, n, default_type),
            _ => None,
        }
    }

    fn values(&self, v: &Value, count: usize, n: usize, default_type: &str) -> Option<Vec<f64>> {
        let offset = v["byteOffset"].as_u64()? as usize;
        let kind = v["componentType"].as_str().unwrap_or(default_type);
        read_components(self.binary.get(offset..)?, kind, count * n)
    }
}

/// Little-endian components of a named type (`FLOAT`, `UNSIGNED_SHORT`, ...).
pub fn read_components(bytes: &[u8], kind: &str, count: usize) -> Option<Vec<f64>> {
    let size = match kind {
        "BYTE" | "UNSIGNED_BYTE" => 1,
        "SHORT" | "UNSIGNED_SHORT" => 2,
        "INT" | "UNSIGNED_INT" | "FLOAT" => 4,
        "DOUBLE" => 8,
        _ => return None,
    };
    let bytes = bytes.get(..count * size)?;
    Some(
        bytes
            .chunks_exact(size)
            .map(|c| match kind {
                "BYTE" => c[0] as i8 as f64,
                "UNSIGNED_BYTE" => c[0] as f64,
                "SHORT" => i16::from_le_bytes([c[0], c[1]]) as f64,
                "UNSIGNED_SHORT" => u16::from_le_bytes([c[0], c[1]]) as f64,
                "INT" => i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f64,
                "UNSIGNED_INT" => u32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f64,
                "FLOAT" => f32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f64,
                _ => f64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]),
            })
            .collect(),
    )
}

/// A tile split into its tables and body.
#[derive(Debug, Clone)]
pub struct Tile {
    pub kind: &'static str,
    pub feature_table: FeatureTable,
    pub batch_table: Value,
    /// b3dm and embedded i3dm: the glTF binary; external i3dm: the URI of the glTF (as bytes).
    pub body: Vec<u8>,
    /// i3dm: whether `body` is a URI (`gltfFormat` 0) rather than an embedded glTF.
    pub body_is_uri: bool,
}

/// Splits a b3dm, i3dm or pnts tile into its tables and body.
pub fn parse(bytes: &[u8]) -> Result<Tile, String> {
    let kind = magic(bytes).ok_or("not a 3D Tiles tile")?;
    let length = (u32_at(bytes, 8)? as usize).min(bytes.len());
    let (header, ft_json, ft_bin, bt_json, bt_bin, gltf_format) = match kind {
        "b3dm" => {
            let (a, b, c, d) = (u32_at(bytes, 12)?, u32_at(bytes, 16)?, u32_at(bytes, 20)?, u32_at(bytes, 24)?);
            // Two older headers: [batchLength, batchTableByteLength] and
            // [batchTableJsonByteLength, batchTableBinaryByteLength, batchLength]; their lengths
            // land on the bytes of the glTF magic, which is what gives them away.
            if c >= 0x2200_0000 {
                (20, 0, 0, b, 0, 1)
            } else if d >= 0x2200_0000 {
                (24, 0, 0, a, b, 1)
            } else {
                (28, a, b, c, d, 1)
            }
        }
        "i3dm" => (32, u32_at(bytes, 12)?, u32_at(bytes, 16)?, u32_at(bytes, 20)?, u32_at(bytes, 24)?, u32_at(bytes, 28)?),
        "pnts" => (28, u32_at(bytes, 12)?, u32_at(bytes, 16)?, u32_at(bytes, 20)?, u32_at(bytes, 24)?, 1),
        _ => return Err(format!("{kind}: not a single tile")),
    };
    let (ft_json, ft_bin, bt_json, bt_bin) = (ft_json as usize, ft_bin as usize, bt_json as usize, bt_bin as usize);
    let at = |from: usize, len: usize| bytes.get(from..from + len).ok_or_else(|| format!("{kind}: tables longer than the tile"));
    let mut p = header;
    let feature_table = FeatureTable::parse(at(p, ft_json)?, at(p + ft_json, ft_bin)?)?;
    p += ft_json + ft_bin;
    let bt_text = at(p, bt_json)?;
    let batch_table = std::str::from_utf8(bt_text).ok().map(|t| t.trim_end_matches(['\0', ' '])).filter(|t| !t.trim().is_empty()).and_then(|t| serde_json::from_str(t).ok()).unwrap_or(Value::Null);
    p += bt_json + bt_bin;
    let body = bytes.get(p..length).unwrap_or(&[]).to_vec();
    let body_is_uri = kind == "i3dm" && gltf_format == 0;
    let body = if body_is_uri { body.into_iter().take_while(|&b| b != 0).collect::<Vec<u8>>() } else { body };
    Ok(Tile { kind, feature_table, batch_table, body, body_is_uri })
}

/// The inner tiles of a composite tile (which may themselves be composite).
pub fn composite(bytes: &[u8]) -> Result<Vec<&[u8]>, String> {
    if magic(bytes) != Some("cmpt") {
        return Err("not a composite tile".into());
    }
    let count = u32_at(bytes, 12)? as usize;
    let mut out = Vec::with_capacity(count);
    let mut at = 16;
    for _ in 0..count {
        let length = u32_at(bytes, at + 8)? as usize;
        out.push(bytes.get(at..at + length).ok_or("cmpt: inner tile cut short")?);
        at += length;
    }
    Ok(out)
}

/// Decodes an oct-encoded unit vector (two components in `0..=range`).
pub fn oct_decode(x: f64, y: f64, range: f64) -> [f64; 3] {
    let (mut x, mut y) = (x / range * 2.0 - 1.0, y / range * 2.0 - 1.0);
    let z = 1.0 - x.abs() - y.abs();
    if z < 0.0 {
        let (ox, oy) = (x, y);
        x = (1.0 - oy.abs()) * if ox >= 0.0 { 1.0 } else { -1.0 };
        y = (1.0 - ox.abs()) * if oy >= 0.0 { 1.0 } else { -1.0 };
    }
    crate::math::normalize([x, y, z])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tile(magic: &[u8; 4], ft: &str, body: &[u8]) -> Vec<u8> {
        let mut ft = ft.as_bytes().to_vec();
        while (28 + ft.len()) % 8 != 0 {
            ft.push(b' ');
        }
        let mut out = magic.to_vec();
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&((28 + ft.len() + body.len()) as u32).to_le_bytes());
        for v in [ft.len() as u32, 0, 0, 0] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&ft);
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn splits_a_b3dm_and_a_composite() {
        let b3dm = tile(b"b3dm", r#"{"BATCH_LENGTH":0,"RTC_CENTER":[1,2,3]}"#, b"glTF....");
        let t = parse(&b3dm).unwrap();
        assert_eq!(t.kind, "b3dm");
        assert_eq!(t.feature_table.vector("RTC_CENTER", 3), Some(vec![1.0, 2.0, 3.0]));
        assert_eq!(t.body, b"glTF....");

        let mut cmpt = b"cmpt".to_vec();
        cmpt.extend_from_slice(&1u32.to_le_bytes());
        cmpt.extend_from_slice(&((16 + 2 * b3dm.len()) as u32).to_le_bytes());
        cmpt.extend_from_slice(&2u32.to_le_bytes());
        cmpt.extend_from_slice(&b3dm);
        cmpt.extend_from_slice(&b3dm);
        assert_eq!(composite(&cmpt).unwrap().len(), 2);
    }

    #[test]
    fn oct_decoding() {
        let up = oct_decode(127.5, 127.5, 255.0);
        assert!((up[2] - 1.0).abs() < 1e-9);
        let x = oct_decode(255.0, 127.5, 255.0);
        assert!((x[0] - 1.0).abs() < 1e-9);
    }
}
