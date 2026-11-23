//! The Scenario Builder's document (`sim_studio_ux.md` §7.2, §9.2, §9.4): an editable copy of
//! the open scenario's entities, edits with undo and redo, a sidecar autosave, lint, and export
//! of the canonical `.scn.ron`.
//!
//! The document is plain data with no Bevy UI in it, so its behaviour is tested directly. Every
//! edit goes through [`ScenarioDoc::set`], which rebuilds nothing until it is asked: lint and
//! export rebuild a [`Scenario`] from the parts with `Scenario::from_parts`, which validates and
//! hashes exactly as a parsed file is.

pub mod fields;
pub mod lint;
pub mod params;
pub mod requirements;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bevy::reflect::TypeRegistryArc;
use sim_core::{
    ParamBindings, Scenario, SceneEntity, SceneMeta, SimAppBuilder, SimConfig, SimId, SimWorld,
    Uncertainties,
};

pub use fields::{FieldKind, FieldView};
pub use lint::{LintItem, LintLevel};
pub use params::{ParamKind, ParamRow};
pub use requirements::{ReqField, ReqItem, ReqRow, RequirementDoc};

/// The sidecar every edit is saved to until the scenario is exported (§9.4).
pub const AUTOSAVE_SUFFIX: &str = "~autosave";

/// One entity as the Outliner lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct EntityRow {
    /// Its position in the document (stable while the document is open).
    pub slot: usize,
    /// Its `SimId`, if it has one.
    pub sim_id: Option<u32>,
    /// Its components' short names, `SimId` and the binding components left out.
    pub summary: String,
    /// It carries swept parameters (⚙).
    pub bound: bool,
    /// It carries per-replicate uncertainties (●).
    pub uncertain: bool,
}

/// One component as the Inspector shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct ComponentView {
    /// The registered type path, which edits name.
    pub type_path: String,
    /// The short name, shown as the section heading.
    pub name: String,
    /// Its fields.
    pub fields: Vec<FieldView>,
}

/// A field of an entity: what the bind menu is open on.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FieldRef {
    /// The entity's document slot.
    pub slot: usize,
    /// The component's type path.
    pub component: String,
    /// The field's reflection path.
    pub path: String,
}

/// One edit, kept for undo and redo.
struct Edit {
    id: u64,
    slot: usize,
    change: Change,
}

/// What an edit changed.
enum Change {
    /// One field, as text.
    Field {
        component: String,
        path: String,
        before: String,
        after: String,
    },
    /// The entity's components (bindings, uncertainties, components added or removed).
    Entity {
        before: SceneEntity,
        after: SceneEntity,
    },
}

/// The open scenario, being edited.
pub struct ScenarioDoc {
    /// The scenario file, relative to the workspace root.
    pub rel: String,
    /// The scenario file.
    pub path: PathBuf,
    /// The selected entity's slot.
    pub selected: Option<usize>,
    /// The field the bind menu is open on.
    pub focus: Option<FieldRef>,
    registry: TypeRegistryArc,
    meta: SceneMeta,
    visual: Option<String>,
    entities: Vec<SceneEntity>,
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    next_id: u64,
    /// The undo-stack top when the file was last written (`None`: the stack was empty).
    saved: Option<u64>,
    /// Loaded from the autosave sidecar: unsaved until exported.
    restored: bool,
    lint: Vec<LintItem>,
}

impl std::fmt::Debug for ScenarioDoc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScenarioDoc")
            .field("rel", &self.rel)
            .field("entities", &self.entities.len())
            .field("dirty", &self.dirty())
            .finish_non_exhaustive()
    }
}

/// A registry with the standard model library, as `sim_hpc` builds it.
pub fn library_registry() -> TypeRegistryArc {
    let mut builder = SimAppBuilder::new(SimConfig::new(1.0, 1));
    sim_models::add_library(&mut builder);
    builder.type_registry()
}

/// Build a scenario into a World with the standard model library and run Tick 0's setup
/// (resolution, setup checks) without stepping, as a run starts.
pub(crate) fn start_world(scenario: Arc<Scenario>) -> Result<SimWorld, String> {
    let mut builder = SimAppBuilder::new(scenario.config().clone());
    sim_models::add_library(&mut builder);
    builder.set_config(scenario.config().clone());
    let mut sim = builder.build().map_err(|e| e.to_string())?;
    sim.set_scenario(scenario).map_err(|e| e.to_string())?;
    sim.start(sim_core::RunSeed::default())
        .map_err(|e| e.to_string())?;
    Ok(sim)
}

/// The autosave sidecar of a scenario file.
pub fn autosave_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(AUTOSAVE_SUFFIX);
    PathBuf::from(name)
}

impl ScenarioDoc {
    /// Open `rel` under `root`. A sidecar newer than the file is restored instead (and the
    /// document starts unsaved); the second value says so.
    pub fn open(root: &Path, rel: &str) -> Result<(Self, bool), String> {
        let path = root.join(rel);
        let sidecar = autosave_path(&path);
        let newer = |a: &Path, b: &Path| -> bool {
            let m = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
            matches!((m(a), m(b)), (Some(a), Some(b)) if a > b)
        };
        if sidecar.exists() && newer(&sidecar, &path) {
            let text = std::fs::read_to_string(&sidecar)
                .map_err(|e| format!("{}: {e}", sidecar.display()))?;
            if let Ok(mut doc) = Self::from_text(rel, &path, &text) {
                doc.restored = true;
                return Ok((doc, true));
            }
        }
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::from_text(rel, &path, &text).map(|d| (d, false))
    }

    /// Parse a scenario's text into a document for `path`.
    pub fn from_text(rel: &str, path: &Path, text: &str) -> Result<Self, String> {
        let registry = library_registry();
        let scenario = {
            let reg = registry.read();
            Scenario::from_ron(text, &reg, sim_models::LIBRARY_VERSION)
                .map_err(|e| format!("{rel}: {e}"))?
        };
        let entities = {
            let reg = registry.read();
            scenario
                .entities()
                .iter()
                .map(|e| e.try_clone(&reg))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| format!("{rel}: a component cannot be copied for editing"))?
        };
        let mut doc = Self {
            rel: rel.to_owned(),
            path: path.to_owned(),
            selected: None,
            focus: None,
            registry,
            meta: scenario.meta(),
            visual: scenario.visual().map(str::to_owned),
            entities,
            undo: Vec::new(),
            redo: Vec::new(),
            next_id: 1,
            saved: None,
            restored: false,
            lint: Vec::new(),
        };
        doc.selected = doc.rows().first().map(|r| r.slot);
        doc.relint();
        Ok(doc)
    }

    /// What the scenario says besides its entities.
    pub fn meta(&self) -> &SceneMeta {
        &self.meta
    }

    /// The entities, in `SimId` order (entities without one last, in file order).
    pub fn rows(&self) -> Vec<EntityRow> {
        let mut rows: Vec<EntityRow> = self
            .entities
            .iter()
            .enumerate()
            .map(|(slot, e)| {
                let names: Vec<&str> = e
                    .components
                    .iter()
                    .map(|c| fields::short(c.reflect_type_path()))
                    .filter(|n| !matches!(*n, "SimId" | "ParamBindings" | "Uncertainties"))
                    .collect();
                EntityRow {
                    slot,
                    sim_id: e.sim_id().map(|s| s.0),
                    summary: names.join(" · "),
                    bound: e.get::<ParamBindings>().is_some_and(|b| !b.0.is_empty()),
                    uncertain: e.get::<Uncertainties>().is_some_and(|u| !u.0.is_empty()),
                }
            })
            .collect();
        rows.sort_by_key(|r| (r.sim_id.is_none(), r.sim_id.unwrap_or(0), r.slot));
        rows
    }

    /// The `SimId` of the entity in `slot`.
    pub fn sim_id(&self, slot: usize) -> Option<u32> {
        self.entities
            .get(slot)
            .and_then(|e| e.sim_id())
            .map(|s| s.0)
    }

    /// The components of the entity in `slot`, in file order.
    pub fn components(&self, slot: usize) -> Vec<ComponentView> {
        let Some(e) = self.entities.get(slot) else {
            return Vec::new();
        };
        e.components
            .iter()
            .map(|c| ComponentView {
                type_path: c.reflect_type_path().to_owned(),
                name: fields::short(c.reflect_type_path()).to_owned(),
                fields: fields::fields(c.as_partial_reflect()),
            })
            .collect()
    }

    fn write(
        &mut self,
        slot: usize,
        component: &str,
        path: &str,
        text: &str,
    ) -> Result<(), String> {
        let c = self
            .entities
            .get_mut(slot)
            .ok_or_else(|| format!("no entity in slot {slot}"))?
            .components
            .iter_mut()
            .find(|c| c.reflect_type_path() == component)
            .ok_or_else(|| format!("the entity has no {}", fields::short(component)))?;
        fields::set(c.as_mut(), path, text)
    }

    fn read(&self, slot: usize, component: &str, path: &str) -> Option<String> {
        self.components(slot)
            .into_iter()
            .find(|c| c.type_path == component)?
            .fields
            .into_iter()
            .find(|f| f.path == path)
            .map(|f| f.text)
    }

    /// Set a field from text. `Ok(false)`: the value did not change, so nothing was recorded.
    /// On error the field keeps its value.
    pub fn set(
        &mut self,
        slot: usize,
        component: &str,
        path: &str,
        text: &str,
    ) -> Result<bool, String> {
        let before = self
            .read(slot, component, path)
            .ok_or_else(|| format!("no field {path} on {}", fields::short(component)))?;
        self.write(slot, component, path, text)?;
        let after = self.read(slot, component, path).expect("just written");
        if after == before {
            return Ok(false);
        }
        let id = self.next_id;
        self.next_id += 1;
        self.undo.push(Edit {
            id,
            slot,
            change: Change::Field {
                component: component.to_owned(),
                path: path.to_owned(),
                before,
                after,
            },
        });
        self.redo.clear();
        self.changed();
        Ok(true)
    }

    /// Change an entity's components with `f`, as one undoable edit. A scene that validated
    /// must still validate afterwards, or the entity is put back and the reason returned.
    pub(crate) fn restructure(
        &mut self,
        slot: usize,
        f: impl FnOnce(&mut SceneEntity, &bevy::reflect::TypeRegistry) -> Result<(), String>,
    ) -> Result<(), String> {
        let was_valid = self.scenario().is_ok();
        let registry = self.registry.clone();
        let reg = registry.read();
        let entity = self
            .entities
            .get_mut(slot)
            .ok_or_else(|| format!("no entity in slot {slot}"))?;
        let before = entity
            .try_clone(&reg)
            .ok_or("a component cannot be copied")?;
        if let Err(e) = f(entity, &reg) {
            self.entities[slot] = before;
            return Err(e);
        }
        if was_valid && let Err(e) = self.scenario() {
            self.entities[slot] = before;
            return Err(e);
        }
        let after = self.entities[slot]
            .try_clone(&reg)
            .ok_or("a component cannot be copied")?;
        drop(reg);
        let id = self.next_id;
        self.next_id += 1;
        self.undo.push(Edit {
            id,
            slot,
            change: Change::Entity { before, after },
        });
        self.redo.clear();
        self.changed();
        Ok(())
    }

    /// Put one side of an edit in place.
    fn replay(&mut self, edit: &Edit, forward: bool) {
        match &edit.change {
            Change::Field {
                component,
                path,
                before,
                after,
            } => {
                let text = if forward { after } else { before };
                self.write(edit.slot, component, path, text)
                    .expect("an edit's values fit its field");
            }
            Change::Entity { before, after } => {
                let side = if forward { after } else { before };
                let copy = side
                    .try_clone(&self.registry.read())
                    .expect("copied once already");
                self.entities[edit.slot] = copy;
            }
        }
        self.selected = Some(edit.slot);
    }

    /// Undo the last edit; `false` if there is none.
    pub fn undo(&mut self) -> bool {
        let Some(edit) = self.undo.pop() else {
            return false;
        };
        self.replay(&edit, false);
        self.redo.push(edit);
        self.changed();
        true
    }

    /// Redo the last undone edit; `false` if there is none.
    pub fn redo(&mut self) -> bool {
        let Some(edit) = self.redo.pop() else {
            return false;
        };
        self.replay(&edit, true);
        self.undo.push(edit);
        self.changed();
        true
    }

    /// Edits that can be undone and redone.
    pub fn history(&self) -> (usize, usize) {
        (self.undo.len(), self.redo.len())
    }

    /// The document differs from the scenario file.
    pub fn dirty(&self) -> bool {
        self.restored || self.undo.last().map(|e| e.id) != self.saved
    }

    fn changed(&mut self) {
        self.relint();
        if self.dirty() {
            self.autosave();
        } else {
            let _ = std::fs::remove_file(autosave_path(&self.path));
        }
    }

    /// The scenario the document describes, validated and hashed.
    pub fn scenario(&self) -> Result<Scenario, String> {
        let reg = self.registry.read();
        let entities = self
            .entities
            .iter()
            .map(|e| e.try_clone(&reg))
            .collect::<Option<Vec<_>>>()
            .ok_or("a component cannot be copied")?;
        Scenario::from_parts(entities, self.meta.clone(), self.visual.as_deref(), &reg)
            .map_err(|e| e.to_string())
    }

    /// The scenario as the file it exports to.
    pub fn to_ron(&self) -> Result<String, String> {
        let scenario = self.scenario()?;
        Ok(scenario.to_ron(&self.registry.read()))
    }

    /// Save the edits to the sidecar (a scenario that does not validate is not saved; the last
    /// valid one stays).
    fn autosave(&self) {
        if let Ok(text) = self.to_ron() {
            let _ = crate::workspace::write_atomic(&autosave_path(&self.path), &text);
        }
    }

    /// Write the canonical scenario file and drop the sidecar (§9.4). Refused while the
    /// scenario has lint errors.
    pub fn export(&mut self) -> Result<&Path, String> {
        let text = self.to_ron()?;
        if let Some(e) = self.lint.iter().find(|l| l.level == LintLevel::Error) {
            return Err(format!("fix the lint errors first: {}", e.text));
        }
        crate::workspace::write_atomic(&self.path, &text).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(autosave_path(&self.path));
        self.saved = self.undo.last().map(|e| e.id);
        self.restored = false;
        Ok(&self.path)
    }

    /// The lint items, errors first.
    pub fn lint(&self) -> &[LintItem] {
        &self.lint
    }

    fn relint(&mut self) {
        self.lint = lint::lint(self);
    }

    /// The registry the document was read with.
    pub fn registry(&self) -> &TypeRegistryArc {
        &self.registry
    }

    /// Every editable field with its `SimId`, for rules that look across the scene.
    pub(crate) fn all_fields(&self) -> Vec<(Option<u32>, ComponentView)> {
        (0..self.entities.len())
            .flat_map(|slot| {
                let id = self.sim_id(slot);
                self.components(slot).into_iter().map(move |c| (id, c))
            })
            .collect()
    }

    /// Build the scenario into a World and run Tick 0's setup (resolution, setup checks) without
    /// stepping: the setup error, or the warnings.
    pub(crate) fn tick0(&self, scenario: Scenario) -> Result<Vec<String>, String> {
        let sim = start_world(Arc::new(scenario))?;
        Ok(sim
            .world()
            .get_resource::<sim_core::ResolveReport>()
            .map(|r| r.warnings.clone())
            .unwrap_or_default())
    }

    /// The `SimId` value of an entity, for messages.
    pub fn label(&self, slot: usize) -> String {
        match self.sim_id(slot) {
            Some(id) => format!("#{id}"),
            None => format!("entity {slot}"),
        }
    }

    /// Whether an entity has a `SimId` of this value (lint uses it to point at offenders).
    pub fn slot_of(&self, id: u32) -> Option<usize> {
        self.entities
            .iter()
            .position(|e| e.sim_id() == Some(SimId(id)))
    }
}
