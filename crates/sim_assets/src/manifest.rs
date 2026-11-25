//! `model.ron`: the manifest next to every model file (subsystems §5.10, IB-20). It says how the
//! file's unit and axes map onto the body frame, where the body's reference point is, how big the
//! model is, and under what license it may be used.

use std::fmt;

use glam::{DMat3, DQuat, DVec3};
use ron::ser::PrettyConfig;
use serde::{Deserialize, Serialize};
use sim_schema::Semver;

use crate::hash::ContentHash;

/// The manifest's file name in a model's folder.
pub const MANIFEST_FILE: &str = "model.ron";

/// The manifest schema this build writes and reads (same major, IB-20).
pub const MANIFEST_SCHEMA_VERSION: Semver = Semver::new(1, 0, 0);

/// The length unit a file's coordinates are in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Unit {
    /// Metres (glTF's own unit).
    Metre,
    /// Centimetres.
    Centimetre,
    /// Millimetres (most CAD exports).
    Millimetre,
    /// Inches.
    Inch,
    /// Feet.
    Foot,
}

impl Unit {
    /// Every unit, largest first.
    pub const ALL: [Unit; 5] = [
        Unit::Metre,
        Unit::Foot,
        Unit::Inch,
        Unit::Centimetre,
        Unit::Millimetre,
    ];

    /// One unit in metres.
    pub fn metres(self) -> f64 {
        match self {
            Unit::Metre => 1.0,
            Unit::Centimetre => 0.01,
            Unit::Millimetre => 0.001,
            Unit::Inch => 0.0254,
            Unit::Foot => 0.3048,
        }
    }

    /// Its name in the UI.
    pub fn name(self) -> &'static str {
        match self {
            Unit::Metre => "metre",
            Unit::Centimetre => "centimetre",
            Unit::Millimetre => "millimetre",
            Unit::Inch => "inch",
            Unit::Foot => "foot",
        }
    }

    /// Its symbol: `m`, `cm`, `mm`, `in`, `ft`.
    pub fn short(self) -> &'static str {
        match self {
            Unit::Metre => "m",
            Unit::Centimetre => "cm",
            Unit::Millimetre => "mm",
            Unit::Inch => "in",
            Unit::Foot => "ft",
        }
    }
}

/// A signed coordinate axis of a model file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Axis {
    /// +x.
    PosX,
    /// −x.
    NegX,
    /// +y.
    PosY,
    /// −y.
    NegY,
    /// +z.
    PosZ,
    /// −z.
    NegZ,
}

impl Axis {
    /// Every axis.
    pub const ALL: [Axis; 6] = [
        Axis::PosX,
        Axis::NegX,
        Axis::PosY,
        Axis::NegY,
        Axis::PosZ,
        Axis::NegZ,
    ];

    /// The unit vector.
    pub fn vector(self) -> DVec3 {
        match self {
            Axis::PosX => DVec3::X,
            Axis::NegX => DVec3::NEG_X,
            Axis::PosY => DVec3::Y,
            Axis::NegY => DVec3::NEG_Y,
            Axis::PosZ => DVec3::Z,
            Axis::NegZ => DVec3::NEG_Z,
        }
    }

    /// `+X`, `−Z` and so on.
    pub fn label(self) -> &'static str {
        match self {
            Axis::PosX => "+X",
            Axis::NegX => "−X",
            Axis::PosY => "+Y",
            Axis::NegY => "−Y",
            Axis::PosZ => "+Z",
            Axis::NegZ => "−Z",
        }
    }
}

/// How a file's coordinates map onto the body frame: x forward, y right, z down, as the flight and
/// ballistics models use (subsystems §5.10).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelFrame {
    /// The file's length unit.
    pub unit: Unit,
    /// The file axis that points up (body −z).
    pub up: Axis,
    /// The file axis that points forward (body +x).
    pub forward: Axis,
}

impl ModelFrame {
    /// A file already in the body frame, in metres: built-ins are drawn this way, so they never
    /// need per-entity alignment.
    pub const BODY: ModelFrame = ModelFrame {
        unit: Unit::Metre,
        up: Axis::NegZ,
        forward: Axis::PosX,
    };

    /// glTF's own convention (+y up, +z forward), in metres: what a file from most tools is in.
    pub const GLTF: ModelFrame = ModelFrame {
        unit: Unit::Metre,
        up: Axis::PosY,
        forward: Axis::PosZ,
    };

    /// The rotation from file axes to body axes, or `None` when up and forward are the same line.
    pub fn rotation(&self) -> Option<DMat3> {
        let up = self.up.vector();
        let forward = self.forward.vector();
        if up.cross(forward) == DVec3::ZERO {
            return None;
        }
        // Body x is forward, z is down, and y = z × x is right: −up × forward = forward × up.
        let right = forward.cross(up);
        Some(DMat3::from_cols(forward, right, -up).transpose())
    }

    /// The same rotation as a quaternion.
    pub fn rotation_quat(&self) -> Option<DQuat> {
        self.rotation().map(|m| DQuat::from_mat3(&m))
    }

    /// A file point in the body frame, metres (before the manifest's origin offset).
    pub fn to_body(&self, file: DVec3) -> Option<DVec3> {
        self.rotation().map(|m| m * (file * self.unit.metres()))
    }
}

impl fmt::Display for ModelFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}, up {}, forward {}",
            self.unit.name(),
            self.up.label(),
            self.forward.label()
        )
    }
}

/// Where the body's reference point is relative to the file's origin.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Origin {
    /// The file's origin is the reference point.
    FileOrigin,
    /// Added to every point after the frame's rotation and unit: body frame, metres.
    Offset([f64; 3]),
}

impl Origin {
    /// The offset, body frame, metres.
    pub fn offset(&self) -> DVec3 {
        match self {
            Origin::FileOrigin => DVec3::ZERO,
            Origin::Offset(o) => DVec3::from(*o),
        }
    }
}

/// An axis-aligned box.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bounds {
    /// Smallest corner.
    pub min: [f64; 3],
    /// Largest corner.
    pub max: [f64; 3],
}

impl Bounds {
    /// The box around these points, or `None` without any.
    pub fn around(points: impl IntoIterator<Item = DVec3>) -> Option<Self> {
        let mut it = points.into_iter();
        let first = it.next()?;
        let (lo, hi) = it.fold((first, first), |(lo, hi), p| (lo.min(p), hi.max(p)));
        Some(Self {
            min: lo.to_array(),
            max: hi.to_array(),
        })
    }

    /// Its edge lengths.
    pub fn size(&self) -> DVec3 {
        DVec3::from(self.max) - DVec3::from(self.min)
    }

    /// Its centre.
    pub fn center(&self) -> DVec3 {
        0.5 * (DVec3::from(self.max) + DVec3::from(self.min))
    }

    /// Its eight corners.
    pub fn corners(&self) -> [DVec3; 8] {
        let (a, b) = (self.min, self.max);
        std::array::from_fn(|i| {
            DVec3::new(
                if i & 1 == 0 { a[0] } else { b[0] },
                if i & 2 == 0 { a[1] } else { b[1] },
                if i & 4 == 0 { a[2] } else { b[2] },
            )
        })
    }

    /// Whether every corner is finite and `min ≤ max`.
    pub fn is_valid(&self) -> bool {
        (0..3).all(|i| {
            self.min[i].is_finite() && self.max[i].is_finite() && self.min[i] <= self.max[i]
        })
    }

    /// The largest corner difference to another box.
    pub fn distance(&self, other: &Bounds) -> f64 {
        (0..3)
            .map(|i| {
                (self.min[i] - other.min[i])
                    .abs()
                    .max((self.max[i] - other.max[i]).abs())
            })
            .fold(0.0, f64::max)
    }
}

/// The original file format of a model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceFormat {
    /// A `.gltf` with separate files, packed into one `.glb`.
    Gltf,
    /// Wavefront OBJ (stretch goal, D17).
    Obj,
}

/// Where the stored file came from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileSource {
    /// Stored as it was given.
    #[default]
    Native,
    /// Converted or packed on import.
    Converted {
        /// The original's format.
        format: SourceFormat,
        /// The original's hash.
        hash: ContentHash,
        /// The tool and version that converted it.
        converter: String,
    },
}

/// What drives a moving part (reserved: moving parts are deferred, D17).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Drive {
    /// Spins about an axis at a signal's rate, rad/s.
    Spin {
        /// The axis, in the node's frame.
        axis: Axis,
        /// The signal path.
        signal: String,
    },
    /// Turns about a hinge by a signal's angle, rad.
    Hinge {
        /// The axis, in the node's frame.
        axis: Axis,
        /// The signal path.
        signal: String,
    },
}

/// A node of the file moved by a signal (reserved, D17).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VisualBinding {
    /// The node's name in the file.
    pub node: String,
    /// What moves it.
    pub drive: Drive,
}

/// A coarser file drawn when the model is small on screen.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Lod {
    /// The file, in the model's folder.
    pub file: String,
    /// Drawn when the model is under this many pixels across.
    pub below_px: f32,
}

/// How a collision shape is derived from a model, on request (IB-19).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeriveMethod {
    /// One cuboid.
    BoundingBox,
    /// A capsule along the principal axis.
    Capsule,
    /// A cylinder along the principal axis.
    Cylinder,
    /// One convex hull.
    ConvexHull,
    /// Convex parts.
    ConvexDecomposition {
        /// At most this many parts.
        max_parts: u32,
    },
    /// The triangles themselves (static bodies and terrain only).
    TriMesh,
}

/// A model's manifest, `model.ron` (IB-20).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VisualAssetManifest {
    /// `major.minor.patch`; this build reads [`MANIFEST_SCHEMA_VERSION`]'s major.
    pub schema_version: String,
    /// `rotorcraft/quad_x` for a built-in, `models/uav_v3` in a workspace, the folder name in a
    /// library root.
    pub id: String,
    /// Bumped when a built-in's shape or frame changes, so `@1` keeps its meaning.
    pub version: u32,
    /// Its name in the UI.
    pub display_name: String,
    /// The model file in the same folder: `model.glb`.
    pub file: String,
    /// The model file's hash.
    pub content_hash: ContentHash,
    /// Where the file came from.
    #[serde(default)]
    pub source: FileSource,
    /// The file's unit and axes.
    pub frame: ModelFrame,
    /// The body's reference point.
    pub origin: Origin,
    /// The model's extent, body frame, metres (informational).
    pub bounds: Bounds,
    /// Triangles drawn.
    pub triangles: u64,
    /// Moving parts (reserved, D17).
    #[serde(default)]
    pub bindings: Vec<VisualBinding>,
    /// Coarser files for distance.
    #[serde(default)]
    pub lods: Vec<Lod>,
    /// The collision derivation the model's author suggests.
    #[serde(default)]
    pub suggested_collision: Option<DeriveMethod>,
    /// Its license.
    pub license: String,
    /// Credit owed, if any.
    #[serde(default)]
    pub attribution: Option<String>,
}

/// Why a manifest is not usable.
#[derive(Clone, Debug, PartialEq)]
pub enum ManifestError {
    /// The text is not a manifest.
    Parse(String),
    /// A field breaks a rule.
    Invalid(String),
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManifestError::Parse(e) => write!(f, "not a model manifest: {e}"),
            ManifestError::Invalid(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for ManifestError {}

/// Whether `name` is a plain file name: no folders, no `..`, nothing absolute.
pub fn is_plain_file_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':'])
        && !name.starts_with('.')
}

impl VisualAssetManifest {
    /// Parse and check a `model.ron`.
    pub fn from_ron(text: &str) -> Result<Self, ManifestError> {
        let m: Self = ron::from_str(text).map_err(|e| ManifestError::Parse(e.to_string()))?;
        m.check()?;
        Ok(m)
    }

    /// The manifest as `model.ron` text.
    pub fn to_ron(&self) -> String {
        let config = PrettyConfig::new().new_line("\n".to_owned());
        let body = ron::ser::to_string_pretty(self, config).expect("a manifest serialises");
        format!("VisualAssetManifest{body}\n")
    }

    /// The rules every manifest keeps.
    pub fn check(&self) -> Result<(), ManifestError> {
        let invalid = |m: String| Err(ManifestError::Invalid(m));
        match Semver::parse(&self.schema_version) {
            Some(v) if MANIFEST_SCHEMA_VERSION.satisfies(Semver::new(v.major, 0, 0)) => {}
            _ => {
                return invalid(format!(
                    "schema_version `{}` is not readable by this build (reads {}.x)",
                    self.schema_version, MANIFEST_SCHEMA_VERSION.major
                ));
            }
        }
        if self.id.trim().is_empty() {
            return invalid("the id is empty".into());
        }
        if self.version == 0 {
            return invalid("version starts at 1".into());
        }
        let lower = self.file.to_ascii_lowercase();
        if !is_plain_file_name(&self.file) || !(lower.ends_with(".glb") || lower.ends_with(".gltf"))
        {
            return invalid(format!(
                "file `{}` must be a .glb or .gltf in the model's folder",
                self.file
            ));
        }
        if self.frame.rotation().is_none() {
            return invalid(format!(
                "up {} and forward {} are the same line",
                self.frame.up.label(),
                self.frame.forward.label()
            ));
        }
        if !self.origin.offset().is_finite() {
            return invalid("the origin offset is not finite".into());
        }
        if !self.bounds.is_valid() {
            return invalid("the bounds are not a box".into());
        }
        if self.license.trim().is_empty() {
            return invalid("the license is empty".into());
        }
        if let Some(l) = self.lods.iter().find(|l| !is_plain_file_name(&l.file)) {
            return invalid(format!(
                "LOD file `{}` must be in the model's folder",
                l.file
            ));
        }
        Ok(())
    }

    /// A file point in the body frame, metres: the frame's rotation and unit, then the origin.
    pub fn to_body(&self, file: DVec3) -> DVec3 {
        self.frame.to_body(file).unwrap_or(DVec3::NAN) + self.origin.offset()
    }

    /// `quad_x`: the id's last part.
    pub fn slug(&self) -> &str {
        self.id.rsplit('/').next().unwrap_or(&self.id)
    }
}
