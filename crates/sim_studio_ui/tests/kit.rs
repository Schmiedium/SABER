//! The kit's logic without a window (verification §8.3): theme tokens and their contrast (§11),
//! dock layout arithmetic, the toast log, and text laid out with the symbol font behind Fira.

use bevy::feathers::theme::{SurfaceLevel, ThemeToken};
use bevy::feathers::tokens as ft;
use bevy::prelude::*;
use bevy::render::RenderPlugin;
use bevy::render::settings::{RenderCreation, WgpuSettings};
use bevy::text::{FontAtlasSet, FontSource, TextLayoutInfo};
use bevy::window::ExitCondition;
use bevy::winit::WinitPlugin;
use sim_studio_ui::dock::{DockLayout, DockSide};
use sim_studio_ui::fonts::SYMBOLS;
use sim_studio_ui::theme::{ThemeMode, contrast, theme_props, token};
use sim_studio_ui::toast::{Toast, ToastLevel, ToastLog};
use sim_studio_ui::widgets::{heading, label, mono};
use sim_studio_ui::{KitCorePlugin, KitPlugin};

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

/// A plot draws its series across the whole image: the first sample at the left edge, the last
/// at the right, the largest value at the top, the smallest at the bottom, and the zero line.
#[test]
fn plots_draw_a_series_across_the_image() {
    use sim_studio_ui::plot::{PlotColors, draw, value_range};
    let colors = PlotColors {
        background: [0, 0, 0, 255],
        line: [255, 0, 0, 255],
        zero: [0, 0, 255, 255],
    };
    let (w, h) = (40u32, 21u32);
    let mut px = vec![0u8; (w * h * 4) as usize];
    let at = |px: &[u8], x: u32, y: u32| {
        let i = ((y * w + x) * 4) as usize;
        [px[i], px[i + 1], px[i + 2], px[i + 3]]
    };
    let ramp: Vec<[f64; 2]> = (0..=10).map(|i| [i as f64, i as f64 - 5.0]).collect();
    let (lo, hi) = value_range(&ramp).unwrap();
    assert!(lo < -5.0 && hi > 5.0);
    draw(&mut px, w, h, &ramp, colors);
    let row_of = |v: f64| (((hi - v) / (hi - lo)) * (h - 1) as f64).round() as u32;
    assert_eq!(
        at(&px, 0, row_of(-5.0)),
        colors.line,
        "first sample, lowest"
    );
    assert_eq!(
        at(&px, w - 1, row_of(5.0)),
        colors.line,
        "last sample, highest"
    );
    assert_eq!(at(&px, w / 2, 0), colors.background);
    assert_eq!(at(&px, 2, row_of(0.0)), colors.zero, "the zero line");

    // A gap where the value is not finite; a constant series still has a range.
    let gap = [[0.0, 1.0], [1.0, f64::NAN], [2.0, 1.0]];
    draw(&mut px, w, h, &gap, colors);
    assert_eq!(
        at(&px, w / 2, row_of_const(h)),
        colors.background,
        "no line across the gap"
    );
    assert_eq!(value_range(&[[0.0, 3.0]]), Some((2.0, 4.0)));
    assert_eq!(value_range(&[[0.0, f64::NAN]]), None);

    fn row_of_const(h: u32) -> u32 {
        (h - 1) / 2
    }
}

/// A track over a fixed range: a level outside the samples shares the axis and draws dashed; a
/// column marks one item of many, the first at the left edge and the last at the right.
#[test]
fn tracks_draw_levels_and_columns() {
    use sim_studio_ui::plot::{PlotColors, column_of, draw_column, draw_in, draw_level, row_of};
    let colors = PlotColors {
        background: [0, 0, 0, 255],
        line: [255, 0, 0, 255],
        zero: [0, 0, 255, 255],
    };
    let (w, h) = (16u32, 11u32);
    let mut px = vec![0u8; (w * h * 4) as usize];
    let at = |px: &[u8], x: u32, y: u32| {
        let i = ((y * w + x) * 4) as usize;
        [px[i], px[i + 1], px[i + 2], px[i + 3]]
    };
    let range = (0.0, 10.0);
    assert_eq!(row_of(10.0, h, range), Some(0));
    assert_eq!(row_of(0.0, h, range), Some(h - 1));
    assert_eq!(row_of(5.0, h, range), Some(5));
    for v in [-1.0, 11.0, f64::NAN] {
        assert_eq!(row_of(v, h, range), None, "{v}");
    }
    assert_eq!(row_of(1.0, h, (1.0, 1.0)), None, "an empty range");

    // Samples from 1 to 2 drawn over 0..10 sit low; the level at 8 is dashed above them.
    let low = [[0.0, 1.0], [1.0, 2.0]];
    draw_in(&mut px, w, h, &low, Some(range), colors);
    assert_eq!(at(&px, 0, row_of(1.0, h, range).unwrap()), colors.line);
    assert_eq!(at(&px, w - 1, row_of(2.0, h, range).unwrap()), colors.line);
    let y = row_of(8.0, h, range).unwrap();
    draw_level(&mut px, w, h, y, colors.zero);
    assert_eq!(at(&px, 0, y), colors.zero);
    assert_eq!(at(&px, 4, y), colors.background, "a gap in the dashes");
    draw_level(&mut px, w, h, h, colors.zero); // outside: nothing, no panic

    assert_eq!(column_of(0, 8, w), 0);
    assert_eq!(column_of(7, 8, w), w - 1);
    assert_eq!(column_of(99, 8, w), w - 1);
    assert_eq!(column_of(3, 1, w), 0);
    draw_column(&mut px, w, h, 5, 2..99, colors.line);
    assert_eq!(at(&px, 5, 1), colors.background);
    assert_eq!(at(&px, 5, 2), colors.line);
    assert_eq!(at(&px, 5, h - 1), colors.line);
    draw_column(&mut px, w, h, w, 0..h, colors.line); // outside: nothing, no panic

    // No range, or an empty one, leaves the background.
    draw_in(&mut px, w, h, &low, None, colors);
    assert!(px.chunks_exact(4).all(|p| p == colors.background));
    draw_in(&mut px, w, h, &[], Some(range), colors);
    assert!(px.chunks_exact(4).all(|p| p == colors.background));
}

/// The whole kit on `DefaultPlugins` without a window system or GPU: text is still laid out and
/// its glyphs rasterized into font atlases on the CPU.
fn text_app() -> App {
    let mut app = App::new();
    app.add_plugins((
        DefaultPlugins
            .build()
            .disable::<WinitPlugin>()
            .set(WindowPlugin {
                primary_window: Some(Window::default()),
                exit_condition: ExitCondition::DontExit,
                ..default()
            })
            .set(RenderPlugin {
                render_creation: RenderCreation::Automatic(Box::new(WgpuSettings {
                    backends: None,
                    ..default()
                })),
                ..default()
            }),
        KitPlugin,
    ));
    app.world_mut().spawn(Camera2d);
    app
}

/// Update until every text has glyphs (Feathers' fonts load from embedded assets).
fn lay_out(app: &mut App) {
    for _ in 0..200 {
        app.update();
        let mut q = app.world_mut().query::<(&Text, &TextLayoutInfo)>();
        if q.iter(app.world())
            .all(|(t, l)| t.0.is_empty() || !l.glyphs.is_empty())
        {
            return;
        }
    }
    panic!("text was never laid out");
}

/// The fonts (by font data id) that drew glyph 0, `.notdef`, the empty box of a character the
/// font lacks.
fn notdef_fonts(app: &App) -> Vec<u32> {
    let atlases = app.world().resource::<FontAtlasSet>();
    let mut ids: Vec<u32> = atlases
        .iter()
        .filter(|(_, a)| {
            a.iter()
                .any(|a| a.glyph_to_atlas_index.keys().any(|k| k.glyph_id == 0))
        })
        .map(|(k, _)| k.id)
        .collect();
    ids.sort_unstable();
    ids
}

/// Symbols Fira lacks come from the bundled symbol font, in the regular, bold and mono styles,
/// and none draws an empty box; a character no bundled font has still does, so the check works.
#[test]
fn symbols_fall_back_to_the_bundled_font() {
    let mut app = text_app();
    let symbols = "PASS ✓ FAIL ✖ ⚠ ◐ ☰ ▸ ⚙ ℹ ● ○ ↳ ★ −12.5 m · γ";
    let world = app.world_mut();
    world.spawn_scene(label(symbols)).unwrap();
    world.spawn_scene(heading(symbols)).unwrap();
    world.spawn_scene(mono(symbols)).unwrap();
    lay_out(&mut app);

    let mut q = app.world_mut().query::<(&Text, &TextFont)>();
    for (_, font) in q.iter(app.world()) {
        match &font.font {
            FontSource::List(list) => {
                assert_eq!(list.len(), 2);
                assert_eq!(list[1], FontSource::Handle(SYMBOLS));
            }
            other => panic!("no symbol fallback: {other:?}"),
        }
    }
    let symbol_id = app
        .world()
        .resource::<Assets<Font>>()
        .get(&SYMBOLS)
        .expect("the symbol font is added with the kit")
        .data
        .id() as u32;
    let atlases = app.world().resource::<FontAtlasSet>();
    assert!(
        atlases.keys().any(|k| k.id == symbol_id),
        "the symbols were drawn from the symbol font"
    );
    assert!(
        atlases.keys().any(|k| k.id != symbol_id),
        "the letters from Fira"
    );
    assert_eq!(notdef_fonts(&app), Vec::<u32>::new());

    // U+23F8 (pause) is in neither Fira nor the symbol font: it draws the empty box.
    app.world_mut().spawn_scene(label("\u{23F8}")).unwrap();
    lay_out(&mut app);
    assert!(!notdef_fonts(&app).is_empty());
}
