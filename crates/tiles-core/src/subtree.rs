//! Implicit tiling (3D Tiles 1.1): the subtree files that say which tiles, contents and child
//! subtrees exist, as bitstreams in Morton order.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum Availability {
    Constant(bool),
    Bits(Vec<u8>),
}

impl Availability {
    pub fn get(&self, index: u64) -> bool {
        match self {
            Availability::Constant(c) => *c,
            Availability::Bits(b) => b.get((index / 8) as usize).is_some_and(|byte| byte >> (index % 8) & 1 == 1),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Subtree {
    pub tiles: Availability,
    /// One per content of the tile (most tilesets have one).
    pub contents: Vec<Availability>,
    pub child_subtrees: Availability,
}

/// Reads a subtree file (binary `subt` or JSON); `resolve` reads external buffers by URI.
pub fn parse(bytes: &[u8], resolve: &mut dyn FnMut(&str) -> Result<Vec<u8>, String>) -> Result<Subtree, String> {
    let (json, binary): (Value, &[u8]) = if bytes.starts_with(b"subt") {
        let u64_at = |at: usize| bytes.get(at..at + 8).map(|b| u64::from_le_bytes(b.try_into().unwrap_or([0; 8])) as usize).ok_or("subtree header cut short");
        let (json_len, bin_len) = (u64_at(8)?, u64_at(16)?);
        let json = bytes.get(24..24 + json_len).ok_or("subtree JSON cut short")?;
        let text = std::str::from_utf8(json).map_err(|e| format!("subtree: {e}"))?.trim_end_matches(['\0', ' ']);
        (serde_json::from_str(text).map_err(|e| format!("subtree: {e}"))?, bytes.get(24 + json_len..24 + json_len + bin_len).unwrap_or(&[]))
    } else {
        (serde_json::from_slice(bytes).map_err(|e| format!("subtree: {e}"))?, &[])
    };
    let mut buffers: Vec<Vec<u8>> = Vec::new();
    for b in json["buffers"].as_array().into_iter().flatten() {
        buffers.push(match b["uri"].as_str() {
            Some(uri) => resolve(uri)?,
            None => binary.to_vec(),
        });
    }
    let view = |i: u64| -> Option<Vec<u8>> {
        let v = &json["bufferViews"][i as usize];
        let buffer = buffers.get(v["buffer"].as_u64()? as usize)?;
        let at = v["byteOffset"].as_u64().unwrap_or(0) as usize;
        buffer.get(at..at + v["byteLength"].as_u64()? as usize).map(<[u8]>::to_vec)
    };
    let availability = |a: &Value| -> Availability {
        if let Some(c) = a["constant"].as_u64() {
            return Availability::Constant(c == 1);
        }
        // the draft name of the key was "bufferView"
        match a["bitstream"].as_u64().or(a["bufferView"].as_u64()).and_then(view) {
            Some(bits) => Availability::Bits(bits),
            None => Availability::Constant(false),
        }
    };
    let contents = match &json["contentAvailability"] {
        Value::Array(list) => list.iter().map(availability).collect(),
        v @ Value::Object(_) => vec![availability(v)],
        _ => Vec::new(),
    };
    Ok(Subtree { tiles: availability(&json["tileAvailability"]), contents, child_subtrees: availability(&json["childSubtreeAvailability"]) })
}

/// The Morton index of a tile's coordinates within its level (x in the lowest bit).
pub fn morton(x: u64, y: u64, z: u64, octree: bool) -> u64 {
    let mut out = 0u64;
    for bit in 0..21 {
        if octree {
            out |= ((x >> bit) & 1) << (3 * bit) | ((y >> bit) & 1) << (3 * bit + 1) | ((z >> bit) & 1) << (3 * bit + 2);
        } else {
            out |= ((x >> bit) & 1) << (2 * bit) | ((y >> bit) & 1) << (2 * bit + 1);
        }
    }
    out
}

/// Where a level starts in a subtree's tile bitstream: the tiles of the levels above it.
pub fn level_offset(level: u32, octree: bool) -> u64 {
    let n: u64 = if octree { 8 } else { 4 };
    (n.pow(level) - 1) / (n - 1)
}

/// Fills a template URI (`{level}`, `{x}`, `{y}`, `{z}`).
pub fn fill(template: &str, level: u32, x: u64, y: u64, z: u64) -> String {
    template.replace("{level}", &level.to_string()).replace("{x}", &x.to_string()).replace("{y}", &y.to_string()).replace("{z}", &z.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn morton_and_offsets() {
        assert_eq!(morton(1, 0, 0, false), 1);
        assert_eq!(morton(0, 1, 0, false), 2);
        assert_eq!(morton(3, 3, 0, false), 15);
        assert_eq!(morton(1, 1, 1, true), 7);
        assert_eq!(level_offset(0, false), 0);
        assert_eq!(level_offset(2, false), 5);
        assert_eq!(level_offset(2, true), 9);
        assert_eq!(fill("c/{level}/{x}_{y}.glb", 3, 2, 1, 0), "c/3/2_1.glb");
    }

    #[test]
    fn reads_a_json_subtree_with_constant_availability() {
        let s = parse(br#"{"tileAvailability":{"constant":1},"contentAvailability":[{"constant":0}],"childSubtreeAvailability":{"constant":0}}"#, &mut |_| Err("none".into())).unwrap();
        assert!(s.tiles.get(5));
        assert!(!s.contents[0].get(0));
    }
}
