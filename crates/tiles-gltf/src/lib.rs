//! glTF made plain.
//!
//! Many glTF files, and nearly all 3D Tiles content, carry their geometry and textures in a
//! compressed form that a plain glTF 2.0 reader does not decode:
//!
//! - `KHR_draco_mesh_compression`: a primitive's attributes and indices as one Draco stream;
//! - `EXT_meshopt_compression` and `KHR_meshopt_compression`: buffer views compressed with the
//!   meshopt codecs, often over a "fallback" buffer that has no data of its own;
//! - `KHR_mesh_quantization`: positions, normals and texture coordinates as integers;
//! - `KHR_texture_basisu`: textures as Basis Universal images in a KTX2 container.
//!
//! [`unpack`] decodes all four and writes the result as one self-contained glTF binary (`.glb`):
//! one buffer, every image embedded (KTX2 transcoded to PNG), the decoded extensions taken out of
//! `extensionsUsed` and `extensionsRequired`. Required extensions it does not decode are taken out
//! of `extensionsRequired` too (they stay in `extensionsUsed`), so a reader that only knows the
//! core opens the file and ignores what it does not know.

use serde_json::{json, Map, Value};

const DRACO: &str = "KHR_draco_mesh_compression";
const MESHOPT: [&str; 2] = ["EXT_meshopt_compression", "KHR_meshopt_compression"];
const QUANTIZATION: &str = "KHR_mesh_quantization";
const BASISU: &str = "KHR_texture_basisu";
const HANDLED: [&str; 5] = [DRACO, MESHOPT[0], MESHOPT[1], QUANTIZATION, BASISU];

/// Largest side of a texture after unpacking; KTX2 images are transcoded at the first mip level
/// that fits, larger plain images are kept as they are.
pub const MAX_TEXTURE: u32 = 1024;

/// Whether a glTF document needs [`unpack`] before a plain glTF 2.0 reader can open it: it uses
/// one of the extensions this crate decodes, or it requires any extension at all.
pub fn needs_unpack(doc: &Value) -> bool {
    let required = doc["extensionsRequired"].as_array().is_some_and(|a| !a.is_empty());
    required || names(&doc["extensionsUsed"]).any(|e| HANDLED.contains(&e))
}

fn names(list: &Value) -> impl Iterator<Item = &str> {
    list.as_array().into_iter().flatten().filter_map(Value::as_str)
}

/// The JSON and the binary chunk of a glTF: a `.glb` or a `.gltf` (JSON text).
pub fn split(bytes: &[u8]) -> Result<(Value, Option<Vec<u8>>), String> {
    if bytes.starts_with(b"glTF") {
        let u32_at = |at: usize| bytes.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
        let total = u32_at(8).ok_or("glTF binary: header cut short")?.min(bytes.len());
        let (mut json, mut bin) = (None, None);
        let mut at = 12;
        while at + 8 <= total {
            let (len, kind) = (u32_at(at).unwrap_or(0), u32_at(at + 4).unwrap_or(0));
            let chunk = bytes.get(at + 8..at + 8 + len).ok_or("glTF binary: chunk cut short")?;
            match kind {
                0x4E4F_534A => json = Some(serde_json::from_slice(chunk).map_err(|e| format!("glTF binary JSON: {e}"))?),
                0x004E_4942 => bin = Some(chunk.to_vec()),
                _ => {}
            }
            at += 8 + len;
        }
        return Ok((json.ok_or("glTF binary: no JSON chunk")?, bin));
    }
    let text = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    Ok((serde_json::from_slice(text).map_err(|e| format!("glTF JSON: {e}"))?, None))
}

/// Writes a glTF binary: the JSON chunk and, when it is not empty, the binary chunk.
pub fn write_glb(doc: &Value, bin: &[u8]) -> Vec<u8> {
    let mut json = serde_json::to_vec(doc).unwrap_or_default();
    while json.len() % 4 != 0 {
        json.push(b' ');
    }
    let mut bin = bin.to_vec();
    while bin.len() % 4 != 0 {
        bin.push(0);
    }
    let total = 12 + 8 + json.len() + if bin.is_empty() { 0 } else { 8 + bin.len() };
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(&0x4E4F_534Au32.to_le_bytes());
    out.extend_from_slice(&json);
    if !bin.is_empty() {
        out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        out.extend_from_slice(&0x004E_4942u32.to_le_bytes());
        out.extend_from_slice(&bin);
    }
    out
}

/// Reads the file a relative URI of the glTF names (buffers and images that are not embedded).
pub type Resolve<'a> = dyn FnMut(&str) -> Result<Vec<u8>, String> + 'a;

/// Unpacks a glTF (`.glb` or `.gltf`) when [`needs_unpack`] says so: the plain, self-contained
/// glTF binary, or `None` when the file can be read as it is.
pub fn unpack(bytes: &[u8], resolve: &mut Resolve) -> Result<Option<Vec<u8>>, String> {
    let (doc, bin) = split(bytes)?;
    if !needs_unpack(&doc) {
        return Ok(None);
    }
    unpack_document(doc, bin, resolve).map(Some)
}

/// [`unpack`] on a document already split into JSON and binary chunk; always rewrites.
pub fn unpack_document(mut doc: Value, bin: Option<Vec<u8>>, resolve: &mut Resolve) -> Result<Vec<u8>, String> {
    let mut out = Packer::default();

    // 1. Every buffer into one; buffers without data (meshopt fallbacks) as zeros to decode into.
    let buffers = doc["buffers"].as_array().cloned().unwrap_or_default();
    let mut bin = bin;
    let mut base = Vec::with_capacity(buffers.len());
    for (i, buffer) in buffers.iter().enumerate() {
        let length = buffer["byteLength"].as_u64().unwrap_or(0) as usize;
        let fallback = MESHOPT.iter().any(|m| buffer["extensions"][m]["fallback"].as_bool() == Some(true));
        let data = match buffer["uri"].as_str() {
            Some(uri) => match data_uri(uri) {
                Some(data) => data?,
                None if fallback => resolve(uri).unwrap_or_default(),
                None => resolve(uri).map_err(|e| format!("glTF buffer {uri}: {e}"))?,
            },
            None if i == 0 && bin.is_some() => bin.take().unwrap_or_default(),
            None if fallback => Vec::new(),
            None => return Err(format!("glTF buffer {i} has no data")),
        };
        let mut data = data;
        if data.len() < length {
            data.resize(length, 0);
        }
        base.push(out.push(&data));
    }

    // 2. meshopt: decode each compressed view into its place.
    let views = doc.get_mut("bufferViews").and_then(Value::as_array_mut).map(std::mem::take).unwrap_or_default();
    let mut views_out = Vec::with_capacity(views.len());
    for mut view in views {
        if let Some((name, ext)) = MESHOPT.iter().find_map(|m| view["extensions"].get(*m).map(|e| (*m, e.clone()))) {
            let target = base.get(view["buffer"].as_u64().unwrap_or(0) as usize).copied().unwrap_or(0) + view["byteOffset"].as_u64().unwrap_or(0) as usize;
            let source = base.get(ext["buffer"].as_u64().unwrap_or(0) as usize).copied().unwrap_or(0) + ext["byteOffset"].as_u64().unwrap_or(0) as usize;
            let decoded = meshopt_decode(&ext, out.bytes.get(source..source + ext["byteLength"].as_u64().unwrap_or(0) as usize).ok_or("meshopt: source outside its buffer")?)?;
            out.bytes.get_mut(target..target + decoded.len()).ok_or("meshopt: target outside its buffer")?.copy_from_slice(&decoded);
            remove_extension(&mut view, name);
        }
        let buffer = view["buffer"].as_u64().unwrap_or(0) as usize;
        let offset = base.get(buffer).copied().unwrap_or(0) + view["byteOffset"].as_u64().unwrap_or(0) as usize;
        view["buffer"] = json!(0);
        view["byteOffset"] = json!(offset);
        views_out.push(view);
    }
    doc["bufferViews"] = Value::Array(views_out);

    // 3. Draco primitives.
    let meshes = doc["meshes"].as_array().map_or(0, Vec::len);
    for m in 0..meshes {
        let prims = doc["meshes"][m]["primitives"].as_array().map_or(0, Vec::len);
        for p in 0..prims {
            let ext = doc["meshes"][m]["primitives"][p]["extensions"][DRACO].clone();
            if ext.is_object() {
                draco_primitive(&mut doc, &mut out, m, p, &ext)?;
            }
        }
    }

    // 4. Quantized vertex attributes as floats.
    dequantize(&mut doc, &mut out);

    // 5. Textures and images: Basis KTX2 transcoded, every image embedded.
    if let Some(textures) = doc.get_mut("textures").and_then(Value::as_array_mut) {
        for texture in textures {
            if let Some(source) = texture["extensions"][BASISU]["source"].as_u64() {
                texture["source"] = json!(source);
                remove_extension(texture, BASISU);
            }
        }
    }
    let images = doc["images"].as_array().map_or(0, Vec::len);
    for i in 0..images {
        let image = doc["images"][i].clone();
        let (bytes, mime) = match (image["uri"].as_str(), image["bufferView"].as_u64()) {
            (Some(uri), _) => match data_uri(uri) {
                Some(data) => (data.unwrap_or_default(), image["mimeType"].as_str().map(str::to_owned).or_else(|| uri_mime(uri))),
                None => (resolve(uri).unwrap_or_default(), image["mimeType"].as_str().map(str::to_owned).or_else(|| uri_mime(uri))),
            },
            (None, Some(view)) => (out.view(&doc, view as usize).unwrap_or_default().to_vec(), image["mimeType"].as_str().map(str::to_owned)),
            _ => continue,
        };
        let is_ktx2 = bytes.starts_with(KTX2_MAGIC) || mime.as_deref() == Some("image/ktx2");
        let (bytes, mime) = if is_ktx2 {
            (ktx2_to_png(&bytes).unwrap_or_else(|_| png(1, 1, &[255, 255, 255, 255])), "image/png".to_owned())
        } else if bytes.is_empty() {
            (png(1, 1, &[255, 255, 255, 255]), "image/png".to_owned())
        } else {
            let mime = mime.unwrap_or_else(|| if bytes.starts_with(&[0xFF, 0xD8]) { "image/jpeg".into() } else { "image/png".into() });
            (bytes, mime)
        };
        let view = out.add_view(&mut doc, &bytes);
        let mut entry = Map::new();
        if let Some(name) = image.get("name") {
            entry.insert("name".into(), name.clone());
        }
        entry.insert("bufferView".into(), json!(view));
        entry.insert("mimeType".into(), json!(mime));
        doc["images"][i] = Value::Object(entry);
    }

    // 6. One buffer, and the extension lists without what is decoded now.
    doc["buffers"] = json!([{ "byteLength": out.bytes.len() }]);
    if let Some(used) = doc.get_mut("extensionsUsed").and_then(Value::as_array_mut) {
        used.retain(|e| !e.as_str().is_some_and(|e| HANDLED.contains(&e)));
    }
    if let Some(root) = doc.as_object_mut() {
        root.remove("extensionsRequired");
        root.retain(|_, v| !v.is_null());
    }
    Ok(write_glb(&doc, &out.bytes))
}

#[derive(Default)]
struct Packer {
    bytes: Vec<u8>,
}

impl Packer {
    /// Appends data on a four-byte boundary; its offset.
    fn push(&mut self, data: &[u8]) -> usize {
        while self.bytes.len() % 4 != 0 {
            self.bytes.push(0);
        }
        let at = self.bytes.len();
        self.bytes.extend_from_slice(data);
        at
    }

    /// Appends data as a new buffer view of the document; its index.
    fn add_view(&mut self, doc: &mut Value, data: &[u8]) -> usize {
        let at = self.push(data);
        if !doc["bufferViews"].is_array() {
            doc["bufferViews"] = json!([]);
        }
        let views = doc["bufferViews"].as_array_mut().expect("buffer views are an array");
        views.push(json!({ "buffer": 0, "byteOffset": at, "byteLength": data.len() }));
        views.len() - 1
    }

    /// The bytes of a buffer view (after the views are moved into the one buffer).
    fn view<'a>(&'a self, doc: &Value, view: usize) -> Option<&'a [u8]> {
        let v = &doc["bufferViews"][view];
        let at = v["byteOffset"].as_u64().unwrap_or(0) as usize;
        self.bytes.get(at..at + v["byteLength"].as_u64()? as usize)
    }
}

fn remove_extension(object: &mut Value, name: &str) {
    if let Some(exts) = object.get_mut("extensions").and_then(Value::as_object_mut) {
        exts.remove(name);
        if exts.is_empty() {
            object.as_object_mut().map(|o| o.remove("extensions"));
        }
    }
}

fn uri_mime(uri: &str) -> Option<String> {
    let lower = uri.to_ascii_lowercase();
    let ext = lower.rsplit('.').next()?;
    Some(match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "ktx2" => "image/ktx2",
        "webp" => "image/webp",
        _ => return None,
    }
    .to_owned())
}

/// The bytes of a `data:` URI (base64 or percent-encoded); `None` when the URI is not one.
pub fn data_uri(uri: &str) -> Option<Result<Vec<u8>, String>> {
    let rest = uri.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    Some(if meta.ends_with(";base64") { base64(data) } else { Ok(data.as_bytes().to_vec()) })
}

fn base64(text: &str) -> Result<Vec<u8>, String> {
    let value = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' | b'-' => Some(62),
        b'/' | b'_' => Some(63),
        _ => None,
    };
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for c in text.bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        acc = acc << 6 | value(c).ok_or("base64: not a base64 character")? as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// meshopt

fn meshopt_decode(ext: &Value, source: &[u8]) -> Result<Vec<u8>, String> {
    let stride = ext["byteStride"].as_u64().unwrap_or(0) as usize;
    let count = ext["count"].as_u64().unwrap_or(0) as usize;
    let err = |e: meshoptimizer::Error| format!("meshopt: {e:?}");
    let mut out = vec![0u8; stride * count];
    match ext["mode"].as_str().unwrap_or("ATTRIBUTES") {
        "ATTRIBUTES" => meshoptimizer::decode_vertex_buffer(&mut out, count, stride, source, source.len()).map_err(err)?,
        mode @ ("TRIANGLES" | "INDICES") => match stride {
            2 => {
                let mut indices = vec![0u16; count];
                if mode == "TRIANGLES" {
                    meshoptimizer::decode_index_buffer(&mut indices, count, source, source.len()).map_err(err)?;
                } else {
                    meshoptimizer::decode_index_sequence(&mut indices, count, source, source.len()).map_err(err)?;
                }
                out = indices.iter().flat_map(|i| i.to_le_bytes()).collect();
            }
            4 => {
                let mut indices = vec![0u32; count];
                if mode == "TRIANGLES" {
                    meshoptimizer::decode_index_buffer(&mut indices, count, source, source.len()).map_err(err)?;
                } else {
                    meshoptimizer::decode_index_sequence(&mut indices, count, source, source.len()).map_err(err)?;
                }
                out = indices.iter().flat_map(|i| i.to_le_bytes()).collect();
            }
            _ => return Err(format!("meshopt: index stride {stride}")),
        },
        mode => return Err(format!("meshopt: mode {mode}")),
    }
    match ext["filter"].as_str().unwrap_or("NONE") {
        "NONE" => {}
        "OCTAHEDRAL" => meshoptimizer::decode_filter_oct(&mut out, count, stride).map_err(err)?,
        "QUATERNION" => meshoptimizer::decode_filter_quat(&mut out, count, stride).map_err(err)?,
        "EXPONENTIAL" => meshoptimizer::decode_filter_exp(&mut out, count, stride).map_err(err)?,
        "COLOR" => meshoptimizer::decode_filter_color(&mut out, count, stride).map_err(err)?,
        filter => return Err(format!("meshopt: filter {filter}")),
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Draco

fn components(kind: &str) -> usize {
    match kind {
        "SCALAR" => 1,
        "VEC2" => 2,
        "VEC3" => 3,
        "VEC4" | "MAT2" => 4,
        "MAT3" => 9,
        "MAT4" => 16,
        _ => 1,
    }
}

/// The largest value of an integer component type, for normalized values.
fn type_max(component_type: u64) -> f32 {
    match component_type {
        5120 => 127.0,
        5121 => 255.0,
        5122 => 32767.0,
        5123 => 65535.0,
        5125 => 4_294_967_295.0,
        _ => 1.0,
    }
}

/// Values in a component type, little endian.
fn encode(values: &[f32], component_type: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 4);
    for &v in values {
        match component_type {
            5120 => out.push(v.round() as i8 as u8),
            5121 => out.push(v.round() as u8),
            5122 => out.extend_from_slice(&(v.round() as i16).to_le_bytes()),
            5123 => out.extend_from_slice(&(v.round() as u16).to_le_bytes()),
            5125 => out.extend_from_slice(&(v.round() as u32).to_le_bytes()),
            _ => out.extend_from_slice(&v.to_le_bytes()),
        }
    }
    out
}

fn draco_primitive(doc: &mut Value, out: &mut Packer, m: usize, p: usize, ext: &Value) -> Result<(), String> {
    let view = ext["bufferView"].as_u64().ok_or("Draco: no buffer view")? as usize;
    let data = out.view(doc, view).ok_or("Draco: buffer view outside the buffer")?.to_vec();
    let mut buffer = draco_core::DecoderBuffer::new(&data);
    let mut mesh = draco_core::Mesh::new();
    let decoded_mesh = draco_core::MeshDecoder::new().decode(&mut buffer, &mut mesh).is_ok();
    let cloud: draco_core::PointCloud = if decoded_mesh {
        mesh.clone().into_point_cloud()
    } else {
        let mut buffer = draco_core::DecoderBuffer::new(&data);
        let mut cloud = draco_core::PointCloud::new();
        draco_core::PointCloudDecoder::new().decode(&mut buffer, &mut cloud).map_err(|e| format!("Draco: {e:?}"))?;
        cloud
    };
    let points = cloud.num_points();

    let attributes = ext["attributes"].as_object().cloned().unwrap_or_default();
    for (name, unique) in attributes {
        let Some(accessor) = doc["meshes"][m]["primitives"][p]["attributes"][&name].as_u64() else { continue };
        let Some(attribute) = unique.as_u64().and_then(|u| cloud.attribute_by_unique_id(u as u32)) else { continue };
        let acc = doc["accessors"][accessor as usize].clone();
        let n = components(acc["type"].as_str().unwrap_or("SCALAR"));
        let component_type = acc["componentType"].as_u64().unwrap_or(5126);
        let mut values = attribute.read_f32s(points, n);
        if component_type == 5126 && attribute.normalized() {
            let max = draco_type_max(attribute.data_type());
            values.iter_mut().for_each(|v| *v = (*v / max).max(-1.0));
        }
        let view = out.add_view(doc, &encode(&values, component_type));
        let a = &mut doc["accessors"][accessor as usize];
        a["bufferView"] = json!(view);
        a["byteOffset"] = json!(0);
        a["count"] = json!(points);
        a.as_object_mut().map(|o| o.remove("sparse"));
        if name == "POSITION" {
            let (min, max) = min_max(&values, n);
            a["min"] = json!(min);
            a["max"] = json!(max);
        }
    }

    let prim = &doc["meshes"][m]["primitives"][p];
    match (prim["indices"].as_u64(), decoded_mesh) {
        (Some(accessor), true) => {
            let indices: Vec<u8> = mesh.faces().iter().flat_map(|f| f.iter().flat_map(|i| i.0.to_le_bytes())).collect();
            let view = out.add_view(doc, &indices);
            let a = &mut doc["accessors"][accessor as usize];
            a["bufferView"] = json!(view);
            a["byteOffset"] = json!(0);
            a["count"] = json!(mesh.num_faces() * 3);
            a["componentType"] = json!(5125);
            a["type"] = json!("SCALAR");
            if let Some(o) = a.as_object_mut() {
                o.remove("min");
                o.remove("max");
                o.remove("sparse");
            }
        }
        (Some(_), false) => {
            doc["meshes"][m]["primitives"][p].as_object_mut().map(|o| o.remove("indices"));
        }
        _ => {}
    }
    remove_extension(&mut doc["meshes"][m]["primitives"][p], DRACO);
    Ok(())
}

fn draco_type_max(t: draco_core::DataType) -> f32 {
    use draco_core::DataType::*;
    match t {
        Int8 => 127.0,
        Uint8 => 255.0,
        Int16 => 32767.0,
        Uint16 => 65535.0,
        Int32 => 2_147_483_647.0,
        Uint32 => 4_294_967_295.0,
        _ => 1.0,
    }
}

fn min_max(values: &[f32], n: usize) -> (Vec<f32>, Vec<f32>) {
    let mut min = vec![f32::INFINITY; n];
    let mut max = vec![f32::NEG_INFINITY; n];
    for row in values.chunks_exact(n) {
        for k in 0..n {
            min[k] = min[k].min(row[k]);
            max[k] = max[k].max(row[k]);
        }
    }
    if values.is_empty() {
        return (vec![0.0; n], vec![0.0; n]);
    }
    (min, max)
}

// ---------------------------------------------------------------------------------------------
// Quantized attributes

/// Integer positions, normals, tangents and texture coordinates (`KHR_mesh_quantization`)
/// rewritten as floats, so a core reader reads them; the node transforms that scale them stay.
fn dequantize(doc: &mut Value, out: &mut Packer) {
    let mut todo = std::collections::BTreeSet::new();
    for mesh in doc["meshes"].as_array().into_iter().flatten() {
        for prim in mesh["primitives"].as_array().into_iter().flatten() {
            let sets = std::iter::once(&prim["attributes"]).chain(prim["targets"].as_array().into_iter().flatten());
            for set in sets {
                for (name, accessor) in set.as_object().into_iter().flatten() {
                    let float_only = name == "POSITION" || name == "NORMAL" || name == "TANGENT" || name.starts_with("TEXCOORD_");
                    if let Some(a) = accessor.as_u64().filter(|_| float_only) {
                        if doc["accessors"][a as usize]["componentType"].as_u64().is_some_and(|t| t != 5126) {
                            todo.insert(a as usize);
                        }
                    }
                }
            }
        }
    }
    for a in todo {
        let acc = doc["accessors"][a].clone();
        let component_type = acc["componentType"].as_u64().unwrap_or(5126);
        let n = components(acc["type"].as_str().unwrap_or("SCALAR"));
        let count = acc["count"].as_u64().unwrap_or(0) as usize;
        let normalized = acc["normalized"].as_bool() == Some(true);
        let size = match component_type {
            5120 | 5121 => 1,
            5122 | 5123 => 2,
            _ => 4,
        };
        let convert = |v: f32| if normalized { (v / type_max(component_type)).max(-1.0) } else { v };
        let mut values = vec![0.0f32; count * n];
        if let Some(view) = acc["bufferView"].as_u64() {
            let v = &doc["bufferViews"][view as usize];
            let stride = v["byteStride"].as_u64().map_or(n * size, |s| s as usize);
            let start = v["byteOffset"].as_u64().unwrap_or(0) as usize + acc["byteOffset"].as_u64().unwrap_or(0) as usize;
            for i in 0..count {
                for k in 0..n {
                    let at = start + i * stride + k * size;
                    let Some(b) = out.bytes.get(at..at + size) else { continue };
                    let raw = match component_type {
                        5120 => b[0] as i8 as f32,
                        5121 => b[0] as f32,
                        5122 => i16::from_le_bytes([b[0], b[1]]) as f32,
                        5123 => u16::from_le_bytes([b[0], b[1]]) as f32,
                        5125 => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32,
                        _ => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                    };
                    values[i * n + k] = convert(raw);
                }
            }
        }
        let view = out.add_view(doc, &encode(&values, 5126));
        let acc = &mut doc["accessors"][a];
        acc["bufferView"] = json!(view);
        acc["byteOffset"] = json!(0);
        acc["componentType"] = json!(5126);
        if let Some(o) = acc.as_object_mut() {
            o.remove("normalized");
            o.remove("sparse");
        }
        for key in ["min", "max"] {
            if let Some(list) = acc[key].as_array_mut() {
                for v in list.iter_mut() {
                    *v = json!(convert(v.as_f64().unwrap_or(0.0) as f32));
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// KTX2 and PNG

const KTX2_MAGIC: &[u8] = &[0xAB, b'K', b'T', b'X', b' ', b'2', b'0', 0xBB, b'\r', b'\n', 0x1A, b'\n'];

/// A Basis Universal KTX2 texture transcoded to RGBA and written as PNG, at the first mip level
/// no larger than [`MAX_TEXTURE`] (scaled down to it when the file has no such level).
pub fn ktx2_to_png(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let (width, height, rgba) = ktx2_to_rgba(bytes)?;
    let (width, height, rgba) = shrink(width, height, rgba, MAX_TEXTURE);
    Ok(png(width, height, &rgba))
}

/// A Basis Universal KTX2 texture as RGBA pixels (width, height, 4 bytes a pixel).
pub fn ktx2_to_rgba(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let texture = basisu::Transcoder::new(bytes).map_err(|e| format!("KTX2: {e:?}"))?;
    let levels = texture.level_count().max(1);
    let mut level = 0;
    while level + 1 < levels {
        let info = texture.image_level_info(level).map_err(|e| format!("KTX2: {e:?}"))?;
        if info.width.max(info.height) <= MAX_TEXTURE {
            break;
        }
        level += 1;
    }
    let info = texture.image_level_info(level).map_err(|e| format!("KTX2: {e:?}"))?;
    let rgba = texture.transcode(level, basisu::TargetFormat::Rgba32, basisu::DecodeFlags::NONE).map_err(|e| format!("KTX2: {e:?}"))?;
    if rgba.len() < (info.width * info.height * 4) as usize {
        return Err("KTX2: transcoded image too small".into());
    }
    Ok((info.width, info.height, rgba))
}

/// Halves an image (box filter) until its larger side is at most `max`.
fn shrink(mut width: u32, mut height: u32, mut rgba: Vec<u8>, max: u32) -> (u32, u32, Vec<u8>) {
    while width.max(height) > max && width > 1 && height > 1 {
        let (w, h) = (width / 2, height / 2);
        let mut next = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                for c in 0..4 {
                    let at = |dx: u32, dy: u32| rgba[(((y * 2 + dy) * width + x * 2 + dx) * 4 + c) as usize] as u32;
                    next[((y * w + x) * 4 + c) as usize] = ((at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1) + 2) / 4) as u8;
                }
            }
        }
        (width, height, rgba) = (w, h, next);
    }
    (width, height, rgba)
}

/// An RGBA image as PNG (8 bits a channel, stored without compression).
pub fn png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let row = width as usize * 4;
    let mut raw = Vec::with_capacity((row + 1) * height as usize);
    for y in 0..height as usize {
        raw.push(0);
        raw.extend_from_slice(rgba.get(y * row..(y + 1) * row).unwrap_or(&[]));
    }
    // zlib stream with stored blocks
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = if raw.is_empty() { vec![&[][..]] } else { raw.chunks(65_535).collect() };
    for (i, block) in blocks.iter().enumerate() {
        z.push(u8::from(i + 1 == blocks.len()));
        let len = block.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in &raw {
        a = (a + byte as u32) % 65_521;
        b = (b + a) % 65_521;
    }
    z.extend_from_slice(&(b << 16 | a).to_be_bytes());

    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8; 4], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let crc = crc32(&out[start..]);
        out.extend_from_slice(&crc.to_be_bytes());
    };
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(b"IHDR", &header);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { 0xEDB8_8320 ^ (crc >> 1) } else { crc >> 1 };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_has_the_known_crc_of_its_end_chunk() {
        let image = png(2, 1, &[255, 0, 0, 255, 0, 255, 0, 255]);
        assert!(image.starts_with(b"\x89PNG\r\n\x1a\n"));
        // the IEND chunk is always 00 00 00 00 49 45 4E 44 AE 42 60 82
        assert_eq!(&image[image.len() - 12..], &[0, 0, 0, 0, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82]);
    }

    #[test]
    fn base64_and_data_uris() {
        assert_eq!(data_uri("data:application/octet-stream;base64,AAECAw==").unwrap().unwrap(), vec![0, 1, 2, 3]);
        assert!(data_uri("model.bin").is_none());
    }

    #[test]
    fn plain_gltf_needs_no_unpacking() {
        let doc: Value = serde_json::from_str(r#"{"asset":{"version":"2.0"},"extensionsUsed":["KHR_materials_unlit"]}"#).unwrap();
        assert!(!needs_unpack(&doc));
        let doc: Value = serde_json::from_str(r#"{"asset":{"version":"2.0"},"extensionsUsed":["KHR_mesh_quantization"]}"#).unwrap();
        assert!(needs_unpack(&doc));
    }

    /// A quantized triangle (u16 positions, normalized) comes out as floats.
    #[test]
    fn dequantizes_normalized_positions() {
        let mut bin = Vec::new();
        for v in [0u16, 0, 0, 65535, 0, 0, 0, 65535, 0, 0] {
            bin.extend_from_slice(&v.to_le_bytes());
        }
        let doc = json!({
            "asset": {"version": "2.0"}, "extensionsUsed": [QUANTIZATION], "extensionsRequired": [QUANTIZATION],
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}],
            "accessors": [{"bufferView": 0, "componentType": 5123, "normalized": true, "count": 3, "type": "VEC3", "min": [0, 0, 0], "max": [65535, 65535, 0]}],
            "bufferViews": [{"buffer": 0, "byteLength": 18}],
            "buffers": [{"byteLength": 20}]
        });
        let glb = unpack_document(doc, Some(bin), &mut |_| Err("no files".into())).unwrap();
        let (doc, bin) = split(&glb).unwrap();
        assert!(doc.get("extensionsRequired").is_none());
        let acc = &doc["accessors"][0];
        assert_eq!(acc["componentType"], 5126);
        assert_eq!(acc["max"], json!([1.0, 1.0, 0.0]));
        let view = &doc["bufferViews"][acc["bufferView"].as_u64().unwrap() as usize];
        let at = view["byteOffset"].as_u64().unwrap() as usize;
        let bin = bin.unwrap();
        let f = |i: usize| f32::from_le_bytes(bin[at + i * 4..at + i * 4 + 4].try_into().unwrap());
        assert_eq!([f(3), f(4), f(5), f(7)], [1.0, 0.0, 0.0, 1.0]);
    }
}
