//! The Inspector's view of a component: its fields flattened to reflection paths, each with its
//! value as text and how it may be edited (`sim_studio_ux.md` §7.2).
//!
//! Numbers are shown in Rust's shortest round-trip form (`{:?}`), so writing back the text shown
//! never changes a value.

use bevy::reflect::enums::{DynamicEnum, DynamicVariant, VariantInfo};
use bevy::reflect::{GetPath, PartialReflect, Reflect, ReflectRef, TypeInfo};

/// How a field is edited.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldKind {
    /// A floating-point number (finite only).
    Float,
    /// An integer of the named Rust type (its range is checked).
    Int(&'static str),
    /// `true` or `false`.
    Bool,
    /// Free text.
    Text,
    /// One of the variants of an enum whose variants carry no data.
    Choice(Vec<String>),
    /// Shown, not edited here (the active variant of an enum with data, a map, an unknown type).
    Fixed,
}

/// One field of a component.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldView {
    /// The reflection path inside the component, e.g. `.rotors[0].time_constant`.
    pub path: String,
    /// The path as a label, without the leading dot.
    pub label: String,
    /// The value as text.
    pub text: String,
    /// How it is edited.
    pub kind: FieldKind,
}

impl FieldView {
    /// Whether the Inspector may edit it.
    pub fn editable(&self) -> bool {
        self.kind != FieldKind::Fixed
    }
}

/// Every field of `value`, depth first, in declaration order.
pub fn fields(value: &dyn PartialReflect) -> Vec<FieldView> {
    let mut out = Vec::new();
    walk(value, String::new(), &mut out);
    out
}

fn push(out: &mut Vec<FieldView>, path: String, text: String, kind: FieldKind) {
    let label = path.strip_prefix('.').unwrap_or(&path).to_owned();
    let label = if label.is_empty() {
        "(value)".to_owned()
    } else {
        label
    };
    out.push(FieldView {
        path,
        label,
        text,
        kind,
    });
}

fn walk(value: &dyn PartialReflect, path: String, out: &mut Vec<FieldView>) {
    if let Some((text, kind)) = leaf(value) {
        push(out, path, text, kind);
        return;
    }
    match value.reflect_ref() {
        ReflectRef::Struct(s) => {
            for i in 0..s.field_len() {
                let name = s.name_at(i).unwrap_or("?");
                walk(
                    s.field_at(i).expect("in range"),
                    format!("{path}.{name}"),
                    out,
                );
            }
        }
        ReflectRef::TupleStruct(s) => {
            for i in 0..s.field_len() {
                walk(s.field(i).expect("in range"), format!("{path}.{i}"), out);
            }
        }
        ReflectRef::Tuple(t) => {
            for i in 0..t.field_len() {
                walk(t.field(i).expect("in range"), format!("{path}.{i}"), out);
            }
        }
        ReflectRef::List(l) => {
            for i in 0..l.len() {
                walk(l.get(i).expect("in range"), format!("{path}[{i}]"), out);
            }
        }
        ReflectRef::Array(a) => {
            for i in 0..a.len() {
                walk(a.get(i).expect("in range"), format!("{path}[{i}]"), out);
            }
        }
        ReflectRef::Enum(e) => {
            if let Some(choices) = unit_choices(value) {
                push(
                    out,
                    path,
                    e.variant_name().to_owned(),
                    FieldKind::Choice(choices),
                );
                return;
            }
            push(
                out,
                format!("{path}#variant"),
                e.variant_name().to_owned(),
                FieldKind::Fixed,
            );
            for i in 0..e.field_len() {
                let field = e.field_at(i).expect("in range");
                match e.name_at(i) {
                    Some(name) => walk(field, format!("{path}.{name}"), out),
                    None => walk(field, format!("{path}.{i}"), out),
                }
            }
        }
        ReflectRef::Map(m) => push(out, path, format!("{} entries", m.len()), FieldKind::Fixed),
        ReflectRef::Set(s) => push(out, path, format!("{} entries", s.len()), FieldKind::Fixed),
        _ => push(out, path, format!("{value:?}"), FieldKind::Fixed),
    }
}

/// The variant names of an enum whose variants all carry no data.
fn unit_choices(value: &dyn PartialReflect) -> Option<Vec<String>> {
    let Some(TypeInfo::Enum(info)) = value.get_represented_type_info() else {
        return None;
    };
    info.iter()
        .map(|v| match v {
            VariantInfo::Unit(u) => Some(u.name().to_owned()),
            _ => None,
        })
        .collect()
}

macro_rules! ints {
    ($value:expr, $($t:ty),*) => {
        $(
            if let Some(v) = $value.try_downcast_ref::<$t>() {
                return Some((v.to_string(), FieldKind::Int(stringify!($t))));
            }
        )*
    };
}

/// A value edited as one piece of text.
fn leaf(value: &dyn PartialReflect) -> Option<(String, FieldKind)> {
    if let Some(v) = value.try_downcast_ref::<f64>() {
        return Some((format!("{v:?}"), FieldKind::Float));
    }
    if let Some(v) = value.try_downcast_ref::<f32>() {
        return Some((format!("{v:?}"), FieldKind::Float));
    }
    if let Some(v) = value.try_downcast_ref::<bool>() {
        return Some((v.to_string(), FieldKind::Bool));
    }
    if let Some(v) = value.try_downcast_ref::<String>() {
        return Some((v.clone(), FieldKind::Text));
    }
    ints!(value, u8, u16, u32, u64, usize, i8, i16, i32, i64, isize);
    None
}

macro_rules! set_int {
    ($field:expr, $text:expr, $($t:ty),*) => {
        $(
            if let Some(v) = $field.try_downcast_mut::<$t>() {
                *v = $text.parse::<$t>().map_err(|_| {
                    format!("{:?} is not a whole number from {} to {}", $text, <$t>::MIN, <$t>::MAX)
                })?;
                return Ok(());
            }
        )*
    };
}

/// Write `text` into the field at `path` of `component`. The field keeps its old value when the
/// text does not fit it.
pub fn set(component: &mut dyn Reflect, path: &str, text: &str) -> Result<(), String> {
    let text = text.trim();
    let choices = component.reflect_path(path).ok().and_then(unit_choices);
    let field = component
        .reflect_path_mut(path)
        .map_err(|e| format!("no field {path}: {e}"))?;
    if let Some(v) = field.try_downcast_mut::<f64>() {
        *v = float(text)?;
        return Ok(());
    }
    if let Some(v) = field.try_downcast_mut::<f32>() {
        let x = float(text)?;
        if x.abs() > f32::MAX as f64 {
            return Err(format!("{text} is outside the range of this field (f32)"));
        }
        *v = x as f32;
        return Ok(());
    }
    if let Some(v) = field.try_downcast_mut::<bool>() {
        *v = match text {
            "true" => true,
            "false" => false,
            _ => return Err(format!("{text:?} is not true or false")),
        };
        return Ok(());
    }
    if let Some(v) = field.try_downcast_mut::<String>() {
        text.clone_into(v);
        return Ok(());
    }
    set_int!(
        field, text, u8, u16, u32, u64, usize, i8, i16, i32, i64, isize
    );
    if let Some(choices) = choices {
        if !choices.iter().any(|c| c == text) {
            return Err(format!("{text:?} is not one of {}", choices.join(", ")));
        }
        let variant = DynamicEnum::new(text, DynamicVariant::Unit);
        return field
            .try_apply(&variant)
            .map_err(|e| format!("cannot set {path} to {text}: {e}"));
    }
    Err(format!("{path} is not edited here"))
}

fn float(text: &str) -> Result<f64, String> {
    let v: f64 = text
        .parse()
        .map_err(|_| format!("{text:?} is not a number"))?;
    if v.is_finite() {
        Ok(v)
    } else {
        Err(format!("{text} is not a finite number"))
    }
}

/// The short name of a type path (`sim_core::body::MassProps` → `MassProps`).
pub fn short(type_path: &str) -> &str {
    type_path.rsplit("::").next().unwrap_or(type_path)
}
