//! `Studio.Assets` (subsystems §5.5, §5.10; `sim_studio_ux.md` §7.11.6): which 3D model each body
//! of the scenario on show is drawn with. A body's model comes from the scenario's `visual:`
//! section, else from its Domain Pack's default; a model that is missing or unreadable leaves the
//! body's marker as its placeholder. Looks never change results: nothing here touches the run.

use std::borrow::Cow;
use std::path::PathBuf;

use bevy::prelude::*;
use sim_assets::{
    Catalog, ContentHash, Notice, Resolution, VisualAssetManifest, VisualAssetRef, VisualModel,
    VisualSection,
};
use sim_core::{Scenario, SceneEntity};

use crate::workspace::Workspace;

/// The folder of the `embedded://` asset source the studio hands model files to Bevy through:
/// files are read and hash-checked by `sim_assets`, so Bevy never opens a path itself.
pub const MODEL_ASSET_DIR: &str = "saber_models";

/// A model drawn at under this many pixels across is hidden, leaving the body's marker.
pub const MIN_MODEL_PX: f64 = 4.0;

/// A model drawn at over this many pixels across hides the body's marker.
pub const HIDE_MARKER_PX: f64 = 16.0;

/// A body's look from its physics model (the Domain Packs' defaults, §7.11.2): a quadrotor or
/// hexarotor, a spin-stabilised projectile, a fixed-wing aircraft. Other bodies keep their marker.
pub fn domain_default(entity: &SceneEntity) -> Option<VisualAssetRef> {
    if let Some(r) = entity.get::<sim_models::rotorcraft::Rotorcraft>() {
        let id = if r.rotors.len() >= 6 {
            "rotorcraft/hex_x"
        } else {
            "rotorcraft/quad_x"
        };
        return Some(VisualAssetRef::builtin(id, 1));
    }
    if entity
        .get::<sim_models::ballistics::ProjectileAero>()
        .is_some()
    {
        return Some(VisualAssetRef::builtin("ballistics/ogive_projectile", 1));
    }
    if entity
        .get::<sim_models::fixedwing::FixedWingAero>()
        .is_some()
    {
        return Some(VisualAssetRef::builtin("fixedwing/generic_trainer", 1));
    }
    None
}

/// Whether a body is drawn with its model or a placeholder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LookState {
    /// With its model (once the file has loaded).
    Model,
    /// With its marker: the model is not found or could not be read or drawn.
    Placeholder(String),
}

/// How one body is drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct Look {
    /// The body.
    pub sim_id: u32,
    /// Its link: from the scenario, or the Domain Pack default.
    pub model: VisualModel,
    /// The link is the Domain Pack's default, not the scenario's.
    pub default: bool,
    /// The model's manifest, when found.
    pub manifest: Option<VisualAssetManifest>,
    /// The hash of the file handed to Bevy, when read.
    pub hash: Option<ContentHash>,
    /// What differs from the link (drawn anyway).
    pub warnings: Vec<String>,
    /// Model or placeholder.
    pub state: LookState,
}

impl Look {
    /// The `embedded://` path the file is loaded from.
    pub fn asset_path(&self) -> Option<String> {
        self.hash.map(asset_path)
    }

    /// The model's largest extent at the instance's scale, m.
    pub fn extent_m(&self) -> f64 {
        let Some(m) = &self.manifest else {
            return 0.0;
        };
        let scale = self.model.scale.iter().copied().fold(0.0, f64::max);
        m.bounds.size().max_element() * scale
    }
}

/// The `embedded://` path of a model file by its hash.
pub fn asset_path(hash: ContentHash) -> String {
    format!("embedded://{MODEL_ASSET_DIR}/{}.glb", hash.hex())
}

/// The looks of a scenario's bodies, and the files to hand to Bevy.
#[derive(Clone, Debug, Default)]
pub struct Resolved {
    /// One per body that has a model (linked or default) and is visible, in `SimId` order.
    pub looks: Vec<Look>,
    /// Model files read, by hash (each once).
    pub files: Vec<(ContentHash, Cow<'static, [u8]>)>,
    /// Problems with the `visual:` section itself.
    pub problems: Vec<String>,
}

impl Resolved {
    /// The bodies drawn as placeholders, with why.
    pub fn placeholders(&self) -> impl Iterator<Item = (&Look, &str)> {
        self.looks.iter().filter_map(|l| match &l.state {
            LookState::Placeholder(why) => Some((l, why.as_str())),
            LookState::Model => None,
        })
    }
}

/// Resolve every body's look through the catalogue and read the files (each once).
pub fn resolve_looks(scenario: &Scenario, catalog: &Catalog) -> Resolved {
    let mut out = Resolved::default();
    let section = match scenario.visual().map(VisualSection::from_ron) {
        Some(Ok(v)) => Some(v),
        Some(Err(e)) => {
            out.problems.push(e.to_string());
            None
        }
        None => None,
    };
    for entity in scenario.entities() {
        let Some(id) = entity.sim_id() else { continue };
        let linked = section.as_ref().and_then(|s| s.models.get(&id.0)).cloned();
        let (model, default) = match linked {
            Some(m) => (m, false),
            None => match domain_default(entity) {
                Some(r) => (VisualModel::new(r), true),
                None => continue,
            },
        };
        if !model.visible {
            continue;
        }
        let mut look = Look {
            sim_id: id.0,
            model,
            default,
            manifest: None,
            hash: None,
            warnings: Vec::new(),
            state: LookState::Model,
        };
        match catalog.resolve(&look.model.asset) {
            Resolution::Missing => {
                look.state = LookState::Placeholder(format!("{} not found", look.model.asset));
            }
            Resolution::Found(entry, notices) => {
                for n in notices {
                    look.warnings.push(match n {
                        Notice::OtherVersion { linked, installed } => format!(
                            "{} is version {installed} here, linked at version {linked}",
                            entry.manifest.id
                        ),
                        Notice::HashChanged { .. } => {
                            format!("{} changed since it was linked", entry.manifest.id)
                        }
                    });
                }
                match entry.load() {
                    Ok(loaded) => {
                        if !loaded.matches_manifest {
                            look.warnings.push(format!(
                                "{}'s file changed since it was imported",
                                entry.manifest.id
                            ));
                        }
                        if !out.files.iter().any(|(h, _)| *h == loaded.hash) {
                            out.files.push((loaded.hash, loaded.bytes));
                        }
                        look.hash = Some(loaded.hash);
                        look.manifest = Some(entry.manifest.clone());
                    }
                    Err(e) => {
                        look.state = LookState::Placeholder(format!(
                            "{} could not be read: {e}",
                            look.model.asset
                        ));
                    }
                }
            }
        }
        out.looks.push(look);
    }
    out.looks.sort_by_key(|l| l.sim_id);
    out
}

/// The catalogue for the open workspace: the built-ins, its `models/` and its library roots.
#[derive(Resource, Clone, Debug, Default)]
pub struct ModelLibrary {
    /// Every model that can be linked.
    pub catalog: Catalog,
    /// Bumped whenever the catalogue is rebuilt.
    pub generation: u64,
    built_for: Option<Option<(PathBuf, Vec<PathBuf>)>>,
}

impl ModelLibrary {
    /// Rebuild the catalogue when the workspace (or its library roots) changed.
    pub fn refresh(&mut self, ws: Option<&Workspace>) -> bool {
        let key = ws.map(|w| (w.root.clone(), w.file.model_roots.clone()));
        if self.built_for.as_ref() == Some(&key) {
            return false;
        }
        let mut catalog = Catalog::with_builtins();
        if let Some(w) = ws {
            catalog.add_workspace(&w.root);
            for r in &w.file.model_roots {
                let root = if r.is_absolute() {
                    r.clone()
                } else {
                    w.root.join(r)
                };
                catalog.add_library(&root);
            }
        }
        self.catalog = catalog;
        self.generation += 1;
        self.built_for = Some(key);
        true
    }
}
