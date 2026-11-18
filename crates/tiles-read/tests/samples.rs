//! Sample tilesets read end to end.
use std::path::{Path, PathBuf};
use tiles_model::Kind;

fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data").join(name)
}

/// (glTF contents with their placements, point count, mesh triangles)
fn summary(t: &tiles_model::Tileset) -> (usize, usize, usize, usize) {
    let (mut gltf, mut placed, mut points, mut triangles) = (0, 0, 0, 0);
    for c in &t.contents {
        match &c.kind {
            Kind::Gltf { glb, placements } => {
                assert!(glb.starts_with(b"glTF"));
                gltf += 1;
                placed += placements.len();
            }
            Kind::Points { positions, .. } => points += positions.len(),
            Kind::Mesh { indices, .. } => triangles += indices.len() / 3,
        }
    }
    (gltf, placed, points, triangles)
}

/// Every tileset.json, layer.json and lone .terrain under `TILES_SAMPLES` (folders, separated
/// by `;`): read and summed up.
#[test]
fn further_samples() {
    let Ok(dirs) = std::env::var("TILES_SAMPLES") else { return };
    let mut failed = Vec::new();
    for dir in dirs.split(';') {
        for path in walk(Path::new(dir)) {
            let name = path.file_name().unwrap().to_string_lossy().to_lowercase();
            if !(name == "tileset.json" || name == "layer.json" || name.ends_with(".terrain")) {
                continue;
            }
            match tiles_read::open_path(&path, &tiles_read::Options::default()) {
                Ok(t) => eprintln!("OK   {}: {:?} read {}/{} frame {:?} failed {:?}", path.display(), summary(&t), t.tiles_read, t.tiles_visited, t.frame.geodetic, t.failed.iter().take(2).collect::<Vec<_>>()),
                Err(e) => {
                    eprintln!("FAIL {}: {e}", path.display());
                    failed.push(path);
                }
            }
        }
    }
    eprintln!("{} failed", failed.len());
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() { out.extend(walk(&p)); } else { out.push(p); }
    }
    out
}

fn open(name: &str) -> tiles_model::Tileset {
    tiles_read::open_path(&data(name), &tiles_read::Options::default()).unwrap()
}

/// The largest distance from the local origin of any placement or point.
fn reach(t: &tiles_model::Tileset) -> f64 {
    let mut far = 0.0f64;
    for c in &t.contents {
        match &c.kind {
            Kind::Gltf { placements, .. } => placements.iter().for_each(|p| far = far.max((p[12] * p[12] + p[13] * p[13] + p[14] * p[14]).sqrt())),
            Kind::Points { positions, .. } | Kind::Mesh { positions, .. } => positions.iter().for_each(|p| far = far.max(((p[0] * p[0] + p[1] * p[1] + p[2] * p[2]) as f64).sqrt())),
        }
    }
    far
}

#[test]
fn batched_tile_on_the_earth() {
    let t = open("BatchedColors/tileset.json");
    assert_eq!(summary(&t), (1, 1, 0, 0));
    let [lat, lon, _] = t.frame.geodetic.unwrap();
    assert!((lat - 40.0425).abs() < 1e-3 && (lon + 75.6121).abs() < 1e-3, "{lat} {lon}");
    // in the local east-north-up frame the tile is near the origin
    assert!(reach(&t) < 100.0, "{}", reach(&t));
}

#[test]
fn instanced_tile_has_a_placement_per_instance() {
    let t = open("InstancedOrientation/tileset.json");
    assert_eq!(summary(&t), (1, 25, 0, 0));
    let Kind::Gltf { placements, .. } = &t.contents[0].kind else { panic!() };
    // the instances stand on a grid of some 160 m across the tile's region
    assert!(reach(&t) < 300.0, "{:?}", &placements[0]);
}

#[test]
fn draco_point_cloud() {
    let t = open("PointCloudDraco/tileset.json");
    assert_eq!(summary(&t), (0, 0, 1000, 0));
    let Kind::Points { colors, .. } = &t.contents[0].kind else { panic!() };
    assert_eq!(colors.as_ref().map(Vec::len), Some(1000));
    assert!(reach(&t) < 100.0);
}

#[test]
fn composite_tile() {
    let t = open("Composite/tileset.json");
    assert_eq!(summary(&t), (2, 26, 0, 0));
}

#[test]
fn implicit_quadtree() {
    let t = open("ImplicitTileset/tileset_1.1.json");
    assert!(t.failed.is_empty(), "{:?}", t.failed);
    let (gltf, _, _, _) = summary(&t);
    assert!(gltf >= 1, "{gltf}");
    assert!(t.tiles_visited >= gltf);
}

#[test]
fn quantized_mesh_terrain() {
    for entry in ["terrain/layer.json", "terrain/9/759/335.terrain"] {
        let t = open(entry);
        assert_eq!(summary(&t), (0, 0, 0, 4755), "{entry}");
        let [lat, lon, _] = t.frame.geodetic.unwrap();
        // level 9, column 759, row 335 of the geographic scheme: lat 27.6..28.1, lon 86.8..87.2
        assert!((27.5..28.2).contains(&lat) && (86.8..87.2).contains(&lon), "{lat} {lon}");
        assert!(reach(&t) < 60_000.0);
    }
}

/// The geometric error is scaled with the tile's transform (a tile scaled 100 times has errors
/// 100 times as large): a tileset of one tile read twice, as is and scaled, refines alike.
#[test]
fn error_scales_with_the_transform() {
    let mut tileset: serde_json::Value = serde_json::from_slice(&std::fs::read(data("BatchedColors/tileset.json")).unwrap()).unwrap();
    tileset["root"]["children"] = serde_json::json!([{ "boundingVolume": tileset["root"]["boundingVolume"].clone(), "geometricError": 0, "content": {"uri": "missing.b3dm"} }]);
    tileset["root"]["geometricError"] = serde_json::json!(0.0001);
    let dir = std::env::temp_dir().join(format!("tiles-scale-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(data("BatchedColors/batchedColors.b3dm"), dir.join("batchedColors.b3dm")).unwrap();
    std::fs::write(dir.join("tileset.json"), tileset.to_string()).unwrap();
    // an error far below a pixel: the root is not refined, the missing child is not asked for
    let t = tiles_read::open_path(&dir.join("tileset.json"), &tiles_read::Options::default()).unwrap();
    assert!(t.failed.is_empty(), "{:?}", t.failed);
    tileset["root"]["transform"] = serde_json::json!([1e6, 0, 0, 0, 0, 1e6, 0, 0, 0, 0, 1e6, 0, 0, 0, 0, 1]);
    std::fs::write(dir.join("tileset.json"), tileset.to_string()).unwrap();
    let t = tiles_read::open_path(&dir.join("tileset.json"), &tiles_read::Options::default()).unwrap();
    std::fs::remove_dir_all(&dir).ok();
    // scaled a million times, the root's error is seen and the child is asked for
    assert_eq!(t.failed.len(), 1, "{:?}", t.failed);
}
