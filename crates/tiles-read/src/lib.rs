//! Reader of 3D Tiles (1.0 and 1.1) and quantized-mesh terrain into the neutral model of
//! `tiles-model`.
//!
//! A tileset is a tree of tiles, each coarser than its children, with a geometric error (metres)
//! that says how coarse. The reader walks the tree from the root and goes down where a tile
//! would look too coarse: its geometric error, seen from a viewpoint that frames the whole
//! tileset in [`Options::view_pixels`], is more than [`Options::max_screen_error`] pixels. That
//! is the level of detail that fits the first view of the whole tileset; within the limits of
//! [`Options::max_bytes`] and [`Options::max_tiles`].
//!
//! Tiles on the earth (earth-centred coordinates or regions) are moved into an east-north-up
//! frame at the tileset's centre, so the numbers stay small; `Tileset::frame` says where that is.

pub mod files;

use files::{dir_of, join, Files};
use serde_json::Value;
use tiles_core::formats::{self, FeatureTable};
use tiles_core::math::{self, Mat4, Vec3};
use tiles_core::quantized_mesh;
use tiles_core::subtree::{self, Subtree};
use tiles_core::volume::Volume;
use tiles_core::{geo, gunzip};
use tiles_model::{Content, Frame, Kind, Tileset};

#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// The largest error a tile may show, in pixels.
    pub max_screen_error: f64,
    /// The height of the view the whole tileset is framed in, in pixels.
    pub view_pixels: f64,
    /// Stop going into finer tiles once this many bytes of content are read.
    pub max_bytes: usize,
    /// Stop going into finer tiles once this many tiles are read.
    pub max_tiles: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options { max_screen_error: 16.0, view_pixels: 1080.0, max_bytes: 400 << 20, max_tiles: 4000 }
    }
}

/// The tangent of half the vertical field of view of the viewpoint (60 degrees).
const TAN_HALF_FOV: f64 = 0.577_350_269_189_625_8;

/// Reads the tileset (or single tile, or terrain) at `entry` among `files`.
pub fn read(files: &dyn Files, entry: &str, options: &Options) -> Result<Tileset, String> {
    let lower = entry.to_ascii_lowercase();
    if lower.ends_with(".terrain") || lower.ends_with("layer.json") {
        return terrain(files, entry);
    }
    let mut reader = Reader { files, options: *options, local: math::IDENTITY, distance: 1.0, out: Tileset::default(), bytes: 0 };
    if lower.ends_with(".json") {
        reader.tileset(entry, &math::IDENTITY, true)?;
    } else {
        // a single tile: content of a tileset of its own
        reader.out.tiles_visited = 1;
        reader.content(entry, &math::IDENTITY, 0.0)?;
        reader.out.tiles_read = 1;
        reader.place_single();
    }
    if reader.out.contents.is_empty() {
        let why = reader.out.failed.first().map(|(u, e)| format!(": {u}: {e}")).unwrap_or_default();
        return Err(format!("3D Tiles: no tile could be read{why}"));
    }
    Ok(reader.out)
}

/// Finds the tileset in a list of files: the `tileset.json` nearest the root, else any `.json`
/// with "tileset" in its name, else a terrain `layer.json`.
pub fn find_entry(list: &[String]) -> Option<String> {
    let depth = |p: &&String| p.matches('/').count();
    let lower = |p: &String| p.to_ascii_lowercase();
    list.iter().filter(|p| lower(p).ends_with("tileset.json")).min_by_key(depth).or_else(|| list.iter().filter(|p| lower(p).ends_with(".json") && lower(p).contains("tileset")).min_by_key(depth)).or_else(|| list.iter().filter(|p| lower(p).ends_with("layer.json")).min_by_key(depth)).cloned()
}

/// Opens a tileset on disk: a `tileset.json` (or another tileset JSON), a folder or a zip
/// holding one, a terrain `layer.json` or `.terrain` tile, or a single tile file.
pub fn open_path(path: &std::path::Path, options: &Options) -> Result<Tileset, String> {
    if path.is_dir() {
        let dir = files::Dir(path.to_path_buf());
        let entry = find_entry(&dir.list()).ok_or("3D Tiles: the folder holds no tileset.json")?;
        return read(&dir, &entry, options);
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if name.to_ascii_lowercase().ends_with(".zip") {
        return open_bytes(&name, std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?, options);
    }
    let parent = path.parent().unwrap_or(std::path::Path::new("."));
    if name.to_ascii_lowercase().ends_with(".terrain") {
        // the tile's level and column are the folders above it: <level>/<x>/<y>.terrain
        let mut up = parent.to_path_buf();
        let x = up.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        up.pop();
        let level = up.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        up.pop();
        if !up.as_os_str().is_empty() && x.parse::<u64>().is_ok() && level.parse::<u32>().is_ok() {
            return read(&files::Dir(up), &format!("{level}/{x}/{name}"), options);
        }
    }
    read(&files::Dir(parent.to_path_buf()), &name, options)
}

/// Opens a tileset given as bytes: a zip holding one, or a single file (a tileset JSON then
/// only reads content embedded as data URIs).
pub fn open_bytes(name: &str, bytes: Vec<u8>, options: &Options) -> Result<Tileset, String> {
    if bytes.starts_with(b"PK") {
        let zip = files::Zip::new(bytes)?;
        let entry = find_entry(&zip.list()).ok_or("3D Tiles: the zip holds no tileset.json")?;
        return read(&zip, &entry, options);
    }
    let mut memory = files::Memory::default();
    memory.0.insert(name.to_owned(), bytes);
    read(&memory, name, options)
}

#[derive(Clone, Copy, PartialEq)]
enum Refine {
    Add,
    Replace,
}

struct Reader<'a> {
    files: &'a dyn Files,
    options: Options,
    /// From world (tileset) coordinates to the local frame.
    local: Mat4,
    /// The distance of the viewpoint, for the screen error.
    distance: f64,
    out: Tileset,
    bytes: usize,
}

struct Implicit {
    octree: bool,
    subtree_levels: u32,
    available_levels: u32,
    subtrees: String,
    contents: Vec<String>,
    error: f64,
    refine: Refine,
    base: String,
}

impl Reader<'_> {
    fn read_file(&self, path: &str) -> Result<Vec<u8>, String> {
        if let Some(data) = tiles_gltf::data_uri(path) {
            return data;
        }
        gunzip(self.files.read(path)?)
    }

    /// Reads a tileset JSON; `root` when it is the tileset opened (it fixes the frame).
    fn tileset(&mut self, path: &str, parent: &Mat4, root: bool) -> Result<(), String> {
        let bytes = self.read_file(path)?;
        let json: Value = serde_json::from_slice(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes)).map_err(|e| format!("{path}: {e}"))?;
        if !json["root"].is_object() {
            return Err(format!("{path}: not a tileset (no root tile)"));
        }
        if root {
            let asset = &json["asset"];
            for (key, label) in [("version", "3D Tiles version"), ("tilesetVersion", "Tileset version"), ("generator", "Generator")] {
                if let Some(v) = asset[key].as_str() {
                    self.out.info.push((label.into(), v.into()));
                }
            }
            if let Some(c) = asset["copyright"].as_str().or(asset["extras"]["copyright"].as_str()) {
                self.out.info.push(("Copyright".into(), c.into()));
            }
            self.fix_frame(&json["root"]);
        }
        let base = dir_of(path).to_owned();
        let refine = refine_of(&json["root"]).unwrap_or(Refine::Replace);
        self.tile(&json["root"], &base, parent, refine);
        Ok(())
    }

    /// Sets the local frame and the viewpoint from the root tile.
    fn fix_frame(&mut self, root: &Value) {
        let world = transform_of(root, &math::IDENTITY);
        let Some(volume) = Volume::parse(&root["boundingVolume"]) else { return };
        let (center, radius) = volume.sphere(&world);
        // the viewpoint that frames the bounding sphere in the 60-degree view: radius / sin(30 degrees)
        self.distance = (2.0 * radius).max(1e-6);
        if matches!(volume, Volume::Region(_)) || geo::is_earth_centred(center) {
            let frame = geo::enu_frame(center);
            self.local = math::rigid_inverse(&frame);
            let (lon, lat, h) = geo::to_geodetic(center);
            self.out.frame = Frame { ecef: Some(center), geodetic: Some([lat.to_degrees(), lon.to_degrees(), h]) };
        }
    }

    /// For a single tile: the frame from the tile's own content when it is earth-centred.
    fn place_single(&mut self) {
        let mut sum = [0.0; 3];
        let mut n = 0.0;
        for c in &self.out.contents {
            match &c.kind {
                Kind::Gltf { placements, .. } => placements.iter().for_each(|p| {
                    sum = math::add(sum, [p[12], p[13], p[14]]);
                    n += 1.0;
                }),
                Kind::Points { positions, .. } => {
                    if let Some(p) = positions.first() {
                        sum = math::add(sum, p.map(f64::from));
                        n += 1.0;
                    }
                }
                Kind::Mesh { .. } => {}
            }
        }
        if n == 0.0 {
            return;
        }
        let center = math::scale(sum, 1.0 / n);
        if !geo::is_earth_centred(center) {
            return;
        }
        let local = math::rigid_inverse(&geo::enu_frame(center));
        for c in &mut self.out.contents {
            match &mut c.kind {
                Kind::Gltf { placements, .. } => placements.iter_mut().for_each(|p| *p = math::mul(&local, p)),
                Kind::Points { positions, .. } => positions.iter_mut().for_each(|p| *p = math::point(&local, p.map(f64::from)).map(|v| v as f32)),
                Kind::Mesh { .. } => {}
            }
        }
        let (lon, lat, h) = geo::to_geodetic(center);
        self.out.frame = Frame { ecef: Some(center), geodetic: Some([lat.to_degrees(), lon.to_degrees(), h]) };
    }

    fn screen_error(&self, error: f64) -> f64 {
        error * self.options.view_pixels / (2.0 * self.distance * TAN_HALF_FOV)
    }

    fn over_budget(&self) -> bool {
        self.bytes >= self.options.max_bytes || self.out.tiles_read >= self.options.max_tiles
    }

    /// Reads a tile and the finer tiles it needs; whether anything was read.
    fn tile(&mut self, tile: &Value, base: &str, parent: &Mat4, inherited: Refine) -> bool {
        self.out.tiles_visited += 1;
        let world = transform_of(tile, parent);
        let refine = refine_of(tile).unwrap_or(inherited);
        let error = tile["geometricError"].as_f64().unwrap_or(0.0);
        if tile["implicitTiling"].is_object() {
            return self.implicit_root(tile, base, &world, refine, error);
        }
        let uris = content_uris(tile);
        let children = tile["children"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        let refine_here = !children.is_empty() && self.screen_error(error) > self.options.max_screen_error && !self.over_budget();
        let mut read_any = false;
        if !refine_here || refine == Refine::Add || uris.iter().any(|u| u.ends_with(".json")) {
            read_any |= self.contents(&uris, base, &world, error);
        }
        if refine_here {
            let mut children_read = false;
            for child in children {
                children_read |= self.tile(child, base, &world, refine);
            }
            if !children_read && refine == Refine::Replace && !read_any {
                read_any |= self.contents(&uris, base, &world, error);
            }
            read_any |= children_read;
        }
        read_any
    }

    fn contents(&mut self, uris: &[String], base: &str, world: &Mat4, error: f64) -> bool {
        let mut any = false;
        for uri in uris {
            let path = if uri.starts_with("data:") { Ok(uri.clone()) } else { join(base, uri) };
            match path.and_then(|path| self.content(&path, world, error)) {
                Ok(()) => any = true,
                Err(e) => self.out.failed.push((uri.clone(), e)),
            }
        }
        if any {
            self.out.tiles_read += 1;
        }
        any
    }

    /// Reads one content file (or an external tileset) into the model.
    fn content(&mut self, path: &str, world: &Mat4, error: f64) -> Result<(), String> {
        if path.to_ascii_lowercase().ends_with(".json") {
            return self.tileset(path, world, false);
        }
        let bytes = self.read_file(path)?;
        self.bytes += bytes.len();
        self.tile_bytes(path, &bytes, world, error)
    }

    fn tile_bytes(&mut self, path: &str, bytes: &[u8], world: &Mat4, error: f64) -> Result<(), String> {
        let name = if path.starts_with("data:") { "(embedded)".to_owned() } else { path.to_owned() };
        let push = |out: &mut Tileset, kind: Kind| out.contents.push(Content { uri: name.clone(), geometric_error: error, kind });
        match formats::magic(bytes) {
            Some("cmpt") => {
                for inner in formats::composite(bytes)? {
                    self.tile_bytes(path, inner, world, error)?;
                }
                Ok(())
            }
            Some("b3dm") => {
                let tile = formats::parse(bytes)?;
                let rtc = tile.feature_table.vector("RTC_CENTER", 3).map_or([0.0; 3], |v| [v[0], v[1], v[2]]);
                let (glb, cesium_rtc) = self.plain_gltf(path, &tile.body)?;
                let placement = math::mul(&math::mul(&self.local, world), &math::translation(math::add(rtc, cesium_rtc)));
                push(&mut self.out, Kind::Gltf { glb, placements: vec![placement] });
                Ok(())
            }
            Some("i3dm") => {
                let tile = formats::parse(bytes)?;
                let body = if tile.body_is_uri {
                    let uri = String::from_utf8_lossy(&tile.body).into_owned();
                    self.read_file(&join(dir_of(path), &uri)?)?
                } else {
                    tile.body.clone()
                };
                let (glb, cesium_rtc) = self.plain_gltf(path, &body)?;
                let placements = instances(&tile.feature_table, world)?
                    .into_iter()
                    .map(|instance| math::mul(&math::mul(&math::mul(&self.local, world), &instance), &math::translation(cesium_rtc)))
                    .collect();
                push(&mut self.out, Kind::Gltf { glb, placements });
                Ok(())
            }
            Some("pnts") => {
                let tile = formats::parse(bytes)?;
                let (positions, colors) = points(&tile.feature_table)?;
                let to_local = math::mul(&self.local, world);
                let positions = positions.into_iter().map(|p| math::point(&to_local, p).map(|v| v as f32)).collect();
                push(&mut self.out, Kind::Points { positions, colors });
                Ok(())
            }
            Some("glb") | None => {
                if !bytes.starts_with(b"glTF") && !bytes.trim_ascii_start().starts_with(b"{") {
                    return Err("not a tile format this reader knows".into());
                }
                let (glb, cesium_rtc) = self.plain_gltf(path, bytes)?;
                let placement = math::mul(&math::mul(&self.local, world), &math::translation(cesium_rtc));
                push(&mut self.out, Kind::Gltf { glb, placements: vec![placement] });
                Ok(())
            }
            Some(other) => Err(format!("{other}: not read")),
        }
    }

    /// A glTF made plain and self-contained, and its `CESIUM_RTC` centre (Z up).
    fn plain_gltf(&self, path: &str, bytes: &[u8]) -> Result<(Vec<u8>, Vec3), String> {
        let (doc, bin) = tiles_gltf::split(bytes)?;
        if doc["asset"]["version"].as_str().is_some_and(|v| v.starts_with('1')) {
            return Err("glTF 1.0 content is not read".into());
        }
        let rtc = doc["extensions"]["CESIUM_RTC"]["center"].as_array().and_then(|c| Some([c.first()?.as_f64()?, c.get(1)?.as_f64()?, c.get(2)?.as_f64()?])).unwrap_or([0.0; 3]);
        let dir = dir_of(path).to_owned();
        let files = self.files;
        let glb = tiles_gltf::unpack_document(doc, bin, &mut |uri| {
            let p = join(&dir, uri)?;
            gunzip(files.read(&p)?)
        })?;
        Ok((glb, rtc))
    }

    fn implicit_root(&mut self, tile: &Value, base: &str, world: &Mat4, refine: Refine, error: f64) -> bool {
        let it = &tile["implicitTiling"];
        let subtree_levels = it["subtreeLevels"].as_u64().unwrap_or(1).max(1) as u32;
        let available_levels = it["availableLevels"].as_u64().or_else(|| it["maximumLevel"].as_u64().map(|m| m + 1)).unwrap_or(subtree_levels as u64) as u32;
        let implicit = Implicit {
            octree: it["subdivisionScheme"].as_str() == Some("OCTREE"),
            subtree_levels,
            available_levels,
            subtrees: it["subtrees"]["uri"].as_str().or(it["subtrees"]["url"].as_str()).unwrap_or("").to_owned(),
            contents: content_uris(tile),
            error,
            refine,
            base: base.to_owned(),
        };
        self.subtree(&implicit, world, 0, 0, 0, 0)
    }

    /// Reads the subtree rooted at the global tile (`level`, `x`, `y`, `z`) and its tiles.
    fn subtree(&mut self, it: &Implicit, world: &Mat4, level: u32, x: u64, y: u64, z: u64) -> bool {
        let path = match join(&it.base, &subtree::fill(&it.subtrees, level, x, y, z)) {
            Ok(p) => p,
            Err(e) => {
                self.out.failed.push((it.subtrees.clone(), e));
                return false;
            }
        };
        let dir = dir_of(&path).to_owned();
        let files = self.files;
        let parsed = self.read_file(&path).and_then(|bytes| subtree::parse(&bytes, &mut |uri| files.read(&join(&dir, uri)?)));
        match parsed {
            Ok(sub) => self.implicit_tile(it, &sub, world, (level, x, y, z), 0, (0, 0, 0)),
            Err(e) => {
                self.out.failed.push((path, e));
                false
            }
        }
    }

    /// A tile of a subtree: `root` is the subtree's root (global), `l` and `(lx, ly, lz)` the
    /// tile within the subtree.
    fn implicit_tile(&mut self, it: &Implicit, sub: &Subtree, world: &Mat4, root: (u32, u64, u64, u64), l: u32, (lx, ly, lz): (u64, u64, u64)) -> bool {
        let index = subtree::level_offset(l, it.octree) + subtree::morton(lx, ly, lz, it.octree);
        if !sub.tiles.get(index) {
            return false;
        }
        self.out.tiles_visited += 1;
        let (r0, rx, ry, rz) = root;
        let (level, gx, gy, gz) = (r0 + l, (rx << l) + lx, (ry << l) + ly, (rz << l) + lz);
        let error = it.error / (1u64 << level) as f64;
        let uris: Vec<String> = it.contents.iter().enumerate().filter(|(k, _)| sub.contents.get(*k).is_some_and(|a| a.get(index))).map(|(_, t)| subtree::fill(t, level, gx, gy, gz)).collect();
        let deeper = level + 1 < it.available_levels;
        let refine_here = deeper && self.screen_error(error) > self.options.max_screen_error && !self.over_budget();
        let mut read_any = false;
        if !refine_here || it.refine == Refine::Add {
            read_any |= self.contents(&uris, &it.base, world, error);
        }
        if refine_here {
            let mut children_read = false;
            let zs: &[u64] = if it.octree { &[0, 1] } else { &[0] };
            for &cz in zs {
                for cy in 0..2 {
                    for cx in 0..2 {
                        let (cx2, cy2, cz2) = (lx * 2 + cx, ly * 2 + cy, if it.octree { lz * 2 + cz } else { 0 });
                        if l + 1 < it.subtree_levels {
                            children_read |= self.implicit_tile(it, sub, world, root, l + 1, (cx2, cy2, cz2));
                        } else if sub.child_subtrees.get(subtree::morton(cx2, cy2, cz2, it.octree)) {
                            let (cgx, cgy, cgz) = (gx * 2 + cx, gy * 2 + cy, if it.octree { gz * 2 + cz } else { 0 });
                            children_read |= self.subtree(it, world, level + 1, cgx, cgy, cgz);
                        }
                    }
                }
            }
            if !children_read && it.refine == Refine::Replace && !read_any {
                read_any |= self.contents(&uris, &it.base, world, error);
            }
            read_any |= children_read;
        }
        read_any
    }
}

fn refine_of(tile: &Value) -> Option<Refine> {
    match tile["refine"].as_str()?.to_ascii_uppercase().as_str() {
        "ADD" => Some(Refine::Add),
        "REPLACE" => Some(Refine::Replace),
        _ => None,
    }
}

fn transform_of(tile: &Value, parent: &Mat4) -> Mat4 {
    match tile["transform"].as_array().and_then(|t| math::from_slice(t)) {
        Some(t) => math::mul(parent, &t),
        None => *parent,
    }
}

/// The content URIs of a tile: `content` (with the 1.0 draft's `url`) and 1.1's `contents`.
fn content_uris(tile: &Value) -> Vec<String> {
    let one = |c: &Value| c["uri"].as_str().or(c["url"].as_str()).map(str::to_owned);
    let mut out: Vec<String> = one(&tile["content"]).into_iter().collect();
    out.extend(tile["contents"].as_array().into_iter().flatten().filter_map(one));
    out
}

/// The instance transforms of an i3dm (in the tile's frame).
fn instances(ft: &FeatureTable, world: &Mat4) -> Result<Vec<Mat4>, String> {
    let n = ft.number("INSTANCES_LENGTH").ok_or("i3dm: no INSTANCES_LENGTH")? as usize;
    let rtc = ft.vector("RTC_CENTER", 3).map_or([0.0; 3], |v| [v[0], v[1], v[2]]);
    let positions: Vec<f64> = match ft.property("POSITION", n, 3, "FLOAT") {
        Some(p) => p,
        None => {
            let q = ft.property("POSITION_QUANTIZED", n, 3, "UNSIGNED_SHORT").ok_or("i3dm: no positions")?;
            let offset = ft.vector("QUANTIZED_VOLUME_OFFSET", 3).ok_or("i3dm: no QUANTIZED_VOLUME_OFFSET")?;
            let scale = ft.vector("QUANTIZED_VOLUME_SCALE", 3).ok_or("i3dm: no QUANTIZED_VOLUME_SCALE")?;
            q.iter().enumerate().map(|(i, v)| offset[i % 3] + v / 65535.0 * scale[i % 3]).collect()
        }
    };
    let up = ft.property("NORMAL_UP", n, 3, "FLOAT").or_else(|| ft.property("NORMAL_UP_OCT32P", n, 2, "UNSIGNED_SHORT").map(|o| o.chunks(2).flat_map(|c| formats::oct_decode(c[0], c[1], 65535.0)).collect()));
    let right = ft.property("NORMAL_RIGHT", n, 3, "FLOAT").or_else(|| ft.property("NORMAL_RIGHT_OCT32P", n, 2, "UNSIGNED_SHORT").map(|o| o.chunks(2).flat_map(|c| formats::oct_decode(c[0], c[1], 65535.0)).collect()));
    let scale = ft.property("SCALE", n, 1, "FLOAT");
    let scale3 = ft.property("SCALE_NON_UNIFORM", n, 3, "FLOAT");
    let east_north_up = ft.json["EAST_NORTH_UP"].as_bool() == Some(true);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let p = positions.get(i * 3..i * 3 + 3).ok_or("i3dm: positions cut short")?;
        let p = math::add([p[0], p[1], p[2]], rtc);
        let (r, u) = match (&right, &up) {
            (Some(r), Some(u)) if r.len() >= i * 3 + 3 && u.len() >= i * 3 + 3 => ([r[i * 3], r[i * 3 + 1], r[i * 3 + 2]], [u[i * 3], u[i * 3 + 1], u[i * 3 + 2]]),
            _ if east_north_up => {
                // east and north at the instance's place on the earth, back in the tile's frame
                let w = math::point(world, p);
                let enu = geo::enu_frame(w);
                let inverse = math::rigid_inverse(&math::from_axes([world[0], world[1], world[2]], [world[4], world[5], world[6]], [world[8], world[9], world[10]], [0.0; 3]));
                (math::normalize(math::vector(&inverse, [enu[0], enu[1], enu[2]])), math::normalize(math::vector(&inverse, [enu[4], enu[5], enu[6]])))
            }
            _ => ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
        };
        let forward = math::cross(r, u);
        let s = match (&scale3, &scale) {
            (Some(s), _) if s.len() >= i * 3 + 3 => [s[i * 3], s[i * 3 + 1], s[i * 3 + 2]],
            (_, Some(s)) if s.len() > i => [s[i]; 3],
            _ => [1.0; 3],
        };
        out.push(math::from_axes(math::scale(r, s[0]), math::scale(u, s[1]), math::scale(forward, s[2]), p));
    }
    Ok(out)
}

/// The points of a pnts tile (in the tile's frame) and their colours.
#[allow(clippy::type_complexity)]
fn points(ft: &FeatureTable) -> Result<(Vec<Vec3>, Option<Vec<[u8; 4]>>), String> {
    let n = ft.number("POINTS_LENGTH").ok_or("pnts: no POINTS_LENGTH")? as usize;
    let rtc = ft.vector("RTC_CENTER", 3).map_or([0.0; 3], |v| [v[0], v[1], v[2]]);
    let draco = &ft.json["extensions"]["3DTILES_draco_point_compression"];
    let (mut positions, mut colors) = (None, None);
    if draco.is_object() {
        let (pos, col) = draco_points(ft, draco, n)?;
        positions = pos;
        colors = col;
    }
    let positions = match positions {
        Some(p) => p,
        None => match ft.property("POSITION", n, 3, "FLOAT") {
            Some(p) => p,
            None => {
                let q = ft.property("POSITION_QUANTIZED", n, 3, "UNSIGNED_SHORT").ok_or("pnts: no positions")?;
                let offset = ft.vector("QUANTIZED_VOLUME_OFFSET", 3).ok_or("pnts: no QUANTIZED_VOLUME_OFFSET")?;
                let scale = ft.vector("QUANTIZED_VOLUME_SCALE", 3).ok_or("pnts: no QUANTIZED_VOLUME_SCALE")?;
                q.iter().enumerate().map(|(i, v)| offset[i % 3] + v / 65535.0 * scale[i % 3]).collect()
            }
        },
    };
    let colors = colors.or_else(|| {
        if let Some(c) = ft.property("RGBA", n, 4, "UNSIGNED_BYTE") {
            return Some(c.chunks(4).map(|c| [c[0] as u8, c[1] as u8, c[2] as u8, c[3] as u8]).collect());
        }
        if let Some(c) = ft.property("RGB", n, 3, "UNSIGNED_BYTE") {
            return Some(c.chunks(3).map(|c| [c[0] as u8, c[1] as u8, c[2] as u8, 255]).collect());
        }
        if let Some(c) = ft.property("RGB565", n, 1, "UNSIGNED_SHORT") {
            return Some(c.iter().map(|&v| { let v = v as u16; [((v >> 11) as u32 * 255 / 31) as u8, ((v >> 5 & 63) as u32 * 255 / 63) as u8, ((v & 31) as u32 * 255 / 31) as u8, 255] }).collect());
        }
        let c = ft.vector("CONSTANT_RGBA", 4)?;
        Some(vec![[c[0] as u8, c[1] as u8, c[2] as u8, c[3] as u8]; n])
    });
    let positions = positions.chunks_exact(3).take(n).map(|p| math::add([p[0], p[1], p[2]], rtc)).collect();
    Ok((positions, colors))
}

#[allow(clippy::type_complexity)]
fn draco_points(ft: &FeatureTable, draco: &Value, n: usize) -> Result<(Option<Vec<f64>>, Option<Vec<[u8; 4]>>), String> {
    let at = draco["byteOffset"].as_u64().unwrap_or(0) as usize;
    let len = draco["byteLength"].as_u64().unwrap_or(0) as usize;
    let data = ft.binary.get(at..at + len).ok_or("pnts: Draco data outside the feature table")?;
    let mut buffer = draco_core::DecoderBuffer::new(data);
    let mut cloud = draco_core::PointCloud::new();
    draco_core::PointCloudDecoder::new().decode(&mut buffer, &mut cloud).map_err(|e| format!("pnts Draco: {e:?}"))?;
    let points = cloud.num_points().min(n.max(cloud.num_points()));
    let attribute = |name: &str| draco["properties"][name].as_u64().and_then(|id| cloud.attribute_by_unique_id(id as u32));
    let positions = attribute("POSITION").map(|a| a.read_f32s(points, 3).into_iter().map(f64::from).collect());
    let colors = if let Some(a) = attribute("RGBA") {
        Some(a.read_f32s(points, 4).chunks(4).map(|c| [c[0] as u8, c[1] as u8, c[2] as u8, c[3] as u8]).collect())
    } else {
        attribute("RGB").map(|a| a.read_f32s(points, 3).chunks(3).map(|c| [c[0] as u8, c[1] as u8, c[2] as u8, 255]).collect())
    };
    Ok((positions, colors))
}

/// Quantized-mesh terrain: one `.terrain` tile (its level and column from the folders it is in),
/// or the tiles beside a `layer.json`, at the finest level with no more than 256 tiles there.
fn terrain(files: &dyn Files, entry: &str) -> Result<Tileset, String> {
    let lower = entry.to_ascii_lowercase();
    let root = if lower.ends_with("layer.json") { dir_of(entry).to_owned() } else { String::new() };
    let layer: Value = if lower.ends_with("layer.json") { serde_json::from_slice(&files.read(entry)?).map_err(|e| format!("{entry}: {e}"))? } else { Value::Null };
    let mercator = layer["projection"].as_str() == Some("EPSG:3857");
    let xyz_scheme = layer["scheme"].as_str() == Some("slippyMap");
    let tiles: Vec<(String, u32, u64, u64)> = if lower.ends_with(".terrain") {
        vec![(entry.to_owned(), tile_address(entry).ok_or("terrain: the tile must lie in <level>/<x>/<y>.terrain")?)].into_iter().map(|(p, (l, x, y))| (p, l, x, y)).collect()
    } else {
        let all: Vec<(String, u32, u64, u64)> = files.list().into_iter().filter(|p| p.starts_with(&root) && p.to_ascii_lowercase().ends_with(".terrain")).filter_map(|p| tile_address(&p[root.len()..]).map(|(l, x, y)| (p, l, x, y))).collect();
        let finest = (0..=32).rev().find(|&level| {
            let n = all.iter().filter(|t| t.1 == level).count();
            n > 0 && n <= 256
        });
        let level = finest.ok_or("terrain: no .terrain tiles beside the layer.json")?;
        all.into_iter().filter(|t| t.1 == level).collect()
    };
    let mut out = Tileset::default();
    let mut local = None;
    for (path, level, x, y) in tiles {
        out.tiles_visited += 1;
        let mesh = match files.read(&path).and_then(gunzip).and_then(|b| quantized_mesh::decode(&b)) {
            Ok(m) => m,
            Err(e) => {
                out.failed.push((path, e));
                continue;
            }
        };
        let rows = 1u64 << level;
        let y = if xyz_scheme { rows - 1 - y } else { y };
        let rect = if mercator { mercator_rect(level, x, y) } else { quantized_mesh::geographic_rect(level, x, y) };
        let local = *local.get_or_insert_with(|| {
            let frame = geo::enu_frame(mesh.center);
            let (lon, lat, h) = geo::to_geodetic(mesh.center);
            out.frame = Frame { ecef: Some(mesh.center), geodetic: Some([lat.to_degrees(), lon.to_degrees(), h]) };
            math::rigid_inverse(&frame)
        });
        let positions = mesh.positions(rect).into_iter().map(|p| math::point(&local, p).map(|v| v as f32)).collect();
        let normals = mesh.normals.as_ref().map(|n| n.iter().map(|v| math::vector(&local, v.map(f64::from)).map(|v| v as f32)).collect());
        out.contents.push(Content { uri: path, geometric_error: 0.0, kind: Kind::Mesh { positions, normals, indices: mesh.indices } });
        out.tiles_read += 1;
    }
    if out.contents.is_empty() {
        let why = out.failed.first().map(|(u, e)| format!(": {u}: {e}")).unwrap_or_default();
        return Err(format!("terrain: no tile could be read{why}"));
    }
    out.info.push(("Terrain".into(), format!("quantized mesh, {} tiles", out.tiles_read)));
    Ok(out)
}

/// The level, column and row of a terrain tile from its path `.../<level>/<x>/<y>.terrain`.
fn tile_address(path: &str) -> Option<(u32, u64, u64)> {
    let parts: Vec<&str> = path.split('/').collect();
    let n = parts.len();
    if n < 3 {
        return None;
    }
    let y = parts[n - 1].split('.').next()?.parse().ok()?;
    Some((parts[n - 3].parse().ok()?, parts[n - 2].parse().ok()?, y))
}

/// The rectangle of a tile of the web mercator tiling scheme (EPSG:3857, one tile at level 0,
/// rows from the south), in radians.
fn mercator_rect(level: u32, x: u64, y: u64) -> [f64; 4] {
    use std::f64::consts::PI;
    let n = (1u64 << level) as f64;
    let lat = |row: f64| (PI * (2.0 * row / n - 1.0)).sinh().atan();
    [-PI + 2.0 * PI * x as f64 / n, lat(y as f64), -PI + 2.0 * PI * (x + 1) as f64 / n, lat(y as f64 + 1.0)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terrain_tile_address() {
        assert_eq!(tile_address("9/759/335.terrain"), Some((9, 759, 335)));
        assert_eq!(tile_address("a/b/335.terrain"), None);
    }

    #[test]
    fn finds_the_tileset_nearest_the_root() {
        let list = vec!["a/b/tileset.json".to_owned(), "a/tileset.json".to_owned(), "a/x.b3dm".to_owned()];
        assert_eq!(find_entry(&list).as_deref(), Some("a/tileset.json"));
    }
}
