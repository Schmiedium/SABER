//! `Assets.Inspector`: reads a glTF 2.0 file without rendering it (subsystems §5.10): bounds,
//! triangles, materials, node names, extensions, external files, and the import rules' verdicts.
//! SABER fetches nothing (D7), so a remote, absolute or escaping URI is refused, never followed.

use std::borrow::Cow;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use glam::{DMat4, DVec3, Mat4};

use crate::manifest::{Bounds, Unit};

/// Triangles above which a model is over the studio's budget (`sim_studio_ux.md` §11).
pub const TRIANGLE_BUDGET: u64 = 500_000;

/// File size above which a model is over the studio's budget, bytes.
pub const SIZE_BUDGET: u64 = 100 * 1024 * 1024;

/// Required extensions the studio's glTF loader (Bevy 0.20.1) can draw.
pub const SUPPORTED_REQUIRED: [&str; 8] = [
    "KHR_lights_punctual",
    "KHR_materials_emissive_strength",
    "KHR_materials_ior",
    "KHR_materials_specular",
    "KHR_materials_transmission",
    "KHR_materials_unlit",
    "KHR_materials_volume",
    "KHR_texture_transform",
];

/// How much a finding matters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Worth knowing; nothing to do.
    Info,
    /// Allowed, but probably a mistake or over budget.
    Warning,
    /// The file cannot be used as it is.
    Refused,
}

/// One finding about a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    /// How much it matters.
    pub severity: Severity,
    /// What it is, and the fix when there is one.
    pub message: String,
}

impl Problem {
    fn new(severity: Severity, message: impl Into<String>) -> Self {
        Self {
            severity,
            message: message.into(),
        }
    }
}

/// How the file is stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Container {
    /// One binary `.glb`.
    Glb,
    /// JSON `.gltf`, with its data embedded or in separate files.
    Gltf,
}

/// What [`inspect`] found (`ImportReport` in the design).
#[derive(Clone, Debug, PartialEq)]
pub struct ModelReport {
    /// GLB or JSON.
    pub container: Container,
    /// The file's size, bytes (without separate files).
    pub bytes: u64,
    /// The drawn scene's extent in the file's own axes and unit; `None` when nothing is drawn.
    pub bounds: Option<Bounds>,
    /// Triangles drawn (a mesh used twice counts twice).
    pub triangles: u64,
    /// Vertices drawn.
    pub vertices: u64,
    /// Meshes in the file.
    pub meshes: usize,
    /// Materials in the file.
    pub materials: usize,
    /// Images in the file.
    pub images: usize,
    /// The largest image, pixels, when its size could be read.
    pub largest_image: Option<(u32, u32)>,
    /// Node names, in file order (unnamed nodes left out).
    pub nodes: Vec<String>,
    /// Extensions used.
    pub extensions_used: Vec<String>,
    /// Extensions required.
    pub extensions_required: Vec<String>,
    /// Separate files the file refers to, as written.
    pub external: Vec<String>,
    /// Cameras (ignored when drawn).
    pub cameras: usize,
    /// Lights (ignored when drawn).
    pub lights: usize,
    /// Skins (kept, not played in v1).
    pub skins: usize,
    /// Animation clips (kept, not played in v1).
    pub animations: usize,
    /// The import rules' findings.
    pub problems: Vec<Problem>,
}

impl ModelReport {
    /// Whether nothing refuses the file.
    pub fn usable(&self) -> bool {
        self.problems
            .iter()
            .all(|p| p.severity != Severity::Refused)
    }

    /// The findings of one severity.
    pub fn of(&self, severity: Severity) -> impl Iterator<Item = &Problem> {
        self.problems.iter().filter(move |p| p.severity == severity)
    }
}

/// Why a file could not be inspected at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InspectError {
    /// Not a glTF file.
    NotGltf(String),
    /// A glTF, but not version 2.
    Version(String),
}

impl fmt::Display for InspectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InspectError::NotGltf(e) => write!(f, "not a glTF 2.0 file: {e}"),
            InspectError::Version(v) => write!(f, "glTF version {v}; only 2.0 can be imported"),
        }
    }
}

impl std::error::Error for InspectError {}

/// A parsed file with its buffers, for reading triangles.
pub struct GltfData {
    /// The document.
    pub document: gltf::Document,
    /// Each buffer's bytes, `None` where it could not be read.
    pub buffers: Vec<Option<Vec<u8>>>,
    /// GLB or JSON.
    pub container: Container,
    /// Findings while reading.
    pub problems: Vec<Problem>,
}

impl fmt::Debug for GltfData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GltfData")
            .field("container", &self.container)
            .field("buffers", &self.buffers.len())
            .field("problems", &self.problems)
            .finish_non_exhaustive()
    }
}

/// Why a URI cannot be followed, or `None` when it names a file beside the model.
pub fn uri_problem(uri: &str) -> Option<String> {
    let lower = uri.to_ascii_lowercase();
    if lower.starts_with("data:") {
        return None;
    }
    if lower.contains("://") || lower.starts_with("file:") {
        return Some(format!(
            "`{uri}` is a remote or file URL; SABER fetches nothing, so pack the file into a .glb first"
        ));
    }
    let path = percent_decode(uri);
    let p = Path::new(&path);
    let windows_drive = path.len() > 1 && path.as_bytes()[1] == b':';
    if p.is_absolute() || path.starts_with('/') || path.starts_with('\\') || windows_drive {
        return Some(format!(
            "`{uri}` is an absolute path; keep the files beside the model"
        ));
    }
    if p.components().any(|c| matches!(c, Component::ParentDir)) || path.contains("..\\") {
        return Some(format!(
            "`{uri}` leaves the model's folder; keep the files beside the model"
        ));
    }
    None
}

/// Read a glTF or GLB and its buffers. `folder` is where separate files are looked for (a
/// `.gltf`'s folder); without it, separate files are refused.
pub fn read(bytes: &[u8], folder: Option<&Path>) -> Result<GltfData, InspectError> {
    let gltf = gltf::Gltf::from_slice_without_validation(bytes)
        .map_err(|e| InspectError::NotGltf(e.to_string()))?;
    let container = if bytes.starts_with(b"glTF") {
        Container::Glb
    } else {
        Container::Gltf
    };
    let version = gltf.document.as_json().asset.version.clone();
    if !version.starts_with("2.") {
        return Err(InspectError::Version(version));
    }
    let mut problems = Vec::new();
    for ext in gltf.document.extensions_required() {
        if !SUPPORTED_REQUIRED.contains(&ext) {
            problems.push(Problem::new(Severity::Refused, unsupported(ext)));
        }
    }
    if problems.is_empty()
        && let Err(e) = gltf::Document::from_json(gltf.document.clone().into_json())
    {
        problems.push(Problem::new(
            Severity::Refused,
            format!("the file breaks the glTF 2.0 rules: {e}"),
        ));
    }
    let blob = gltf.blob.clone();
    let buffers = gltf
        .document
        .buffers()
        .map(|b| match b.source() {
            gltf::buffer::Source::Bin => match &blob {
                Some(blob) => Some(blob.clone()),
                None => {
                    problems.push(Problem::new(
                        Severity::Refused,
                        "the binary chunk is missing",
                    ));
                    None
                }
            },
            gltf::buffer::Source::Uri(uri) => match load_uri(uri, folder) {
                Ok(data) => Some(data.into_owned()),
                Err(e) => {
                    problems.push(Problem::new(Severity::Refused, e));
                    None
                }
            },
        })
        .collect();
    Ok(GltfData {
        document: gltf.document,
        buffers,
        container,
        problems,
    })
}

fn unsupported(ext: &str) -> String {
    let what = match ext {
        "KHR_draco_mesh_compression" => "Meshes use Draco compression",
        "EXT_meshopt_compression" | "KHR_meshopt_compression" => "Meshes use Meshopt compression",
        "KHR_texture_basisu" => "Textures are KTX2/Basis",
        "EXT_texture_webp" => "Textures are WebP",
        "KHR_mesh_quantization" => "Vertices are quantised",
        _ => return format!("The file requires `{ext}`, which the studio cannot load."),
    };
    format!(
        "{what}, which the studio cannot load. Convert the file with a local tool and import it again."
    )
}

fn load_uri<'a>(uri: &'a str, folder: Option<&Path>) -> Result<Cow<'a, [u8]>, String> {
    if let Some(data) = uri.strip_prefix("data:") {
        let (head, payload) = data
            .split_once(',')
            .ok_or_else(|| format!("a data URI without data: `{}`", short(uri)))?;
        if !head.ends_with(";base64") {
            return Err("a data URI that is not base64".into());
        }
        return base64_decode(payload)
            .map(Cow::Owned)
            .ok_or_else(|| "a data URI whose base64 does not decode".into());
    }
    if let Some(p) = uri_problem(uri) {
        return Err(p);
    }
    let Some(folder) = folder else {
        return Err(format!(
            "`{uri}` is a separate file; pack the model into one .glb first"
        ));
    };
    let path: PathBuf = folder.join(percent_decode(uri));
    std::fs::read(&path)
        .map(Cow::Owned)
        .map_err(|e| format!("`{uri}` could not be read: {e}"))
}

fn short(s: &str) -> &str {
    &s[..s.len().min(40)]
}

/// `%20` and friends decoded; invalid escapes kept as written.
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let escaped = (b[i] == b'%' && i + 2 < b.len())
            .then(|| std::str::from_utf8(&b[i + 1..i + 3]).ok())
            .flatten()
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        if let Some(v) = escaped {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Standard base64 (with or without padding); `None` on any other character.
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn value(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32)
    }
    let clean: Vec<u8> = s.bytes().filter(|c| !c.is_ascii_whitespace()).collect();
    let body = clean
        .strip_suffix(b"==")
        .or_else(|| clean.strip_suffix(b"="))
        .unwrap_or(&clean);
    let mut out = Vec::with_capacity(body.len() * 3 / 4);
    for chunk in body.chunks(4) {
        if chunk.len() == 1 {
            return None;
        }
        let mut acc = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            acc |= value(*c)? << (18 - 6 * i);
        }
        let bytes = acc.to_be_bytes();
        out.extend_from_slice(&bytes[1..chunk.len()]);
    }
    Some(out)
}

/// An image's size from its PNG or JPEG header.
pub fn image_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.len() >= 24 {
        let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
        let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
        return Some((w, h));
    }
    if bytes.starts_with(&[0xFF, 0xD8]) {
        let mut i = 2;
        while i + 9 < bytes.len() {
            if bytes[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = bytes[i + 1];
            let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
            if matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) {
                let h = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
                let w = u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]) as u32;
                return Some((w, h));
            }
            i += 2 + len;
        }
    }
    None
}

/// The drawn scene's nodes with their world matrices, in the file's units and axes.
pub fn drawn_nodes(document: &gltf::Document) -> Vec<(gltf::Node<'_>, DMat4)> {
    fn walk<'a>(n: gltf::Node<'a>, parent: DMat4, out: &mut Vec<(gltf::Node<'a>, DMat4)>) {
        let m = parent * Mat4::from_cols_array_2d(&n.transform().matrix()).as_dmat4();
        out.push((n.clone(), m));
        for c in n.children() {
            walk(c, m, out);
        }
    }
    let mut out = Vec::new();
    let roots: Vec<gltf::Node<'_>> = match document
        .default_scene()
        .or_else(|| document.scenes().next())
    {
        Some(scene) => scene.nodes().collect(),
        None => {
            let children: Vec<usize> = document
                .nodes()
                .flat_map(|n| n.children().map(|c| c.index()).collect::<Vec<_>>())
                .collect();
            document
                .nodes()
                .filter(|n| !children.contains(&n.index()))
                .collect()
        }
    };
    for r in roots {
        walk(r, DMat4::IDENTITY, &mut out);
    }
    out
}

fn primitive_triangles(p: &gltf::Primitive<'_>) -> u64 {
    use gltf::mesh::Mode;
    let n = p
        .indices()
        .map(|a| a.count())
        .or_else(|| p.get(&gltf::Semantic::Positions).map(|a| a.count()))
        .unwrap_or(0) as u64;
    match p.mode() {
        Mode::Triangles => n / 3,
        Mode::TriangleStrip | Mode::TriangleFan => n.saturating_sub(2),
        _ => 0,
    }
}

/// Every drawn triangle's corners, in the file's units and axes.
pub fn triangles(data: &GltfData) -> Vec<[DVec3; 3]> {
    use gltf::mesh::Mode;
    let mut out = Vec::new();
    for (node, m) in drawn_nodes(&data.document) {
        let Some(mesh) = node.mesh() else { continue };
        for p in mesh.primitives() {
            let reader = p.reader(|b| data.buffers.get(b.index()).and_then(|d| d.as_deref()));
            let Some(pos) = reader.read_positions() else {
                continue;
            };
            let pos: Vec<DVec3> = pos
                .map(|v| m.transform_point3(DVec3::new(v[0] as f64, v[1] as f64, v[2] as f64)))
                .collect();
            let idx: Vec<usize> = match reader.read_indices() {
                Some(i) => i.into_u32().map(|i| i as usize).collect(),
                None => (0..pos.len()).collect(),
            };
            let get = |i: usize| pos.get(i).copied();
            let mut push = |a: usize, b: usize, c: usize| {
                if let (Some(a), Some(b), Some(c)) = (get(a), get(b), get(c)) {
                    out.push([a, b, c]);
                }
            };
            match p.mode() {
                Mode::Triangles => idx.chunks_exact(3).for_each(|t| push(t[0], t[1], t[2])),
                Mode::TriangleStrip => idx.windows(3).enumerate().for_each(|(k, t)| {
                    if k % 2 == 0 {
                        push(t[0], t[1], t[2]);
                    } else {
                        push(t[1], t[0], t[2]);
                    }
                }),
                Mode::TriangleFan => {
                    for k in 1..idx.len().saturating_sub(1) {
                        push(idx[0], idx[k], idx[k + 1]);
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// Inspect a glTF or GLB file's bytes (`folder`: where a `.gltf`'s separate files are).
pub fn inspect(bytes: &[u8], folder: Option<&Path>) -> Result<ModelReport, InspectError> {
    let data = read(bytes, folder)?;
    let doc = &data.document;
    let mut problems = data.problems.clone();
    let mut external = Vec::new();
    for b in doc.buffers() {
        if let gltf::buffer::Source::Uri(uri) = b.source()
            && !uri.starts_with("data:")
        {
            external.push(uri.to_owned());
        }
    }
    let mut largest: Option<(u32, u32)> = None;
    for img in doc.images() {
        let bytes: Option<Cow<'_, [u8]>> = match img.source() {
            gltf::image::Source::View { view, .. } => data
                .buffers
                .get(view.buffer().index())
                .and_then(|b| b.as_deref())
                .and_then(|b| b.get(view.offset()..view.offset() + view.length()))
                .map(Cow::Borrowed),
            gltf::image::Source::Uri { uri, .. } => {
                if !uri.starts_with("data:") {
                    external.push(uri.to_owned());
                }
                match load_uri(uri, folder) {
                    Ok(b) => Some(b),
                    Err(e) => {
                        problems.push(Problem::new(Severity::Refused, e));
                        None
                    }
                }
            }
        };
        if let Some(size) = bytes.as_deref().and_then(image_size) {
            let area = |s: (u32, u32)| s.0 as u64 * s.1 as u64;
            if largest.is_none_or(|l| area(size) > area(l)) {
                largest = Some(size);
            }
        }
    }
    let drawn = drawn_nodes(doc);
    let mut triangles = 0u64;
    let mut vertices = 0u64;
    let mut corners = Vec::new();
    for (node, m) in &drawn {
        let Some(mesh) = node.mesh() else { continue };
        for p in mesh.primitives() {
            triangles += primitive_triangles(&p);
            let reader = p.reader(|b| data.buffers.get(b.index()).and_then(|d| d.as_deref()));
            if let Some(pos) = reader.read_positions() {
                for v in pos {
                    vertices += 1;
                    corners.push(m.transform_point3(DVec3::new(
                        v[0] as f64,
                        v[1] as f64,
                        v[2] as f64,
                    )));
                }
            }
        }
    }
    let cameras = doc.cameras().count();
    let lights = doc.lights().map(|l| l.count()).unwrap_or(0);
    let skins = doc.skins().count();
    let animations = doc.animations().count();
    if cameras + lights > 0 {
        problems.push(Problem::new(
            Severity::Info,
            format!(
                "{} in the file will be ignored",
                counted(&[(cameras, "camera"), (lights, "light")])
            ),
        ));
    }
    if skins + animations > 0 {
        problems.push(Problem::new(
            Severity::Info,
            format!(
                "{} are kept but not played",
                counted(&[(skins, "skin"), (animations, "animation clip")])
            ),
        ));
    }
    if triangles > TRIANGLE_BUDGET {
        problems.push(Problem::new(
            Severity::Warning,
            format!(
                "{} triangles is over the studio's budget of {} k; consider a lighter model or an LOD",
                triangles,
                TRIANGLE_BUDGET / 1000
            ),
        ));
    }
    if bytes.len() as u64 > SIZE_BUDGET {
        problems.push(Problem::new(
            Severity::Warning,
            format!(
                "{} MB is over the studio's budget of {} MB",
                bytes.len() / (1024 * 1024),
                SIZE_BUDGET / (1024 * 1024)
            ),
        ));
    }
    let mut nodes = Vec::new();
    for n in doc.nodes() {
        if let Some(name) = n.name()
            && !nodes.iter().any(|x| x == name)
        {
            nodes.push(name.to_owned());
        }
    }
    Ok(ModelReport {
        container: data.container,
        bytes: bytes.len() as u64,
        bounds: Bounds::around(corners),
        triangles,
        vertices,
        meshes: doc.meshes().count(),
        materials: doc.materials().count(),
        images: doc.images().count(),
        largest_image: largest,
        nodes,
        extensions_used: doc.extensions_used().map(str::to_owned).collect(),
        extensions_required: doc.extensions_required().map(str::to_owned).collect(),
        external,
        cameras,
        lights,
        skins,
        animations,
        problems,
    })
}

fn counted(items: &[(usize, &str)]) -> String {
    items
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, what)| format!("{n} {what}{}", if *n == 1 { "" } else { "s" }))
        .collect::<Vec<_>>()
        .join(" and ")
}

/// The largest extent a model may have and still look plausible, m.
pub const PLAUSIBLE_MAX_M: f64 = 1000.0;

/// The smallest largest-extent a model may have and still look plausible, m.
pub const PLAUSIBLE_MIN_M: f64 = 0.001;

/// A warning when a file's largest extent at `unit` is under 1 mm or over 1 km, with the unit that
/// would make it plausible.
pub fn unit_problem(bounds: &Bounds, unit: Unit) -> Option<Problem> {
    let extent = bounds.size().max_element();
    let metres = extent * unit.metres();
    if !(metres.is_finite() && extent > 0.0)
        || (PLAUSIBLE_MIN_M..=PLAUSIBLE_MAX_M).contains(&metres)
    {
        return None;
    }
    let guess = guess_unit(bounds);
    Some(Problem::new(
        Severity::Warning,
        format!(
            "The model is {} long in {}s. Looks like {}s.",
            show_length(metres),
            unit.name(),
            guess.name()
        ),
    ))
}

/// The unit that makes a file's largest extent closest to a few metres.
pub fn guess_unit(bounds: &Bounds) -> Unit {
    let extent = bounds.size().max_element();
    let score = |u: Unit| {
        let m = extent * u.metres();
        if (PLAUSIBLE_MIN_M..=PLAUSIBLE_MAX_M).contains(&m) {
            (m.log10() - 0.5).abs()
        } else {
            f64::INFINITY
        }
    };
    // Prefer metres, then millimetres (the usual CAD export), when several are plausible.
    let preferred = [Unit::Metre, Unit::Millimetre];
    for u in preferred {
        let m = extent * u.metres();
        if (0.05..=200.0).contains(&m) {
            return u;
        }
    }
    Unit::ALL
        .into_iter()
        .min_by(|a, b| score(*a).total_cmp(&score(*b)))
        .unwrap_or(Unit::Metre)
}

fn show_length(m: f64) -> String {
    if m >= 1000.0 {
        format!("{:.1} km", m / 1000.0)
    } else if m >= 1.0 {
        format!("{m:.2} m")
    } else {
        format!("{:.2} mm", m * 1000.0)
    }
}
