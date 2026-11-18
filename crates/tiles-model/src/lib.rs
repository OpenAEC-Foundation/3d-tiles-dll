//! The neutral model the 3D Tiles reader returns: plain types that know nothing of any app.
//!
//! Coordinates are metres in a local frame, Z up. For a tileset placed on the earth (its tiles in
//! earth-centred, earth-fixed coordinates, EPSG:4978) the local frame is east-north-up at the
//! tileset's centre, so the numbers stay small; [`Frame`] says where that centre lies.

/// A tileset, read at one level of detail.
#[derive(Debug, Clone, Default)]
pub struct Tileset {
    /// The content of the tiles chosen, in the order they were read.
    pub contents: Vec<Content>,
    /// Where the local frame lies on the earth.
    pub frame: Frame,
    /// What the tileset says of itself (version, generator, copyright) and of the reading.
    pub info: Vec<(String, String)>,
    /// How many tiles the tileset has that were looked at, and how many of them were read.
    pub tiles_visited: usize,
    pub tiles_read: usize,
    /// Content that could not be read: its URI and why.
    pub failed: Vec<(String, String)>,
}

/// The place of the local frame.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Frame {
    /// The origin of the local frame in earth-centred, earth-fixed coordinates (metres, WGS 84);
    /// `None` when the tileset is not placed on the earth (its coordinates are kept as they are).
    pub ecef: Option<[f64; 3]>,
    /// The same origin as latitude and longitude (degrees) and height above the ellipsoid (m).
    pub geodetic: Option<[f64; 3]>,
}

/// The content of one tile.
#[derive(Debug, Clone)]
pub struct Content {
    /// The URI of the content, as the tileset names it.
    pub uri: String,
    /// The tile's geometric error (metres): how coarse this content is.
    pub geometric_error: f64,
    pub kind: Kind,
}

#[derive(Debug, Clone)]
pub enum Kind {
    /// A plain glTF binary (no compressed extensions, every buffer and image embedded), Y up as
    /// glTF is. Each placement is a column-major 4x4 matrix from the glTF's Z-up frame (after the
    /// standard turn of Y up to Z up: x, y, z to x, -z, y) into the local frame; an instanced tile
    /// has one placement per instance.
    Gltf { glb: Vec<u8>, placements: Vec<[f64; 16]> },
    /// A point cloud in the local frame, with a colour per point when the tile has them.
    Points { positions: Vec<[f32; 3]>, colors: Option<Vec<[u8; 4]>> },
    /// A triangle mesh in the local frame (terrain).
    Mesh { positions: Vec<[f32; 3]>, normals: Option<Vec<[f32; 3]>>, indices: Vec<u32> },
}
