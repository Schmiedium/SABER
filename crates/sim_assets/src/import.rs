//! `Assets.Importer` (subsystems §5.10; `sequence-diagrams.md` §2.1): reads a glTF or GLB, applies
//! the import rules, packs a `.gltf` and its separate files into one `.glb`, works out the manifest
//! from the chosen unit, axes and origin, and writes `<models>/<slug>/{model.glb, model.ron,
//! thumb.png}`. The studio's Import dialog and `saber-cli model import` both use it.

use std::fmt;
use std::path::{Path, PathBuf};

use glam::DVec3;
use serde_json::{Value, json};

use crate::catalog::CatalogEntry;
use crate::hash::ContentHash;
use crate::inspect::{
    Container, InspectError, ModelReport, Problem, Severity, guess_unit, inspect, load_uri, read,
    unit_problem,
};
use crate::manifest::{
    Axis, Bounds, FileSource, MANIFEST_FILE, MANIFEST_SCHEMA_VERSION, ModelFrame, Origin,
    SourceFormat, Unit, VisualAssetManifest,
};
use crate::thumb::{self, Drawn, Extras, Facet, THUMB_FILE, View};
use crate::write::glb_container;

/// The stored file's name in a model's folder.
pub const MODEL_FILE: &str = "model.glb";

/// The license an import records until one is given.
pub const UNSPECIFIED_LICENSE: &str = "unspecified";

/// Where the body's reference point is put.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OriginChoice {
    /// The file's own origin.
    FileOrigin,
    /// The centre of the model's bounds.
    BoundsCentre,
    /// The centre of the bounds' bottom face (largest z, since z points down).
    BottomCentre,
    /// A point, body frame, metres (before the origin is applied).
    Point([f64; 3]),
}

impl OriginChoice {
    /// The choices the dialog offers by name.
    pub const NAMED: [OriginChoice; 3] = [
        OriginChoice::FileOrigin,
        OriginChoice::BoundsCentre,
        OriginChoice::BottomCentre,
    ];

    /// Its name.
    pub fn label(&self) -> &'static str {
        match self {
            OriginChoice::FileOrigin => "File origin",
            OriginChoice::BoundsCentre => "Bounding-box centre",
            OriginChoice::BottomCentre => "Bottom centre",
            OriginChoice::Point(_) => "Picked point",
        }
    }

    /// Parse `file`, `centre`, `bottom` or `x,y,z`.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "file" | "file-origin" => Some(Self::FileOrigin),
            "centre" | "center" | "bounds" => Some(Self::BoundsCentre),
            "bottom" => Some(Self::BottomCentre),
            other => {
                let v: Vec<f64> = other
                    .split(',')
                    .map(|p| p.trim().parse().ok())
                    .collect::<Option<_>>()?;
                (v.len() == 3 && v.iter().all(|x| x.is_finite()))
                    .then(|| Self::Point([v[0], v[1], v[2]]))
            }
        }
    }

    /// The origin offset that puts the reference point here, for a model with these bounds
    /// (body frame, metres, at the file's origin).
    pub fn origin(&self, bounds: &Bounds) -> Origin {
        let c = bounds.center();
        let p = match self {
            OriginChoice::FileOrigin => return Origin::FileOrigin,
            OriginChoice::BoundsCentre => c,
            OriginChoice::BottomCentre => DVec3::new(c.x, c.y, bounds.max[2]),
            OriginChoice::Point(p) => DVec3::from(*p),
        };
        Origin::Offset((-p).to_array().map(|v| if v == 0.0 { 0.0 } else { v }))
    }
}

/// What the importer is told (the dialog's fields, or the command line's flags).
#[derive(Clone, Debug, PartialEq)]
pub struct ImportSettings {
    /// The display name; the folder and id come from it ([`slug`]).
    pub name: String,
    /// The file's unit and axes.
    pub frame: ModelFrame,
    /// Where the reference point goes.
    pub origin: OriginChoice,
    /// The license recorded in the manifest.
    pub license: String,
    /// Credit, when the license asks for it.
    pub attribution: Option<String>,
}

/// Why an import failed.
#[derive(Clone, Debug, PartialEq)]
pub enum ImportError {
    /// The file could not be read.
    Read(String),
    /// The file is not a glTF 2.0 file.
    Inspect(InspectError),
    /// The import rules refuse it; each reason has its fix.
    Refused(Vec<String>),
    /// The settings cannot make a manifest.
    Invalid(String),
    /// A model with this folder already exists.
    Exists(PathBuf),
    /// The files could not be written.
    Write(String),
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportError::Read(e) => write!(f, "the file could not be read: {e}"),
            ImportError::Inspect(e) => write!(f, "{e}"),
            ImportError::Refused(r) => write!(f, "refused: {}", r.join("; ")),
            ImportError::Invalid(e) => write!(f, "{e}"),
            ImportError::Exists(p) => write!(
                f,
                "{} already exists; choose another name or replace it",
                p.display()
            ),
            ImportError::Write(e) => write!(f, "the model could not be written: {e}"),
        }
    }
}

impl std::error::Error for ImportError {}

/// A folder and id name from a display name: lower case letters, digits and `_`.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    let out = out.trim_end_matches('_').to_owned();
    if out.is_empty() { "model".into() } else { out }
}

/// A file opened for import: read, inspected and ready to preview.
#[derive(Clone, Debug)]
pub struct Candidate {
    /// The file.
    pub path: PathBuf,
    /// Its bytes.
    pub bytes: Vec<u8>,
    /// What the inspector found.
    pub report: ModelReport,
    /// Its drawn triangles with colours, in the file's units and axes.
    pub facets: Vec<Facet>,
}

impl Candidate {
    /// Read and inspect a `.glb` or `.gltf` (separate files are looked for beside it).
    pub fn open(path: &Path) -> Result<Self, ImportError> {
        let bytes = std::fs::read(path).map_err(|e| ImportError::Read(e.to_string()))?;
        Self::from_bytes(path, bytes)
    }

    /// Inspect bytes as if read from `path`.
    pub fn from_bytes(path: &Path, bytes: Vec<u8>) -> Result<Self, ImportError> {
        let folder = path.parent();
        let report = inspect(&bytes, folder).map_err(ImportError::Inspect)?;
        let data = read(&bytes, folder).map_err(ImportError::Inspect)?;
        let facets = thumb::facets(&data);
        Ok(Self {
            path: path.to_owned(),
            bytes,
            report,
            facets,
        })
    }

    /// The settings to start from: the file's name, the unit its size suggests, glTF's axes
    /// (+Y up, +Z forward) and the bounds' centre as the reference point.
    pub fn defaults(&self) -> ImportSettings {
        let name = self
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "model".into());
        let unit = self
            .report
            .bounds
            .as_ref()
            .map(guess_unit)
            .unwrap_or(Unit::Metre);
        ImportSettings {
            name,
            frame: ModelFrame {
                unit,
                ..ModelFrame::GLTF
            },
            origin: OriginChoice::BoundsCentre,
            license: UNSPECIFIED_LICENSE.into(),
            attribution: None,
        }
    }

    /// The model's bounds in the body frame at the file's origin, or `None` when the axes are on
    /// one line or nothing is drawn.
    fn raw_bounds(&self, frame: &ModelFrame) -> Option<Bounds> {
        frame.rotation()?;
        Bounds::around(
            self.facets
                .iter()
                .flat_map(|(t, _)| t.iter())
                .filter_map(|p| frame.to_body(*p)),
        )
    }

    /// The origin these settings give.
    pub fn origin(&self, s: &ImportSettings) -> Option<Origin> {
        self.raw_bounds(&s.frame).map(|b| s.origin.origin(&b))
    }

    /// The model's bounds in the body frame with these settings, metres.
    pub fn bounds(&self, s: &ImportSettings) -> Option<Bounds> {
        let raw = self.raw_bounds(&s.frame)?;
        let o = s.origin.origin(&raw).offset();
        Some(Bounds {
            min: (DVec3::from(raw.min) + o).to_array(),
            max: (DVec3::from(raw.max) + o).to_array(),
        })
    }

    /// The facets in the body frame with these settings.
    pub fn body_facets(&self, s: &ImportSettings) -> Vec<Facet> {
        let Some(o) = self.origin(s).map(|o| o.offset()) else {
            return Vec::new();
        };
        self.facets
            .iter()
            .filter_map(|(t, c)| {
                let [a, b, cc] = t.map(|p| s.frame.to_body(p));
                Some(([a? + o, b? + o, cc? + o], *c))
            })
            .collect()
    }

    /// The preview: the model in the body frame with the axis triad and a reference bar.
    pub fn preview(&self, s: &ImportSettings, size: u32) -> Drawn {
        thumb::draw(
            &self.body_facets(s),
            size,
            View::THUMB,
            Extras {
                axes: true,
                bar: true,
            },
        )
    }

    /// The inspector's findings and the ones these settings add.
    pub fn problems(&self, s: &ImportSettings) -> Vec<Problem> {
        let mut out = self.report.problems.clone();
        if s.frame.rotation().is_none() {
            out.push(Problem::new(
                Severity::Refused,
                format!(
                    "Up {} and forward {} are on one line; choose axes at right angles.",
                    s.frame.up.label(),
                    s.frame.forward.label()
                ),
            ));
        }
        if let Some(p) = self
            .report
            .bounds
            .as_ref()
            .and_then(|b| unit_problem(b, s.frame.unit))
        {
            out.push(p);
        }
        if s.name.trim().is_empty() {
            out.push(Problem::new(Severity::Refused, "Give the model a name."));
        }
        if s.license.trim().is_empty() {
            out.push(Problem::new(
                Severity::Refused,
                format!("Give a license, or `{UNSPECIFIED_LICENSE}`."),
            ));
        }
        out.sort_by_key(|p| std::cmp::Reverse(p.severity));
        out
    }

    /// The manifest these settings give for the packed file (`id_prefix`: `models/` in a
    /// workspace, empty in a library root).
    pub fn manifest(
        &self,
        s: &ImportSettings,
        glb: &[u8],
        id_prefix: &str,
    ) -> Result<VisualAssetManifest, ImportError> {
        let bounds = self
            .bounds(s)
            .ok_or_else(|| ImportError::Invalid("nothing in the file is drawn".into()))?;
        let origin = self
            .origin(s)
            .ok_or_else(|| ImportError::Invalid("the axes are on one line".into()))?;
        let hash = ContentHash::of(glb);
        let source = if glb == self.bytes.as_slice() {
            FileSource::Native
        } else {
            FileSource::Converted {
                format: SourceFormat::Gltf,
                hash: ContentHash::of(&self.bytes),
                converter: format!("sim_assets {}", env!("CARGO_PKG_VERSION")),
            }
        };
        let m = VisualAssetManifest {
            schema_version: MANIFEST_SCHEMA_VERSION.to_string(),
            id: format!("{id_prefix}{}", slug(&s.name)),
            version: 1,
            display_name: s.name.trim().to_owned(),
            file: MODEL_FILE.into(),
            content_hash: hash,
            source,
            frame: s.frame,
            origin,
            bounds,
            triangles: self.report.triangles,
            bindings: Vec::new(),
            lods: Vec::new(),
            suggested_collision: None,
            license: s.license.trim().to_owned(),
            attribution: s.attribution.clone().filter(|a| !a.trim().is_empty()),
        };
        m.check().map_err(|e| ImportError::Invalid(e.to_string()))?;
        Ok(m)
    }

    /// Import into `models_dir/<slug>/`: refused when a rule refuses the file or the settings, or
    /// when the folder exists and `replace` is false.
    pub fn commit(
        &self,
        s: &ImportSettings,
        models_dir: &Path,
        id_prefix: &str,
        replace: bool,
    ) -> Result<Imported, ImportError> {
        let refused: Vec<String> = self
            .problems(s)
            .into_iter()
            .filter(|p| p.severity == Severity::Refused)
            .map(|p| p.message)
            .collect();
        if !refused.is_empty() {
            return Err(ImportError::Refused(refused));
        }
        let glb =
            pack(&self.bytes, self.path.parent()).map_err(|e| ImportError::Refused(vec![e]))?;
        let manifest = self.manifest(s, &glb, id_prefix)?;
        let dir = models_dir.join(slug(&s.name));
        if dir.exists() && !replace {
            return Err(ImportError::Exists(dir));
        }
        let data = read(&glb, None).map_err(ImportError::Inspect)?;
        let thumb = thumb::thumbnail(&data, &manifest);
        write_model(&dir, &glb, &manifest, Some(&thumb))?;
        Ok(Imported { dir, manifest })
    }
}

/// A model written by the importer.
#[derive(Clone, Debug, PartialEq)]
pub struct Imported {
    /// Its folder.
    pub dir: PathBuf,
    /// Its manifest.
    pub manifest: VisualAssetManifest,
}

fn write_model(
    dir: &Path,
    glb: &[u8],
    manifest: &VisualAssetManifest,
    thumb: Option<&[u8]>,
) -> Result<(), ImportError> {
    let w = |e: std::io::Error| ImportError::Write(format!("{}: {e}", dir.display()));
    std::fs::create_dir_all(dir).map_err(w)?;
    std::fs::write(dir.join(&manifest.file), glb).map_err(w)?;
    if let Some(t) = thumb {
        std::fs::write(dir.join(THUMB_FILE), t).map_err(w)?;
    }
    // The manifest last: a folder is listed only once its `model.ron` is there.
    std::fs::write(dir.join(MANIFEST_FILE), manifest.to_ron()).map_err(w)?;
    Ok(())
}

/// Copy a catalogue model into `models_dir/<slug>/` (Duplicate to workspace, to customise a
/// built-in or keep a library model with the workspace).
pub fn duplicate(
    entry: &CatalogEntry,
    models_dir: &Path,
    slug_name: &str,
    id_prefix: &str,
) -> Result<Imported, ImportError> {
    let loaded = entry.load().map_err(|e| ImportError::Read(e.to_string()))?;
    let dir = models_dir.join(slug(slug_name));
    if dir.exists() {
        return Err(ImportError::Exists(dir));
    }
    let mut manifest = entry.manifest.clone();
    manifest.id = format!("{id_prefix}{}", slug(slug_name));
    manifest.version = 1;
    manifest.content_hash = loaded.hash;
    let thumb = match entry.thumb() {
        Some(t) => t,
        None => {
            let data = read(&loaded.bytes, None).map_err(ImportError::Inspect)?;
            thumb::thumbnail(&data, &manifest)
        }
    };
    write_model(&dir, &loaded.bytes, &manifest, Some(&thumb))?;
    Ok(Imported { dir, manifest })
}

/// Accept a model whose file changed since it was imported (Re-import): the manifest takes the
/// file's hash, bounds and triangle count; its frame and origin stay; the thumbnail is redrawn.
pub fn refresh(dir: &Path) -> Result<Imported, ImportError> {
    let text = std::fs::read_to_string(dir.join(MANIFEST_FILE))
        .map_err(|e| ImportError::Read(e.to_string()))?;
    let mut manifest =
        VisualAssetManifest::from_ron(&text).map_err(|e| ImportError::Invalid(e.to_string()))?;
    let bytes =
        std::fs::read(dir.join(&manifest.file)).map_err(|e| ImportError::Read(e.to_string()))?;
    let report = inspect(&bytes, Some(dir)).map_err(ImportError::Inspect)?;
    let refused: Vec<String> = report
        .of(Severity::Refused)
        .map(|p| p.message.clone())
        .collect();
    if !refused.is_empty() {
        return Err(ImportError::Refused(refused));
    }
    let data = read(&bytes, Some(dir)).map_err(ImportError::Inspect)?;
    let corners = thumb::body_facets(&data, &manifest);
    manifest.bounds = Bounds::around(corners.iter().flat_map(|(t, _)| t.iter().copied()))
        .ok_or_else(|| ImportError::Invalid("nothing in the file is drawn".into()))?;
    manifest.triangles = report.triangles;
    manifest.content_hash = ContentHash::of(&bytes);
    let thumb = thumb::draw(&corners, thumb::THUMB_PX, View::THUMB, Extras::default())
        .picture
        .png();
    let w = |e: std::io::Error| ImportError::Write(format!("{}: {e}", dir.display()));
    std::fs::write(dir.join(THUMB_FILE), thumb).map_err(w)?;
    std::fs::write(dir.join(MANIFEST_FILE), manifest.to_ron()).map_err(w)?;
    Ok(Imported {
        dir: dir.to_owned(),
        manifest,
    })
}

/// The JSON and binary chunks of a GLB, or the whole text of a `.gltf`.
fn chunks(bytes: &[u8]) -> Result<(&[u8], Option<&[u8]>), String> {
    if !bytes.starts_with(b"glTF") {
        return Ok((bytes, None));
    }
    let u32_at = |i: usize| -> Result<usize, String> {
        bytes
            .get(i..i + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
            .ok_or_else(|| "the GLB is cut short".to_owned())
    };
    let mut at = 12;
    let (mut json, mut bin) = (None, None);
    while at + 8 <= bytes.len() {
        let len = u32_at(at)?;
        let kind = &bytes[at + 4..at + 8];
        let body = bytes
            .get(at + 8..at + 8 + len)
            .ok_or("a GLB chunk runs past the end")?;
        match kind {
            b"JSON" => json = Some(body),
            b"BIN\0" => bin = Some(body),
            _ => {}
        }
        at += 8 + len;
    }
    Ok((json.ok_or("the GLB has no JSON chunk")?, bin))
}

/// An image's media type from its first bytes.
fn mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8]) {
        Some("image/jpeg")
    } else {
        None
    }
}

fn align(bin: &mut Vec<u8>) {
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
}

/// Pack a glTF or GLB and every file it names into one GLB: every buffer goes into the binary
/// chunk and every image into a buffer view. A GLB with nothing outside it comes back unchanged.
/// `folder` is where separate files are (the `.gltf`'s folder).
pub fn pack(bytes: &[u8], folder: Option<&Path>) -> Result<Vec<u8>, String> {
    let (json, bin) = chunks(bytes)?;
    let mut doc: Value =
        serde_json::from_slice(json).map_err(|e| format!("the glTF JSON does not parse: {e}"))?;
    let has_uri = |key: &str, doc: &Value| {
        doc.get(key)
            .and_then(Value::as_array)
            .is_some_and(|a| a.iter().any(|v| v.get("uri").is_some()))
    };
    if bin.is_some() && !has_uri("buffers", &doc) && !has_uri("images", &doc) {
        return Ok(bytes.to_vec());
    }
    let mut out = Vec::new();
    let mut base = Vec::new();
    let buffers = doc
        .get("buffers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for (i, b) in buffers.iter().enumerate() {
        let data: Vec<u8> = match b.get("uri").and_then(Value::as_str) {
            Some(uri) => load_uri(uri, folder)?.into_owned(),
            None if i == 0 => bin.ok_or("buffer 0 has no data")?.to_vec(),
            None => return Err(format!("buffer {i} has no data")),
        };
        let len = b
            .get("byteLength")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("buffer {i} has no byteLength"))? as usize;
        let data = data
            .get(..len)
            .ok_or_else(|| format!("buffer {i} is shorter than its byteLength"))?;
        align(&mut out);
        base.push(out.len());
        out.extend_from_slice(data);
    }
    if let Some(views) = doc.get_mut("bufferViews").and_then(Value::as_array_mut) {
        for (k, v) in views.iter_mut().enumerate() {
            let b = v.get("buffer").and_then(Value::as_u64).unwrap_or(0) as usize;
            let at = *base
                .get(b)
                .ok_or_else(|| format!("buffer view {k} names buffer {b}, which is not there"))?;
            let off = v.get("byteOffset").and_then(Value::as_u64).unwrap_or(0) as usize;
            v["buffer"] = json!(0);
            v["byteOffset"] = json!(at + off);
        }
    }
    let mut new_views = Vec::new();
    let first_new = doc
        .get("bufferViews")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    if let Some(images) = doc.get_mut("images").and_then(Value::as_array_mut) {
        for (k, img) in images.iter_mut().enumerate() {
            let Some(uri) = img.get("uri").and_then(Value::as_str).map(str::to_owned) else {
                continue;
            };
            let data = load_uri(&uri, folder)?.into_owned();
            let kind = mime(&data).ok_or_else(|| {
                format!("image {k} (`{uri}`) is not PNG or JPEG, which the studio cannot load")
            })?;
            align(&mut out);
            new_views.push(json!({
                "buffer": 0,
                "byteOffset": out.len(),
                "byteLength": data.len(),
            }));
            out.extend_from_slice(&data);
            let obj = img.as_object_mut().ok_or("an image is not an object")?;
            obj.remove("uri");
            obj.insert("bufferView".into(), json!(first_new + new_views.len() - 1));
            obj.insert("mimeType".into(), json!(kind));
        }
    }
    if !new_views.is_empty() {
        let views = doc
            .as_object_mut()
            .ok_or("the glTF JSON is not an object")?
            .entry("bufferViews")
            .or_insert_with(|| json!([]));
        views
            .as_array_mut()
            .ok_or("bufferViews is not a list")?
            .extend(new_views);
    }
    align(&mut out);
    if out.is_empty() {
        doc.as_object_mut()
            .ok_or("the glTF JSON is not an object")?
            .remove("buffers");
    } else {
        doc["buffers"] = json!([{ "byteLength": out.len() }]);
    }
    let text = serde_json::to_vec(&doc).map_err(|e| e.to_string())?;
    Ok(glb_container(&text, &out))
}

/// Which file a glTF container is, by its first bytes.
pub fn container_of(bytes: &[u8]) -> Container {
    if bytes.starts_with(b"glTF") {
        Container::Glb
    } else {
        Container::Gltf
    }
}

/// Parse an axis as the command line writes it: `+y`, `-z`, `y`.
pub fn parse_axis(s: &str) -> Option<Axis> {
    let s = s.trim().to_ascii_lowercase();
    let (neg, letter) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(&s)),
    };
    Some(match (neg, letter) {
        (false, "x") => Axis::PosX,
        (false, "y") => Axis::PosY,
        (false, "z") => Axis::PosZ,
        (true, "x") => Axis::NegX,
        (true, "y") => Axis::NegY,
        (true, "z") => Axis::NegZ,
        _ => return None,
    })
}

/// Parse a unit as the command line writes it: `mm`, `cm`, `m`, `in`, `ft`.
pub fn parse_unit(s: &str) -> Option<Unit> {
    let s = s.trim().to_ascii_lowercase();
    Unit::ALL
        .into_iter()
        .find(|u| u.name() == s || u.short() == s)
}
