//! Container layer of the 3D Tiles reader: the tile formats (b3dm, i3dm, pnts, cmpt), implicit
//! tiling's subtree files, quantized-mesh terrain, bounding volumes and the earth's geometry.

pub mod formats;
pub mod geo;
pub mod math;
pub mod quantized_mesh;
pub mod subtree;
pub mod volume;

/// Undoes gzip when the bytes are gzip (tiles are often stored compressed).
pub fn gunzip(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    if !bytes.starts_with(&[0x1F, 0x8B]) {
        return Ok(bytes);
    }
    use std::io::Read;
    let mut out = Vec::new();
    flate2::read::MultiGzDecoder::new(&bytes[..]).read_to_end(&mut out).map_err(|e| format!("gzip: {e}"))?;
    Ok(out)
}
