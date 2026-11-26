//! Collision from a model, mass properties from a shape, and Align mode in the scene document
//! (`sim_studio_ux.md` §7.11.4–§7.11.5; `sequence-diagrams.md` §2.2).
//!
//! **Derive collision from model** writes an ordinary `CollisionShape` and a `DerivedCollision`
//! record into the entity, as one undoable edit of the sim section, so it changes `scene_hash`.
//! The shape is made from the model's triangles where the entity draws them (the manifest's frame
//! and origin, then the instance's scale and offset). Lint warns when the model has changed since;
//! nothing re-derives on its own. **Estimate mass properties** fills `MassProps.inertia` from the
//! shape and the entity's mass as a one-off edit. **Align** moves the look's offset only (the
//! `visual:` section), a whole session as one undo step, and can fold the offset into the model's
//! own manifest.

use std::path::Path;

use bevy::math::{DQuat, DVec3};
use sim_assets::derive::{FromModel, derived_notes};
use sim_assets::{AssetSource, Location, Resolution, VisualAssetRef, VisualModel};
use sim_core::geometry::derive::{DEFAULT_PARTS, Solid, solid};
use sim_core::{
    CollisionShape, DeriveMethod, DerivedCollision, MassProps, PointMass3, RigidBody6, ShapeDesc,
};

use super::lint::{LintItem, LintLevel};
use super::{Change, ScenarioDoc};

/// One step of Align mode (§7.11.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlignOp {
    /// Turn the model +90° about a body axis (0 x, 1 y, 2 z) through the body's origin.
    Rotate(usize),
    /// Move the model so its bounding box's centre is on the body's origin.
    OriginToCentre,
    /// Move the model so the centre of its bounding box's bottom face is on the body's origin.
    OriginToBottom,
    /// No offset (scale and tint stay).
    Reset,
}

impl AlignOp {
    /// Its button's label.
    pub fn label(self) -> &'static str {
        match self {
            AlignOp::Rotate(0) => "Rotate +90° about x",
            AlignOp::Rotate(1) => "Rotate +90° about y",
            AlignOp::Rotate(_) => "Rotate +90° about z",
            AlignOp::OriginToCentre => "Origin to bounding-box centre",
            AlignOp::OriginToBottom => "Origin to bottom centre",
            AlignOp::Reset => "Reset",
        }
    }
}

/// What **Save as model default** did.
#[derive(Clone, Debug, PartialEq)]
pub struct SavedDefault {
    /// The model now holding the offset (a workspace copy when the model was a built-in or in a
    /// library root that cannot be written).
    pub model: VisualAssetRef,
    /// It was copied into the workspace first.
    pub duplicated: bool,
}

/// A shape's parts in words: `1 cuboid`, `6 convex hulls`.
pub fn shape_summary(shape: &CollisionShape) -> String {
    let mut kinds: Vec<(&'static str, usize)> = Vec::new();
    for p in &shape.parts {
        let name = match &p.shape {
            ShapeDesc::Ball { .. } => "ball",
            ShapeDesc::Capsule { .. } => "capsule",
            ShapeDesc::Cuboid { .. } => "cuboid",
            ShapeDesc::Cylinder { .. } => "cylinder",
            ShapeDesc::ConvexHull { .. } => "convex hull",
            ShapeDesc::HalfSpace { .. } => "half-space",
            ShapeDesc::HeightField { .. } => "height field",
            ShapeDesc::TriMesh { .. } => "triangle mesh",
        };
        match kinds.iter_mut().find(|(k, _)| *k == name) {
            Some((_, n)) => *n += 1,
            None => kinds.push((name, 1)),
        }
    }
    if kinds.is_empty() {
        return "no parts".into();
    }
    kinds
        .iter()
        .map(|(k, n)| format!("{n} {k}{}", if *n == 1 { "" } else { "s" }))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Replace a component of type `T`, or add it.
fn put<T: bevy::reflect::Reflect>(e: &mut sim_core::SceneEntity, value: T) {
    match e.components.iter_mut().find(|c| c.as_ref().is::<T>()) {
        Some(c) => *c = Box::new(value),
        None => e.components.push(Box::new(value)),
    }
}

impl ScenarioDoc {
    /// Whether the entity in `slot` has a body (`RigidBody6` or `PointMass3`), so its collider
    /// moves with it; otherwise a collider is static and written in the world frame.
    pub fn on_body(&self, slot: usize) -> bool {
        self.entities
            .get(slot)
            .is_some_and(|e| e.get::<RigidBody6>().is_some() || e.get::<PointMass3>().is_some())
    }

    /// The entity's collision shape.
    pub fn collision(&self, slot: usize) -> Option<CollisionShape> {
        self.entities.get(slot)?.get::<CollisionShape>().cloned()
    }

    /// The record of a shape derived from a model.
    pub fn derived(&self, slot: usize) -> Option<DerivedCollision> {
        self.entities.get(slot)?.get::<DerivedCollision>().cloned()
    }

    /// The look the entity is drawn with: its link, else its Domain Pack default (hidden or not).
    pub fn shown_look(&self, slot: usize) -> Option<VisualModel> {
        self.look(slot)
            .or_else(|| self.default_look(slot).map(VisualModel::new))
    }

    /// Derive a collision shape from the entity's model without applying it (the dialog's
    /// preview). `max_parts` bounds a decomposition.
    pub fn derive_preview(
        &self,
        slot: usize,
        method: DeriveMethod,
        max_parts: u32,
    ) -> Result<FromModel, String> {
        let catalog = self.catalog().ok_or("the model catalogue is not loaded")?;
        let look = self
            .shown_look(slot)
            .ok_or("the entity has no model to derive a shape from; pick one first")?;
        sim_assets::derive::derive(catalog, &look, method, max_parts, self.on_body(slot))
    }

    /// Write a derived shape and its record into the entity, replacing its collision shape, as
    /// one undoable edit (refused when the scene would no longer validate).
    pub fn apply_derived(&mut self, slot: usize, derived: &FromModel) -> Result<(), String> {
        let shape = derived.derived.shape.clone();
        let record = derived.record.clone();
        self.restructure(slot, move |e, _| {
            put(e, shape);
            put(e, record);
            Ok(())
        })
    }

    /// Derive and apply in one step, with the method a dialog would start from.
    pub fn derive_collision(
        &mut self,
        slot: usize,
        method: DeriveMethod,
        max_parts: u32,
    ) -> Result<FromModel, String> {
        let d = self.derive_preview(slot, method, max_parts)?;
        self.apply_derived(slot, &d)?;
        Ok(d)
    }

    /// The method and part limit a (re-)derivation starts from: the record's, else a bounding box
    /// for a body and a triangle mesh for a static collider.
    pub fn derive_start(&self, slot: usize) -> (DeriveMethod, u32) {
        match self.derived(slot) {
            Some(r) => (
                r.method,
                if r.max_parts == 0 {
                    DEFAULT_PARTS
                } else {
                    r.max_parts
                },
            ),
            None if self.on_body(slot) => (DeriveMethod::BoundingBox, DEFAULT_PARTS),
            None => (DeriveMethod::TriMesh, DEFAULT_PARTS),
        }
    }

    /// A uniform solid of the entity's mass filling its collision shape: what **Estimate mass
    /// properties** would write.
    pub fn estimate_inertia(&self, slot: usize) -> Result<Solid, String> {
        if !self.on_body(slot) {
            return Err("only a body has mass properties".into());
        }
        let e = self.entities.get(slot).ok_or("no such entity")?;
        let mass = e
            .get::<MassProps>()
            .ok_or("the entity has no MassProps; add one with its mass first")?
            .mass;
        let shape = e
            .get::<CollisionShape>()
            .ok_or("the entity has no collision shape; derive one from its model first")?;
        solid(shape, mass)
    }

    /// Fill `MassProps.inertia` from the collision shape and the mass (one undoable edit; the
    /// mass stays). Returns the solid it was taken from.
    pub fn apply_inertia(&mut self, slot: usize) -> Result<Solid, String> {
        let s = self.estimate_inertia(slot)?;
        self.restructure(slot, move |e, _| {
            let m = e
                .components
                .iter_mut()
                .find_map(|c| c.as_mut().downcast_mut::<MassProps>())
                .ok_or("the entity has no MassProps")?;
            m.inertia = s.inertia;
            Ok(())
        })?;
        Ok(s)
    }

    /// The model's triangles where the entity draws them (for Align and the previews).
    pub fn look_mesh(&self, slot: usize) -> Result<sim_core::geometry::derive::ModelMesh, String> {
        let catalog = self.catalog().ok_or("the model catalogue is not loaded")?;
        let look = self.shown_look(slot).ok_or("the entity has no model")?;
        match catalog.resolve(&look.asset) {
            Resolution::Found(entry, _) => sim_assets::derive::entry_mesh(entry, &look),
            Resolution::Missing => Err(format!("{} not found", look.asset)),
        }
    }

    /// One step of Align mode: the look's offset only (a default look is linked first).
    pub fn align(&mut self, slot: usize, op: AlignOp) -> Result<bool, String> {
        let id = self
            .sim_id(slot)
            .ok_or("looks are kept by SimId; give the entity one first")?;
        let look = self.shown_look(slot).ok_or("pick a model first")?;
        let t = look.offset.translation();
        let r = look.offset.rotation();
        let (t, r) = match op {
            AlignOp::Rotate(axis) => {
                let q =
                    DQuat::from_axis_angle(DVec3::AXES[axis.min(2)], std::f64::consts::FRAC_PI_2);
                (q * t, (q * r).normalize())
            }
            AlignOp::OriginToCentre | AlignOp::OriginToBottom => {
                let (lo, hi) = self
                    .look_mesh(slot)?
                    .bounds()
                    .ok_or("the model has no triangles")?;
                let c = (lo + hi) * 0.5;
                let target = if op == AlignOp::OriginToBottom {
                    DVec3::new(c.x, c.y, hi.z)
                } else {
                    c
                };
                (t - target, r)
            }
            AlignOp::Reset => (DVec3::ZERO, DQuat::IDENTITY),
        };
        let clean = |v: f64| {
            // Quarter turns leave 1e-17 crumbs; keep the numbers people typed.
            let k = (v * 1e12).round() / 1e12;
            if k == 0.0 { 0.0 } else { k }
        };
        let offset = sim_assets::Pose(t.to_array().map(clean), r.to_array().map(clean));
        self.edit_visual(slot, move |v| {
            let m = v.models.entry(id).or_insert(look);
            m.offset = offset;
            Ok(())
        })
    }

    /// The undo stack's height now, to pass to [`ScenarioDoc::squash_since`].
    pub fn history_mark(&self) -> usize {
        self.undo.len()
    }

    /// Make the `visual:` edits since `mark` one undo step (an Align session). Edits of anything
    /// else stay as they are. Returns how many edits were merged.
    pub fn squash_since(&mut self, mark: usize) -> usize {
        if self.undo.len() <= mark + 1
            || !self.undo[mark..]
                .iter()
                .all(|e| matches!(e.change, Change::Visual { .. }))
        {
            return 0;
        }
        let tail: Vec<_> = self.undo.drain(mark..).collect();
        let n = tail.len();
        let slot = tail[0].slot;
        let before = match &tail[0].change {
            Change::Visual { before, .. } => before.clone(),
            _ => unreachable!("checked above"),
        };
        let last = tail.last().expect("more than one");
        let after = match &last.change {
            Change::Visual { after, .. } => after.clone(),
            _ => unreachable!("checked above"),
        };
        if before != after {
            self.undo.push(super::Edit {
                id: last.id,
                slot,
                change: Change::Visual { before, after },
            });
        }
        self.changed();
        n
    }

    /// **Save as model default** (§7.11.4): fold the entity's offset into its model's manifest,
    /// so every entity using the model gets it, and set this entity's offset back to none. A
    /// built-in, or a model in a library root, is first copied into `models_dir` (the workspace's
    /// `models/`) and the entity linked to the copy. Refused for a rotation that is not a whole
    /// number of quarter turns, or a scale that differs between axes.
    pub fn save_look_as_default(
        &mut self,
        slot: usize,
        models_dir: Option<&Path>,
    ) -> Result<SavedDefault, String> {
        let id = self.sim_id(slot).ok_or("no SimId")?;
        let look = self.shown_look(slot).ok_or("pick a model first")?;
        let s = look.scale;
        if s[0] != s[1] || s[1] != s[2] {
            return Err(
                "a scale that differs between axes stays on the entity; make it one \
                        number first"
                    .into(),
            );
        }
        let catalog = self.catalog().ok_or("the model catalogue is not loaded")?;
        let Resolution::Found(entry, _) = catalog.resolve(&look.asset) else {
            return Err(format!("{} not found", look.asset));
        };
        let writable =
            entry.source == AssetSource::Workspace && matches!(entry.location, Location::Dir(_));
        let (dir, model, duplicated) = if writable {
            let Location::Dir(d) = &entry.location else {
                unreachable!("checked above")
            };
            (d.clone(), entry.reference(), false)
        } else {
            let models = models_dir.ok_or("open a workspace to keep a copy of the model in")?;
            let slug = entry.manifest.slug().to_owned();
            let name = (1..)
                .map(|k| {
                    if k == 1 {
                        slug.clone()
                    } else {
                        format!("{slug}_{k}")
                    }
                })
                .find(|n| !models.join(n).exists())
                .expect("some name is free");
            let copy = sim_assets::import::duplicate(entry, models, &name, "models/")
                .map_err(|e| e.to_string())?;
            let r = VisualAssetRef {
                source: AssetSource::Workspace,
                id: copy.manifest.id.clone(),
                version: copy.manifest.version,
                content_hash: Some(copy.manifest.content_hash),
            };
            (copy.dir, r, true)
        };
        let shift = look.offset.translation() / s[0];
        let saved = sim_assets::import::fold_offset(&dir, look.offset.rotation(), shift)
            .map_err(|e| e.to_string());
        let saved = match saved {
            Ok(m) => m,
            Err(e) => {
                if duplicated {
                    let _ = std::fs::remove_dir_all(&dir);
                }
                return Err(e);
            }
        };
        let mut relinked = look.clone();
        relinked.asset = model.clone();
        relinked.asset.content_hash = Some(saved.manifest.content_hash);
        relinked.offset = sim_assets::Pose::IDENTITY;
        self.edit_visual(slot, move |v| {
            v.models.insert(id, relinked);
            Ok(())
        })?;
        Ok(SavedDefault { model, duplicated })
    }
}

/// Lint for derived shapes: the model changed, or the entity shows another one now.
pub(super) fn lint(doc: &ScenarioDoc, out: &mut Vec<LintItem>) {
    let Some(catalog) = doc.catalog() else {
        return;
    };
    for slot in 0..doc.entities.len() {
        let (Some(id), Some(record)) = (doc.sim_id(slot), doc.derived(slot)) else {
            continue;
        };
        let shown = doc.shown_look(slot).map(|m| m.asset);
        for note in derived_notes(&record, shown.as_ref(), catalog) {
            out.push(LintItem {
                level: LintLevel::Warning,
                sim_id: Some(id),
                req: None,
                text: format!("#{id}: {note}"),
            });
        }
    }
}
