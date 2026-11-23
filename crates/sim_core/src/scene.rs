//! The scenario file `*.scn.ron` (IB-01): a SABER envelope around the scene's reflected entities.
//!
//! ```text
//! (
//!     header: (magic: "SABER", format: "scenario", schema_version: "1.0.0", ...),
//!     units: "SI",
//!     frame: ENU,
//!     model_library_version: "0.1.0",
//!     config: (dt: 0.01, max_ticks: 2000, ...),          // SimConfig
//!     scene: (
//!         entities: {
//!             0: (components: {
//!                 "sim_core::body::SimId": (1),
//!                 "sim_core::body::PointMass3": (position: (0.0, 0.0, 0.0), velocity: ...),
//!             }),
//!         },
//!     ),
//!     visual: ( ... ),                                    // optional, opaque to the kernel
//! )
//! ```
//!
//! The `scene` section has the shape of Bevy's reflected world RON (entities keyed by a file-local
//! number, components keyed by full type path), but SABER reads and writes it itself with
//! `bevy_reflect`'s typed serde: Bevy 0.20 moved `DynamicScene` into `bevy_world_serialization`,
//! which pulls in `bevy_app`, `bevy_asset` and the renderer's type crates, and the kernel and
//! `sim_hpc` stay on `bevy_ecs` alone (IB-08). Only components registered in the `SimTypeRegistry`
//! may appear.
//!
//! `scene_hash` is BLAKE3 (derive-key context [`SCENE_HASH_CONTEXT`]) over the canonical sim
//! section: `units`, `frame`, `model_library_version` and the entities in `SimId` order with their
//! components in type-path order, re-serialised as compact RON, so formatting, comments, entity
//! keys and component order never change it. `config` is not in it; it goes into
//! `sim_config_hash` together with `scene_hash` (`SimConfig::hash`). The `visual:` section has its
//! own `visual_hash` (D17). Geometry sidecars join `scene_hash` with step 5d.

use std::cell::RefCell;

use bevy_ecs::prelude::*;
use bevy_ecs::reflect::ReflectComponent;
use bevy_reflect::serde::{TypedReflectDeserializer, TypedReflectSerializer};
use bevy_reflect::{PartialReflect, Reflect, ReflectFromReflect, TypeRegistry};
use ron::value::RawValue;
use serde::de::{self, DeserializeSeed, MapAccess, Visitor};
use serde::ser::{SerializeMap, SerializeStruct};
use serde::{Deserialize, Serialize, Serializer};
use sim_schema::{FileHeader, Frame, HeaderError, SCENE_FORMAT, SCENE_SCHEMA_VERSION, Semver};

use crate::app::{SetupError, SimConfig};
use crate::body::SimId;
use crate::hash::finish;
use crate::parametric::{
    ParamBinding, ParamBindings, Uncertainties, check_binding, check_target, check_uncertainty,
};

/// Derive-key context of `scene_hash`.
pub const SCENE_HASH_CONTEXT: &str = "SABER 2026-11-02 scene hash v1";
/// Derive-key context of `visual_hash`.
pub const VISUAL_HASH_CONTEXT: &str = "SABER 2026-11-02 visual hash v1";

/// Why a scenario file was refused (IB-01). Studio shows these in the validation panel and blocks
/// export; `sim_hpc` aborts in preflight before any work is dispatched.
#[derive(Clone, Debug, PartialEq)]
pub enum SceneError {
    /// The text is not valid RON, or a value does not fit its type.
    Parse {
        /// 1-based line in the file.
        line: usize,
        /// 1-based column.
        col: usize,
        /// What the parser said.
        message: String,
    },
    /// Not a SABER scenario: wrong `magic` or `format`, unreadable version string, or units other
    /// than `"SI"`.
    BadHeader(String),
    /// The file's schema or model library version is not readable by this build.
    VersionMismatch {
        /// `schema_version` or `model_library_version`.
        what: &'static str,
        /// The file's version.
        found: String,
        /// This build's version.
        supported: String,
    },
    /// A component type path that is not a registered scenario component.
    UnknownType {
        /// The entity's key in the file.
        entity: u64,
        /// The type path.
        path: String,
    },
    /// A component value that cannot be built, or a component listed twice on one entity.
    BadValue {
        /// The entity's key in the file.
        entity: u64,
        /// The type path.
        path: String,
        /// What is wrong.
        message: String,
    },
    /// An entity without a `SimId`.
    MissingSimId {
        /// The entity's key in the file.
        entity: u64,
    },
    /// Two entities with the same `SimId`.
    DuplicateSimId {
        /// The id.
        sim_id: u32,
    },
    /// A parameter binding or uncertainty that does not resolve, or whose values are invalid
    /// (bounds, default, distribution, stream name).
    BadBinding {
        /// The entity.
        sim_id: u32,
        /// The binding's target path.
        path: String,
        /// Why.
        reason: String,
    },
    /// The `config` section is invalid.
    BadConfig(SetupError),
}

impl std::fmt::Display for SceneError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use SceneError::*;
        match self {
            Parse { line, col, message } => write!(f, "{line}:{col}: {message}"),
            BadHeader(m) => write!(f, "not a SABER scenario: {m}"),
            VersionMismatch {
                what,
                found,
                supported,
            } => write!(
                f,
                "{what} {found} is not supported by this build ({supported})"
            ),
            UnknownType { entity, path } => {
                write!(
                    f,
                    "entity {entity}: {path} is not a registered scenario component"
                )
            }
            BadValue {
                entity,
                path,
                message,
            } => write!(f, "entity {entity}: {path}: {message}"),
            MissingSimId { entity } => write!(f, "entity {entity} has no SimId"),
            DuplicateSimId { sim_id } => write!(f, "SimId {sim_id} is used by two entities"),
            BadBinding {
                sim_id,
                path,
                reason,
            } => write!(f, "SimId {sim_id}: binding {path:?}: {reason}"),
            BadConfig(e) => write!(f, "config: {e}"),
        }
    }
}

impl std::error::Error for SceneError {}

/// One scenario entity: its reflected components (concrete values, built through `FromReflect`).
#[derive(Debug)]
pub struct SceneEntity {
    /// The entity's key in the file it came from (renumbered `0..` in `SimId` order on export).
    pub key: u64,
    /// Its components, in file order.
    pub components: Vec<Box<dyn Reflect>>,
}

impl SceneEntity {
    /// The entity's `SimId` (present after validation).
    pub fn sim_id(&self) -> Option<SimId> {
        self.get::<SimId>().copied()
    }

    /// A component by type.
    pub fn get<T: Reflect>(&self) -> Option<&T> {
        self.components
            .iter()
            .find_map(|c| c.as_ref().downcast_ref::<T>())
    }

    /// A deep copy through reflection (the studio's editable copy of a scene). `None` if a
    /// component cannot be cloned, which a registered scene component always can.
    pub fn try_clone(&self, registry: &TypeRegistry) -> Option<SceneEntity> {
        let components = self
            .components
            .iter()
            .map(|c| {
                c.reflect_clone().ok().or_else(|| {
                    registry
                        .get(c.as_ref().type_id())
                        .and_then(|r| r.data::<ReflectFromReflect>())
                        .and_then(|f| f.from_reflect(c.as_partial_reflect()))
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(SceneEntity {
            key: self.key,
            components,
        })
    }
}

/// What a scenario says besides its entities, for export from a World.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneMeta {
    /// The world frame.
    pub frame: Frame,
    /// How the World simulates.
    pub config: SimConfig,
    /// The model library the scene was authored against.
    pub model_library_version: Semver,
}

/// A parsed, validated scenario. Built by [`Scenario::from_ron`] or [`Scenario::from_world`];
/// immutable afterwards, so its hashes stay true. Shared read-only (`Arc`) between the Worlds of a
/// node; each run spawns it with [`Scenario::spawn`].
#[derive(Debug)]
pub struct Scenario {
    header: FileHeader,
    frame: Frame,
    model_library_version: Semver,
    config: SimConfig,
    entities: Vec<SceneEntity>,
    visual: Option<Box<RawValue>>,
    scene_hash: u64,
    visual_hash: Option<u64>,
}

/// The envelope as it is on disk; `scene` and `visual` are kept as raw RON (the scene is read with
/// the registry afterwards, the visual section is never read by the kernel).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<'a> {
    header: FileHeader,
    units: String,
    frame: Frame,
    model_library_version: String,
    config: SimConfig,
    #[serde(borrow)]
    scene: &'a RawValue,
    #[serde(default, borrow, deserialize_with = "present")]
    visual: Option<&'a RawValue>,
}

/// An optional section written without `Some(..)`.
fn present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<&'de RawValue>, D::Error> {
    <&RawValue>::deserialize(d).map(Some)
}

impl Scenario {
    /// Parse and validate a scenario file against the registry and this build's model library.
    pub fn from_ron(
        text: &str,
        registry: &TypeRegistry,
        library: Semver,
    ) -> Result<Self, SceneError> {
        let env: Envelope<'_> = ron::from_str(text).map_err(|e| parse_error(&e, text, 0))?;
        if let Err(e) = env.header.check(SCENE_FORMAT, SCENE_SCHEMA_VERSION) {
            return Err(match e {
                HeaderError::Unsupported { found, supported } => SceneError::VersionMismatch {
                    what: "schema_version",
                    found: found.to_string(),
                    supported: supported.to_string(),
                },
                other => SceneError::BadHeader(other.to_string()),
            });
        }
        if env.units != "SI" {
            return Err(SceneError::BadHeader(format!(
                "units are {:?}; SABER scenarios are in \"SI\"",
                env.units
            )));
        }
        let model_library_version = Semver::parse(&env.model_library_version).ok_or_else(|| {
            SceneError::BadHeader(format!(
                "model_library_version {:?} is not major.minor.patch",
                env.model_library_version
            ))
        })?;
        if !library.satisfies(model_library_version) {
            return Err(SceneError::VersionMismatch {
                what: "model_library_version",
                found: model_library_version.to_string(),
                supported: library.to_string(),
            });
        }
        env.config.validate().map_err(SceneError::BadConfig)?;

        let scene_text = env.scene.get_ron();
        let offset = (scene_text.as_ptr() as usize)
            .checked_sub(text.as_ptr() as usize)
            .filter(|&o| {
                text.get(o..)
                    .is_some_and(|rest| rest.starts_with(scene_text))
            })
            .unwrap_or(0);
        let ctx = Ctx {
            registry,
            error: RefCell::new(None),
        };
        let mut de =
            ron::Deserializer::from_str(scene_text).map_err(|e| parse_error(&e, text, offset))?;
        let entities = match (SceneSeed { ctx: &ctx }).deserialize(&mut de) {
            Ok(entities) => entities,
            Err(e) => {
                if let Some(structured) = ctx.error.take() {
                    return Err(structured);
                }
                return Err(parse_error(&de.span_error(e), text, offset));
            }
        };

        let mut scenario = Self {
            header: env.header,
            frame: env.frame,
            model_library_version,
            config: env.config,
            entities,
            visual: env.visual.map(RawValue::to_owned),
            scene_hash: 0,
            visual_hash: None,
        };
        scenario.validate(registry)?;
        scenario.rehash(registry);
        Ok(scenario)
    }

    /// Export the scenario entities of a World: every entity with a `SimId`, in `SimId` order, with
    /// each of its components that the registry knows as a component.
    pub fn from_world(
        world: &World,
        registry: &TypeRegistry,
        meta: SceneMeta,
    ) -> Result<Self, SceneError> {
        meta.config.validate().map_err(SceneError::BadConfig)?;
        let mut ids: Vec<(SimId, Entity)> = world
            .iter_entities()
            .filter_map(|e| e.get::<SimId>().map(|id| (*id, e.id())))
            .collect();
        ids.sort_unstable();
        let mut entities = Vec::with_capacity(ids.len());
        for (key, (_, entity)) in ids.into_iter().enumerate() {
            let entity_ref = world.entity(entity);
            let mut components: Vec<Box<dyn Reflect>> = Vec::new();
            let infos = world
                .inspect_entity(entity)
                .expect("entity listed by iter_entities");
            for (_, info) in infos {
                let Some(registration) = info.type_id().and_then(|t| registry.get(t)) else {
                    continue;
                };
                let Some(rc) = registration.data::<ReflectComponent>() else {
                    continue;
                };
                let Some(value) = rc.reflect(entity_ref) else {
                    continue;
                };
                let path = registration.type_info().type_path();
                let cloned = value
                    .reflect_clone()
                    .ok()
                    .or_else(|| {
                        registration
                            .data::<ReflectFromReflect>()
                            .and_then(|f| f.from_reflect(value.as_partial_reflect()))
                    })
                    .ok_or_else(|| SceneError::BadValue {
                        entity: key as u64,
                        path: path.to_owned(),
                        message: "the component cannot be copied through reflection".to_owned(),
                    })?;
                components.push(cloned);
            }
            components.sort_by(|a, b| a.reflect_type_path().cmp(b.reflect_type_path()));
            entities.push(SceneEntity {
                key: key as u64,
                components,
            });
        }
        let mut header = FileHeader::new(
            SCENE_FORMAT,
            SCENE_SCHEMA_VERSION,
            env!("CARGO_PKG_VERSION"),
        );
        header.scene_hash = None;
        let mut scenario = Self {
            header,
            frame: meta.frame,
            model_library_version: meta.model_library_version,
            config: meta.config,
            entities,
            visual: None,
            scene_hash: 0,
            visual_hash: None,
        };
        scenario.validate(registry)?;
        scenario.rehash(registry);
        Ok(scenario)
    }

    /// Build a scenario from edited parts (the studio's Builder, `sim_studio_ux.md` §7.2): the
    /// entities keep their keys and component order, so export writes them as authored. Validated
    /// and hashed like a parsed file; the header is this build's, as in [`Scenario::from_world`].
    pub fn from_parts(
        entities: Vec<SceneEntity>,
        meta: SceneMeta,
        visual: Option<&str>,
        registry: &TypeRegistry,
    ) -> Result<Self, SceneError> {
        meta.config.validate().map_err(SceneError::BadConfig)?;
        let visual = match visual {
            Some(v) => Some(
                RawValue::from_ron(v)
                    .map_err(|e| SceneError::BadHeader(format!("the visual section: {}", e.code)))?
                    .to_owned(),
            ),
            None => None,
        };
        let mut header = FileHeader::new(
            SCENE_FORMAT,
            SCENE_SCHEMA_VERSION,
            env!("CARGO_PKG_VERSION"),
        );
        header.scene_hash = None;
        let mut scenario = Self {
            header,
            frame: meta.frame,
            model_library_version: meta.model_library_version,
            config: meta.config,
            entities,
            visual,
            scene_hash: 0,
            visual_hash: None,
        };
        scenario.validate(registry)?;
        scenario.rehash(registry);
        Ok(scenario)
    }

    /// What the scenario says besides its entities.
    pub fn meta(&self) -> SceneMeta {
        SceneMeta {
            frame: self.frame,
            config: self.config.clone(),
            model_library_version: self.model_library_version,
        }
    }

    /// Structural checks after parsing: `SimId`s, then every binding and uncertainty.
    fn validate(&self, registry: &TypeRegistry) -> Result<(), SceneError> {
        let mut ids = Vec::with_capacity(self.entities.len());
        for e in &self.entities {
            let id = e
                .sim_id()
                .ok_or(SceneError::MissingSimId { entity: e.key })?;
            ids.push(id);
        }
        ids.sort_unstable();
        if let Some(w) = ids.windows(2).find(|w| w[0] == w[1]) {
            return Err(SceneError::DuplicateSimId { sim_id: w[0].0 });
        }

        let mut keys: Vec<(&str, &ParamBinding, SimId)> = Vec::new();
        for e in self.sorted_entities() {
            let sim_id = e.sim_id().expect("checked above");
            let bad = |path: &str, reason: String| SceneError::BadBinding {
                sim_id: sim_id.0,
                path: path.to_owned(),
                reason,
            };
            if let Some(ParamBindings(bindings)) = e.get::<ParamBindings>() {
                for b in bindings {
                    check_binding(b).map_err(|r| bad(&b.target, r))?;
                    check_target(registry, &e.components, &b.target)
                        .map_err(|r| bad(&b.target, r))?;
                    if let Some((_, other, other_id)) =
                        keys.iter().find(|(k, ..)| *k == b.param_key)
                        && (other.default != b.default || other.bounds != b.bounds)
                    {
                        return Err(bad(
                            &b.target,
                            format!(
                                "param_key {:?} is also bound on SimId {} with a different \
                                 default or bounds",
                                b.param_key, other_id.0
                            ),
                        ));
                    }
                    keys.push((&b.param_key, b, sim_id));
                }
            }
            if let Some(Uncertainties(list)) = e.get::<Uncertainties>() {
                for u in list {
                    check_uncertainty(u).map_err(|r| bad(&u.target, r))?;
                    check_target(registry, &e.components, &u.target)
                        .map_err(|r| bad(&u.target, r))?;
                }
            }
        }
        Ok(())
    }

    fn sorted_entities(&self) -> Vec<&SceneEntity> {
        let mut sorted: Vec<&SceneEntity> = self.entities.iter().collect();
        sorted.sort_by_key(|e| e.sim_id());
        sorted
    }

    fn rehash(&mut self, registry: &TypeRegistry) {
        let canonical = ron::to_string(&Canonical {
            scenario: self,
            registry,
        })
        .expect("a validated scene serialises");
        let mut h = blake3::Hasher::new_derive_key(SCENE_HASH_CONTEXT);
        h.update(canonical.as_bytes());
        self.scene_hash = finish(&h);
        self.visual_hash = self.visual.as_ref().map(|v| {
            let mut h = blake3::Hasher::new_derive_key(VISUAL_HASH_CONTEXT);
            h.update(v.get_ron().trim().as_bytes());
            finish(&h)
        });
    }

    /// The file header (on export, `scene_hash` is filled in).
    pub fn header(&self) -> &FileHeader {
        &self.header
    }

    /// The world frame.
    pub fn frame(&self) -> Frame {
        self.frame
    }

    /// The model library version the scene needs.
    pub fn model_library_version(&self) -> Semver {
        self.model_library_version
    }

    /// How the World simulates.
    pub fn config(&self) -> &SimConfig {
        &self.config
    }

    /// The entities, in file order.
    pub fn entities(&self) -> &[SceneEntity] {
        &self.entities
    }

    /// The raw `visual:` section, if any (D17; read by the studio, never by the kernel).
    pub fn visual(&self) -> Option<&str> {
        self.visual.as_deref().map(RawValue::get_ron)
    }

    /// `scene_hash` of the canonical sim section.
    pub fn scene_hash(&self) -> u64 {
        self.scene_hash
    }

    /// `visual_hash` of the `visual:` section, if there is one.
    pub fn visual_hash(&self) -> Option<u64> {
        self.visual_hash
    }

    /// `sim_config_hash` of this scenario (physics §10.5).
    pub fn sim_config_hash(&self) -> u64 {
        self.config.hash(self.scene_hash)
    }

    /// Every parameter binding, as `(SimId, binding)` in `SimId` order: the `param_*` columns a
    /// sweep manifest for this scene must have (IB-02).
    pub fn param_bindings(&self) -> Vec<(SimId, &ParamBinding)> {
        let mut out = Vec::new();
        for e in self.sorted_entities() {
            if let Some(ParamBindings(bindings)) = e.get::<ParamBindings>() {
                let id = e.sim_id().expect("validated");
                out.extend(bindings.iter().map(|b| (id, b)));
            }
        }
        out
    }

    /// Write the scenario as pretty RON, with `header.scene_hash` set.
    pub fn to_ron(&self, registry: &TypeRegistry) -> String {
        let config = ron::ser::PrettyConfig::new().indentor("    ");
        ron::ser::to_string_pretty(
            &Pretty {
                scenario: self,
                registry,
            },
            config,
        )
        .expect("a validated scene serialises")
    }

    /// Spawn the entities into a World (each run's Tick 0, before resolution). Returns them in
    /// file order.
    pub fn spawn(&self, world: &mut World, registry: &TypeRegistry) -> Vec<Entity> {
        self.entities
            .iter()
            .map(|e| {
                let mut entity = world.spawn_empty();
                for c in &e.components {
                    let rc = registry
                        .get(c.as_ref().type_id())
                        .and_then(|r| r.data::<ReflectComponent>())
                        .expect("validated against the same registry");
                    rc.insert(&mut entity, c.as_partial_reflect(), registry);
                }
                entity.id()
            })
            .collect()
    }
}

fn parse_error(e: &ron::error::SpannedError, text: &str, offset: usize) -> SceneError {
    // Positions inside the scene section are relative to it; shift them to the whole file.
    let before = &text[..offset];
    let line0 = before.matches('\n').count();
    let col0 = before.len() - before.rfind('\n').map_or(0, |i| i + 1);
    let p = e.span.start;
    let (line, col) = if p.line <= 1 {
        (line0 + 1, col0 + p.col)
    } else {
        (line0 + p.line, p.col)
    };
    SceneError::Parse {
        line,
        col,
        message: e.code.to_string(),
    }
}

// ---------------------------------------------------------------------------------------------
// Reading the scene section.

struct Ctx<'a> {
    registry: &'a TypeRegistry,
    /// A structured error raised inside the serde visitors (serde errors carry only text).
    error: RefCell<Option<SceneError>>,
}

impl Ctx<'_> {
    fn fail<E: de::Error>(&self, err: SceneError) -> E {
        let msg = err.to_string();
        self.error.borrow_mut().get_or_insert(err);
        E::custom(msg)
    }
}

struct SceneSeed<'c, 'a> {
    ctx: &'c Ctx<'a>,
}

impl<'de> DeserializeSeed<'de> for SceneSeed<'_, '_> {
    type Value = Vec<SceneEntity>;

    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_struct("Scene", &["entities"], self)
    }
}

impl<'de> Visitor<'de> for SceneSeed<'_, '_> {
    type Value = Vec<SceneEntity>;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a scene `(entities: { ... })`")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut entities = None;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "entities" if entities.is_none() => {
                    entities = Some(map.next_value_seed(EntitiesSeed { ctx: self.ctx })?);
                }
                "entities" => return Err(de::Error::duplicate_field("entities")),
                other => return Err(de::Error::unknown_field(other, &["entities"])),
            }
        }
        entities.ok_or_else(|| de::Error::missing_field("entities"))
    }
}

struct EntitiesSeed<'c, 'a> {
    ctx: &'c Ctx<'a>,
}

impl<'de> DeserializeSeed<'de> for EntitiesSeed<'_, '_> {
    type Value = Vec<SceneEntity>;

    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for EntitiesSeed<'_, '_> {
    type Value = Vec<SceneEntity>;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a map of entity key to `(components: { ... })`")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut out: Vec<SceneEntity> = Vec::new();
        while let Some(key) = map.next_key::<u64>()? {
            if out.iter().any(|e| e.key == key) {
                return Err(de::Error::custom(format!("entity key {key} appears twice")));
            }
            let components = map.next_value_seed(EntitySeed { ctx: self.ctx, key })?;
            out.push(SceneEntity { key, components });
        }
        Ok(out)
    }
}

struct EntitySeed<'c, 'a> {
    ctx: &'c Ctx<'a>,
    key: u64,
}

impl<'de> DeserializeSeed<'de> for EntitySeed<'_, '_> {
    type Value = Vec<Box<dyn Reflect>>;

    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_struct("Entity", &["components"], self)
    }
}

impl<'de> Visitor<'de> for EntitySeed<'_, '_> {
    type Value = Vec<Box<dyn Reflect>>;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("an entity `(components: { ... })`")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut components = None;
        while let Some(field) = map.next_key::<String>()? {
            match field.as_str() {
                "components" if components.is_none() => {
                    components = Some(map.next_value_seed(ComponentsSeed {
                        ctx: self.ctx,
                        key: self.key,
                    })?);
                }
                "components" => return Err(de::Error::duplicate_field("components")),
                other => return Err(de::Error::unknown_field(other, &["components"])),
            }
        }
        components.ok_or_else(|| de::Error::missing_field("components"))
    }
}

struct ComponentsSeed<'c, 'a> {
    ctx: &'c Ctx<'a>,
    key: u64,
}

impl<'de> DeserializeSeed<'de> for ComponentsSeed<'_, '_> {
    type Value = Vec<Box<dyn Reflect>>;

    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for ComponentsSeed<'_, '_> {
    type Value = Vec<Box<dyn Reflect>>;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a map of component type path to value")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let ctx = self.ctx;
        let mut out: Vec<Box<dyn Reflect>> = Vec::new();
        while let Some(path) = map.next_key::<String>()? {
            let registration = ctx
                .registry
                .get_with_type_path(&path)
                .filter(|r| r.data::<ReflectComponent>().is_some())
                .ok_or_else(|| {
                    ctx.fail(SceneError::UnknownType {
                        entity: self.key,
                        path: path.clone(),
                    })
                })?;
            let bad = |message: &str| SceneError::BadValue {
                entity: self.key,
                path: path.clone(),
                message: message.to_owned(),
            };
            if out
                .iter()
                .any(|c| c.as_ref().type_id() == registration.type_id())
            {
                return Err(ctx.fail(bad("the component appears twice on the entity")));
            }
            let value: Box<dyn PartialReflect> =
                map.next_value_seed(TypedReflectDeserializer::new(registration, ctx.registry))?;
            let concrete = registration
                .data::<ReflectFromReflect>()
                .and_then(|f| f.from_reflect(value.as_ref()))
                .ok_or_else(|| {
                    ctx.fail(bad(
                        "the value does not fit the type (is a required field missing?)",
                    ))
                })?;
            out.push(concrete);
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------------------------
// Writing.

/// The whole file, pretty.
struct Pretty<'s, 'r> {
    scenario: &'s Scenario,
    registry: &'r TypeRegistry,
}

impl Serialize for Pretty<'_, '_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let sc = self.scenario;
        let mut header = sc.header.clone();
        header.scene_hash = Some(sc.scene_hash);
        let mut st = s.serialize_struct("Scenario", 7)?;
        st.serialize_field("header", &header)?;
        st.serialize_field("units", "SI")?;
        st.serialize_field("frame", &sc.frame)?;
        st.serialize_field(
            "model_library_version",
            &sc.model_library_version.to_string(),
        )?;
        st.serialize_field("config", &sc.config)?;
        st.serialize_field(
            "scene",
            &SceneSer {
                entities: sc.entities.iter().collect(),
                renumber: false,
                registry: self.registry,
            },
        )?;
        match &sc.visual {
            Some(v) => st.serialize_field("visual", v.as_ref())?,
            None => st.skip_field("visual")?,
        }
        st.end()
    }
}

/// The canonical sim section that `scene_hash` covers.
struct Canonical<'s, 'r> {
    scenario: &'s Scenario,
    registry: &'r TypeRegistry,
}

impl Serialize for Canonical<'_, '_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let sc = self.scenario;
        let mut st = s.serialize_struct("Sim", 4)?;
        st.serialize_field("units", "SI")?;
        st.serialize_field("frame", &sc.frame)?;
        st.serialize_field(
            "model_library_version",
            &sc.model_library_version.to_string(),
        )?;
        st.serialize_field(
            "scene",
            &SceneSer {
                entities: sc.sorted_entities(),
                renumber: true,
                registry: self.registry,
            },
        )?;
        st.end()
    }
}

struct SceneSer<'s, 'r> {
    entities: Vec<&'s SceneEntity>,
    /// Canonical form: keys `0..` in the given order, components sorted by type path.
    renumber: bool,
    registry: &'r TypeRegistry,
}

impl Serialize for SceneSer<'_, '_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("Scene", 1)?;
        st.serialize_field("entities", &EntitiesSer(self))?;
        st.end()
    }
}

struct EntitiesSer<'a, 's, 'r>(&'a SceneSer<'s, 'r>);

impl Serialize for EntitiesSer<'_, '_, '_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let scene = self.0;
        let mut map = s.serialize_map(Some(scene.entities.len()))?;
        for (i, e) in scene.entities.iter().enumerate() {
            let key = if scene.renumber { i as u64 } else { e.key };
            let mut components: Vec<&dyn Reflect> =
                e.components.iter().map(|c| c.as_ref()).collect();
            if scene.renumber {
                components.sort_by(|a, b| a.reflect_type_path().cmp(b.reflect_type_path()));
            }
            map.serialize_entry(
                &key,
                &EntitySer {
                    components,
                    registry: scene.registry,
                },
            )?;
        }
        map.end()
    }
}

struct EntitySer<'s, 'r> {
    components: Vec<&'s dyn Reflect>,
    registry: &'r TypeRegistry,
}

impl Serialize for EntitySer<'_, '_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("Entity", 1)?;
        st.serialize_field("components", &ComponentsSer(self))?;
        st.end()
    }
}

struct ComponentsSer<'a, 's, 'r>(&'a EntitySer<'s, 'r>);

impl Serialize for ComponentsSer<'_, '_, '_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let e = self.0;
        let mut map = s.serialize_map(Some(e.components.len()))?;
        for c in &e.components {
            map.serialize_entry(
                c.reflect_type_path(),
                &TypedReflectSerializer::new(c.as_partial_reflect(), e.registry),
            )?;
        }
        map.end()
    }
}
