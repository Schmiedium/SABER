//! `StudioTheme`: the §10.1 tokens of `sim_studio_ux.md`, dark and light, mapped onto Feathers'
//! semantic tokens (contextual theming, Bevy 0.20).
//!
//! Panels name colors only through [`Token`]s: the studio's own (`token::STATUS_PASS`, …) or
//! Feathers' widget tokens, which this module assigns. Status is never shown by color alone
//! ([`crate::widgets::status_mark`] adds ✓ ✖ ◐ ⚠).

use bevy::color::{Alpha, Color, LinearRgba, Luminance, Srgba};
use bevy::feathers::dark_theme::create_dark_theme;
use bevy::feathers::theme::{SemanticToken, SurfaceLevel, ThemeProps, UiTheme};
use bevy::feathers::tokens::{self as ft, semantic};
use bevy::platform::collections::HashMap;
use bevy::prelude::*;

/// A design token: the key panels use for a color. Re-exported so panels never name Feathers.
pub use bevy::feathers::theme::ThemeToken as Token;

/// Dark or light (`sim_studio_ux.md` §10.1). Dark is the default.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, Reflect,
)]
pub enum ThemeMode {
    /// Dark surfaces, light text.
    #[default]
    Dark,
    /// Light surfaces, dark text.
    Light,
}

/// The studio's theme. Changing it re-themes every widget on the next frame.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq, Reflect)]
#[reflect(Resource)]
pub struct StudioTheme {
    /// Dark or light.
    pub mode: ThemeMode,
}

/// The studio's own tokens (§10.1), beside the Feathers widget tokens in [`feathers`].
pub mod token {
    use super::Token;

    /// Window background (`bg.app`).
    pub const BG_APP: Token = Token::new_static("saber.bg.app");
    /// Dock panels (`bg.panel`).
    pub const BG_PANEL: Token = Token::new_static("saber.bg.panel");
    /// Title bar, rail and status bar: a step off `bg.app`.
    pub const BG_CHROME: Token = Token::new_static("saber.bg.chrome");
    /// Hairlines between regions, the splitter at rest.
    pub const BORDER: Token = Token::new_static("saber.border");
    /// Body text (`fg.text`).
    pub const FG_TEXT: Token = Token::new_static("saber.fg.text");
    /// Labels and units (`fg.muted`).
    pub const FG_MUTED: Token = Token::new_static("saber.fg.muted");
    /// Selection, focus, primary buttons, a splitter being dragged (`accent`).
    pub const ACCENT: Token = Token::new_static("saber.accent");
    /// PASS, healthy (`status.pass`).
    pub const STATUS_PASS: Token = Token::new_static("saber.status.pass");
    /// FAIL, crash, NaN origin (`status.fail`).
    pub const STATUS_FAIL: Token = Token::new_static("saber.status.fail");
    /// Warnings, INCONCLUSIVE (`status.warn`).
    pub const STATUS_WARN: Token = Token::new_static("saber.status.warn");
    /// Parameter-bound values (`bound`).
    pub const BOUND: Token = Token::new_static("saber.bound");
    /// Stochastic values (`uncertain`).
    pub const UNCERTAIN: Token = Token::new_static("saber.uncertain");
    /// Ground plane base (`grid.paper`, §7.10.3).
    pub const GRID_PAPER: Token = Token::new_static("saber.grid.paper");
    /// Minor grid lines (`grid.minor`).
    pub const GRID_MINOR: Token = Token::new_static("saber.grid.minor");
    /// Major grid lines (`grid.major`).
    pub const GRID_MAJOR: Token = Token::new_static("saber.grid.major");
    /// Scenario x axis on the ground (`grid.axis_x`).
    pub const GRID_AXIS_X: Token = Token::new_static("saber.grid.axis_x");
    /// Scenario y axis on the ground (`grid.axis_y`).
    pub const GRID_AXIS_Y: Token = Token::new_static("saber.grid.axis_y");
    /// Distance markings (`grid.label`).
    pub const GRID_LABEL: Token = Token::new_static("saber.grid.label");

    /// Every studio token, for tests and the widget gallery.
    pub const ALL: [Token; 18] = [
        BG_APP,
        BG_PANEL,
        BG_CHROME,
        BORDER,
        FG_TEXT,
        FG_MUTED,
        ACCENT,
        STATUS_PASS,
        STATUS_FAIL,
        STATUS_WARN,
        BOUND,
        UNCERTAIN,
        GRID_PAPER,
        GRID_MINOR,
        GRID_MAJOR,
        GRID_AXIS_X,
        GRID_AXIS_Y,
        GRID_LABEL,
    ];
}

/// The Feathers widget tokens, re-exported for the few panels that style a kit widget.
pub mod feathers {
    pub use bevy::feathers::tokens::*;
}

/// The §10.1 palette of one mode, as `#RRGGBB`.
struct Palette {
    bg_app: &'static str,
    bg_panel: &'static str,
    bg_chrome: &'static str,
    border: &'static str,
    fg_text: &'static str,
    fg_muted: &'static str,
    accent: &'static str,
    on_accent: &'static str,
    fill_solid: &'static str,
    fill_field: &'static str,
    raised: &'static str,
    status_pass: &'static str,
    status_fail: &'static str,
    status_warn: &'static str,
    bound: &'static str,
    uncertain: &'static str,
    grid_paper: &'static str,
    grid_minor: &'static str,
    grid_major: &'static str,
    grid_axis_x: &'static str,
    grid_axis_y: &'static str,
    grid_label: &'static str,
}

// The rows of §10.1 are verbatim. `bg_chrome`, `border`, `on_accent`, `fill_*` and `raised` are
// the kit's own steps between them; text on accent is dark in the dark theme because white on
// #4C9AFF is below WCAG AA (§11).
const DARK: Palette = Palette {
    bg_app: "#15171C",
    bg_panel: "#1C1F26",
    bg_chrome: "#111317",
    border: "#30363D",
    fg_text: "#E6E8EC",
    fg_muted: "#9AA3AF",
    accent: "#4C9AFF",
    on_accent: "#15171C",
    fill_solid: "#2D333B",
    fill_field: "#22272E",
    raised: "#252A33",
    status_pass: "#3FB950",
    status_fail: "#F85149",
    status_warn: "#D29922",
    bound: "#A371F7",
    uncertain: "#39C5CF",
    grid_paper: "#0F2238",
    grid_minor: "#2A4A6B",
    grid_major: "#5B86B3",
    grid_axis_x: "#E5534B",
    grid_axis_y: "#57AB5A",
    grid_label: "#C9D7E8",
};

const LIGHT: Palette = Palette {
    bg_app: "#F6F7F9",
    bg_panel: "#FFFFFF",
    bg_chrome: "#ECEEF2",
    border: "#D0D7DE",
    fg_text: "#1B1F24",
    fg_muted: "#5B6470",
    accent: "#1F6FEB",
    on_accent: "#FFFFFF",
    fill_solid: "#E3E7EC",
    fill_field: "#F0F2F5",
    raised: "#EEF0F3",
    status_pass: "#1A7F37",
    status_fail: "#CF222E",
    status_warn: "#9A6700",
    bound: "#8250DF",
    uncertain: "#1B7C83",
    grid_paper: "#FBF8EF",
    grid_minor: "#CFE3DD",
    grid_major: "#8DBFB3",
    grid_axis_x: "#CF222E",
    grid_axis_y: "#1A7F37",
    grid_label: "#3D5A55",
};

/// `#RRGGBB` to a color. The palettes above are checked by the tests.
fn hex(s: &str) -> Color {
    Srgba::hex(s).map(Color::from).unwrap_or(Color::NONE)
}

impl ThemeMode {
    fn palette(self) -> &'static Palette {
        match self {
            ThemeMode::Dark => &DARK,
            ThemeMode::Light => &LIGHT,
        }
    }

    /// One step "up" from a surface: lighter on dark, darker on light.
    fn step(self, c: Color, amount: f32) -> Color {
        match self {
            ThemeMode::Dark => c.lighter(amount),
            ThemeMode::Light => c.darker(amount),
        }
    }
}

/// The Feathers theme for a mode: Feathers' own token assignments with the §10.1 colors, the
/// studio's tokens added, and control text moved off `text.onAccent` so it reads on light fills.
pub fn theme_props(mode: ThemeMode) -> ThemeProps {
    let p = mode.palette();
    let mut props = create_dark_theme();
    let fill_solid = hex(p.fill_solid);
    let fill_field = hex(p.fill_field);
    let accent = hex(p.accent);
    let panel = hex(p.bg_panel);
    let text = hex(p.fg_text);
    let base: [(SemanticToken, Color); 33] = [
        (semantic::SURFACE_WINDOW, hex(p.bg_app)),
        (semantic::SURFACE_PANE_HEADER, hex(p.bg_app)),
        (semantic::SURFACE_PANE_BODY, panel),
        (semantic::SURFACE_SUBPANE_BODY, panel),
        (semantic::SURFACE_SUBPANE_HEADER, hex(p.raised)),
        (semantic::SURFACE_GROUP, hex(p.raised)),
        (semantic::SURFACE_DIALOG, panel),
        (semantic::SURFACE_MENU, panel),
        (semantic::BORDER_DEFAULT, hex(p.border)),
        (semantic::TEXT_DEFAULT, text),
        (semantic::TEXT_DIM, hex(p.fg_muted)),
        (semantic::TEXT_DISABLED, text.with_alpha(0.4)),
        (semantic::TEXT_ON_ACCENT, hex(p.on_accent)),
        (semantic::FILL_ACCENT_DEFAULT, accent),
        (semantic::FILL_ACCENT_HOVER, mode.step(accent, 0.05)),
        (semantic::FILL_ACCENT_PRESSED, mode.step(accent, 0.1)),
        (semantic::FILL_ACCENT_DISABLED, fill_solid.with_alpha(0.7)),
        (semantic::FILL_SOLID_DEFAULT, fill_solid),
        (semantic::FILL_SOLID_HOVER, mode.step(fill_solid, 0.05)),
        (semantic::FILL_SOLID_PRESSED, mode.step(fill_solid, 0.1)),
        (semantic::FILL_SOLID_DISABLED, fill_solid.with_alpha(0.7)),
        (semantic::FILL_ITEM_DEFAULT, Color::NONE),
        (semantic::FILL_ITEM_HOVER, mode.step(panel, 0.05)),
        (semantic::FILL_ITEM_PRESSED, mode.step(panel, 0.1)),
        (semantic::FILL_FIELD_DEFAULT, fill_field),
        (semantic::FILL_FIELD_HOVER, mode.step(fill_field, 0.03)),
        (semantic::FILL_FIELD_PRESSED, mode.step(fill_field, 0.06)),
        (semantic::FILL_NONE, Color::NONE),
        (semantic::AXIS_X, hex(p.grid_axis_x)),
        (semantic::AXIS_Y, hex(p.grid_axis_y)),
        (semantic::AXIS_Z, accent),
        (semantic::AXIS_W, hex(p.fg_muted)),
        (semantic::FOCUS_RING, accent.with_alpha(0.6)),
    ];
    props.semantic_base = base.into_iter().collect();
    // Controls on raised surfaces (groups, menus, dialogs) step once more, as Feathers does.
    let raised: HashMap<SemanticToken, Color> = [
        (semantic::FILL_SOLID_DEFAULT, mode.step(fill_solid, 0.05)),
        (semantic::FILL_SOLID_HOVER, mode.step(fill_solid, 0.1)),
        (semantic::FILL_SOLID_PRESSED, mode.step(fill_solid, 0.15)),
        (semantic::FILL_FIELD_DEFAULT, mode.step(fill_field, 0.04)),
        (semantic::FILL_ITEM_HOVER, mode.step(panel, 0.08)),
        (semantic::FILL_ITEM_PRESSED, mode.step(panel, 0.12)),
    ]
    .into_iter()
    .collect();
    props.semantic_overrides = [
        (SurfaceLevel::Highest, raised.clone()),
        (SurfaceLevel::Floating, raised),
    ]
    .into_iter()
    .collect();
    // Control text reads on its fill, not on the accent.
    for t in [
        ft::BUTTON_TEXT,
        ft::MENUITEM_TEXT,
        ft::TEXT_INPUT_TEXT,
        ft::LISTROW_TEXT,
        ft::SLIDER_TEXT,
    ] {
        props.token_assignments.insert(t, semantic::TEXT_DEFAULT);
    }
    let own: [(Token, Color); 18] = [
        (token::BG_APP, hex(p.bg_app)),
        (token::BG_PANEL, panel),
        (token::BG_CHROME, hex(p.bg_chrome)),
        (token::BORDER, hex(p.border)),
        (token::FG_TEXT, text),
        (token::FG_MUTED, hex(p.fg_muted)),
        (token::ACCENT, accent),
        (token::STATUS_PASS, hex(p.status_pass)),
        (token::STATUS_FAIL, hex(p.status_fail)),
        (token::STATUS_WARN, hex(p.status_warn)),
        (token::BOUND, hex(p.bound)),
        (token::UNCERTAIN, hex(p.uncertain)),
        (token::GRID_PAPER, hex(p.grid_paper)),
        (token::GRID_MINOR, hex(p.grid_minor)),
        (token::GRID_MAJOR, hex(p.grid_major)),
        (token::GRID_AXIS_X, hex(p.grid_axis_x)),
        (token::GRID_AXIS_Y, hex(p.grid_axis_y)),
        (token::GRID_LABEL, hex(p.grid_label)),
    ];
    for (t, c) in own {
        let s = SemanticToken::new(t.to_string().into());
        props.token_assignments.insert(t, s.clone());
        props.semantic_base.insert(s, c);
    }
    props
}

/// WCAG 2 contrast ratio of two opaque colors (1 to 21).
pub fn contrast(a: Color, b: Color) -> f32 {
    let l = |c: Color| {
        let LinearRgba {
            red, green, blue, ..
        } = c.to_linear();
        0.2126 * red + 0.7152 * green + 0.0722 * blue
    };
    let (la, lb) = (l(a), l(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Keep Feathers' `UiTheme` in step with [`StudioTheme`].
pub(crate) fn apply_theme(theme: Res<StudioTheme>, mut ui: ResMut<UiTheme>) {
    if theme.is_changed() {
        ui.0 = theme_props(theme.mode);
    }
}
