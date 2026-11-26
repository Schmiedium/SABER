//! The Library's 3D Models tab, the Import dialog, the Derive collision dialog and Align mode
//! (`sim_studio_ux.md` §7.11.1, §7.11.3–§7.11.5; `sequence-diagrams.md` §2.1, §2.2), as plain
//! data: what the tab lists, which model is selected, the dialogs' settings, findings and
//! pictures. The screens are in `ui::library`; the importer and the shape deriver are in
//! `sim_assets`.

use std::path::PathBuf;

use bevy::prelude::*;
use bevy::tasks::Task;
use sim_assets::derive::{FromModel, entry_facets};
use sim_assets::import::OriginChoice;
use sim_assets::thumb::{Drawn, Extras, Segment, View, draw_over};
use sim_assets::{
    AssetSource, Axis, Candidate, Catalog, CatalogEntry, ImportSettings, Problem, Severity, Unit,
};
use sim_core::DeriveMethod;
use sim_core::geometry::derive::wireframe;

use crate::builder::ScenarioDoc;
use crate::models::library_roots;
use crate::workspace::Workspace;

/// A model in the catalogue: its source and id.
pub type ModelKey = (AssetSource, String);

/// The key of a catalogue entry.
pub fn key_of(entry: &CatalogEntry) -> ModelKey {
    (entry.source.clone(), entry.manifest.id.clone())
}

/// Which sources the tab lists.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum SourceFilter {
    /// Every source.
    #[default]
    All,
    /// The built-in library.
    Builtin,
    /// The workspace's `models/`.
    Workspace,
    /// One library root, by its folder name.
    Library(String),
}

impl SourceFilter {
    /// Whether models from `source` are listed.
    pub fn admits(&self, source: &AssetSource) -> bool {
        match (self, source) {
            (SourceFilter::All, _)
            | (SourceFilter::Builtin, AssetSource::Builtin)
            | (SourceFilter::Workspace, AssetSource::Workspace) => true,
            (SourceFilter::Library(a), AssetSource::Library(b)) => a == b,
            _ => false,
        }
    }

    /// Its name on the filter button.
    pub fn label(&self) -> String {
        match self {
            SourceFilter::All => "All".into(),
            SourceFilter::Builtin => "Built-in".into(),
            SourceFilter::Workspace => "Workspace".into(),
            SourceFilter::Library(n) => n.clone(),
        }
    }
}

/// A source's heading in the tab.
pub fn source_heading(source: &AssetSource) -> String {
    match source {
        AssetSource::Builtin => "Built-in".into(),
        AssetSource::Workspace => "Workspace".into(),
        AssetSource::Library(n) => format!("Library {n}"),
    }
}

/// Where an import can write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Destination {
    /// Its name in the dialog.
    pub label: String,
    /// The folder models go into (`<slug>/` is made under it).
    pub dir: PathBuf,
    /// The id prefix: `models/` in a workspace, nothing in a library root.
    pub prefix: &'static str,
    /// The source its models are listed under.
    pub source: AssetSource,
}

/// The places a workspace can import into: its `models/`, then each library root.
pub fn destinations(ws: &Workspace) -> Vec<Destination> {
    let mut out = vec![Destination {
        label: "Workspace models/".into(),
        dir: ws.root.join("models"),
        prefix: "models/",
        source: AssetSource::Workspace,
    }];
    for root in library_roots(ws) {
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string());
        out.push(Destination {
            label: format!("Library {}", root.display()),
            dir: root,
            prefix: "",
            source: AssetSource::Library(name),
        });
    }
    out
}

/// The preview's edge in the Import dialog, px.
pub const PREVIEW_PX: u32 = 240;

/// The Import dialog.
#[derive(Clone, Debug, Default)]
pub struct ImportDialog {
    /// The file, as typed or dropped.
    pub path: String,
    /// The file read and inspected, or why it could not be.
    pub opened: Option<Result<Candidate, String>>,
    /// The settings, from the file's defaults on.
    pub settings: Option<ImportSettings>,
    /// The destination ([`destinations`]).
    pub dest: usize,
    /// Replace a model of the same name.
    pub replace: bool,
    /// The preview with the current settings.
    pub preview: Option<Drawn>,
    /// Bumped whenever the preview is redrawn.
    pub rev: u64,
}

/// One change to the Import dialog.
#[derive(Clone, Debug, PartialEq)]
pub enum ImportEdit {
    /// The file to import (read when set).
    Path(String),
    /// The model's name.
    Name(String),
    /// The file's unit.
    Unit(Unit),
    /// The file's up axis.
    Up(Axis),
    /// The file's forward axis.
    Forward(Axis),
    /// Where the reference point goes.
    Origin(OriginChoice),
    /// The license recorded.
    License(String),
    /// The destination.
    Dest(usize),
    /// Replace a model of the same name.
    Replace(bool),
}

impl ImportDialog {
    /// The dialog on a file (read and inspected now), or empty.
    pub fn new(path: Option<&std::path::Path>) -> Self {
        let mut d = Self::default();
        if let Some(p) = path {
            d.set_path(&p.display().to_string());
        }
        d
    }

    fn set_path(&mut self, path: &str) {
        self.path = path.trim().to_owned();
        if self.path.is_empty() {
            self.opened = None;
            self.settings = None;
            self.preview = None;
            return;
        }
        let p = std::path::Path::new(&self.path);
        let ext = p
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        self.opened = Some(match ext.as_str() {
            "glb" | "gltf" => Candidate::open(p).map_err(|e| e.to_string()),
            "obj" => Err("OBJ files are converted by step S5-4 (stretch); export glTF or GLB from your tool for now".into()),
            _ => Err("choose a .glb or .gltf file".into()),
        });
        self.settings = self
            .opened
            .as_ref()
            .and_then(|o| o.as_ref().ok())
            .map(Candidate::defaults);
        self.redraw();
    }

    /// Apply one change; the preview is redrawn when the model's pose changes.
    pub fn apply(&mut self, edit: &ImportEdit) {
        if let ImportEdit::Path(p) = edit {
            self.set_path(p);
            return;
        }
        match edit {
            ImportEdit::Dest(i) => self.dest = *i,
            ImportEdit::Replace(r) => self.replace = *r,
            _ => {}
        }
        let Some(s) = &mut self.settings else { return };
        let pose_changed = match edit {
            ImportEdit::Name(n) => {
                s.name = n.clone();
                false
            }
            ImportEdit::License(l) => {
                s.license = l.clone();
                false
            }
            ImportEdit::Unit(u) => {
                s.frame.unit = *u;
                true
            }
            ImportEdit::Up(a) => {
                s.frame.up = *a;
                true
            }
            ImportEdit::Forward(a) => {
                s.frame.forward = *a;
                true
            }
            ImportEdit::Origin(o) => {
                s.origin = *o;
                true
            }
            ImportEdit::Path(_) | ImportEdit::Dest(_) | ImportEdit::Replace(_) => false,
        };
        if pose_changed {
            self.redraw();
        }
    }

    /// The file inspected, when it could be.
    pub fn candidate(&self) -> Option<&Candidate> {
        self.opened.as_ref().and_then(|o| o.as_ref().ok())
    }

    fn redraw(&mut self) {
        self.preview = match (self.candidate(), &self.settings) {
            (Some(c), Some(s)) => Some(c.preview(s, PREVIEW_PX)),
            _ => None,
        };
        self.rev += 1;
    }

    /// The inspector's findings and the settings' (refused first).
    pub fn problems(&self) -> Vec<Problem> {
        match (self.candidate(), &self.settings) {
            (Some(c), Some(s)) => c.problems(s),
            _ => Vec::new(),
        }
    }

    /// Why Import is not offered, or `None` when it is.
    pub fn blocked(&self) -> Option<String> {
        match &self.opened {
            None => return Some("Choose a .glb or .gltf file.".into()),
            Some(Err(e)) => return Some(e.clone()),
            Some(Ok(_)) => {}
        }
        self.problems()
            .into_iter()
            .find(|p| p.severity == Severity::Refused)
            .map(|p| p.message)
    }
}

/// Which way the centre's model picture looks (the Derive dialog and Align).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ModelView {
    /// In front, to the left and above.
    #[default]
    ThreeQuarter,
    /// From straight ahead.
    Front,
    /// From the right.
    Side,
    /// From above.
    Top,
}

impl ModelView {
    /// Every view, in the buttons' order.
    pub const ALL: [ModelView; 4] = [
        ModelView::ThreeQuarter,
        ModelView::Front,
        ModelView::Side,
        ModelView::Top,
    ];

    /// Its button's label.
    pub fn label(self) -> &'static str {
        match self {
            ModelView::ThreeQuarter => "3/4 view",
            ModelView::Front => "Front view",
            ModelView::Side => "Side view",
            ModelView::Top => "Top view",
        }
    }

    /// The picture's view.
    pub fn view(self) -> View {
        match self {
            ModelView::ThreeQuarter => View::THUMB,
            ModelView::Front => View::FRONT,
            ModelView::Side => View::SIDE,
            ModelView::Top => View::TOP,
        }
    }
}

/// The colour collision outlines are drawn in on the model pictures (the theme's `bound`).
pub const OUTLINE: [u8; 4] = [232, 150, 40, 255];

/// The model picture's edge in the Derive dialog and Align, px.
pub const MODEL_PX: u32 = 320;

/// The most outline segments a picture draws.
const MAX_SEGMENTS: usize = 20_000;

/// The Derive collision dialog (§7.11.5).
#[derive(Clone, Debug)]
pub struct DeriveDialog {
    /// The entity's document slot.
    pub slot: usize,
    /// The method.
    pub method: DeriveMethod,
    /// The most parts a decomposition may make.
    pub max_parts: u32,
    /// The derivation asked for last; a result for an older one is dropped.
    pub request: u64,
    /// A derivation is running.
    pub running: bool,
    /// The last result.
    pub result: Option<Result<FromModel, String>>,
}

/// Align mode on one entity (§7.11.4).
#[derive(Clone, Debug)]
pub struct AlignSession {
    /// The entity's document slot.
    pub slot: usize,
    /// The undo stack's height when it started (the session becomes one undo step).
    pub mark: usize,
    /// The collision shape is drawn over the model.
    pub collision: bool,
}

/// The Library tab and the Appearance picker's state.
#[derive(Resource, Debug, Default)]
pub struct ModelPanel {
    /// The search text.
    pub query: String,
    /// The source filter.
    pub source: SourceFilter,
    /// The selected model.
    pub selected: Option<ModelKey>,
    /// The Appearance section's model picker is open.
    pub picking: bool,
    /// The Import dialog, while open.
    pub import: Option<ImportDialog>,
    /// The Derive collision dialog, while open.
    pub derive: Option<DeriveDialog>,
    /// Align mode, while on.
    pub align: Option<AlignSession>,
    /// Which way the Derive dialog's and Align's picture looks.
    pub view: ModelView,
    /// That picture.
    pub picture: Option<Drawn>,
    /// Bumped whenever the picture is redrawn.
    pub picture_rev: u64,
}

/// The running derivation (a decomposition can take a while, so it runs off the UI thread;
/// dropping the task cancels it).
#[derive(Resource, Default)]
pub struct DeriveJob {
    /// The request it answers, and the task.
    pub task: Option<(u64, Task<Result<FromModel, String>>)>,
}

impl ModelPanel {
    /// Close the centre's dialogs (Import, Derive, Align), keeping the Library's selection.
    pub fn close_dialogs(&mut self) {
        self.import = None;
        self.derive = None;
        self.align = None;
        self.picture = None;
    }

    /// Redraw the Derive dialog's or Align's picture: the model where the entity draws it, the
    /// body axes at its origin, and a collision outline (the derived shape, or in Align the
    /// entity's own when shown).
    pub fn redraw_picture(&mut self, doc: &ScenarioDoc) {
        let (slot, shape) = if let Some(a) = &self.align {
            (a.slot, a.collision.then(|| doc.collision(a.slot)).flatten())
        } else if let Some(d) = &self.derive {
            let shape = match &d.result {
                Some(Ok(r)) => Some(r.derived.shape.clone()),
                _ => None,
            };
            (d.slot, shape)
        } else {
            self.picture = None;
            return;
        };
        let facets = doc
            .shown_look(slot)
            .zip(doc.catalog())
            .and_then(|(look, catalog)| match catalog.resolve(&look.asset) {
                sim_assets::Resolution::Found(entry, _) => entry_facets(entry, &look).ok(),
                sim_assets::Resolution::Missing => None,
            })
            .unwrap_or_default();
        let segments: Vec<Segment> = shape
            .map(|s| wireframe(&s, MAX_SEGMENTS))
            .unwrap_or_default()
            .into_iter()
            .map(|s| (s, OUTLINE))
            .collect();
        self.picture = Some(draw_over(
            &facets,
            &segments,
            MODEL_PX,
            self.view.view(),
            Extras {
                axes: true,
                bar: true,
            },
        ));
        self.picture_rev += 1;
    }
    /// Whether an entry passes the search and the source filter (the search matches the id, the
    /// display name or the source, ignoring case).
    pub fn admits(&self, entry: &CatalogEntry) -> bool {
        if !self.source.admits(&entry.source) {
            return false;
        }
        let q = self.query.trim().to_lowercase();
        q.is_empty()
            || entry.manifest.id.to_lowercase().contains(&q)
            || entry.manifest.display_name.to_lowercase().contains(&q)
            || source_heading(&entry.source).to_lowercase().contains(&q)
    }

    /// The listed models, grouped by source in catalogue order.
    pub fn listed<'a>(&self, catalog: &'a Catalog) -> Vec<(AssetSource, Vec<&'a CatalogEntry>)> {
        let mut out: Vec<(AssetSource, Vec<&CatalogEntry>)> = Vec::new();
        for e in catalog.entries().iter().filter(|e| self.admits(e)) {
            match out.iter_mut().find(|(s, _)| *s == e.source) {
                Some((_, v)) => v.push(e),
                None => out.push((e.source.clone(), vec![e])),
            }
        }
        out
    }

    /// The source filters a catalogue offers.
    pub fn filters(catalog: &Catalog) -> Vec<SourceFilter> {
        let mut out = vec![
            SourceFilter::All,
            SourceFilter::Builtin,
            SourceFilter::Workspace,
        ];
        for e in catalog.entries() {
            if let AssetSource::Library(n) = &e.source {
                let f = SourceFilter::Library(n.clone());
                if !out.contains(&f) {
                    out.push(f);
                }
            }
        }
        out
    }
}
