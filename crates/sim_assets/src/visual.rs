//! The scenario's `visual:` section (IB-01, IB-20): which model each entity shows, keyed by
//! `SimId`, and look-only scenery. The kernel keeps the section as raw RON with its own
//! `visual_hash` and never reads it, so nothing here can change a result (D17).

use std::collections::BTreeMap;
use std::fmt;

use glam::{DQuat, DVec3};
use ron::ser::PrettyConfig;
use serde::{Deserialize, Serialize};
use sim_schema::Semver;

use crate::hash::ContentHash;
use crate::manifest::VisualBinding;

/// The `visual:` schema this build writes and reads (same major).
pub const VISUAL_SCHEMA_VERSION: Semver = Semver::new(1, 0, 0);

/// Where a model is found.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AssetSource {
    /// Shipped with SABER (`crates/sim_assets/builtin/`).
    Builtin,
    /// The workspace's `models/` folder.
    Workspace,
    /// A library root listed under `model_roots`, by its folder name.
    Library(String),
}

/// A link to a model: source, id, version and the hash it had when linked.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VisualAssetRef {
    /// Where it is found.
    pub source: AssetSource,
    /// `rotorcraft/quad_x`, `models/uav_v3`, or a library root's folder name.
    pub id: String,
    /// The built-in version linked (1 for workspace and library models).
    #[serde(default = "one")]
    pub version: u32,
    /// The model file's hash when it was linked; a different file now is a warning, never an
    /// error. Written bare (`content_hash: "blake3:…"`); absent in hand-written files, which then
    /// skip the check.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "bare_hash")]
    pub content_hash: Option<ContentHash>,
}

/// An optional hash written as a bare string, as the design's examples write it.
pub(crate) mod bare_hash {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use crate::hash::ContentHash;

    pub fn serialize<S: Serializer>(h: &Option<ContentHash>, s: S) -> Result<S::Ok, S::Error> {
        match h {
            Some(h) => h.serialize(s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<ContentHash>, D::Error> {
        ContentHash::deserialize(d).map(Some)
    }
}

fn one() -> u32 {
    1
}

impl VisualAssetRef {
    /// A built-in, unpinned to a hash.
    pub fn builtin(id: &str, version: u32) -> Self {
        Self {
            source: AssetSource::Builtin,
            id: id.to_owned(),
            version,
            content_hash: None,
        }
    }
}

impl fmt::Display for VisualAssetRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.source {
            AssetSource::Builtin => write!(f, "builtin:{}@{}", self.id, self.version),
            AssetSource::Workspace => write!(f, "workspace:{}", self.id),
            AssetSource::Library(root) => write!(f, "lib:{root}/{}", self.id),
        }
    }
}

/// A pose in the body frame: translation in metres and a unit quaternion `(x, y, z, w)`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pose(pub [f64; 3], pub [f64; 4]);

impl Pose {
    /// No offset.
    pub const IDENTITY: Pose = Pose([0.0; 3], [0.0, 0.0, 0.0, 1.0]);

    /// The translation.
    pub fn translation(&self) -> DVec3 {
        DVec3::from(self.0)
    }

    /// The rotation (normalised).
    pub fn rotation(&self) -> DQuat {
        DQuat::from_array(self.1).normalize()
    }

    /// Whether both parts are finite and the quaternion is not zero.
    pub fn is_valid(&self) -> bool {
        self.translation().is_finite()
            && self.1.iter().all(|v| v.is_finite())
            && DQuat::from_array(self.1).length_squared() > 1e-12
    }
}

impl Default for Pose {
    fn default() -> Self {
        Self::IDENTITY
    }
}

fn ones() -> [f64; 3] {
    [1.0; 3]
}

fn yes() -> bool {
    true
}

/// How one entity looks (`sim_assets::visual`, subsystems §5.10).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VisualModel {
    /// The model.
    pub asset: VisualAssetRef,
    /// An extra pose on top of the manifest's origin, body frame, for exceptions.
    #[serde(default)]
    pub offset: Pose,
    /// Per-axis scale about the reference point; 1 is the manifest's size.
    #[serde(default = "ones")]
    pub scale: [f64; 3],
    /// A colour multiplied into the model's own, sRGB with alpha.
    #[serde(default)]
    pub tint: Option<[f32; 4]>,
    /// Hidden models are not drawn.
    #[serde(default = "yes")]
    pub visible: bool,
    /// A per-instance override of the manifest's bindings (reserved, D17).
    #[serde(default)]
    pub bindings: Option<Vec<VisualBinding>>,
}

impl VisualModel {
    /// A model at its manifest's pose and size.
    pub fn new(asset: VisualAssetRef) -> Self {
        Self {
            asset,
            offset: Pose::IDENTITY,
            scale: ones(),
            tint: None,
            visible: true,
            bindings: None,
        }
    }
}

/// A look-only object: no `SimId`, never simulated, never collides.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Scenery {
    /// Its name in the Outliner.
    pub name: String,
    /// The model.
    pub asset: VisualAssetRef,
    /// Where it stands, world frame.
    #[serde(default)]
    pub pose: Pose,
    /// Per-axis scale.
    #[serde(default = "ones")]
    pub scale: [f64; 3],
}

/// The `visual:` section of a scenario.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VisualSection {
    /// `major.minor.patch`.
    pub schema_version: String,
    /// Each entity's model, by `SimId`.
    #[serde(default)]
    pub models: BTreeMap<u32, VisualModel>,
    /// Look-only objects.
    #[serde(default)]
    pub scenery: Vec<Scenery>,
}

impl Default for VisualSection {
    fn default() -> Self {
        Self {
            schema_version: VISUAL_SCHEMA_VERSION.to_string(),
            models: BTreeMap::new(),
            scenery: Vec::new(),
        }
    }
}

/// Why a `visual:` section is not usable.
#[derive(Clone, Debug, PartialEq)]
pub struct VisualError(pub String);

impl fmt::Display for VisualError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the visual section: {}", self.0)
    }
}

impl std::error::Error for VisualError {}

impl VisualSection {
    /// Parse the section's raw RON (as `sim_core::Scenario::visual` gives it) and check it.
    pub fn from_ron(text: &str) -> Result<Self, VisualError> {
        let section: Self = ron::from_str(text).map_err(|e| VisualError(e.to_string()))?;
        section.check()?;
        Ok(section)
    }

    /// The section as RON, for a scenario's `visual:` field.
    pub fn to_ron(&self) -> String {
        let config = PrettyConfig::new().new_line("\n".to_owned());
        ron::ser::to_string_pretty(self, config).expect("a visual section serialises")
    }

    /// The rules every section keeps.
    pub fn check(&self) -> Result<(), VisualError> {
        match Semver::parse(&self.schema_version) {
            Some(v) if v.major == VISUAL_SCHEMA_VERSION.major => {}
            _ => {
                return Err(VisualError(format!(
                    "schema_version `{}` is not readable by this build (reads {}.x)",
                    self.schema_version, VISUAL_SCHEMA_VERSION.major
                )));
            }
        }
        let refs = self
            .models
            .iter()
            .map(|(id, m)| (format!("entity {id}"), &m.asset, &m.offset, &m.scale))
            .chain(
                self.scenery
                    .iter()
                    .map(|s| (format!("scenery `{}`", s.name), &s.asset, &s.pose, &s.scale)),
            );
        for (what, asset, pose, scale) in refs {
            if asset.id.trim().is_empty() {
                return Err(VisualError(format!("{what} links a model with no id")));
            }
            if !pose.is_valid() {
                return Err(VisualError(format!("{what} has an invalid pose")));
            }
            if !scale.iter().all(|s| s.is_finite() && *s > 0.0) {
                return Err(VisualError(format!(
                    "{what} has a scale that is not positive"
                )));
            }
        }
        Ok(())
    }
}
