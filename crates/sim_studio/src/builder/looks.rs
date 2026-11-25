//! Bodies' looks in the scene document (`sim_studio_ux.md` §7.11.2; `sequence-diagrams.md` §2.2):
//! the Inspector's Appearance section reads and edits the scenario's `visual:` section, one
//! undoable edit at a time, and lint reports models that are missing or changed. Only the
//! `visual:` section changes, so `scene_hash` never does.

use std::sync::Arc;

use bevy::math::{DQuat, EulerRot};
use sim_assets::{
    AssetSource, Catalog, CatalogEntry, Notice, Resolution, VisualAssetRef, VisualModel,
    VisualSection,
};

use super::lint::{LintItem, LintLevel};
use super::{Change, Edit, ScenarioDoc};
use crate::models::domain_default;

/// A part of a body's look that the Appearance section edits as text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LookField {
    /// Offset forward, m.
    X,
    /// Offset right, m.
    Y,
    /// Offset down, m.
    Z,
    /// Roll, deg.
    Roll,
    /// Pitch, deg.
    Pitch,
    /// Yaw, deg.
    Yaw,
    /// Scale: one number, or three.
    Scale,
    /// Tint: `none`, `#rrggbb[aa]`, or `r, g, b[, a]` in 0–1.
    Tint,
}

impl LookField {
    /// Every field, in the section's order.
    pub const ALL: [LookField; 8] = [
        LookField::X,
        LookField::Y,
        LookField::Z,
        LookField::Roll,
        LookField::Pitch,
        LookField::Yaw,
        LookField::Scale,
        LookField::Tint,
    ];

    /// Its label.
    pub fn label(self) -> &'static str {
        match self {
            LookField::X => "x",
            LookField::Y => "y",
            LookField::Z => "z",
            LookField::Roll => "roll",
            LookField::Pitch => "pitch",
            LookField::Yaw => "yaw",
            LookField::Scale => "scale",
            LookField::Tint => "tint",
        }
    }

    /// Its unit, shown after the field.
    pub fn unit(self) -> &'static str {
        match self {
            LookField::X | LookField::Y | LookField::Z => "m",
            LookField::Roll | LookField::Pitch | LookField::Yaw => "deg",
            LookField::Scale => "× (one number, or x, y, z)",
            LookField::Tint => "none, #rrggbb or r, g, b",
        }
    }
}

/// What the picker sets.
#[derive(Clone, Debug, PartialEq)]
pub enum LookChoice {
    /// This model.
    Model(VisualAssetRef),
    /// The Domain Pack's default (the link is removed).
    Default,
    /// No model: the body's marker only.
    None,
}

/// How a body's look stands.
#[derive(Clone, Debug, PartialEq)]
pub enum LookStatus {
    /// Drawn with its model.
    Found {
        /// The model's triangles.
        triangles: u64,
        /// What differs from the link, or the file's change since import.
        notes: Vec<String>,
    },
    /// The model is not in the catalogue: drawn as a marker.
    Missing,
    /// Hidden (Visible off, or None picked): drawn as a marker.
    Hidden,
    /// No model and no Domain Pack default.
    Plain,
}

/// The Appearance section of one body.
#[derive(Clone, Debug, PartialEq)]
pub struct Appearance {
    /// The body's `SimId`.
    pub sim_id: u32,
    /// Its link in the `visual:` section.
    pub linked: Option<VisualModel>,
    /// Its Domain Pack's default model.
    pub default: Option<VisualAssetRef>,
    /// How it stands.
    pub status: LookStatus,
    /// Each field's text.
    pub fields: Vec<(LookField, String)>,
    /// Visible.
    pub visible: bool,
}

impl Appearance {
    /// The model drawn: the link's, else the default.
    pub fn shown(&self) -> Option<&VisualAssetRef> {
        self.linked
            .as_ref()
            .map(|m| &m.asset)
            .or(self.default.as_ref())
    }

    /// A field's text.
    pub fn text(&self, field: LookField) -> &str {
        self.fields
            .iter()
            .find(|(f, _)| *f == field)
            .map_or("", |(_, t)| t.as_str())
    }
}

/// A short number for a field: at most six decimals, no trailing zeros.
pub fn short_number(v: f64) -> String {
    let s = format!("{v:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.to_owned() }
}

/// A model's place for people: `quad_x · built-in`.
pub fn describe(r: &VisualAssetRef) -> String {
    let source = match &r.source {
        AssetSource::Builtin => "built-in".to_owned(),
        AssetSource::Workspace => "workspace".to_owned(),
        AssetSource::Library(root) => format!("library {root}"),
    };
    let slug = r.id.rsplit('/').next().unwrap_or(&r.id);
    format!("{slug} · {source}")
}

fn number(text: &str) -> Result<f64, String> {
    let v: f64 = text
        .trim()
        .parse()
        .map_err(|_| format!("{:?} is not a number", text.trim()))?;
    if v.is_finite() {
        Ok(v)
    } else {
        Err(format!("{text} is not a finite number"))
    }
}

/// Parse a tint: `none`, `#rrggbb`, `#rrggbbaa`, or `r, g, b[, a]` in 0–1 (sRGB).
pub fn parse_tint(text: &str) -> Result<Option<[f32; 4]>, String> {
    let t = text.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    if let Some(hex) = t.strip_prefix('#') {
        let byte = |i: usize| {
            hex.get(i..i + 2)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .map(|b| b as f32 / 255.0)
        };
        return match hex.len() {
            6 | 8 => {
                let a = if hex.len() == 8 { byte(6) } else { Some(1.0) };
                match (byte(0), byte(2), byte(4), a) {
                    (Some(r), Some(g), Some(b), Some(a)) => Ok(Some([r, g, b, a])),
                    _ => Err(format!("{t} is not a colour like #ff8800")),
                }
            }
            _ => Err(format!("{t} is not a colour like #ff8800")),
        };
    }
    let parts: Vec<f64> = t
        .split(',')
        .map(number)
        .collect::<Result<_, _>>()
        .map_err(|_| format!("{t} is not none, #rrggbb or r, g, b"))?;
    if !(3..=4).contains(&parts.len()) || parts.iter().any(|v| !(0.0..=1.0).contains(v)) {
        return Err(format!(
            "{t}: give r, g, b and an optional a, each from 0 to 1"
        ));
    }
    let a = parts.get(3).copied().unwrap_or(1.0);
    Ok(Some([
        parts[0] as f32,
        parts[1] as f32,
        parts[2] as f32,
        a as f32,
    ]))
}

fn tint_text(t: Option<[f32; 4]>) -> String {
    match t {
        None => "none".into(),
        Some(c) => {
            let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            if c[3] >= 1.0 {
                format!("#{:02x}{:02x}{:02x}", b(c[0]), b(c[1]), b(c[2]))
            } else {
                format!(
                    "#{:02x}{:02x}{:02x}{:02x}",
                    b(c[0]),
                    b(c[1]),
                    b(c[2]),
                    b(c[3])
                )
            }
        }
    }
}

fn fields_of(m: &VisualModel) -> Vec<(LookField, String)> {
    let t = m.offset.translation();
    let (yaw, pitch, roll) = m.offset.rotation().to_euler(EulerRot::ZYX);
    let s = m.scale;
    let scale = if s[0] == s[1] && s[1] == s[2] {
        short_number(s[0])
    } else {
        s.map(short_number).join(", ")
    };
    vec![
        (LookField::X, short_number(t.x)),
        (LookField::Y, short_number(t.y)),
        (LookField::Z, short_number(t.z)),
        (LookField::Roll, short_number(roll.to_degrees())),
        (LookField::Pitch, short_number(pitch.to_degrees())),
        (LookField::Yaw, short_number(yaw.to_degrees())),
        (LookField::Scale, scale),
        (LookField::Tint, tint_text(m.tint)),
    ]
}

/// Write one field's text into a look.
fn apply(m: &mut VisualModel, field: LookField, text: &str) -> Result<(), String> {
    let mut t = m.offset.0;
    let (mut yaw, mut pitch, mut roll) = m.offset.rotation().to_euler(EulerRot::ZYX);
    match field {
        LookField::X => t[0] = number(text)?,
        LookField::Y => t[1] = number(text)?,
        LookField::Z => t[2] = number(text)?,
        LookField::Roll => roll = number(text)?.to_radians(),
        LookField::Pitch => pitch = number(text)?.to_radians(),
        LookField::Yaw => yaw = number(text)?.to_radians(),
        LookField::Scale => {
            let v: Vec<f64> = text.split(',').map(number).collect::<Result<_, _>>()?;
            let s = match v[..] {
                [k] => [k; 3],
                [a, b, c] => [a, b, c],
                _ => return Err(format!("{text}: give one scale, or three (x, y, z)")),
            };
            if s.iter().any(|k| *k <= 0.0) {
                return Err(format!("{text}: a scale is above 0"));
            }
            m.scale = s;
            return Ok(());
        }
        LookField::Tint => {
            m.tint = parse_tint(text)?;
            return Ok(());
        }
    }
    let q = DQuat::from_euler(EulerRot::ZYX, yaw, pitch, roll);
    m.offset.0 = t;
    m.offset.1 = q.to_array();
    Ok(())
}

impl ScenarioDoc {
    /// The scenario's `visual:` section; empty when it has none or it does not parse (lint says
    /// so).
    pub fn visual_section(&self) -> VisualSection {
        self.visual
            .as_deref()
            .and_then(|v| VisualSection::from_ron(v).ok())
            .unwrap_or_default()
    }

    /// Give the document the model catalogue, for the Appearance section and lint.
    pub fn set_catalog(&mut self, catalog: Arc<Catalog>) {
        self.catalog = Some(catalog);
        self.relint();
    }

    /// The catalogue the document checks models against.
    pub fn catalog(&self) -> Option<&Catalog> {
        self.catalog.as_deref()
    }

    /// The Domain Pack's default model of the entity in `slot`.
    pub fn default_look(&self, slot: usize) -> Option<VisualAssetRef> {
        self.entities.get(slot).and_then(domain_default)
    }

    /// The entity's link in the `visual:` section.
    pub fn look(&self, slot: usize) -> Option<VisualModel> {
        let id = self.sim_id(slot)?;
        self.visual_section().models.get(&id).cloned()
    }

    /// The Appearance section of the entity in `slot`; `None` when it has no `SimId` (looks are
    /// keyed by it).
    pub fn appearance(&self, slot: usize) -> Option<Appearance> {
        let sim_id = self.sim_id(slot)?;
        let linked = self.look(slot);
        let default = self.default_look(slot);
        let shown = linked.as_ref().map(|m| &m.asset).or(default.as_ref());
        let visible = linked.as_ref().is_none_or(|m| m.visible);
        let status = match shown {
            None => LookStatus::Plain,
            Some(_) if !visible => LookStatus::Hidden,
            Some(r) => match self.catalog().map(|c| (c, c.resolve(r))) {
                Some((c, Resolution::Found(entry, notices))) => LookStatus::Found {
                    triangles: entry.manifest.triangles,
                    notes: notes(c, entry, &notices),
                },
                Some((_, Resolution::Missing)) => LookStatus::Missing,
                None => LookStatus::Found {
                    triangles: 0,
                    notes: Vec::new(),
                },
            },
        };
        let base = linked
            .clone()
            .or_else(|| default.clone().map(VisualModel::new));
        Some(Appearance {
            sim_id,
            fields: base.as_ref().map(fields_of).unwrap_or_default(),
            linked,
            default,
            status,
            visible,
        })
    }

    /// Change the `visual:` section with `f`, as one undoable edit. `Ok(false)`: nothing changed.
    fn edit_visual(
        &mut self,
        slot: usize,
        f: impl FnOnce(&mut VisualSection) -> Result<(), String>,
    ) -> Result<bool, String> {
        if let Some(text) = &self.visual
            && let Err(e) = VisualSection::from_ron(text)
        {
            return Err(format!(
                "the visual: section does not parse, so it is not edited: {e}"
            ));
        }
        let mut section = self.visual_section();
        f(&mut section)?;
        section.check().map_err(|e| e.to_string())?;
        let after =
            (!section.models.is_empty() || !section.scenery.is_empty()).then(|| section.to_ron());
        if after == self.visual {
            return Ok(false);
        }
        let before = std::mem::replace(&mut self.visual, after.clone());
        let id = self.next_id;
        self.next_id += 1;
        self.undo.push(Edit {
            id,
            slot,
            change: Change::Visual { before, after },
        });
        self.redo.clear();
        self.changed();
        Ok(true)
    }

    /// Pick a body's model (§7.11.2): every slot in `slots` in one edit. Picking the model a body
    /// already links keeps its offset, scale and tint (and accepts the model's current hash).
    pub fn set_look(&mut self, slots: &[usize], choice: &LookChoice) -> Result<bool, String> {
        let targets: Vec<(u32, Option<VisualAssetRef>)> = slots
            .iter()
            .filter_map(|s| Some((self.sim_id(*s)?, self.default_look(*s))))
            .collect();
        if targets.is_empty() {
            return Err("looks are kept by SimId; give the entity one first".into());
        }
        let first = slots[0];
        self.edit_visual(first, |v| {
            for (id, default) in &targets {
                match choice {
                    LookChoice::Default => {
                        v.models.remove(id);
                    }
                    LookChoice::Model(r) => {
                        let m = v
                            .models
                            .entry(*id)
                            .or_insert_with(|| VisualModel::new(r.clone()));
                        if m.asset.source != r.source || m.asset.id != r.id {
                            *m = VisualModel::new(r.clone());
                        } else {
                            m.asset = r.clone();
                        }
                        m.visible = true;
                    }
                    LookChoice::None => {
                        let asset = v
                            .models
                            .get(id)
                            .map(|m| m.asset.clone())
                            .or_else(|| default.clone());
                        match asset {
                            Some(a) => {
                                v.models
                                    .entry(*id)
                                    .or_insert_with(|| VisualModel::new(a))
                                    .visible = false;
                            }
                            None => {
                                v.models.remove(id);
                            }
                        }
                    }
                }
            }
            Ok(())
        })
    }

    /// Set one field of a body's look from text. A body drawn with its Domain Pack default gets
    /// a link to that default first.
    pub fn set_look_field(
        &mut self,
        slot: usize,
        field: LookField,
        text: &str,
    ) -> Result<bool, String> {
        let id = self
            .sim_id(slot)
            .ok_or("looks are kept by SimId; give the entity one first")?;
        let default = self.default_look(slot);
        self.edit_visual(slot, |v| {
            let m = match v.models.get_mut(&id) {
                Some(m) => m,
                None => v
                    .models
                    .entry(id)
                    .or_insert(VisualModel::new(default.ok_or("pick a model first")?)),
            };
            apply(m, field, text)
        })
    }

    /// Show or hide a body's model.
    pub fn set_look_visible(&mut self, slot: usize, visible: bool) -> Result<bool, String> {
        if visible {
            let id = self.sim_id(slot).ok_or("no SimId")?;
            return self.edit_visual(slot, |v| {
                if let Some(m) = v.models.get_mut(&id) {
                    m.visible = true;
                }
                Ok(())
            });
        }
        self.set_look(&[slot], &LookChoice::None)
    }

    /// The entities that link a catalogue model, by `SimId`.
    pub fn users(&self, entry: &CatalogEntry) -> Vec<u32> {
        let mut out = entry.users(&self.visual_section());
        for slot in 0..self.entities.len() {
            if let (Some(id), Some(d)) = (self.sim_id(slot), self.default_look(slot))
                && self.look(slot).is_none()
                && d.source == entry.source
                && d.id == entry.manifest.id
            {
                out.push(id);
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

fn notes(catalog: &Catalog, entry: &CatalogEntry, notices: &[Notice]) -> Vec<String> {
    let mut out: Vec<String> = notices
        .iter()
        .map(|n| match n {
            Notice::OtherVersion { linked, installed } => {
                format!("version {installed} here, linked at version {linked}")
            }
            Notice::HashChanged { .. } => {
                "changed since it was linked; pick it again to accept it".into()
            }
        })
        .collect();
    if catalog.file_changed(entry) {
        out.push("its file changed since it was imported; Re-import it in the Library".into());
    }
    out
}

/// Lint for looks: a `visual:` section that does not parse, links to bodies not in the scene, and
/// bodies whose model is missing or changed (⚠ in the Outliner).
pub(super) fn lint(doc: &ScenarioDoc, out: &mut Vec<LintItem>) {
    let item = |level, sim_id, text| LintItem {
        level,
        sim_id,
        req: None,
        text,
    };
    if let Some(text) = &doc.visual
        && let Err(e) = VisualSection::from_ron(text)
    {
        out.push(item(
            LintLevel::Warning,
            None,
            format!(
                "The visual: section does not parse (looks only, results are not affected): {e}"
            ),
        ));
        return;
    }
    let section = doc.visual_section();
    for id in section.models.keys() {
        if doc.slot_of(*id).is_none() {
            out.push(item(
                LintLevel::Info,
                None,
                format!("visual: a model is linked to #{id}, which is not in the scene"),
            ));
        }
    }
    if doc.catalog().is_none() {
        return;
    }
    for slot in 0..doc.entities.len() {
        let Some(a) = doc.appearance(slot) else {
            continue;
        };
        let Some(shown) = a.shown() else { continue };
        let who = format!("#{}", a.sim_id);
        match &a.status {
            LookStatus::Missing => out.push(item(
                LintLevel::Warning,
                Some(a.sim_id),
                format!("{who}: model {shown} not found; drawn as a marker"),
            )),
            LookStatus::Found { notes, .. } => {
                for n in notes {
                    out.push(item(
                        LintLevel::Warning,
                        Some(a.sim_id),
                        format!("{who}: model {}: {n}", describe(shown)),
                    ));
                }
            }
            LookStatus::Hidden | LookStatus::Plain => {}
        }
    }
}
