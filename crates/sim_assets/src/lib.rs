//! `sim_assets`: 3D models for entities (D17). The catalogue of built-in, workspace and library
//! models; `model.ron` manifests; the scenario's `visual:` section; a glTF inspector with the
//! import rules; the importer, which packs a `.gltf` into one `.glb`; the OBJ converter; CPU-drawn
//! thumbnails; a GLB writer; and collision shapes derived from a model through
//! `sim_core::geometry::derive`. A model
//! is only how an entity looks: `scene_hash` never covers it, and it changes a result only through
//! a shape derived from it on request, which is then plain data in the scenario.
//!
//! Design: `docs/design/subsystems-and-interfaces.md` §5.10, IB-20; `sim_studio_ux.md` §7.11.
//! Linked only by `sim_studio` and `saber-cli`; never by `sim_hpc`, `sim_explorer` or `sim_models`.
//! It has no Bevy render crates: the studio loads and draws (`Studio.Assets`).

pub mod builtin;
pub mod catalog;
pub mod derive;
pub mod hash;
pub mod import;
pub mod inspect;
pub mod manifest;
pub mod obj;
pub mod thumb;
pub mod visual;
pub mod write;

pub use catalog::{Catalog, CatalogEntry, Loaded, Location, Notice, Resolution};
pub use hash::ContentHash;
pub use import::{Candidate, FrameChosen, ImportError, ImportSettings, Imported, OriginChoice};
pub use inspect::{ModelReport, Problem, Severity, inspect};
pub use manifest::{Axis, Bounds, ModelFrame, Origin, Unit, VisualAssetManifest};
pub use visual::{AssetSource, Pose, Scenery, VisualAssetRef, VisualModel, VisualSection};
