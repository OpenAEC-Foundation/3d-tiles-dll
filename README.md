# 3d-tiles-dll

Rust reader for 3D Tiles 1.0 and 1.1 (b3dm, i3dm, pnts, cmpt, glTF tiles, implicit tiling) and
quantized-mesh terrain, for Open BIM Suite and Open 3D Viewer Studio. Pure Rust on bytes, so it
also runs in the browser (WebAssembly).

| Crate | What it holds |
|---|---|
| `tiles-core` | The containers: b3dm, i3dm, pnts and cmpt tiles with their feature tables, implicit tiling's subtree files, quantized-mesh terrain, bounding volumes, the WGS 84 ellipsoid and east-north-up frames. |
| `tiles-model` | The neutral model: tile content as plain glTF with its placements, point clouds and meshes in a local east-north-up frame, and where that frame lies on the earth. |
| `tiles-read` | The reader: a `tileset.json` (1.0 and 1.1, external tilesets, implicit quadtrees and octrees, multiple contents), a folder or zip holding one, a single tile, or quantized-mesh terrain (`layer.json`, `<level>/<x>/<y>.terrain`). |
| `tiles-gltf` | glTF made plain: Draco (`KHR_draco_mesh_compression`), meshopt (`EXT_` and `KHR_meshopt_compression`), `KHR_mesh_quantization` and Basis Universal KTX2 textures (`KHR_texture_basisu`) decoded into one self-contained glTF binary that any glTF 2.0 reader opens. |

## Level of detail

The reader walks the tileset from the root and goes into finer tiles while a tile's geometric
error, seen from a viewpoint that frames the whole tileset in a view 1080 pixels high, is more
than 16 pixels (the defaults of `tiles_read::Options`), within a budget of bytes and tiles. Tiles
are read from disk or a zip; tiles on a server are not read.

## Licence

LGPL-3.0-or-later. Decoders used: `draco-core` (Apache-2.0), `meshopt-pure` (MIT), `basisu`
(Apache-2.0). Test files: see `crates/*/tests/data/NOTICE.md`.
