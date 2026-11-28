//! `Assets.ObjConverter` (subsystems §5.10; the stretch goal of D17): a Wavefront OBJ and its MTL
//! materials converted to one self-contained GLB, which the importer then treats as any other
//! model. The studio never loads OBJ at run time.
//!
//! The OBJ is read with `tobj` (triangulated, one index per vertex). Each `o` or `g` becomes a node
//! of that name, so its parts keep their names. Normals are generated (flat) where the file has
//! none. MTL maps to glTF's metallic-roughness: `Kd` is the base colour, `map_Kd` its texture
//! (PNG or JPEG, read from the OBJ's folder), `d` (or `Tr`) the alpha, `Ns` the roughness
//! (√(2/(Ns+2))), metallic 0; faces are drawn from both sides, since OBJ winding is often
//! inconsistent. OBJ has no unit or axes: the importer asks for both, and [`convert`] can bake a
//! given frame into a root node so that a stand-alone file is in glTF's (metres, +Y up, +Z
//! forward). A materials file or texture that is missing, or named outside the OBJ's folder, is
//! left out with a warning: SABER reads nothing outside the model's folder (D7).

use std::cell::RefCell;
use std::io::BufReader;
use std::path::Path;

use glam::{DQuat, Vec3};

use crate::hash::ContentHash;
use crate::import::mime;
use crate::inspect::{Problem, Severity, uri_problem};
use crate::manifest::ModelFrame;
use crate::write::{ImageSpec, MaterialSpec, MeshData, NodeSpec, write_glb_with};

/// The converter's name and version, as a manifest's `source` records it.
pub fn converter() -> String {
    format!("sim_assets {} (tobj)", env!("CARGO_PKG_VERSION"))
}

/// Whether a path names an OBJ file (by its extension, any case).
pub fn is_obj(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("obj"))
}

/// An OBJ converted to a GLB.
#[derive(Clone, Debug, PartialEq)]
pub struct Converted {
    /// The GLB: one node per object or group, textures inside.
    pub glb: Vec<u8>,
    /// The OBJ's own hash, which the manifest records.
    pub source_hash: ContentHash,
    /// What was left out or made up on the way.
    pub notes: Vec<Problem>,
    /// Triangles drawn.
    pub triangles: u64,
}

/// Convert the OBJ at `path`; its materials file and textures are read from its folder. With
/// `frame`, the file's unit and axes are baked into a root node, so the GLB is in glTF's frame;
/// without, the vertices stay as written and the manifest records the frame.
pub fn convert(path: &Path, frame: Option<&ModelFrame>) -> Result<Converted, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    convert_bytes(&bytes, path.parent(), frame)
}

/// Convert OBJ text whose materials and textures are in `folder` (none: drawn grey).
pub fn convert_bytes(
    obj: &[u8],
    folder: Option<&Path>,
    frame: Option<&ModelFrame>,
) -> Result<Converted, String> {
    let notes = RefCell::new(Vec::new());
    let note = |severity, text: String| notes.borrow_mut().push(Problem::new(severity, text));
    let options = tobj::LoadOptions {
        single_index: true,
        triangulate: true,
        ignore_points: true,
        ignore_lines: true,
    };
    let (models, materials) = tobj::load_obj_buf(&mut BufReader::new(obj), &options, |p| {
        let name = file_name(&p.to_string_lossy());
        match read_beside(&name, folder) {
            Ok(bytes) => tobj::load_mtl_buf(&mut BufReader::new(bytes.as_slice())),
            Err(why) => {
                note(
                    Severity::Warning,
                    format!("the materials file {why}; its materials are left out (drawn grey)"),
                );
                Err(tobj::LoadError::OpenFileFailed)
            }
        }
    })
    .map_err(|e| format!("the OBJ does not parse: {e}"))?;
    let materials = match materials {
        Ok(m) => m,
        Err(tobj::LoadError::OpenFileFailed) => Vec::new(),
        Err(e) => {
            note(
                Severity::Warning,
                format!("the materials file does not parse ({e}); drawn grey"),
            );
            Vec::new()
        }
    };

    // Materials, then one grey for faces without one.
    let mut images: Vec<(String, ImageSpec)> = Vec::new();
    let mut specs: Vec<MaterialSpec> = materials
        .iter()
        .map(|m| {
            let kd = m.diffuse.unwrap_or([0.8, 0.8, 0.8]);
            let alpha = m
                .dissolve
                .or_else(|| {
                    m.unknown_param
                        .get("Tr")
                        .and_then(|t| t.trim().parse::<f32>().ok())
                        .map(|t| 1.0 - t)
                })
                .unwrap_or(1.0)
                .clamp(0.0, 1.0);
            let roughness = m
                .shininess
                .map_or(1.0, |ns| (2.0 / (ns.max(0.0) + 2.0)).sqrt())
                .clamp(0.0, 1.0);
            let texture = m.diffuse_texture.as_deref().and_then(|t| {
                let name = file_name(t);
                if let Some(k) = images.iter().position(|(n, _)| *n == name) {
                    return Some(k);
                }
                let bytes = match read_beside(&name, folder) {
                    Ok(b) => b,
                    Err(why) => {
                        note(
                            Severity::Warning,
                            format!("the texture {why}; {} is drawn without it", m.name),
                        );
                        return None;
                    }
                };
                let Some(kind) = mime(&bytes) else {
                    note(
                        Severity::Warning,
                        format!(
                            "the texture `{name}` is not PNG or JPEG, which the studio cannot load; {} is drawn without it",
                            m.name
                        ),
                    );
                    return None;
                };
                images.push((
                    name.clone(),
                    ImageSpec {
                        name,
                        mime: kind,
                        bytes,
                    },
                ));
                Some(images.len() - 1)
            });
            MaterialSpec {
                name: m.name.clone(),
                base_color: [kd[0], kd[1], kd[2], alpha],
                metallic: 0.0,
                roughness,
                texture,
                blend: alpha < 1.0,
                double_sided: true,
            }
        })
        .collect();
    let grey = specs.len();
    specs.push(MaterialSpec {
        name: "obj_default".into(),
        base_color: [0.8, 0.8, 0.8, 1.0],
        roughness: 0.9,
        double_sided: true,
        ..Default::default()
    });

    let mut nodes = Vec::new();
    let (mut triangles, mut flat) = (0u64, 0usize);
    for model in &models {
        let m = &model.mesh;
        if m.indices.len() < 3 {
            continue;
        }
        let n = m.positions.len() / 3;
        let at = |v: &[f32], i: usize| [v[3 * i], v[3 * i + 1], v[3 * i + 2]];
        let has_uv = m.texcoords.len() == 2 * n;
        let uv = |i: usize| [m.texcoords[2 * i], 1.0 - m.texcoords[2 * i + 1]];
        let mut mesh = MeshData::default();
        if m.normals.len() == 3 * n {
            mesh.positions = (0..n).map(|i| at(&m.positions, i)).collect();
            mesh.normals = (0..n)
                .map(|i| {
                    let v = Vec3::from(at(&m.normals, i)).normalize_or(Vec3::Z);
                    v.to_array()
                })
                .collect();
            if has_uv {
                mesh.uvs = (0..n).map(uv).collect();
            }
            mesh.indices = m.indices.clone();
        } else {
            // Flat normals: each triangle gets its own three vertices.
            flat += 1;
            for t in m.indices.chunks_exact(3) {
                let p = [t[0], t[1], t[2]].map(|i| Vec3::from(at(&m.positions, i as usize)));
                let normal = (p[1] - p[0]).cross(p[2] - p[0]).normalize_or(Vec3::Z);
                for (k, &i) in t.iter().enumerate() {
                    mesh.indices.push(mesh.positions.len() as u32);
                    mesh.positions.push(p[k].to_array());
                    mesh.normals.push(normal.to_array());
                    if has_uv {
                        mesh.uvs.push(uv(i as usize));
                    }
                }
            }
        }
        triangles += (mesh.indices.len() / 3) as u64;
        let material = m.material_id.filter(|k| *k < grey).unwrap_or(grey);
        let name = if model.name.trim().is_empty() {
            format!("object_{}", nodes.len() + 1)
        } else {
            model.name.clone()
        };
        nodes.push(NodeSpec::new(&name).with(mesh, material));
    }
    if triangles == 0 {
        return Err("the OBJ has no faces to draw".into());
    }
    if flat > 0 {
        note(
            Severity::Info,
            format!(
                "{flat} of {} objects had no normals; flat normals were made",
                nodes.len()
            ),
        );
    }
    let roots = match frame {
        None => nodes,
        Some(f) => {
            let (file, gltf) = (
                f.rotation()
                    .ok_or("the up and forward axes are on one line")?,
                ModelFrame::GLTF.rotation().expect("glTF's axes"),
            );
            let turn = DQuat::from_mat3(&(gltf.transpose() * file)).normalize();
            let s = f.unit.metres() as f32;
            let mut root = NodeSpec::new("obj_frame");
            root.rotation = turn.as_quat().to_array();
            root.scale = (s != 1.0).then_some([s; 3]);
            root.children = nodes;
            vec![root]
        }
    };
    let images: Vec<ImageSpec> = images.into_iter().map(|(_, i)| i).collect();
    Ok(Converted {
        glb: write_glb_with(&specs, &images, &roots, &converter()),
        source_hash: ContentHash::of(obj),
        notes: notes.into_inner(),
        triangles,
    })
}

/// A file named in an OBJ or MTL: options before the name dropped (`-s 1 1 1 wood.png`),
/// backslashes read as folder separators.
fn file_name(raw: &str) -> String {
    let raw = raw.trim();
    let name = if raw.starts_with('-') {
        raw.split_whitespace().last().unwrap_or(raw)
    } else {
        raw
    };
    name.replace('\\', "/")
}

/// Read a file the OBJ names from its folder, refusing a URL, an absolute path or one that leaves
/// the folder.
fn read_beside(name: &str, folder: Option<&Path>) -> Result<Vec<u8>, String> {
    if let Some(why) = uri_problem(name) {
        return Err(why);
    }
    let folder = folder.ok_or_else(|| format!("`{name}` has no folder to be read from"))?;
    std::fs::read(folder.join(name)).map_err(|_| format!("`{name}` was not found beside the OBJ"))
}
