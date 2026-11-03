//! The kit's facade (`sim_studio_ux.md` §6.1.3): scene functions panels compose with `bsn!`, and
//! the events they listen to. Panels never name Feathers types; when Feathers changes, this
//! module changes.
//!
//! Every interactive widget carries an accessible label (§11).

use bevy::app::PropagateOver;
use bevy::ecs::system::SystemParam;
use bevy::feathers::constants::{fonts, size};
use bevy::feathers::controls::{
    ButtonVariant, FeathersButton, FeathersTextInput, FeathersTextInputContainer,
};
use bevy::feathers::display::caption;
use bevy::feathers::font_styles::InheritableFont;
use bevy::feathers::theme::{ThemeBackgroundColor, ThemeTextColor};
use bevy::prelude::*;
use bevy::text::{EditableText, FontSourceTemplate, FontWeight, TextEdit};
use bevy::ui::InteractionDisabled;

use crate::theme::{Token, token};

/// A button or menu item was activated (click, Enter or Space).
pub use bevy::ui_widgets::Activate as KitActivated;
/// A value widget changed.
pub use bevy::ui_widgets::ValueChange as KitChanged;

fn text_scene(
    text: String,
    font: &'static str,
    font_size: f32,
    weight: FontWeight,
    color: Token,
) -> impl Scene {
    bsn! {
        Text({text})
        TextFont {
            font: FontSourceTemplate::Handle({font}),
            font_size: {bevy::text::FontSize::Px(font_size)},
            weight: {weight},
        }
        PropagateOver<TextFont>
        ThemeTextColor({color})
    }
}

/// A section or screen title (bold, 16 px).
pub fn heading(text: impl Into<String>) -> impl Scene {
    text_scene(
        text.into(),
        fonts::BOLD,
        16.0,
        FontWeight::BOLD,
        token::FG_TEXT,
    )
}

/// A large title (the Launcher's wordmark, 28 px bold).
pub fn title(text: impl Into<String>) -> impl Scene {
    text_scene(
        text.into(),
        fonts::BOLD,
        28.0,
        FontWeight::BOLD,
        token::FG_TEXT,
    )
}

/// Body text (13 px).
pub fn label(text: impl Into<String>) -> impl Scene {
    text_scene(
        text.into(),
        fonts::REGULAR,
        13.0,
        FontWeight::NORMAL,
        token::FG_TEXT,
    )
}

/// Labels, units and hints (`fg.muted`, 12 px).
pub fn muted(text: impl Into<String>) -> impl Scene {
    text_scene(
        text.into(),
        fonts::REGULAR,
        12.0,
        FontWeight::NORMAL,
        token::FG_MUTED,
    )
}

/// Numbers and paths in the tabular monospace (12 px), so columns align (§10.2).
pub fn mono(text: impl Into<String>) -> impl Scene {
    text_scene(
        text.into(),
        fonts::MONO,
        12.0,
        FontWeight::NORMAL,
        token::FG_TEXT,
    )
}

/// A symbol in a status color, such as ✓ in `status.pass`. Status never rests on color alone,
/// so pair it with text or use [`status_text`].
pub fn mark(symbol: &'static str, color: Token) -> impl Scene {
    text_scene(
        symbol.to_owned(),
        fonts::REGULAR,
        13.0,
        FontWeight::BOLD,
        color,
    )
}

/// A status: its mark and word in its color (PASS ✓, FAIL ✖, INCONCLUSIVE ◐, warning ⚠).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Status {
    /// Passing, healthy, done.
    Pass,
    /// Failing, crashed.
    Fail,
    /// Neither (INCONCLUSIVE).
    Inconclusive,
    /// A warning.
    Warning,
    /// Running, in progress.
    Running,
}

impl Status {
    /// The mark that goes with the color.
    pub fn mark(self) -> &'static str {
        match self {
            Status::Pass => "✓",
            Status::Fail => "✖",
            Status::Inconclusive => "◐",
            Status::Warning => "⚠",
            Status::Running => "●",
        }
    }

    /// The color token.
    pub fn token(self) -> Token {
        match self {
            Status::Pass => token::STATUS_PASS,
            Status::Fail => token::STATUS_FAIL,
            Status::Inconclusive | Status::Warning => token::STATUS_WARN,
            Status::Running => token::ACCENT,
        }
    }
}

/// A status mark followed by its text, both in the status color.
pub fn status_text(status: Status, text: impl Into<String>) -> impl Scene {
    let text = format!("{} {}", status.mark(), text.into());
    text_scene(
        text,
        fonts::REGULAR,
        13.0,
        FontWeight::NORMAL,
        status.token(),
    )
}

fn button_scene(text: String, variant: ButtonVariant) -> impl Scene {
    let a11y = text.clone();
    bsn! {
        @FeathersButton {
            @caption: bsn! { @caption({text}) },
            @variant: {variant},
        }
        AccessibleLabel({a11y})
    }
}

/// A button. Listen for [`KitActivated`] with `on(...)`.
pub fn button(text: impl Into<String>) -> impl Scene {
    button_scene(text.into(), ButtonVariant::Normal)
}

/// The call-to-action button of a screen or dialog.
pub fn primary_button(text: impl Into<String>) -> impl Scene {
    button_scene(text.into(), ButtonVariant::Primary)
}

/// A button with no background until hovered: list rows, rail items, links.
pub fn plain_button(text: impl Into<String>) -> impl Scene {
    button_scene(text.into(), ButtonVariant::Plain)
}

/// A plain button whose caption is laid out by the caller (several labels in a row, for
/// example). `a11y` is what a screen reader announces.
pub fn plain_row(a11y: impl Into<String>, content: impl SceneList) -> impl Scene {
    let a11y = a11y.into();
    bsn! {
        @FeathersButton {
            @caption: {Box::new(content) as Box<dyn SceneList>},
            @variant: ButtonVariant::Plain,
        }
        Node {
            justify_content: JustifyContent::Start,
            height: auto(),
            padding: px(6),
        }
        AccessibleLabel({a11y})
    }
}

/// Names a [`text_field`] so panels can read and set it through [`Fields`].
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Reflect)]
#[reflect(Component, Default)]
pub struct FieldId(pub &'static str);

/// Text a field starts with (applied once, when it appears).
#[derive(Component, Clone, Debug, Default, PartialEq, Eq, Reflect)]
#[reflect(Component, Default)]
pub struct FieldInit(pub String);

/// A single-line text field named `id`, starting with `initial`, announced as `a11y`.
pub fn text_field(
    id: &'static str,
    initial: impl Into<String>,
    a11y: impl Into<String>,
) -> impl Scene {
    let initial = initial.into();
    let a11y = a11y.into();
    bsn! {
        @FeathersTextInputContainer
        Node {
            min_width: px(240),
        }
        Children [
            @FeathersTextInput
            InheritableFont {
                font: fonts::MONO,
                font_size: size::COMPACT_FONT,
            }
            FieldId({id})
            FieldInit({initial})
            AccessibleLabel({a11y})
        ]
    }
}

/// A value field for editors that build many fields at run time (the Inspector): no [`FieldId`],
/// starting with `initial`, announced as `a11y`. Its value arrives as [`KitSubmit`], which bubbles
/// up to the returned container, so observe it there.
pub fn value_field(initial: impl Into<String>, a11y: impl Into<String>) -> impl Scene {
    let initial = initial.into();
    let a11y = a11y.into();
    bsn! {
        @FeathersTextInputContainer
        Node {
            min_width: px(120),
            flex_grow: 1.0,
        }
        Children [
            @FeathersTextInput
            InheritableFont {
                font: fonts::MONO,
                font_size: size::COMPACT_FONT,
            }
            FieldInit({initial})
            AccessibleLabel({a11y})
        ]
    }
}

/// A text field's value is committed: Enter was pressed in it, or it lost focus. Bubbles from the
/// input up through its parents, so a panel can observe its fields' container. Sent for every
/// commit, changed or not; the receiver compares with what it has.
#[derive(EntityEvent, Clone, Debug, PartialEq)]
#[entity_event(propagate, auto_propagate)]
pub struct KitSubmit {
    /// The input, then each parent the event bubbles to.
    pub entity: Entity,
    /// The field's text, trimmed.
    pub value: String,
}

/// Enter in a text input commits it.
pub(crate) fn submit_on_enter(
    ev: On<bevy::input_focus::FocusedInput<bevy::input::keyboard::KeyboardInput>>,
    inputs: Query<&EditableText, With<bevy::ui_widgets::TextInput>>,
    mut commands: Commands,
) {
    let key = &ev.input;
    if key.state.is_pressed()
        && matches!(key.logical_key, bevy::input::keyboard::Key::Enter)
        && let Ok(text) = inputs.get(ev.focused_entity)
    {
        commands.trigger(KitSubmit {
            entity: ev.focused_entity,
            value: text.value().to_string().trim().to_owned(),
        });
    }
}

/// Leaving a text input commits it.
pub(crate) fn submit_on_blur(
    ev: On<bevy::input_focus::FocusLost>,
    inputs: Query<&EditableText, With<bevy::ui_widgets::TextInput>>,
    mut commands: Commands,
) {
    if let Ok(text) = inputs.get(ev.entity) {
        commands.trigger(KitSubmit {
            entity: ev.entity,
            value: text.value().to_string().trim().to_owned(),
        });
    }
}

/// Put each field's initial text in.
pub(crate) fn init_fields(mut fields: Query<(&FieldInit, &mut EditableText), Added<FieldInit>>) {
    for (init, mut text) in &mut fields {
        text.editor_mut().set_text(&init.0);
        text.queue_edit(TextEdit::TextEnd(false));
    }
}

/// Read and write [`text_field`]s by their [`FieldId`].
#[derive(SystemParam)]
pub struct Fields<'w, 's> {
    fields: Query<'w, 's, (&'static FieldId, &'static mut EditableText)>,
}

impl Fields<'_, '_> {
    /// The text of field `id` (trimmed), if it exists.
    pub fn get(&self, id: &str) -> Option<String> {
        self.fields
            .iter()
            .find(|(f, _)| f.0 == id)
            .map(|(_, t)| t.value().to_string().trim().to_owned())
    }

    /// Replace the text of field `id`.
    pub fn set(&mut self, id: &str, text: &str) {
        for (f, mut t) in &mut self.fields {
            if f.0 == id {
                t.editor_mut().set_text(text);
                t.queue_edit(TextEdit::TextEnd(false));
            }
        }
    }
}

/// Enable or disable an interactive widget (a disabled one ignores input and dims).
pub fn set_enabled(commands: &mut Commands, widget: Entity, enabled: bool) {
    if enabled {
        commands.entity(widget).remove::<InteractionDisabled>();
    } else {
        commands.entity(widget).insert(InteractionDisabled);
    }
}

/// A panel frame on `bg.panel`: a padded column with rounded corners. Put a [`heading`] first.
pub fn panel() -> impl Scene {
    bsn! {
        Node {
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            row_gap: px(8),
            padding: px(12),
            border_radius: {BorderRadius::all(px(6))},
        }
        ThemeBackgroundColor(token::BG_PANEL)
    }
}

/// A row with gaps.
pub fn row() -> impl Scene {
    bsn! {
        Node {
            display: Display::Flex,
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: px(8),
        }
    }
}

/// A column with gaps.
pub fn column() -> impl Scene {
    bsn! {
        Node {
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            row_gap: px(6),
        }
    }
}

/// Takes the free space in a row or column.
pub fn spacer() -> impl Scene {
    bsn! {
        Node {
            flex_grow: 1.0,
        }
    }
}
