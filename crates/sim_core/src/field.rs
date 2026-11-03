//! `FieldPath`: read or write a numeric field of an entity's component by its
//! `Component.field.path` (verification probes, IB-09; actuator outputs, step 5c).
//!
//! The path is parsed once (the component's `ReflectComponent` and a parsed `bevy_reflect`
//! field path, the same syntax the Resolver's targets use, §7); every read or write after that is
//! a reflection lookup with no string parsing and no allocation. `f64`, `f32`, the integer types
//! and `bool` (`1.0` or `0.0`) read as `f64`; only `f64` and `f32` fields can be written.

use bevy_ecs::prelude::*;
use bevy_ecs::reflect::ReflectComponent;
use bevy_ecs::world::EntityMut;
use bevy_reflect::{GetPath, ParsedPath, PartialReflect, TypeRegistry};

use crate::parametric::parse_target;

/// A parsed `Component.field.path`.
#[derive(Clone)]
pub struct FieldPath {
    component: ReflectComponent,
    component_name: &'static str,
    field: ParsedPath,
}

impl std::fmt::Debug for FieldPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FieldPath({}{})", self.component_name, self.field)
    }
}

/// Why a field could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldReadError {
    /// The entity does not exist.
    NoEntity,
    /// The entity has no such component.
    NoComponent(&'static str),
    /// The field path does not exist in the component.
    NoField,
    /// The field is not a number or a `bool`; holds its type path when known.
    NotNumeric(&'static str),
    /// A write to a field that is not an `f64` or an `f32`; holds its type path when known.
    NotFloat(&'static str),
}

impl std::fmt::Display for FieldReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FieldReadError::NoEntity => write!(f, "the entity does not exist"),
            FieldReadError::NoComponent(c) => write!(f, "the entity has no {c}"),
            FieldReadError::NoField => write!(f, "the component has no such field"),
            FieldReadError::NotNumeric(t) => write!(f, "the field is a {t}, not a number"),
            FieldReadError::NotFloat(t) => {
                write!(
                    f,
                    "the field is a {t}; only f64 and f32 fields can be written"
                )
            }
        }
    }
}

impl std::error::Error for FieldReadError {}

impl FieldPath {
    /// Parse `Component.field.path` against the registry. `Component` is the short type path, or
    /// the full one when the short one is ambiguous.
    pub fn parse(registry: &TypeRegistry, text: &str) -> Result<Self, String> {
        let target = parse_target(registry, text)?;
        Ok(Self {
            component: target
                .registration
                .data::<ReflectComponent>()
                .expect("checked by parse_target")
                .clone(),
            component_name: target
                .registration
                .type_info()
                .type_path_table()
                .short_path(),
            field: target.field,
        })
    }

    /// The component's short type path.
    pub fn component_name(&self) -> &'static str {
        self.component_name
    }

    /// Read the field as an `f64`.
    pub fn read(&self, world: &World, entity: Entity) -> Result<f64, FieldReadError> {
        let entity = world
            .get_entity(entity)
            .map_err(|_| FieldReadError::NoEntity)?;
        let component = self
            .component
            .reflect(entity)
            .ok_or(FieldReadError::NoComponent(self.component_name))?;
        let value = component
            .reflect_path(&self.field)
            .map_err(|_| FieldReadError::NoField)?;
        as_f64(value)
    }

    /// Write `value` into the field, which must be an `f64` or an `f32` (rounded).
    pub fn write(
        &self,
        world: &mut World,
        entity: Entity,
        value: f64,
    ) -> Result<(), FieldReadError> {
        let mut entity = world
            .get_entity_mut(entity)
            .map_err(|_| FieldReadError::NoEntity)?;
        let mut component = self
            .component
            .reflect_mut(EntityMut::from(&mut entity))
            .ok_or(FieldReadError::NoComponent(self.component_name))?;
        let field = component
            .reflect_path_mut(&self.field)
            .map_err(|_| FieldReadError::NoField)?;
        if let Some(v) = field.try_downcast_mut::<f64>() {
            *v = value;
            return Ok(());
        }
        if let Some(v) = field.try_downcast_mut::<f32>() {
            *v = value as f32;
            return Ok(());
        }
        Err(FieldReadError::NotFloat(
            field
                .get_represented_type_info()
                .map_or("value of unknown type", |i| i.type_path()),
        ))
    }
}

fn as_f64(value: &dyn PartialReflect) -> Result<f64, FieldReadError> {
    macro_rules! try_as {
        ($($t:ty),*) => {
            $(if let Some(v) = value.try_downcast_ref::<$t>() {
                return Ok(*v as f64);
            })*
        };
    }
    try_as!(f64, f32, i8, i16, i32, i64, u8, u16, u32, u64, isize, usize);
    if let Some(v) = value.try_downcast_ref::<bool>() {
        return Ok(if *v { 1.0 } else { 0.0 });
    }
    Err(FieldReadError::NotNumeric(
        value
            .get_represented_type_info()
            .map_or("value of unknown type", |i| i.type_path()),
    ))
}
