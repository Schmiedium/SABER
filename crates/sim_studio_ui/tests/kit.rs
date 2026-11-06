//! The kit's logic without a window (verification §8.3): theme tokens and their contrast (§11),
//! dock layout arithmetic, and the toast log.

use bevy::feathers::theme::{SurfaceLevel, ThemeToken};
use bevy::feathers::tokens as ft;
use bevy::prelude::*;
use sim_studio_ui::KitCorePlugin;
use sim_studio_ui::dock::{DockLayout, DockSide};
use sim_studio_ui::theme::{ThemeMode, contrast, theme_props, token};
use sim_studio_ui::toast::{Toast, ToastLevel, ToastLog};

const MODES: [ThemeMode; 2] = [ThemeMode::Dark, ThemeMode::Light];

/// Every studio token and every Feathers token resolves to a color in both themes (Feathers
/// paints a missing one fuchsia), and the §10.1 rows are the documented colors.
#[test]
fn every_token_resolves_in_both_themes() {
    let fuchsia: Color = bevy::color::palettes::basic::FUCHSIA.into();
    for mode in MODES {
        let props = theme_props(mode);
        for t in token::ALL {
            assert!(props.token_assignments.contains_key(&t), "{mode:?} {t}");
            assert_ne!(props.color(&t), fuchsia, "{mode:?} {t}");
        }
        for (t, _) in &props.token_assignments {
            for level in [
                SurfaceLevel::Base,
                SurfaceLevel::Higher,
                SurfaceLevel::Highest,
                SurfaceLevel::Floating,
            ] {
                assert_ne!(
                    props.context_color(t, level),
                    fuchsia,
                    "{mode:?} {t} {level:?}"
                );
            }
        }
    }
    let hex = |mode, t: &ThemeToken| Srgba::from(theme_props(mode).color(t)).to_hex();
    assert_eq!(hex(ThemeMode::Dark, &token::BG_APP), "#15171C");
    assert_eq!(hex(ThemeMode::Dark, &token::ACCENT), "#4C9AFF");
    assert_eq!(hex(ThemeMode::Dark, &token::STATUS_FAIL), "#F85149");
    assert_eq!(hex(ThemeMode::Light, &token::BG_PANEL), "#FFFFFF");
    assert_eq!(hex(ThemeMode::Light, &token::STATUS_WARN), "#9A6700");
    assert_eq!(hex(ThemeMode::Light, &token::GRID_PAPER), "#FBF8EF");
    // Feathers' window background is the studio's.
    assert_eq!(hex(ThemeMode::Light, &ft::WINDOW_BG), "#F6F7F9");
}

/// Text reads on its background at WCAG AA (4.5:1) in both themes: body, muted and status text
/// on the window and on panels, and every control's text on its fill (§11).
#[test]
fn text_meets_wcag_aa_contrast() {
    let pairs: [(ThemeToken, ThemeToken); 16] = [
        (token::FG_TEXT, token::BG_APP),
        (token::FG_TEXT, token::BG_PANEL),
        (token::FG_TEXT, token::BG_CHROME),
        (token::FG_MUTED, token::BG_APP),
        (token::FG_MUTED, token::BG_PANEL),
        (token::FG_MUTED, token::BG_CHROME),
        (token::STATUS_PASS, token::BG_PANEL),
        (token::STATUS_FAIL, token::BG_PANEL),
        (token::STATUS_WARN, token::BG_PANEL),
        (token::STATUS_WARN, token::BG_APP),
        (token::BOUND, token::BG_PANEL),
        (token::UNCERTAIN, token::BG_PANEL),
        (ft::BUTTON_TEXT, ft::BUTTON_BG),
        (ft::BUTTON_PRIMARY_TEXT, ft::BUTTON_PRIMARY_BG),
        (ft::TEXT_INPUT_TEXT, ft::TEXT_INPUT_BG),
        (token::GRID_LABEL, token::GRID_PAPER),
    ];
    for mode in MODES {
        let props = theme_props(mode);
        for (fg, bg) in &pairs {
            let c = contrast(props.color(fg), props.color(bg));
            assert!(c >= 4.5, "{mode:?}: {fg} on {bg} is {c:.2}:1");
        }
    }
    // The ratio itself: white on black is 21:1, a color on itself 1:1.
    assert!((contrast(Color::WHITE, Color::BLACK) - 21.0).abs() < 1e-3);
    assert!((contrast(Color::WHITE, Color::WHITE) - 1.0).abs() < 1e-6);
}

/// Dragging a splitter resizes its region within the window, collapsing hides a region and keeps
/// its size, and a shrinking window or a hand-edited file never leaves a region too small.
#[test]
fn dock_layout_resizes_within_bounds() {
    let window = Vec2::new(1600.0, 900.0);
    let mut l = DockLayout::default();
    l.drag(DockSide::Left, Vec2::new(40.0, 0.0), window);
    assert_eq!(l.left, 320.0);
    l.drag(DockSide::Right, Vec2::new(40.0, 0.0), window);
    assert_eq!(
        l.right, 280.0,
        "dragging the right splitter right narrows the right panel"
    );
    l.drag(DockSide::Bottom, Vec2::new(0.0, -50.0), window);
    assert_eq!(l.bottom, 250.0, "dragging up grows the bottom panel");
    // Clamped: never below the minimum, never squeezing the center below its minimum.
    l.drag(DockSide::Left, Vec2::new(-1000.0, 0.0), window);
    assert_eq!(l.left, DockLayout::MIN);
    l.drag(DockSide::Left, Vec2::new(5000.0, 0.0), window);
    let max = window.x - l.right - DockLayout::MIN_CENTER - 2.0 * DockLayout::SPLITTER;
    assert_eq!(l.left, max);
    l.drag(DockSide::Bottom, Vec2::new(0.0, -5000.0), window);
    assert_eq!(
        l.bottom,
        window.y - DockLayout::MIN_CENTER - DockLayout::SPLITTER
    );

    // Collapse and restore keep the size; a collapsed side frees its width.
    let mut l = DockLayout::default();
    l.toggle(DockSide::Right);
    assert!(l.collapsed(DockSide::Right));
    assert_eq!(
        (l.shown(DockSide::Right), l.size(DockSide::Right)),
        (0.0, 320.0)
    );
    assert!(
        l.max_size(DockSide::Left, window) > DockLayout::default().max_size(DockSide::Left, window)
    );
    l.toggle(DockSide::Right);
    assert_eq!(l.shown(DockSide::Right), 320.0);
    // Dragging a collapsed splitter restores the region.
    l.toggle(DockSide::Bottom);
    l.drag(DockSide::Bottom, Vec2::new(0.0, -10.0), window);
    assert!(!l.collapsed(DockSide::Bottom));
    assert_eq!(l.bottom, 210.0);

    // A small window fits the sides, never below the minimum.
    let mut l = DockLayout {
        left: 600.0,
        right: 600.0,
        bottom: 700.0,
        ..DockLayout::default()
    };
    l.fit(Vec2::new(1000.0, 600.0));
    assert!(l.left <= 1000.0 - 600.0 - DockLayout::MIN_CENTER - 8.0);
    assert_eq!(
        l.bottom,
        600.0 - DockLayout::MIN_CENTER - DockLayout::SPLITTER
    );
    let mut tiny = DockLayout::default();
    tiny.fit(Vec2::new(100.0, 100.0));
    assert_eq!((tiny.left, tiny.right, tiny.bottom), (120.0, 120.0, 120.0));

    // Non-finite or too-small sizes from a file fall back to the defaults.
    let bad = DockLayout {
        left: f32::NAN,
        right: 10.0,
        bottom: 250.0,
        ..DockLayout::default()
    }
    .sanitized();
    assert_eq!((bad.left, bad.right, bad.bottom), (280.0, 320.0, 250.0));
}

/// Toasts are logged in order (capped) even without a window, and every level carries a mark so
/// it never rests on color alone.
#[test]
fn toasts_are_logged_with_marks() {
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, KitCorePlugin));
    app.world_mut().write_message(Toast::info("one"));
    app.world_mut().write_message(Toast::error("two"));
    app.update();
    let log = app.world().resource::<ToastLog>();
    let texts: Vec<_> = log.entries.iter().map(|(_, t)| t.text.as_str()).collect();
    assert_eq!(texts, ["one", "two"]);
    assert_eq!(log.entries[1].1.level, ToastLevel::Error);

    for _ in 0..ToastLog::CAP + 3 {
        app.world_mut().write_message(Toast::success("x"));
    }
    app.update();
    let log = app.world().resource::<ToastLog>();
    assert_eq!(log.entries.len(), ToastLog::CAP);
    assert_eq!(log.entries[0].1.text, "x", "the oldest went first");

    let marks: Vec<_> = [
        ToastLevel::Info,
        ToastLevel::Success,
        ToastLevel::Warning,
        ToastLevel::Error,
    ]
    .iter()
    .map(|l| l.mark())
    .collect();
    assert_eq!(marks, ["ℹ", "✓", "⚠", "✖"]);
    assert!(ToastLevel::Error.lifetime() > ToastLevel::Info.lifetime());
}
