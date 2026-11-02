//! `sim_assets`: 3D model catalogue (built-in, workspace, library roots), `model.ron` manifests,
//! glTF validation and import, OBJ conversion (stretch), authoring-time collision derivation.
//!
//! Design: `docs/design/subsystems-and-interfaces.md` §5.10, IB-20 (D17).
//! Linked only by `sim_studio` and `saber-cli`; never by `sim_hpc`, `sim_explorer` or `sim_models`.

#[allow(unused_imports)]
use sim_core as _;
#[allow(unused_imports)]
use sim_schema as _;
