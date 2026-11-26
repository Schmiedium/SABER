//! Writing GLB files: the built-in library's generator and the OBJ converter (subsystems §5.10).
//! Output is deterministic: the same meshes give the same bytes, so the same content hash. Keys a
//! file does not use (texture coordinates, textures, blending, scale) are left out, so the
//! built-ins' bytes do not depend on them.

use serde_json::{Value, json};

/// One triangle list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeshData {
    /// Vertex positions.
    pub positions: Vec<[f32; 3]>,
    /// Vertex normals (unit length), one per position.
    pub normals: Vec<[f32; 3]>,
    /// Three indices per triangle, counter-clockwise seen from outside.
    pub indices: Vec<u32>,
    /// Texture coordinates, one per position, or none (glTF's: v down from the top).
    pub uvs: Vec<[f32; 2]>,
}

impl MeshData {
    /// Append another mesh.
    pub fn extend(&mut self, other: &MeshData) {
        let base = self.positions.len() as u32;
        self.positions.extend_from_slice(&other.positions);
        self.normals.extend_from_slice(&other.normals);
        self.uvs.extend_from_slice(&other.uvs);
        self.indices.extend(other.indices.iter().map(|i| i + base));
    }

    /// Triangles.
    pub fn triangles(&self) -> usize {
        self.indices.len() / 3
    }
}

/// A metallic-roughness material with a flat colour, or a texture.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MaterialSpec {
    /// Its name in the file.
    pub name: String,
    /// Linear RGBA.
    pub base_color: [f32; 4],
    /// 0 dielectric, 1 metal.
    pub metallic: f32,
    /// 0 mirror, 1 matte.
    pub roughness: f32,
    /// The base colour texture: an index into the images.
    pub texture: Option<usize>,
    /// Blend by the base colour's alpha (`alphaMode: BLEND`).
    pub blend: bool,
    /// Draw both sides of each triangle.
    pub double_sided: bool,
}

/// An image stored in the file.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageSpec {
    /// Its name in the file.
    pub name: String,
    /// `image/png` or `image/jpeg`.
    pub mime: &'static str,
    /// The encoded image.
    pub bytes: Vec<u8>,
}

/// A node: a name, a pose relative to its parent, its triangles and its children.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NodeSpec {
    /// Its name (the conventional names bindings expect: `rotor_1`, `elevator`, …).
    pub name: String,
    /// Translation from the parent.
    pub translation: [f32; 3],
    /// Rotation from the parent, `(x, y, z, w)`.
    pub rotation: [f32; 4],
    /// Scale, applied before the rotation; `None` is 1.
    pub scale: Option<[f32; 3]>,
    /// Triangle lists with the index of their material.
    pub primitives: Vec<(MeshData, usize)>,
    /// Child nodes.
    pub children: Vec<NodeSpec>,
}

impl NodeSpec {
    /// A node at the parent's origin.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            rotation: [0.0, 0.0, 0.0, 1.0],
            ..Default::default()
        }
    }

    /// With a translation.
    pub fn at(mut self, t: [f32; 3]) -> Self {
        self.translation = t;
        self
    }

    /// With triangles in a material.
    pub fn with(mut self, mesh: MeshData, material: usize) -> Self {
        self.primitives.push((mesh, material));
        self
    }

    /// With a child.
    pub fn child(mut self, node: NodeSpec) -> Self {
        self.children.push(node);
        self
    }
}

struct Writer {
    bin: Vec<u8>,
    views: Vec<Value>,
    accessors: Vec<Value>,
    meshes: Vec<Value>,
    nodes: Vec<Value>,
}

const ARRAY_BUFFER: u32 = 34962;
const ELEMENT_ARRAY_BUFFER: u32 = 34963;
const FLOAT: u32 = 5126;
const UNSIGNED_INT: u32 = 5125;

impl Writer {
    fn view(&mut self, bytes: &[u8], target: u32) -> usize {
        while !self.bin.len().is_multiple_of(4) {
            self.bin.push(0);
        }
        let offset = self.bin.len();
        self.bin.extend_from_slice(bytes);
        self.views.push(json!({
            "buffer": 0,
            "byteOffset": offset,
            "byteLength": bytes.len(),
            "target": target,
        }));
        self.views.len() - 1
    }

    fn vec3(&mut self, data: &[[f32; 3]], bounds: bool) -> usize {
        let bytes: Vec<u8> = data
            .iter()
            .flatten()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let view = self.view(&bytes, ARRAY_BUFFER);
        let mut acc = json!({
            "bufferView": view,
            "componentType": FLOAT,
            "count": data.len(),
            "type": "VEC3",
        });
        if bounds {
            let lo = (0..3).map(|i| data.iter().map(|v| v[i]).fold(f32::INFINITY, f32::min));
            let hi = (0..3).map(|i| data.iter().map(|v| v[i]).fold(f32::NEG_INFINITY, f32::max));
            acc["min"] = json!(lo.collect::<Vec<_>>());
            acc["max"] = json!(hi.collect::<Vec<_>>());
        }
        self.accessors.push(acc);
        self.accessors.len() - 1
    }

    fn vec2(&mut self, data: &[[f32; 2]]) -> usize {
        let bytes: Vec<u8> = data
            .iter()
            .flatten()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let view = self.view(&bytes, ARRAY_BUFFER);
        self.accessors.push(json!({
            "bufferView": view,
            "componentType": FLOAT,
            "count": data.len(),
            "type": "VEC2",
        }));
        self.accessors.len() - 1
    }

    fn indices(&mut self, data: &[u32]) -> usize {
        let bytes: Vec<u8> = data.iter().flat_map(|v| v.to_le_bytes()).collect();
        let view = self.view(&bytes, ELEMENT_ARRAY_BUFFER);
        self.accessors.push(json!({
            "bufferView": view,
            "componentType": UNSIGNED_INT,
            "count": data.len(),
            "type": "SCALAR",
        }));
        self.accessors.len() - 1
    }

    fn node(&mut self, n: &NodeSpec) -> usize {
        let children: Vec<usize> = n.children.iter().map(|c| self.node(c)).collect();
        let mut node = json!({ "name": n.name });
        if n.translation != [0.0; 3] {
            node["translation"] = json!(n.translation);
        }
        if n.rotation != [0.0, 0.0, 0.0, 1.0] {
            node["rotation"] = json!(n.rotation);
        }
        if let Some(scale) = n.scale {
            node["scale"] = json!(scale);
        }
        if !n.primitives.is_empty() {
            let prims: Vec<Value> = n
                .primitives
                .iter()
                .map(|(m, material)| {
                    let pos = self.vec3(&m.positions, true);
                    let nor = self.vec3(&m.normals, false);
                    let mut attributes = json!({ "POSITION": pos, "NORMAL": nor });
                    if !m.uvs.is_empty() {
                        attributes["TEXCOORD_0"] = json!(self.vec2(&m.uvs));
                    }
                    let idx = self.indices(&m.indices);
                    json!({
                        "attributes": attributes,
                        "indices": idx,
                        "material": material,
                    })
                })
                .collect();
            self.meshes
                .push(json!({ "name": n.name, "primitives": prims }));
            node["mesh"] = json!(self.meshes.len() - 1);
        }
        if !children.is_empty() {
            node["children"] = json!(children);
        }
        self.nodes.push(node);
        self.nodes.len() - 1
    }
}

/// A GLB file of these nodes (one scene) and materials.
pub fn write_glb(materials: &[MaterialSpec], roots: &[NodeSpec], generator: &str) -> Vec<u8> {
    write_glb_with(materials, &[], roots, generator)
}

/// A GLB file of these nodes, materials and images (stored after the meshes).
pub fn write_glb_with(
    materials: &[MaterialSpec],
    images: &[ImageSpec],
    roots: &[NodeSpec],
    generator: &str,
) -> Vec<u8> {
    let mut w = Writer {
        bin: Vec::new(),
        views: Vec::new(),
        accessors: Vec::new(),
        meshes: Vec::new(),
        nodes: Vec::new(),
    };
    let roots: Vec<usize> = roots.iter().map(|n| w.node(n)).collect();
    let images: Vec<Value> = images
        .iter()
        .map(|i| {
            while !w.bin.len().is_multiple_of(4) {
                w.bin.push(0);
            }
            let offset = w.bin.len();
            w.bin.extend_from_slice(&i.bytes);
            w.views.push(json!({
                "buffer": 0,
                "byteOffset": offset,
                "byteLength": i.bytes.len(),
            }));
            json!({ "name": i.name, "bufferView": w.views.len() - 1, "mimeType": i.mime })
        })
        .collect();
    while !w.bin.len().is_multiple_of(4) {
        w.bin.push(0);
    }
    let materials: Vec<Value> = materials
        .iter()
        .map(|m| {
            let mut v = json!({
                "name": m.name,
                "pbrMetallicRoughness": {
                    "baseColorFactor": m.base_color,
                    "metallicFactor": m.metallic,
                    "roughnessFactor": m.roughness,
                },
            });
            if let Some(t) = m.texture {
                v["pbrMetallicRoughness"]["baseColorTexture"] = json!({ "index": t });
            }
            if m.blend {
                v["alphaMode"] = json!("BLEND");
            }
            if m.double_sided {
                v["doubleSided"] = json!(true);
            }
            v
        })
        .collect();
    let mut doc = json!({
        "asset": { "version": "2.0", "generator": generator },
        "scene": 0,
        "scenes": [{ "nodes": roots }],
        "nodes": w.nodes,
        "meshes": w.meshes,
        "materials": materials,
        "accessors": w.accessors,
        "bufferViews": w.views,
        "buffers": [{ "byteLength": w.bin.len() }],
    });
    if !images.is_empty() {
        // One texture per image, with the default sampler (repeat, linear).
        doc["textures"] = json!(
            (0..images.len())
                .map(|i| json!({ "source": i }))
                .collect::<Vec<_>>()
        );
        doc["images"] = json!(images);
    }
    let text = serde_json::to_vec(&doc).expect("a glTF document serialises");
    glb_container(&text, &w.bin)
}

/// The GLB container around a JSON chunk and a binary chunk (glTF 2.0 §4.4).
pub fn glb_container(json: &[u8], bin: &[u8]) -> Vec<u8> {
    let pad = |n: usize| n.div_ceil(4) * 4;
    let (jl, bl) = (pad(json.len()), pad(bin.len()));
    let total = 12 + 8 + jl + if bin.is_empty() { 0 } else { 8 + bl };
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(jl as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(json);
    out.resize(out.len() + jl - json.len(), b' ');
    if !bin.is_empty() {
        out.extend_from_slice(&(bl as u32).to_le_bytes());
        out.extend_from_slice(b"BIN\0");
        out.extend_from_slice(bin);
        out.resize(out.len() + bl - bin.len(), 0);
    }
    out
}
