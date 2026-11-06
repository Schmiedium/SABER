//! `sim_studio_ui`: the studio's widget kit over Bevy Feathers (`sim_studio_ux.md` §6.1.3), the
//! only crate that names Feathers types.
//!
//! Panels compose the scene functions in [`widgets`] with `bsn!` and listen to kit events
//! ([`widgets::KitActivated`], [`widgets::KitChanged`]); colors come from [`theme`] tokens. The
//! kit's own widgets so far are the [`dock`] (fixed regions and splitters) and [`toast`]
//! notifications, and [`plot`] (line plots drawn on the CPU); [`fonts`] puts a symbol font behind
//! Feathers' fonts. The data table, tree, pickers and command palette follow with the screens that
//! need them (S2–S6).
//!
//! Design: `docs/design/sim_studio_ux.md` §6.1, §10, §11; subsystems §5.5 (`Studio.UiKit`, D11).

pub mod dock;
pub mod fonts;
pub mod plot;
pub mod theme;
pub mod toast;
pub mod widgets;

use bevy::app::PropagateSet;
use bevy::feathers::FeathersPlugins;
use bevy::feathers::theme::UiTheme;
use bevy::prelude::*;

pub use bevy;

/// What panels need in scope: the scene macros, the facade, tokens and kit events.
pub mod prelude {
    pub use crate::dock::{DockLayout, DockLayoutChanged, DockRegion, DockSide, dock};
    pub use crate::theme::{StudioTheme, ThemeColors, ThemeMode, Token, token};
    pub use crate::toast::{Toast, ToastLevel, ToastLog, toast_stack};
    pub use crate::widgets::{
        FieldId, Fields, KitActivated, KitChanged, KitSubmit, Status, button, column, heading,
        label, mark, mono, muted, panel, plain_button, plain_row, primary_button, row, set_enabled,
        spacer, status_text, text_field, title, value_field,
    };
    pub use bevy::feathers::theme::{
        ThemeBackgroundColor as Background, ThemeTextColor as TextToken,
    };
}

/// The kit's logic without any rendering: the theme resource, dock layout, toast log and their
/// messages. Headless studio tests use this alone.
pub struct KitCorePlugin;

impl Plugin for KitCorePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<theme::StudioTheme>()
            .init_resource::<dock::DockLayout>()
            .init_resource::<toast::ToastLog>()
            .add_message::<dock::DockLayoutChanged>()
            .add_message::<toast::Toast>()
            .add_systems(Update, (toast::show_toasts, toast::expire_toasts).chain());
    }
}

/// The whole kit on a windowed app: Feathers, the theme, the symbol font, the dock's splitters and
/// text fields. Add after `DefaultPlugins`.
pub struct KitPlugin;

impl Plugin for KitPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((FeathersPlugins, KitCorePlugin))
            .insert_resource(UiTheme(theme::theme_props(theme::ThemeMode::Dark)))
            .add_systems(
                Update,
                (
                    theme::apply_theme,
                    dock::fit_layout,
                    dock::apply_layout,
                    widgets::init_fields,
                ),
            )
            .add_observer(dock::on_splitter_drag_start)
            .add_observer(dock::on_splitter_drag)
            .add_observer(dock::on_splitter_drag_end)
            .add_observer(dock::on_splitter_click)
            .add_observer(widgets::submit_on_enter)
            .add_systems(
                PostUpdate,
                fonts::with_symbols
                    .after(PropagateSet::<TextFont>::default())
                    .before(bevy::text::load_font_assets_into_font_collection),
            )
            .add_observer(widgets::submit_on_blur);
        fonts::add_symbol_font(app);
    }
}
