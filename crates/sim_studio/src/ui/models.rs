//! Drawing the bodies' 3D models in the viewport (`Studio.Assets`, subsystems §5.5, §5.10;
//! `sim_studio_ux.md` §7.11.6). Each model file is read and hash-checked by `sim_assets`, handed
//! to Bevy through the `embedded://` source (cached per content hash, so many bodies of one model
//! share one mesh), and spawned as a tree under a `ModelRoot` that [`super::viewport::draw_bodies`]
//! places on the body's f64 state each frame: model root (the body's pose) → instance offset and
//! scale → the manifest's frame and origin → one level per file (the model, then its LOD files)
//! → the file's nodes. `draw_bodies` shows the level that fits the model's size on screen
//! ([`crate::models::level_for`]), or none and the far marker.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use bevy::asset::LoadState;
use bevy::asset::io::embedded::EmbeddedAssetRegistry;
use bevy::camera::visibility::RenderLayers;
use bevy::gltf::{Gltf, GltfLoaderSettings, GltfMaterial, GltfMesh, GltfNode};
use bevy::pbr::gltf::standard_material_from_gltf_material;
use bevy::prelude::*;
use sim_assets::ContentHash;
use sim_core::Scenario;
use sim_studio_ui::toast::Toast;

use super::viewport::VIEW_LAYER;
use crate::actions::{CurrentWorkspace, PreviewState};
use crate::models::{Look, LookState, MODEL_ASSET_DIR, ModelLibrary, resolve_looks};
use crate::replay::ReplayState;
use crate::state::AppState;

/// The looks of the scenario on show, and which scenario they are for.
#[derive(Resource, Clone, Debug, Default)]
pub struct SceneLooks {
    /// `(scene_hash, visual_hash, catalogue generation)` of the scenario resolved.
    pub key: Option<(u64, Option<u64>, u64)>,
    /// One per body with a model.
    pub looks: Vec<Look>,
    /// Problems with the scenario's `visual:` section.
    pub problems: Vec<String>,
}

impl SceneLooks {
    /// A body's look.
    pub fn get(&self, sim_id: u32) -> Option<&Look> {
        self.looks.iter().find(|l| l.sim_id == sim_id)
    }
}

/// Model files handed to Bevy, by content hash.
#[derive(Resource, Clone, Debug, Default)]
pub struct ModelStore {
    handles: HashMap<ContentHash, Handle<Gltf>>,
}

impl ModelStore {
    /// The glTF loaded for a hash.
    pub fn handle(&self, hash: &ContentHash) -> Option<&Handle<Gltf>> {
        self.handles.get(hash)
    }
}

/// The top of a body's model tree; placed on the body every frame.
#[derive(Component, Clone, Debug, PartialEq)]
pub struct ModelRoot {
    /// The body.
    pub sim_id: u32,
    /// The model's largest extent at its scale, m.
    pub extent_m: f64,
    /// The looks it was spawned for ([`SceneLooks::key`]).
    pub key: (u64, Option<u64>, u64),
    /// The tree of each level: the model's own file, then its LOD files.
    pub levels: Vec<Entity>,
    /// Each LOD file's size, px across, largest first (one fewer than `levels`).
    pub below_px: Vec<f64>,
}

/// One level of a model tree: 0 is the model's own file, `k` its `k`th LOD file.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelLevel(pub usize);

fn view_layer() -> RenderLayers {
    RenderLayers::layer(VIEW_LAYER)
}

/// Rebuild the catalogue when the workspace or its library roots change.
pub fn refresh_library(ws: Res<CurrentWorkspace>, mut library: ResMut<ModelLibrary>) {
    if ws.is_changed() || library.generation == 0 {
        library.refresh(ws.0.as_ref());
    }
}

/// The scenario the viewport shows: the Preview's, or the replayed run's.
pub fn shown_scenario(
    state: &AppState,
    preview: &PreviewState,
    replay: &ReplayState,
) -> Option<Arc<Scenario>> {
    match state {
        AppState::LocalRun => preview.run.as_ref().map(|r| r.scenario().clone()),
        AppState::CrashReplay | AppState::RunReplay => replay
            .replay
            .as_ref()
            .and_then(|r| r.rerun.as_ref())
            .map(|r| r.scene.0.clone()),
        _ => None,
    }
}

/// Resolve the looks when the scenario on show (or the catalogue) changes, and hand new model
/// files to Bevy.
#[allow(clippy::too_many_arguments)]
pub fn update_looks(
    state: Res<State<AppState>>,
    preview: Res<PreviewState>,
    replay: Res<ReplayState>,
    library: Res<ModelLibrary>,
    mut looks: ResMut<SceneLooks>,
    mut store: ResMut<ModelStore>,
    embedded: Res<EmbeddedAssetRegistry>,
    server: Res<AssetServer>,
    mut toasts: MessageWriter<Toast>,
) {
    let scenario = shown_scenario(state.get(), &preview, &replay);
    let key = scenario
        .as_ref()
        .map(|s| (s.scene_hash(), s.visual_hash(), library.generation));
    if key == looks.key {
        return;
    }
    let resolved = scenario
        .as_ref()
        .map(|s| resolve_looks(s, &library.catalog))
        .unwrap_or_default();
    for (hash, bytes) in resolved.files.iter().cloned() {
        if store.handles.contains_key(&hash) {
            continue;
        }
        let rel = format!("{MODEL_ASSET_DIR}/{}.glb", hash.hex());
        match bytes {
            std::borrow::Cow::Borrowed(b) => {
                embedded.insert_asset(rel.clone().into(), Path::new(&rel), b)
            }
            std::borrow::Cow::Owned(v) => {
                embedded.insert_asset(rel.clone().into(), Path::new(&rel), v)
            }
        }
        let handle = server
            .load_builder()
            .with_settings(|s: &mut GltfLoaderSettings| {
                s.load_cameras = false;
                s.load_lights = false;
                s.load_animations = false;
            })
            .load(crate::models::asset_path(hash));
        store.handles.insert(hash, handle);
    }
    let missing: Vec<String> = resolved
        .placeholders()
        .map(|(l, why)| format!("#{} ({why})", l.sim_id))
        .collect();
    if !missing.is_empty() {
        toasts.write(Toast::warning(format!(
            "{} model{} not found; drawn as markers: {}",
            missing.len(),
            if missing.len() == 1 { "" } else { "s" },
            missing.join(", ")
        )));
    }
    for p in &resolved.problems {
        toasts.write(Toast::warning(p.clone()));
    }
    looks.key = key;
    looks.looks = resolved.looks;
    looks.problems = resolved.problems;
}

/// Standard materials made from a file's materials, by material and tint.
type MaterialCache = HashMap<(AssetId<GltfMaterial>, [u32; 4]), Handle<StandardMaterial>>;

/// The glTF assets a model tree is built from.
#[derive(bevy::ecs::system::SystemParam)]
pub struct GltfAssets<'w> {
    gltfs: Res<'w, Assets<Gltf>>,
    nodes: Res<'w, Assets<GltfNode>>,
    meshes: Res<'w, Assets<GltfMesh>>,
    materials: Res<'w, Assets<GltfMaterial>>,
}

/// Spawn each loaded model's tree once, and drop the trees of another scenario's looks.
#[allow(clippy::too_many_arguments)]
pub fn spawn_models(
    mut commands: Commands,
    mut looks: ResMut<SceneLooks>,
    store: Res<ModelStore>,
    server: Res<AssetServer>,
    assets: GltfAssets,
    mut std_materials: ResMut<Assets<StandardMaterial>>,
    roots: Query<(Entity, &ModelRoot)>,
    state: Res<State<AppState>>,
    mut cache: Local<MaterialCache>,
    mut toasts: MessageWriter<Toast>,
) {
    let Some(key) = looks.key else {
        for (e, _) in &roots {
            commands.entity(e).despawn();
        }
        return;
    };
    let mut spawned = Vec::new();
    for (e, root) in &roots {
        if root.key == key {
            spawned.push(root.sim_id);
        } else {
            commands.entity(e).despawn();
        }
    }
    let state = *state.get();
    for look in looks.looks.iter_mut() {
        if look.state != LookState::Model || spawned.contains(&look.sim_id) {
            continue;
        }
        let (Some(hash), Some(manifest)) = (look.hash, look.manifest.as_ref()) else {
            continue;
        };
        let Some(handle) = store.handle(&hash) else {
            continue;
        };
        if let Some(LoadState::Failed(e)) = server.get_load_state(handle.id()) {
            let why = format!("{} could not be drawn: {e}", look.model.asset);
            toasts.write(Toast::warning(format!("#{}: {why}", look.sim_id)));
            look.state = LookState::Placeholder(why);
            continue;
        }
        let Some(gltf) = assets.gltfs.get(handle) else {
            continue;
        };
        // The LOD files too: wait for each to load; one that fails is left out.
        let mut waiting = false;
        let mut failed = Vec::new();
        for (k, l) in look.lods.iter().enumerate() {
            match store.handle(&l.hash) {
                Some(h) if matches!(server.get_load_state(h.id()), Some(LoadState::Failed(_))) => {
                    failed.push(k)
                }
                Some(h) if assets.gltfs.get(h).is_none() => waiting = true,
                Some(_) => {}
                None => failed.push(k),
            }
        }
        if waiting {
            continue;
        }
        for k in failed.into_iter().rev() {
            let l = look.lods.remove(k);
            toasts.write(Toast::warning(format!(
                "#{}: {}'s {} could not be drawn; the level above is drawn instead",
                look.sim_id, look.model.asset, l.file
            )));
        }
        let files: Vec<&Gltf> = std::iter::once(Some(gltf))
            .chain(
                look.lods
                    .iter()
                    .map(|l| store.handle(&l.hash).and_then(|h| assets.gltfs.get(h))),
            )
            .flatten()
            .collect();
        let offset = &look.model.offset;
        let root = commands
            .spawn((
                Transform::default(),
                Visibility::Hidden,
                view_layer(),
                DespawnOnExit(state),
            ))
            .id();
        let align = commands
            .spawn((
                Transform {
                    translation: offset.translation().as_vec3(),
                    rotation: offset.rotation().as_quat(),
                    scale: Vec3::from_array(look.model.scale.map(|s| s as f32)),
                },
                Visibility::Inherited,
                view_layer(),
                ChildOf(root),
            ))
            .id();
        let frame = commands
            .spawn((
                Transform {
                    translation: manifest.origin.offset().as_vec3(),
                    rotation: manifest.frame.rotation_quat().unwrap_or_default().as_quat(),
                    scale: Vec3::splat(manifest.frame.unit.metres() as f32),
                },
                Visibility::Inherited,
                view_layer(),
                ChildOf(align),
            ))
            .id();
        let tint = look.model.tint;
        let mut material = |m: &Option<Handle<GltfMaterial>>| -> Handle<StandardMaterial> {
            let gm = m
                .as_ref()
                .and_then(|h| assets.materials.get(h).map(|g| (h.id(), g)));
            let id = gm.map(|(id, _)| id).unwrap_or_default();
            let tint_key = tint.unwrap_or([1.0; 4]).map(f32::to_bits);
            cache
                .entry((id, tint_key))
                .or_insert_with(|| {
                    let mut sm = match gm {
                        Some((_, g)) => standard_material_from_gltf_material(g),
                        None => StandardMaterial::default(),
                    };
                    if let Some(t) = tint {
                        let base = sm.base_color.to_linear();
                        let t = Color::srgba(t[0], t[1], t[2], t[3]).to_linear();
                        sm.base_color = Color::LinearRgba(LinearRgba::new(
                            base.red * t.red,
                            base.green * t.green,
                            base.blue * t.blue,
                            base.alpha * t.alpha,
                        ));
                        if t.alpha < 1.0 {
                            sm.alpha_mode = AlphaMode::Blend;
                        }
                    }
                    std_materials.add(sm)
                })
                .clone()
        };
        let mut levels = Vec::with_capacity(files.len());
        for (k, file) in files.into_iter().enumerate() {
            let level = commands
                .spawn((
                    Transform::IDENTITY,
                    if k == 0 {
                        Visibility::Inherited
                    } else {
                        Visibility::Hidden
                    },
                    ModelLevel(k),
                    view_layer(),
                    ChildOf(frame),
                ))
                .id();
            for top in top_nodes(file, &assets) {
                spawn_node(&mut commands, &assets, top, level, &mut material);
            }
            levels.push(level);
        }
        commands.entity(root).insert(ModelRoot {
            sim_id: look.sim_id,
            extent_m: look.extent_m(),
            key,
            levels,
            below_px: look.lods.iter().map(|l| l.below_px).collect(),
        });
        spawned.push(look.sim_id);
    }
}

/// A file's top nodes: those no other node has as a child.
fn top_nodes<'a>(gltf: &'a Gltf, assets: &GltfAssets) -> Vec<&'a Handle<GltfNode>> {
    let children: Vec<usize> = gltf
        .nodes
        .iter()
        .filter_map(|h| assets.nodes.get(h))
        .flat_map(|n| {
            n.children
                .iter()
                .filter_map(|c| assets.nodes.get(c))
                .map(|c| c.index)
        })
        .collect();
    gltf.nodes
        .iter()
        .filter(|h| {
            assets
                .nodes
                .get(*h)
                .is_some_and(|n| !children.contains(&n.index))
        })
        .collect()
}

fn spawn_node(
    commands: &mut Commands,
    assets: &GltfAssets,
    handle: &Handle<GltfNode>,
    parent: Entity,
    material: &mut impl FnMut(&Option<Handle<GltfMaterial>>) -> Handle<StandardMaterial>,
) {
    let Some(node) = assets.nodes.get(handle) else {
        return;
    };
    let entity = commands
        .spawn((
            node.transform,
            Visibility::Inherited,
            view_layer(),
            Name::new(node.name.clone()),
            ChildOf(parent),
        ))
        .id();
    if let Some(mesh) = node.mesh.as_ref().and_then(|m| assets.meshes.get(m)) {
        for p in &mesh.primitives {
            commands.spawn((
                Mesh3d(p.mesh.clone()),
                MeshMaterial3d(material(&p.material)),
                Transform::IDENTITY,
                view_layer(),
                ChildOf(entity),
            ));
        }
    }
    for c in &node.children {
        spawn_node(commands, assets, c, entity, material);
    }
}
