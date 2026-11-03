//! `Core.Resolver`: Tick-0 expansion of swept parameters and per-replicate uncertainties
//! (subsystems §5.1, §7; seq §6).
//!
//! A scenario entity carries [`ParamBindings`] (values from the sweep manifest row, or their
//! defaults in a local run) and [`Uncertainties`] (values drawn from named random streams). A
//! target is `Component.field.path`, for example `MassProps.mass` or `RigidBody6.velocity.x`, where
//! `Component` is the short type path (or the full one when the short one is ambiguous) of a
//! component in the `SimTypeRegistry`, and the field path follows `bevy_reflect` path syntax and
//! ends at an `f64`.
//!
//! At Tick 0 every target is parsed once into a typed setter (the component's `ReflectComponent`
//! and a parsed field path), the bindings are applied to all entities in `SimId` order, then the
//! uncertainties are drawn in `SimId` order (each entity's in the order listed), each with one
//! uniform from its stream. The two components are then removed, so the running World holds only
//! resolved values and the hot loop never touches reflection.

use bevy_ecs::prelude::*;
use bevy_ecs::reflect::ReflectComponent;
use bevy_reflect::prelude::ReflectDefault;
use bevy_reflect::{GetPath, ParsedPath, Reflect, TypeRegistration, TypeRegistry};

use crate::app::{SetupError, SimTypeRegistry};
use crate::body::SimId;
use crate::distribution::Distribution;
use crate::rng::RngStreams;

/// Swept parameters of one entity: each writes a manifest column's value into a field.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Component, Default)]
pub struct ParamBindings(pub Vec<ParamBinding>);

/// One swept parameter (subsystems §7).
#[derive(Reflect, Clone, Debug, PartialEq)]
pub struct ParamBinding {
    /// Column in `sweep_manifest.parquet`, for example `drag_scale`.
    pub param_key: String,
    /// `Component.field.path` of an `f64` on the same entity.
    pub target: String,
    /// The value used when a run has no manifest row (LocalRun).
    pub default: f64,
    /// Allowed range `[lo, hi]`, checked at preflight and at Tick 0.
    #[reflect(default)]
    pub bounds: Option<(f64, f64)>,
}

/// Per-replicate random values of one entity.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Component, Default)]
pub struct Uncertainties(pub Vec<Uncertainty>);

/// One per-replicate random value (subsystems §7; the draft's `(path, Distribution, stream)`
/// tuple with named fields and two optional ones).
#[derive(Reflect, Clone, Debug, PartialEq)]
pub struct Uncertainty {
    /// `Component.field.path` of an `f64` on the same entity.
    pub target: String,
    /// What to draw.
    pub distribution: Distribution,
    /// Name of the random stream to draw from (`SimRng` substream).
    pub stream: String,
    /// Draw only inside `[lo, hi]`.
    #[reflect(default)]
    pub truncate: Option<(f64, f64)>,
    /// Replace the field's value, or add to it (a jitter around a swept nominal).
    #[reflect(default)]
    pub apply: Apply,
}

/// How a drawn value is written.
#[derive(Reflect, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Apply {
    /// The field becomes the drawn value.
    #[default]
    Set,
    /// The drawn value is added to the field (after bindings).
    Add,
}

/// One run's parameter values by `param_key` (a sweep manifest row).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParamValues {
    values: Vec<(String, f64)>,
}

impl ParamValues {
    /// No values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a value, replacing an earlier one with the same key.
    pub fn insert(&mut self, key: impl Into<String>, value: f64) -> &mut Self {
        let key = key.into();
        match self.values.binary_search_by(|(k, _)| k.as_str().cmp(&key)) {
            Ok(i) => self.values[i].1 = value,
            Err(i) => self.values.insert(i, (key, value)),
        }
        self
    }

    /// The value of a key.
    pub fn get(&self, key: &str) -> Option<f64> {
        self.values
            .binary_search_by(|(k, _)| k.as_str().cmp(key))
            .ok()
            .map(|i| self.values[i].1)
    }

    /// The values, sorted by key.
    pub fn iter(&self) -> impl Iterator<Item = (&str, f64)> {
        self.values.iter().map(|(k, v)| (k.as_str(), *v))
    }

    /// Number of values.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

impl<K: Into<String>> FromIterator<(K, f64)> for ParamValues {
    fn from_iter<I: IntoIterator<Item = (K, f64)>>(iter: I) -> Self {
        let mut out = Self::new();
        for (k, v) in iter {
            out.insert(k, v);
        }
        out
    }
}

/// Where a resolved value came from.
#[derive(Clone, Debug, PartialEq)]
pub enum ValueSource {
    /// A manifest column.
    Param(String),
    /// A binding's default (no manifest row).
    Default(String),
    /// A draw from the named stream.
    Draw(String),
}

/// One value the resolver wrote.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedValue {
    /// The entity.
    pub sim_id: SimId,
    /// The target path.
    pub target: String,
    /// The value written (after `Apply::Add`, the field's new value).
    pub value: f64,
    /// Where it came from.
    pub source: ValueSource,
}

/// What Tick 0 resolution wrote, in order (`ResolveReport`, subsystems §5.1). Replaced every run.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct ResolveReport {
    /// Bindings in `SimId` order, then draws in `SimId` order.
    pub values: Vec<ResolvedValue>,
}

/// A parsed target: the component's registration and the field path inside it.
pub(crate) struct Target<'r> {
    pub(crate) registration: &'r TypeRegistration,
    pub(crate) field: ParsedPath,
}

/// Parse `Component.field.path` against the registry.
pub(crate) fn parse_target<'r>(
    registry: &'r TypeRegistry,
    target: &str,
) -> Result<Target<'r>, String> {
    let Some((name, rest)) = target.split_once('.') else {
        return Err("a target is Component.field".to_owned());
    };
    let registration = match registry.get_with_short_type_path(name) {
        Some(r) => r,
        None if registry.is_ambiguous(name) => {
            return Err(format!(
                "{name} names more than one registered type; use its full type path"
            ));
        }
        None => registry
            .get_with_type_path(name)
            .ok_or_else(|| format!("{name} is not a registered component"))?,
    };
    if registration.data::<ReflectComponent>().is_none() {
        return Err(format!("{name} is not a component"));
    }
    let field = ParsedPath::parse(&format!(".{rest}"))
        .map_err(|e| format!("bad field path {rest:?}: {e}"))?;
    Ok(Target {
        registration,
        field,
    })
}

/// Check a target against an entity's components without a World (scene validation).
pub(crate) fn check_target(
    registry: &TypeRegistry,
    components: &[Box<dyn Reflect>],
    target: &str,
) -> Result<(), String> {
    let t = parse_target(registry, target)?;
    let type_id = t.registration.type_id();
    let component = components
        .iter()
        .find(|c| c.as_ref().type_id() == type_id)
        .ok_or_else(|| {
            format!(
                "the entity has no {}",
                t.registration.type_info().type_path_table().short_path()
            )
        })?;
    let value = component
        .as_ref()
        .reflect_path(&t.field)
        .map_err(|e| e.to_string())?;
    if value.try_downcast_ref::<f64>().is_none() {
        return Err(format!(
            "the target is a {}, not an f64",
            value.reflect_type_path()
        ));
    }
    Ok(())
}

/// Check a binding's own values (bounds and default).
pub(crate) fn check_binding(b: &ParamBinding) -> Result<(), String> {
    if b.param_key.is_empty() {
        return Err("param_key is empty".to_owned());
    }
    if !b.default.is_finite() {
        return Err("the default is not finite".to_owned());
    }
    if let Some((lo, hi)) = b.bounds {
        if lo.is_nan() || hi.is_nan() || lo > hi {
            return Err(format!("bounds ({lo}, {hi}) must have lo ≤ hi"));
        }
        if b.default < lo || b.default > hi {
            return Err(format!(
                "the default {} is outside the bounds ({lo}, {hi})",
                b.default
            ));
        }
    }
    Ok(())
}

/// Check an uncertainty's own values (stream name, distribution, truncation).
pub(crate) fn check_uncertainty(u: &Uncertainty) -> Result<(), String> {
    if u.stream.is_empty() {
        return Err("the stream name is empty".to_owned());
    }
    u.distribution
        .sampler(u.truncate)
        .map(|_| ())
        .map_err(str::to_owned)
}

/// Write `value` (or add it, with `add`) to the target field of an entity; returns the new value.
fn write_target(
    world: &mut World,
    entity: Entity,
    target: &Target<'_>,
    value: f64,
    add: bool,
) -> Result<f64, String> {
    let rc = target
        .registration
        .data::<ReflectComponent>()
        .expect("checked by parse_target");
    let mut entity = world.entity_mut(entity);
    let mut component = rc.reflect_mut(&mut entity).ok_or_else(|| {
        format!(
            "the entity has no {}",
            target
                .registration
                .type_info()
                .type_path_table()
                .short_path()
        )
    })?;
    let field = component
        .reflect_path_mut(&target.field)
        .map_err(|e| e.to_string())?;
    let slot = field
        .try_downcast_mut::<f64>()
        .ok_or_else(|| "the target is not an f64".to_owned())?;
    *slot = if add { *slot + value } else { value };
    Ok(*slot)
}

/// Tick-0 resolution (seq §6): bindings in `SimId` order, then draws in `SimId` order, then the
/// two components are removed. With `params` every bound key must be present (else
/// `SetupError::MissingParam`); without, defaults are used. Writes [`ResolveReport`].
pub(crate) fn resolve(world: &mut World, params: Option<&ParamValues>) -> Result<(), SetupError> {
    let mut entities: Vec<(SimId, Entity, Vec<ParamBinding>, Vec<Uncertainty>)> = world
        .query_filtered::<(
            Entity,
            &SimId,
            Option<&ParamBindings>,
            Option<&Uncertainties>,
        ), Or<(With<ParamBindings>, With<Uncertainties>)>>()
        .iter(world)
        .map(|(e, id, b, u)| {
            (
                *id,
                e,
                b.map(|b| b.0.clone()).unwrap_or_default(),
                u.map(|u| u.0.clone()).unwrap_or_default(),
            )
        })
        .collect();
    entities.sort_by_key(|(id, ..)| *id);
    let registry = world.resource::<SimTypeRegistry>().0.clone();
    let registry = registry.read();
    let bad = |sim_id: SimId, target: &str, reason: String| SetupError::BadBinding {
        sim_id: sim_id.0,
        target: target.to_owned(),
        reason,
    };
    let mut report = Vec::new();

    for (sim_id, entity, bindings, _) in &entities {
        for b in bindings {
            check_binding(b).map_err(|r| bad(*sim_id, &b.target, r))?;
            let (value, source) = match params {
                Some(p) => (
                    p.get(&b.param_key)
                        .ok_or_else(|| SetupError::MissingParam(b.param_key.clone()))?,
                    ValueSource::Param(b.param_key.clone()),
                ),
                None => (b.default, ValueSource::Default(b.param_key.clone())),
            };
            if let Some((lo, hi)) = b.bounds
                && !(lo <= value && value <= hi)
            {
                return Err(SetupError::ParamOutOfBounds {
                    key: b.param_key.clone(),
                    value,
                    lo,
                    hi,
                });
            }
            let target =
                parse_target(&registry, &b.target).map_err(|r| bad(*sim_id, &b.target, r))?;
            let value = write_target(world, *entity, &target, value, false)
                .map_err(|r| bad(*sim_id, &b.target, r))?;
            report.push(ResolvedValue {
                sim_id: *sim_id,
                target: b.target.clone(),
                value,
                source,
            });
        }
    }

    for (sim_id, entity, _, uncertainties) in &entities {
        for u in uncertainties {
            if u.stream.is_empty() {
                return Err(bad(
                    *sim_id,
                    &u.target,
                    "the stream name is empty".to_owned(),
                ));
            }
            let sampler = u
                .distribution
                .sampler(u.truncate)
                .map_err(|r| bad(*sim_id, &u.target, r.to_owned()))?;
            let target =
                parse_target(&registry, &u.target).map_err(|r| bad(*sim_id, &u.target, r))?;
            let draw = {
                let mut streams = world.resource_mut::<RngStreams>();
                let handle = streams.register(&u.stream);
                sampler.sample(streams.get_mut(handle).next_open01())
            };
            let value = write_target(world, *entity, &target, draw, u.apply == Apply::Add)
                .map_err(|r| bad(*sim_id, &u.target, r))?;
            report.push(ResolvedValue {
                sim_id: *sim_id,
                target: u.target.clone(),
                value,
                source: ValueSource::Draw(u.stream.clone()),
            });
        }
    }

    for (_, entity, ..) in &entities {
        world
            .entity_mut(*entity)
            .remove::<(ParamBindings, Uncertainties)>();
    }
    world.insert_resource(ResolveReport { values: report });
    Ok(())
}
