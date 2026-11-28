//! `Assets.ShapeDeriver` (subsystems §5.10; `sequence-diagrams.md` §2.2; `sim_studio_ux.md`
//! §7.11.5): a model's triangles placed as the studio draws them on an entity (the manifest's
//! frame and origin, then the instance's scale and offset), handed to
//! `sim_core::geometry::derive`, and the [`DerivedCollision`] record kept beside the shape. It runs
//! at authoring time only; the shape it makes is plain f64 data in the scenario.

use glam::DVec3;
use sim_core::geometry::derive::{self as core, Derived, ModelMesh};
use sim_core::{DeriveMethod, DerivedCollision};

use crate::catalog::{Catalog, CatalogEntry, Resolution};
use crate::inspect::{GltfData, each_triangle, read};
use crate::manifest::VisualAssetManifest;
use crate::thumb::{Facet, facets};
use crate::visual::{VisualAssetRef, VisualModel};

/// A point of the file (its own units and axes) where the entity draws it: body axes about the
/// body's origin (or the world frame for an entity without a body).
pub fn look_point(manifest: &VisualAssetManifest, look: &VisualModel, file: DVec3) -> DVec3 {
    let p = manifest.to_body(file) * DVec3::from(look.scale);
    look.offset.translation() + look.offset.rotation() * p
}

/// Every drawn triangle of a file, placed by [`look_point`].
pub fn look_mesh(data: &GltfData, manifest: &VisualAssetManifest, look: &VisualModel) -> ModelMesh {
    let mut triangles = Vec::new();
    each_triangle(data, |t, _| {
        triangles.push(t.map(|p| look_point(manifest, look, p)));
    });
    ModelMesh::from_triangles(triangles)
}

/// A catalogue model's triangles placed by `look`, read from its file. Refused when the file no
/// longer matches its manifest (re-import it first), so a record always names a hash the
/// catalogue knows.
pub fn entry_mesh(entry: &CatalogEntry, look: &VisualModel) -> Result<ModelMesh, String> {
    let loaded = entry
        .load()
        .map_err(|e| format!("{}: {e}", entry.manifest.id))?;
    if !loaded.matches_manifest {
        return Err(format!(
            "{}'s file changed since it was imported; re-import it first",
            entry.manifest.id
        ));
    }
    let data = read(&loaded.bytes, None).map_err(|e| format!("{}: {e}", entry.manifest.id))?;
    Ok(look_mesh(&data, &entry.manifest, look))
}

/// A catalogue model's coloured triangles placed by `look`, for pictures (Align, Derive).
pub fn entry_facets(entry: &CatalogEntry, look: &VisualModel) -> Result<Vec<Facet>, String> {
    let loaded = entry
        .load()
        .map_err(|e| format!("{}: {e}", entry.manifest.id))?;
    let data = read(&loaded.bytes, None).map_err(|e| format!("{}: {e}", entry.manifest.id))?;
    Ok(facets(&data)
        .into_iter()
        .map(|(t, c)| (t.map(|p| look_point(&entry.manifest, look, p)), c))
        .collect())
}

/// What a derivation from a catalogue model made, with the record to keep beside it.
#[derive(Clone, Debug, PartialEq)]
pub struct FromModel {
    /// The shape and its numbers.
    pub derived: Derived,
    /// The record for the sim section.
    pub record: DerivedCollision,
    /// The model's own enclosed volume, m³, when its mesh is closed.
    pub mesh_volume: Option<f64>,
}

/// The record of a shape derived from `asset` (found as `entry`).
pub fn record(
    asset: &VisualAssetRef,
    entry: &CatalogEntry,
    method: DeriveMethod,
    max_parts: u32,
) -> DerivedCollision {
    DerivedCollision {
        model: asset.to_string(),
        content_hash: entry.manifest.content_hash.to_string(),
        method,
        max_parts: if method == DeriveMethod::ConvexDecomposition {
            max_parts.clamp(1, core::MAX_PARTS)
        } else {
            0
        },
    }
}

/// Derive a collision shape from the model `look` links, placed as it is drawn. `on_body`: the
/// entity has a body (a triangle mesh is refused).
pub fn derive(
    catalog: &Catalog,
    look: &VisualModel,
    method: DeriveMethod,
    max_parts: u32,
    on_body: bool,
) -> Result<FromModel, String> {
    let entry = match catalog.resolve(&look.asset) {
        Resolution::Found(e, _) => e,
        Resolution::Missing => return Err(format!("{} not found", look.asset)),
    };
    let mesh = entry_mesh(entry, look)?;
    let derived = core::derive(&mesh, method, max_parts, on_body)?;
    Ok(FromModel {
        derived,
        record: record(&look.asset, entry, method, max_parts),
        mesh_volume: mesh.volume(),
    })
}

/// Why a derived shape may no longer match its entity's look (`shown`: the model the entity
/// shows now, linked or by default), for lint. Empty when it still matches or the model cannot
/// be found (a missing model is reported on its own).
pub fn derived_notes(
    record: &DerivedCollision,
    shown: Option<&VisualAssetRef>,
    catalog: &Catalog,
) -> Vec<String> {
    let Some(shown) = shown else {
        return vec![format!(
            "collision was derived from {}, and the entity no longer has a model",
            record.model
        )];
    };
    if shown.to_string() != record.model {
        return vec![format!(
            "collision was derived from {}, but the entity now shows {shown}; re-derive it or \
             keep it as it is",
            record.model
        )];
    }
    match catalog.resolve(shown) {
        Resolution::Found(entry, _)
            if entry.manifest.content_hash.to_string() != record.content_hash
                || catalog.file_changed(entry) =>
        {
            vec![format!(
                "collision was derived from an older version of {}; re-derive it to follow the \
                 model (the studio never re-derives on its own)",
                entry.manifest.slug()
            )]
        }
        _ => Vec::new(),
    }
}
