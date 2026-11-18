# 3d-tiles-dll

Rust reader for 3D Tiles 1.0 and 1.1 (b3dm, i3dm, pnts, cmpt, glTF tiles, implicit tiling) and
quantized-mesh terrain, for Open BIM Suite and Open 3D Viewer Studio. Pure Rust on bytes, so it
also runs in the browser (WebAssembly).

| Crate | What it holds |
|---|---|
| `tiles-gltf` | glTF made plain: Draco (`KHR_draco_mesh_compression`), meshopt (`EXT_` and `KHR_meshopt_compression`), `KHR_mesh_quantization` and Basis Universal KTX2 textures (`KHR_texture_basisu`) decoded into one self-contained glTF binary that any glTF 2.0 reader opens. |

## Licence

LGPL-3.0-or-later. Decoders used: `draco-core` (Apache-2.0), `meshopt-pure` (MIT), `basisu`
(Apache-2.0). Test files: see `crates/*/tests/data/NOTICE.md`.
