//! The Builder's 3D models (`sim_studio_ux.md` §7.11.1–§7.11.3): the Library's 3D Models tab in
//! the left dock (search, source filter, thumbnail tiles), the selected model's details or the
//! Import dialog in the center, and the Inspector's Appearance section on the right. Every change
//! goes through [`StudioCommand`]s; panels refresh on [`DocChanged`].

use std::collections::HashMap;

use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::scene::{CommandsSceneExt, SceneList, bsn, bsn_list};
use bevy::window::FileDragAndDrop;
use sim_assets::import::OriginChoice;
use sim_assets::inspect::{guess_unit, show_length};
use sim_assets::thumb::{Picture, THUMB_FILE, thumbnail};
use sim_assets::{AssetSource, Axis, CatalogEntry, ContentHash, Location, Severity, Unit};
use sim_studio_ui::prelude::*;

use super::{action, child};
use crate::actions::{BuilderTab, DocChanged, ReqPanel, StudioCommand};
use crate::builder::looks::describe;
use crate::builder::{Appearance, LookChoice, LookField, LookStatus, ScenarioDoc};
use crate::library::{
    ImportDialog, ImportEdit, ModelPanel, PREVIEW_PX, destinations, key_of, source_heading,
};
use crate::models::ModelLibrary;
use crate::state::AppState;
use crate::workspace::Workspace;

/// A thumbnail tile's image edge, px.
const TILE_PX: f32 = 64.0;

/// An Appearance field: which entity and part of its look it edits.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct LookFieldOf {
    /// The entity's document slot.
    pub slot: usize,
    /// The field.
    pub field: LookField,
}

/// The catalogue's thumbnails as images, and the Import dialog's preview.
#[derive(Resource, Debug, Default)]
pub struct Thumbs {
    by: HashMap<(AssetSource, String, ContentHash), Handle<Image>>,
    generation: u64,
    /// The preview's image and the dialog revision it shows.
    pub preview: Option<(u64, Handle<Image>)>,
}

impl Thumbs {
    /// A model's thumbnail.
    pub fn get(&self, entry: &CatalogEntry) -> Option<Handle<Image>> {
        self.by
            .get(&(
                entry.source.clone(),
                entry.manifest.id.clone(),
                entry.manifest.content_hash,
            ))
            .cloned()
    }
}

fn image_of(p: &Picture) -> Image {
    Image::new(
        Extent3d {
            width: p.width.max(1),
            height: p.height.max(1),
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        p.rgba.clone(),
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    )
}

/// A model's thumbnail picture: its `thumb.png` (or the built-in's), else drawn now from its file
/// and saved beside it (§7.11.1: rendered on first sight and cached next to the model).
fn thumb_picture(entry: &CatalogEntry) -> Option<Picture> {
    if let Some(p) = entry.thumb().and_then(|b| Picture::from_png(&b)) {
        return Some(p);
    }
    let loaded = entry.load().ok()?;
    let data = sim_assets::inspect::read(&loaded.bytes, None).ok()?;
    let png = thumbnail(&data, &entry.manifest);
    if let Location::Dir(dir) = &entry.location {
        // A read-only library root keeps the picture in memory only.
        let _ = std::fs::write(dir.join(THUMB_FILE), &png);
    }
    Picture::from_png(&png)
}

/// Turn the catalogue's thumbnails into images when it changes, and the Import dialog's preview
/// when it is redrawn; panels refresh when either is new.
pub fn load_thumbs(
    library: Res<ModelLibrary>,
    panel: Res<ModelPanel>,
    mut thumbs: ResMut<Thumbs>,
    mut images: ResMut<Assets<Image>>,
    mut changed: MessageWriter<DocChanged>,
) {
    let mut new = false;
    if thumbs.generation != library.generation {
        thumbs.generation = library.generation;
        for e in library.catalog.entries() {
            let key = (
                e.source.clone(),
                e.manifest.id.clone(),
                e.manifest.content_hash,
            );
            if thumbs.by.contains_key(&key) {
                continue;
            }
            if let Some(p) = thumb_picture(e) {
                thumbs.by.insert(key, images.add(image_of(&p)));
                new = true;
            }
        }
    }
    if let Some(d) = &panel.import
        && thumbs.preview.as_ref().is_none_or(|(rev, _)| *rev != d.rev)
    {
        thumbs.preview = d
            .preview
            .as_ref()
            .map(|p| (d.rev, images.add(image_of(&p.picture))));
        new = true;
    }
    if new {
        changed.write(DocChanged);
    }
}

/// A `.glb` or `.gltf` dropped on the window while the Builder shows opens the Import dialog.
pub fn drop_files(
    mut drops: MessageReader<FileDragAndDrop>,
    state: Res<State<AppState>>,
    mut out: MessageWriter<StudioCommand>,
) {
    for d in drops.read() {
        if let FileDragAndDrop::DroppedFile { path_buf, .. } = d
            && *state.get() == AppState::ScenarioBuilder
        {
            out.write(StudioCommand::ImportOpen(Some(path_buf.clone())));
        }
    }
}

fn field_label(text: impl Into<String>) -> impl bevy::scene::Scene {
    let text = text.into();
    bsn! {
        @muted(text)
        Node {
            width: px(110),
            flex_shrink: 0.0,
        }
    }
}

fn wide_row() -> impl bevy::scene::Scene {
    bsn! {
        @row()
        Node {
            width: percent(100),
        }
    }
}

fn wrap_row() -> impl bevy::scene::Scene {
    bsn! {
        @wide_row()
        Node {
            flex_wrap: FlexWrap::Wrap,
            row_gap: px(4),
        }
    }
}

/// A choice among several: the current one is marked ▸ and does nothing.
fn choice(commands: &mut Commands, row: Entity, name: &str, on: bool, command: StudioCommand) {
    let content: Box<dyn SceneList> = if on {
        Box::new(bsn_list! { @status_text(Status::Running, format!("▸ {name}")) })
    } else {
        Box::new(bsn_list! { @label(name.to_owned()) })
    };
    let a11y = if on {
        format!("{name} (chosen)")
    } else {
        name.to_owned()
    };
    action(commands, row, plain_row(a11y, content), command);
}

fn spawn_image(commands: &mut Commands, parent: Entity, image: Handle<Image>, size: f32) {
    commands.spawn((
        Node {
            width: px(size),
            height: px(size),
            flex_shrink: 0.0,
            ..default()
        },
        ImageNode::new(image),
        ChildOf(parent),
    ));
}

/// The left dock in the 3D Models tab: search, source filter and the models as tiles.
pub fn tab(
    commands: &mut Commands,
    body: Entity,
    panel: &ModelPanel,
    library: &ModelLibrary,
    thumbs: &Thumbs,
) {
    let catalog = &library.catalog;
    child(commands, body, heading("3D Models"));
    let r = child(commands, body, wide_row());
    child(commands, r, field_label("Search"));
    commands
        .spawn_scene(value_field(panel.query.clone(), "Search models"))
        .insert(ChildOf(r))
        .observe(
            |ev: On<KitSubmit>, panel: Res<ModelPanel>, mut out: MessageWriter<StudioCommand>| {
                if ev.value != panel.query {
                    out.write(StudioCommand::LibraryQuery(ev.value.clone()));
                }
            },
        );
    let r = child(commands, body, wrap_row());
    child(commands, r, muted("Source"));
    for f in ModelPanel::filters(catalog) {
        let on = f == panel.source;
        choice(commands, r, &f.label(), on, StudioCommand::LibrarySource(f));
    }
    let groups = panel.listed(catalog);
    if groups.is_empty() {
        child(commands, body, muted("No model matches."));
    }
    for (source, entries) in groups {
        child(
            commands,
            body,
            label(format!("▾ {} ({})", source_heading(&source), entries.len())),
        );
        let r = child(commands, body, wrap_row());
        for e in entries {
            let key = key_of(e);
            let selected = panel.selected.as_ref() == Some(&key);
            let changed = catalog.file_changed(e);
            let tile = commands
                .spawn((
                    Node {
                        display: Display::Flex,
                        flex_direction: FlexDirection::Column,
                        align_items: AlignItems::Center,
                        width: px(TILE_PX + 28.0),
                        ..default()
                    },
                    ChildOf(r),
                ))
                .id();
            if let Some(img) = thumbs.get(e) {
                spawn_image(commands, tile, img, TILE_PX);
            }
            let name = format!(
                "{}{}{}",
                if selected { "▸ " } else { "" },
                e.manifest.slug(),
                if changed { " ⚠" } else { "" }
            );
            let a11y = format!(
                "{}{} ({}){}",
                if selected { "Selected: " } else { "Select " },
                e.manifest.slug(),
                source_heading(&e.source),
                if changed {
                    ", file changed since import"
                } else {
                    ""
                }
            );
            let content: Box<dyn SceneList> = if selected {
                Box::new(bsn_list! { @status_text(Status::Running, name) })
            } else {
                Box::new(bsn_list! { @label(name) })
            };
            action(
                commands,
                tile,
                plain_row(a11y, content),
                StudioCommand::LibrarySelect(Some(key)),
            );
        }
    }
    if !catalog.skipped.is_empty() {
        child(
            commands,
            body,
            muted(format!(
                "{} folder(s) skipped: {}",
                catalog.skipped.len(),
                catalog
                    .skipped
                    .iter()
                    .map(|s| format!("{} ({})", s.dir.display(), s.reason))
                    .collect::<Vec<_>>()
                    .join("; ")
            )),
        );
    }
    action(
        commands,
        body,
        button("+ Import model…"),
        StudioCommand::ImportOpen(None),
    );
}

/// The center in the 3D Models tab: the Import dialog while open, else the selected model.
pub fn center(
    commands: &mut Commands,
    body: Entity,
    doc: &ScenarioDoc,
    panel: &ModelPanel,
    library: &ModelLibrary,
    thumbs: &Thumbs,
    ws: Option<&Workspace>,
) {
    if let Some(d) = &panel.import {
        import_dialog(commands, body, d, thumbs, ws);
        return;
    }
    let entry = panel
        .selected
        .as_ref()
        .and_then(|(s, id)| library.catalog.get(s, id));
    let Some(e) = entry else {
        child(commands, body, title("3D Models"));
        child(
            commands,
            body,
            muted(
                "Select a model in the Library to see it, link it to the selected entity, or copy it into the workspace. Import model… adds a glTF or GLB file. Looks never change results.",
            ),
        );
        return;
    };
    let m = &e.manifest;
    child(commands, body, title(m.display_name.clone()));
    let r = child(commands, body, row());
    if let Some(img) = thumbs.get(e) {
        spawn_image(commands, r, img, 128.0);
    }
    let info = child(commands, r, column());
    let size = m.bounds.size();
    for line in [
        format!("{} · {}", e.reference(), source_heading(&e.source)),
        format!(
            "{} × {} × {} · {} triangles",
            show_length(size.x),
            show_length(size.y),
            show_length(size.z),
            m.triangles
        ),
        format!(
            "source {} · license {}{}",
            match &m.source {
                sim_assets::manifest::FileSource::Native => "glTF".to_owned(),
                sim_assets::manifest::FileSource::Converted { format, .. } =>
                    format!("{format:?}, packed on import"),
            },
            m.license,
            m.attribution
                .as_ref()
                .map(|a| format!(" ({a})"))
                .unwrap_or_default()
        ),
        format!("hash {}", m.content_hash.short()),
    ] {
        child(commands, info, mono(line));
    }
    if library.catalog.file_changed(e) {
        child(
            commands,
            body,
            status_text(
                Status::Warning,
                "⚠ The file changed since it was imported. Re-import records its new hash; entities that link it keep the old one until picked again.",
            ),
        );
    }
    let users = doc.users(e);
    let r = child(commands, body, wrap_row());
    if users.is_empty() {
        child(commands, r, muted("Used by no entity in this scenario."));
    } else {
        child(commands, r, muted("Used by"));
        for id in users {
            if let Some(slot) = doc.slot_of(id) {
                action(
                    commands,
                    r,
                    plain_button(format!("#{id}")),
                    StudioCommand::Select(slot),
                );
            }
        }
    }
    let r = child(commands, body, wrap_row());
    if let Some(slot) = doc.selected
        && doc.sim_id(slot).is_some()
    {
        action(
            commands,
            r,
            primary_button(format!("Link to {}", doc.label(slot))),
            StudioCommand::PickModel(slot, LookChoice::Model(e.reference())),
        );
    }
    if e.source != AssetSource::Workspace && ws.is_some() {
        action(
            commands,
            r,
            button("Duplicate to workspace"),
            StudioCommand::LibraryDuplicate(key_of(e)),
        );
    }
    if library.catalog.file_changed(e) && matches!(e.location, Location::Dir(_)) {
        action(
            commands,
            r,
            button("Re-import"),
            StudioCommand::LibraryReimport(key_of(e)),
        );
    }
    child(
        commands,
        body,
        muted(
            "ℹ Appearance only: linking or changing a model never changes scene_hash or a result.",
        ),
    );
}

fn import_dialog(
    commands: &mut Commands,
    body: Entity,
    d: &ImportDialog,
    thumbs: &Thumbs,
    ws: Option<&Workspace>,
) {
    child(commands, body, title("Import 3D model"));
    let r = child(commands, body, wide_row());
    child(commands, r, field_label("File"));
    commands
        .spawn_scene(value_field(d.path.clone(), "Import file path"))
        .insert(ChildOf(r))
        .observe(
            |ev: On<KitSubmit>, panel: Res<ModelPanel>, mut out: MessageWriter<StudioCommand>| {
                if panel.import.as_ref().is_some_and(|d| d.path != ev.value) {
                    out.write(StudioCommand::ImportSet(ImportEdit::Path(ev.value.clone())));
                }
            },
        );
    if let Some(Err(e)) = &d.opened {
        child(commands, body, status_text(Status::Fail, e.clone()));
    }
    let (Some(c), Some(s)) = (d.candidate(), &d.settings) else {
        child(
            commands,
            body,
            muted(
                "Type or paste the path of a .glb or .gltf file and press Enter, or drop the file on the window. A .gltf with separate files is packed into one .glb; the original is left untouched.",
            ),
        );
        footer(commands, body, d);
        return;
    };
    if !c.report.external.is_empty() {
        child(
            commands,
            body,
            muted(format!("(+ {})", c.report.external.join(", "))),
        );
    }
    let r = child(commands, body, row());
    let left = child(commands, r, column());
    if let Some((_, img)) = &thumbs.preview {
        spawn_image(commands, left, img.clone(), PREVIEW_PX as f32);
    }
    child(
        commands,
        left,
        muted("body frame: x forward (red), y right (green), z down (blue)"),
    );
    if let Some(m) = d.preview.as_ref().and_then(|p| p.bar_m) {
        child(commands, left, muted(format!("bar = {}", show_length(m))));
    }
    if let Some(b) = c.bounds(s) {
        let size = b.size();
        child(
            commands,
            left,
            mono(format!(
                "{} × {} × {}",
                show_length(size.x),
                show_length(size.y),
                show_length(size.z)
            )),
        );
    }
    child(
        commands,
        left,
        mono(format!(
            "{} triangles · {} materials",
            c.report.triangles, c.report.materials
        )),
    );
    let right = child(commands, r, column());
    let row_in = |commands: &mut Commands, name: &str| {
        let r = child(commands, right, wrap_row());
        child(commands, r, field_label(name.to_owned()));
        r
    };
    let r = row_in(commands, "Name");
    commands
        .spawn_scene(value_field(s.name.clone(), "Model name"))
        .insert(ChildOf(r))
        .observe(|ev: On<KitSubmit>, mut out: MessageWriter<StudioCommand>| {
            out.write(StudioCommand::ImportSet(ImportEdit::Name(ev.value.clone())));
        });
    let dests = ws.map(destinations).unwrap_or_default();
    let dest = dests.get(d.dest).or(dests.first());
    let r = row_in(commands, "Id");
    child(
        commands,
        r,
        mono(format!(
            "{}{}",
            dest.map_or("models/", |x| x.prefix),
            sim_assets::import::slug(&s.name)
        )),
    );
    let guess = c.report.bounds.as_ref().map(guess_unit);
    let r = row_in(commands, "Units");
    for u in Unit::ALL {
        choice(
            commands,
            r,
            u.short(),
            s.frame.unit == u,
            StudioCommand::ImportSet(ImportEdit::Unit(u)),
        );
    }
    if guess == Some(s.frame.unit) {
        child(commands, r, muted("ℹ guessed from the size"));
    }
    for (name, current, make) in [
        ("Up", s.frame.up, ImportEdit::Up as fn(Axis) -> ImportEdit),
        (
            "Forward",
            s.frame.forward,
            ImportEdit::Forward as fn(Axis) -> ImportEdit,
        ),
    ] {
        let r = row_in(commands, name);
        for a in Axis::ALL {
            choice(
                commands,
                r,
                a.label(),
                current == a,
                StudioCommand::ImportSet(make(a)),
            );
        }
    }
    let r = row_in(commands, "Origin");
    for o in OriginChoice::NAMED {
        choice(
            commands,
            r,
            o.label(),
            s.origin == o,
            StudioCommand::ImportSet(ImportEdit::Origin(o)),
        );
    }
    let r = row_in(commands, "Save to");
    for (i, x) in dests.iter().enumerate() {
        choice(
            commands,
            r,
            &x.label,
            d.dest == i,
            StudioCommand::ImportSet(ImportEdit::Dest(i)),
        );
    }
    let r = row_in(commands, "License");
    commands
        .spawn_scene(value_field(s.license.clone(), "Model license"))
        .insert(ChildOf(r))
        .observe(|ev: On<KitSubmit>, mut out: MessageWriter<StudioCommand>| {
            out.write(StudioCommand::ImportSet(ImportEdit::License(
                ev.value.clone(),
            )));
        });
    let r = row_in(commands, "Replace");
    choice(
        commands,
        r,
        "Keep an existing model",
        !d.replace,
        StudioCommand::ImportSet(ImportEdit::Replace(false)),
    );
    choice(
        commands,
        r,
        "Replace it",
        d.replace,
        StudioCommand::ImportSet(ImportEdit::Replace(true)),
    );
    for p in d.problems() {
        let status = match p.severity {
            Severity::Refused => Status::Fail,
            Severity::Warning => Status::Warning,
            Severity::Info => Status::Running,
        };
        let mark = match p.severity {
            Severity::Refused => "✖",
            Severity::Warning => "⚠",
            Severity::Info => "ℹ",
        };
        child(
            commands,
            body,
            status_text(status, format!("{mark} {}", p.message)),
        );
    }
    footer(commands, body, d);
}

fn footer(commands: &mut Commands, body: Entity, d: &ImportDialog) {
    let r = child(commands, body, wide_row());
    child(commands, r, spacer());
    action(commands, r, button("Cancel"), StudioCommand::ImportCancel);
    match d.blocked() {
        None => {
            action(
                commands,
                r,
                primary_button("Import"),
                StudioCommand::ImportCommit,
            );
        }
        Some(why) => {
            child(
                commands,
                r,
                status_text(Status::Fail, format!("Cannot import: {why}")),
            );
        }
    }
}

/// The layout the Appearance section shows, for [`super::builder::InspectorOf`]: it is rebuilt
/// only when this changes; values are updated in place.
pub fn appearance_key(doc: &ScenarioDoc, panel: &ModelPanel) -> String {
    let Some(a) = doc.selected.and_then(|s| doc.appearance(s)) else {
        return String::new();
    };
    let status = match &a.status {
        LookStatus::Found { notes, .. } => format!("found{}", notes.len()),
        other => format!("{other:?}"),
    };
    format!(
        "look:{:?}|{}|{status}|{}",
        a.shown(),
        a.visible,
        panel.picking
    )
}

/// The Inspector's Appearance section (§7.11.2) for the entity in `slot`.
pub fn appearance(
    commands: &mut Commands,
    body: Entity,
    doc: &ScenarioDoc,
    slot: usize,
    panel: &ModelPanel,
    library: &ModelLibrary,
) {
    let head = child(commands, body, wide_row());
    child(commands, head, heading("Appearance"));
    let Some(a) = doc.appearance(slot) else {
        child(
            commands,
            body,
            muted("Looks are kept by SimId: give the entity a SimId to give it a model."),
        );
        return;
    };
    let r = child(commands, body, wide_row());
    child(commands, r, field_label("Model"));
    match (&a.linked, &a.default) {
        (Some(m), _) => {
            child(commands, r, label(describe(&m.asset)));
        }
        (None, Some(d)) => {
            child(commands, r, muted(format!("default: {}", describe(d))));
        }
        (None, None) => {
            child(commands, r, muted("none (marker only)"));
        }
    }
    child(commands, r, spacer());
    action(
        commands,
        r,
        plain_button(if panel.picking { "Close" } else { "Change…" }),
        StudioCommand::ModelPicker(!panel.picking),
    );
    status_lines(commands, body, &a);
    if panel.picking {
        picker(commands, body, slot, &a, library);
    }
    if a.shown().is_some() && a.visible {
        for f in LookField::ALL {
            let r = child(commands, body, wide_row());
            child(commands, r, field_label(f.label()));
            let target = LookFieldOf { slot, field: f };
            commands
                .spawn_scene(value_field(
                    a.text(f).to_owned(),
                    format!("Appearance.{} ({})", f.label(), f.unit()),
                ))
                .insert((ChildOf(r), target))
                .observe(
                    move |ev: On<KitSubmit>,
                          docs: Res<crate::actions::BuilderDoc>,
                          mut out: MessageWriter<StudioCommand>| {
                        let current = docs
                            .0
                            .as_ref()
                            .and_then(|d| d.appearance(target.slot))
                            .map(|a| a.text(target.field).to_owned());
                        if current.as_deref() != Some(ev.value.as_str()) {
                            out.write(StudioCommand::SetLook {
                                slot: target.slot,
                                field: target.field,
                                text: ev.value.clone(),
                            });
                        }
                    },
                );
            child(commands, r, muted(f.unit()));
        }
        let r = child(commands, body, wide_row());
        child(commands, r, field_label("Visible"));
        child(commands, r, label("yes"));
        action(
            commands,
            r,
            plain_button("Hide the model"),
            StudioCommand::LookVisible(slot, false),
        );
    } else if a.shown().is_some() {
        let r = child(commands, body, wide_row());
        child(commands, r, field_label("Visible"));
        child(commands, r, muted("no (marker only)"));
        action(
            commands,
            r,
            plain_button("Show the model"),
            StudioCommand::LookVisible(slot, true),
        );
    }
    child(
        commands,
        body,
        muted("ℹ Appearance only: does not affect results or scene_hash."),
    );
}

fn status_lines(commands: &mut Commands, body: Entity, a: &Appearance) {
    match &a.status {
        LookStatus::Found { triangles, notes } => {
            child(
                commands,
                body,
                status_text(Status::Pass, format!("✓ found · {triangles} triangles")),
            );
            for n in notes {
                child(
                    commands,
                    body,
                    status_text(Status::Warning, format!("⚠ {n}")),
                );
            }
        }
        LookStatus::Missing => {
            child(
                commands,
                body,
                status_text(
                    Status::Warning,
                    "⚠ not found: drawn as a marker. Pick another model, or import it into the workspace.",
                ),
            );
        }
        LookStatus::Hidden => {
            child(commands, body, muted("hidden: drawn as a marker"));
        }
        LookStatus::Plain => {
            child(
                commands,
                body,
                muted("Its physics model has no default look; pick one, or leave the marker."),
            );
        }
    }
}

fn picker(
    commands: &mut Commands,
    body: Entity,
    slot: usize,
    a: &Appearance,
    library: &ModelLibrary,
) {
    let menu = child(commands, body, panel());
    child(commands, menu, heading("Pick a model"));
    let r = child(commands, menu, wrap_row());
    if let Some(d) = &a.default {
        choice(
            commands,
            r,
            &format!("Default ({})", describe(d)),
            a.linked.is_none(),
            StudioCommand::PickModel(slot, LookChoice::Default),
        );
    }
    choice(
        commands,
        r,
        "None (marker only)",
        !a.visible,
        StudioCommand::PickModel(slot, LookChoice::None),
    );
    let mut current = None;
    for e in library.catalog.entries() {
        if current.as_ref() != Some(&e.source) {
            child(commands, menu, muted(source_heading(&e.source)));
            current = Some(e.source.clone());
        }
        let on = a
            .linked
            .as_ref()
            .is_some_and(|m| m.asset.source == e.source && m.asset.id == e.manifest.id);
        let r = child(commands, menu, wide_row());
        choice(
            commands,
            r,
            &format!("{} · {} triangles", e.manifest.slug(), e.manifest.triangles),
            on,
            StudioCommand::PickModel(slot, LookChoice::Model(e.reference())),
        );
    }
}

/// The left-dock tab strip's entries.
pub const TABS: [(&str, BuilderTab); 3] = [
    ("Scene", BuilderTab::Scene),
    ("Requirements", BuilderTab::Requirements),
    ("3D Models", BuilderTab::Models),
];

/// Whether the Models tab shows (for [`ReqPanel`] users).
pub fn models_tab(panel: &ReqPanel) -> bool {
    panel.tab == BuilderTab::Models
}
