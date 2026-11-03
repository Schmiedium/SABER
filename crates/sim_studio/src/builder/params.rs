//! Swept parameters and per-replicate uncertainties (`sim_studio_ux.md` §7.3), the field bind
//! menu (§7.2), and adding or removing components. Every change is one undoable edit made with
//! [`ScenarioDoc::restructure`], so a change that would break a valid scene is refused.

use bevy::ecs::reflect::ReflectComponent;
use bevy::reflect::std_traits::ReflectDefault;
use sim_core::{Apply, Distribution, ParamBinding, ParamBindings, Uncertainties, Uncertainty};

use super::fields::short;
use super::{FieldKind, ScenarioDoc};

/// Which kind of variation a row is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ParamKind {
    /// A swept parameter: a manifest column (⚙).
    Sweep,
    /// A per-replicate draw from a random stream (●).
    Rng,
}

/// One row of the Parameters panel.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamRow {
    /// Sweep or random.
    pub kind: ParamKind,
    /// The entity's document slot.
    pub slot: usize,
    /// Its `SimId`.
    pub sim_id: Option<u32>,
    /// The `param_key` (sweep) or the stream name (random).
    pub name: String,
    /// `Component.field.path`.
    pub target: String,
    /// The default (sweep) or `—`.
    pub nominal: String,
    /// The bounds (sweep) or the distribution (random).
    pub range: String,
    /// For a random value: whether the draw replaces or adds to the field.
    pub apply: Option<Apply>,
}

/// The `Component.field.path` target of a field.
pub fn target(component: &str, path: &str) -> String {
    format!("{}{path}", short(component))
}

/// A distribution from its name and parameters as the bind menu takes them.
pub fn distribution(kind: &str, p: &[f64]) -> Result<Distribution, String> {
    let need = |n: usize| {
        if p.len() == n {
            Ok(())
        } else {
            Err(format!("{kind} takes {n} numbers, got {}", p.len()))
        }
    };
    Ok(match kind {
        "Normal" => {
            need(2)?;
            Distribution::Normal {
                mean: p[0],
                std: p[1],
            }
        }
        "Uniform" => {
            need(2)?;
            Distribution::Uniform { lo: p[0], hi: p[1] }
        }
        "LogNormal" => {
            need(2)?;
            Distribution::LogNormal {
                mu: p[0],
                sigma: p[1],
            }
        }
        "Triangular" => {
            need(3)?;
            Distribution::Triangular {
                lo: p[0],
                mode: p[1],
                hi: p[2],
            }
        }
        other => {
            return Err(format!(
                "{other} is not Normal, Uniform, LogNormal or Triangular"
            ));
        }
    })
}

/// A `param_key` is a manifest column name: letters, digits and `_`, not starting with a digit.
pub fn valid_key(key: &str) -> bool {
    key.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn describe(d: &Distribution) -> String {
    match d {
        Distribution::Normal { mean, std } => format!("Normal(μ {mean:?}, σ {std:?})"),
        Distribution::Uniform { lo, hi } => format!("Uniform[{lo:?}, {hi:?}]"),
        Distribution::LogNormal { mu, sigma } => format!("LogNormal(μ {mu:?}, σ {sigma:?})"),
        Distribution::Triangular { lo, mode, hi } => {
            format!("Triangular[{lo:?}, {mode:?}, {hi:?}]")
        }
    }
}

/// A component's starting value: its `Default`, or else what an empty `()` in a scenario file
/// reads as (every field at its `#[reflect(default)]`). `None` when it has neither.
fn starting_value(
    reg: &bevy::reflect::TypeRegistry,
    r: &bevy::reflect::TypeRegistration,
) -> Option<Box<dyn bevy::reflect::Reflect>> {
    if let Some(d) = r.data::<ReflectDefault>() {
        return Some(d.default());
    }
    use serde::de::DeserializeSeed;
    let mut de = ron::Deserializer::from_str("()").ok()?;
    let partial = bevy::reflect::serde::TypedReflectDeserializer::new(r, reg)
        .deserialize(&mut de)
        .ok()?;
    r.data::<bevy::reflect::ReflectFromReflect>()?
        .from_reflect(partial.as_ref())
}

fn get_or_insert<T: bevy::reflect::Reflect + Default>(
    components: &mut Vec<Box<dyn bevy::reflect::Reflect>>,
) -> &mut T {
    let at = match components.iter().position(|c| c.as_ref().is::<T>()) {
        Some(i) => i,
        None => {
            components.push(Box::new(T::default()));
            components.len() - 1
        }
    };
    components[at]
        .as_mut()
        .downcast_mut::<T>()
        .expect("just checked")
}

fn drop_if_empty(components: &mut Vec<Box<dyn bevy::reflect::Reflect>>) {
    components.retain(|c| {
        let c = c.as_ref();
        !(c.downcast_ref::<ParamBindings>()
            .is_some_and(|b| b.0.is_empty())
            || c.downcast_ref::<Uncertainties>()
                .is_some_and(|u| u.0.is_empty()))
    });
}

impl ScenarioDoc {
    /// Every swept parameter, then every random value, in `SimId` order.
    pub fn parameters(&self) -> Vec<ParamRow> {
        let mut rows = Vec::new();
        for r in self.rows() {
            let e = &self.entities[r.slot];
            if let Some(ParamBindings(list)) = e.get::<ParamBindings>() {
                rows.extend(list.iter().map(|b| ParamRow {
                    kind: ParamKind::Sweep,
                    slot: r.slot,
                    sim_id: r.sim_id,
                    name: b.param_key.clone(),
                    target: b.target.clone(),
                    nominal: format!("{:?}", b.default),
                    range: b.bounds.map_or_else(
                        || "unbounded".to_owned(),
                        |(lo, hi)| format!("[{lo:?}, {hi:?}]"),
                    ),
                    apply: None,
                }));
            }
            if let Some(Uncertainties(list)) = e.get::<Uncertainties>() {
                rows.extend(list.iter().map(|u| ParamRow {
                    kind: ParamKind::Rng,
                    slot: r.slot,
                    sim_id: r.sim_id,
                    name: u.stream.clone(),
                    target: u.target.clone(),
                    nominal: "—".to_owned(),
                    range: describe(&u.distribution),
                    apply: Some(u.apply),
                }));
            }
        }
        rows.sort_by_key(|r| r.kind);
        rows
    }

    /// The manifest columns a sweep of this scene needs (IB-02), in `SimId` order.
    pub fn sweep_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = Vec::new();
        for r in self.parameters() {
            if r.kind == ParamKind::Sweep && !keys.contains(&r.name) {
                keys.push(r.name);
            }
        }
        keys
    }

    /// The binding on a field, if any.
    pub fn binding(&self, slot: usize, component: &str, path: &str) -> Option<ParamBinding> {
        let t = target(component, path);
        self.entities
            .get(slot)?
            .get::<ParamBindings>()?
            .0
            .iter()
            .find(|b| b.target == t)
            .cloned()
    }

    /// The uncertainty on a field, if any.
    pub fn uncertainty(&self, slot: usize, component: &str, path: &str) -> Option<Uncertainty> {
        let t = target(component, path);
        self.entities
            .get(slot)?
            .get::<Uncertainties>()?
            .0
            .iter()
            .find(|u| u.target == t)
            .cloned()
    }

    fn float_value(&self, slot: usize, component: &str, path: &str) -> Result<f64, String> {
        let f = self
            .components(slot)
            .into_iter()
            .find(|c| c.type_path == component)
            .and_then(|c| c.fields.into_iter().find(|f| f.path == path))
            .ok_or_else(|| format!("no field {path} on {}", short(component)))?;
        if f.kind != FieldKind::Float {
            return Err(format!("{} is not a number field", f.label));
        }
        f.text
            .parse()
            .map_err(|_| format!("{} is not a number", f.text))
    }

    /// Make a field a swept parameter `key` (§7.2 bind menu, "Sweep parameter"), with its value
    /// as the default; replaces a binding already on the field.
    pub fn bind(
        &mut self,
        slot: usize,
        component: &str,
        path: &str,
        key: &str,
        bounds: Option<(f64, f64)>,
    ) -> Result<(), String> {
        let key = key.trim();
        if !valid_key(key) {
            return Err(format!(
                "{key:?} is not a parameter name: use letters, digits and _, not starting with a digit"
            ));
        }
        let default = self.float_value(slot, component, path)?;
        let t = target(component, path);
        let binding = ParamBinding {
            param_key: key.to_owned(),
            target: t.clone(),
            default,
            bounds,
        };
        self.restructure(slot, move |e, _| {
            let list = &mut get_or_insert::<ParamBindings>(&mut e.components).0;
            list.retain(|b| b.target != t);
            list.push(binding);
            Ok(())
        })
    }

    /// Remove the binding on `target`.
    pub fn unbind(&mut self, slot: usize, target: &str) -> Result<(), String> {
        let t = target.to_owned();
        self.restructure(slot, move |e, _| {
            let list = &mut get_or_insert::<ParamBindings>(&mut e.components).0;
            let n = list.len();
            list.retain(|b| b.target != t);
            if list.len() == n {
                return Err(format!("{t} is not bound"));
            }
            drop_if_empty(&mut e.components);
            Ok(())
        })
    }

    /// Draw a field per replicate from `distribution` (§7.2 bind menu). The draw replaces the
    /// value, or is added to it when the field is also a swept parameter ("Both": the sweep
    /// sets the nominal, the draw jitters it). The stream is named after the field.
    pub fn jitter(
        &mut self,
        slot: usize,
        component: &str,
        path: &str,
        distribution: Distribution,
    ) -> Result<(), String> {
        self.float_value(slot, component, path)?;
        let apply = if self.binding(slot, component, path).is_some() {
            Apply::Add
        } else {
            Apply::Set
        };
        let t = target(component, path);
        let stream = match self.sim_id(slot) {
            Some(id) => format!("{id}.{t}"),
            None => t.clone(),
        };
        let u = Uncertainty {
            target: t.clone(),
            distribution,
            stream,
            truncate: None,
            apply,
        };
        self.restructure(slot, move |e, _| {
            let list = &mut get_or_insert::<Uncertainties>(&mut e.components).0;
            list.retain(|u| u.target != t);
            list.push(u);
            Ok(())
        })
    }

    /// Remove the uncertainty on `target`.
    pub fn unjitter(&mut self, slot: usize, target: &str) -> Result<(), String> {
        let t = target.to_owned();
        self.restructure(slot, move |e, _| {
            let list = &mut get_or_insert::<Uncertainties>(&mut e.components).0;
            let n = list.len();
            list.retain(|u| u.target != t);
            if list.len() == n {
                return Err(format!("{t} has no uncertainty"));
            }
            drop_if_empty(&mut e.components);
            Ok(())
        })
    }

    /// The component and path a `Component.field.path` target names on the entity in `slot`.
    pub fn locate(&self, slot: usize, target: &str) -> Option<super::FieldRef> {
        let (name, rest) = target.split_once('.')?;
        let component = self
            .entities
            .get(slot)?
            .components
            .iter()
            .map(|c| c.reflect_type_path())
            .find(|p| short(p) == name)?
            .to_owned();
        Some(super::FieldRef {
            slot,
            component,
            path: format!(".{rest}"),
        })
    }

    /// What the Inspector's layout depends on (not field values): it is rebuilt when this
    /// changes and updated in place otherwise.
    pub fn inspector_key(&self) -> String {
        let Some(slot) = self.selected else {
            return String::new();
        };
        let names: Vec<String> = self
            .components(slot)
            .into_iter()
            .map(|c| c.type_path)
            .collect();
        let params: Vec<String> = self
            .parameters()
            .into_iter()
            .filter(|r| r.slot == slot)
            .map(|r| format!("{:?}{}{}{}{:?}", r.kind, r.name, r.target, r.range, r.apply))
            .collect();
        format!("{slot}|{:?}|{names:?}|{params:?}", self.focus)
    }

    /// Components the entity in `slot` could get: registered SABER components with a starting
    /// value (a `Default`, or every field defaulted) that it does not have, as
    /// `(type path, short name)` by short name.
    pub fn addable(&self, slot: usize) -> Vec<(String, String)> {
        let Some(e) = self.entities.get(slot) else {
            return Vec::new();
        };
        let reg = self.registry.read();
        let mut out: Vec<(String, String)> = reg
            .iter()
            .filter(|r| r.data::<ReflectComponent>().is_some())
            .filter(|r| r.type_info().type_path().starts_with("sim_"))
            .filter(|r| starting_value(&reg, r).is_some())
            .map(|r| r.type_info().type_path().to_owned())
            .filter(|p| !matches!(short(p), "SimId" | "ParamBindings" | "Uncertainties"))
            .filter(|p| !e.components.iter().any(|c| c.reflect_type_path() == p))
            .map(|p| {
                let s = short(&p).to_owned();
                (p, s)
            })
            .collect();
        out.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
        out
    }

    /// Give the entity a component with its default value ("+ Add component").
    pub fn add_component(&mut self, slot: usize, type_path: &str) -> Result<(), String> {
        let tp = type_path.to_owned();
        self.restructure(slot, move |e, reg| {
            let r = reg
                .get_with_type_path(&tp)
                .ok_or_else(|| format!("{tp} is not registered"))?;
            if r.data::<ReflectComponent>().is_none() {
                return Err(format!("{} is not a component", short(&tp)));
            }
            let value = starting_value(reg, r)
                .ok_or_else(|| format!("{} has no default value to start from", short(&tp)))?;
            if e.components.iter().any(|c| c.reflect_type_path() == tp) {
                return Err(format!("the entity already has {}", short(&tp)));
            }
            e.components.push(value);
            Ok(())
        })
    }

    /// Remove a component (not `SimId`). Refused while a binding or uncertainty targets it.
    pub fn remove_component(&mut self, slot: usize, type_path: &str) -> Result<(), String> {
        if short(type_path) == "SimId" {
            return Err("every entity keeps its SimId".to_owned());
        }
        let tp = type_path.to_owned();
        self.restructure(slot, move |e, _| {
            let n = e.components.len();
            e.components.retain(|c| c.reflect_type_path() != tp);
            if e.components.len() == n {
                return Err(format!("the entity has no {}", short(&tp)));
            }
            Ok(())
        })
    }
}
