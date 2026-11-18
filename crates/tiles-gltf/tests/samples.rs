//! The Khronos sample files that use each extension, unpacked and read with a plain glTF reader.
use std::path::Path;

fn data(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data").join(name)
}

/// Unpacks a .gltf with its files beside it, and reads the result: (triangles, vertices).
fn unpack_and_read(path: &Path) -> (usize, usize, gltf::Document, Vec<gltf::image::Data>) {
    let base = path.parent().unwrap().to_path_buf();
    let bytes = std::fs::read(path).unwrap();
    let glb = tiles_gltf::unpack(&bytes, &mut |uri| std::fs::read(base.join(uri)).map_err(|e| e.to_string())).unwrap().expect("needs unpacking");
    let (doc, buffers, images) = gltf::import_slice(&glb).expect("plain glTF");
    let (mut triangles, mut vertices) = (0, 0);
    for mesh in doc.meshes() {
        for prim in mesh.primitives() {
            let reader = prim.reader(|b| buffers.get(b.index()).map(|d| &d.0[..]));
            vertices += reader.read_positions().expect("positions").count();
            triangles += reader.read_indices().expect("indices").into_u32().count() / 3;
        }
    }
    (triangles, vertices, doc, images)
}

#[test]
fn draco_box() {
    let (triangles, vertices, doc, _) = unpack_and_read(&data("box-draco/Box.gltf"));
    assert_eq!(triangles, 12);
    assert_eq!(vertices, 24);
    assert!(doc.extensions_used().all(|e| e != "KHR_draco_mesh_compression"));
    // the Box is a unit cube around the origin
    let prim = doc.meshes().next().unwrap().primitives().next().unwrap();
    let bb = prim.bounding_box();
    assert!((bb.max[0] - 0.5).abs() < 1e-3 && (bb.min[0] + 0.5).abs() < 1e-3, "{bb:?}");
}

#[test]
fn meshopt_cube() {
    let path = data("meshopt-cube/MeshoptCubeTest.gltf");
    let (triangles, vertices, doc, images) = unpack_and_read(&path);
    assert!(doc.meshes().count() > 30);
    assert!(triangles > 300 && vertices > 0);
    // the PNG images are embedded now
    assert_eq!(images.len(), doc.images().count());
    assert!(images.iter().all(|i| i.width > 1));

    // every decoded view equals the uncompressed data the sample's plain variant ships
    let fallback = std::fs::read(data("meshopt-cube/MeshoptCubeTestFallback.bin")).unwrap();
    let original: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let base = path.parent().unwrap().to_path_buf();
    let glb = tiles_gltf::unpack(&std::fs::read(&path).unwrap(), &mut |uri| std::fs::read(base.join(uri)).map_err(|e| e.to_string())).unwrap().unwrap();
    let (unpacked, bin) = tiles_gltf::split(&glb).unwrap();
    let bin = bin.unwrap();
    let mut compared = 0;
    for (i, view) in original["bufferViews"].as_array().unwrap().iter().enumerate() {
        if view["extensions"]["KHR_meshopt_compression"].is_null() { continue; }
        let (at, len) = (view["byteOffset"].as_u64().unwrap_or(0) as usize, view["byteLength"].as_u64().unwrap() as usize);
        let new_at = unpacked["bufferViews"][i]["byteOffset"].as_u64().unwrap() as usize;
        let (got, want) = (&bin[new_at..new_at + len], &fallback[at..at + len]);
        if view["extensions"]["KHR_meshopt_compression"]["mode"] == "TRIANGLES" {
            // the triangle codec may rotate a triangle's corners: compare with each rotated alike
            let stride = view["extensions"]["KHR_meshopt_compression"]["byteStride"].as_u64().unwrap() as usize;
            assert_eq!(triangles_of(got, stride), triangles_of(want, stride), "view {i}");
        } else {
            assert_eq!(got, want, "view {i}");
        }
        compared += 1;
    }
    assert!(compared > 2);
}

#[test]
fn basis_ktx2_texture_transcodes() {
    let bytes = std::fs::read(data("Dot_C.ktx2")).unwrap();
    let (w, h, rgba) = tiles_gltf::ktx2_to_rgba(&bytes).unwrap();
    assert!(w > 1 && h > 1);
    assert_eq!(rgba.len(), (w * h * 4) as usize);
    // not a blank image
    assert!(rgba.chunks(4).any(|p| p != &rgba[..4]));
    let png = tiles_gltf::ktx2_to_png(&bytes).unwrap();
    assert!(png.starts_with(b"\x89PNG"));
}

/// Further sample folders, outside the repository (licences that do not allow copying here):
/// `TILES_GLTF_SAMPLES` = a folder of .gltf files; each must unpack and read.
#[test]
fn further_samples() {
    let Ok(dir) = std::env::var("TILES_GLTF_SAMPLES") else { return };
    let mut n = 0;
    for entry in walk(Path::new(&dir)) {
        if entry.extension().is_some_and(|e| e == "gltf" || e == "glb") {
            let (triangles, _, _, _) = unpack_and_read(&entry);
            eprintln!("{}: {triangles} triangles", entry.display());
            assert!(triangles > 0);
            n += 1;
        }
    }
    assert!(n > 0);
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() { out.extend(walk(&p)); } else { out.push(p); }
    }
    out
}

/// Triangles of an index buffer, each rotated to start at its smallest index.
fn triangles_of(bytes: &[u8], stride: usize) -> Vec<[u32; 3]> {
    let indices: Vec<u32> = bytes.chunks(stride).map(|c| if stride == 2 { u16::from_le_bytes([c[0], c[1]]) as u32 } else { u32::from_le_bytes([c[0], c[1], c[2], c[3]]) }).collect();
    indices.chunks(3).map(|t| {
        let k = (0..3).min_by_key(|&k| t[k]).unwrap();
        [t[k], t[(k + 1) % 3], t[(k + 2) % 3]]
    }).collect()
}
