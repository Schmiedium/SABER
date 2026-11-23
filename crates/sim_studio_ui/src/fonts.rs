//! Fonts (`sim_studio_ux.md` §10.2): Feathers' Fira Sans and Fira Mono, with a bundled symbol font
//! behind them.
//!
//! Fira has no glyphs for most symbols the studio writes (✓ ✖ ⚠ ◐ ☰ ▸ ⚙ ℹ …), which drew as
//! empty boxes. [`SYMBOLS`] is DejaVu Sans 2.37 cut down to its symbol and arrow blocks
//! (`scripts/make_symbol_font.sh`; license in `assets/fonts/DejaVu-LICENSE.txt`). Every text in a
//! single font gets this font as its fallback ([`with_symbols`]), so a glyph Fira lacks comes from
//! here and everything else still from Fira.

use bevy::prelude::*;
use bevy::text::FontSource;

/// The bundled symbol font, in [`Assets<Font>`] from the moment [`crate::KitPlugin`] is added.
pub const SYMBOLS: Handle<Font> = bevy::asset::uuid_handle!("9d6b2fac-cf13-453e-a3d4-43646da7769e");

/// The symbol font's file: DejaVu Sans 2.37, symbol and arrow blocks only, no hinting.
pub const SYMBOLS_TTF: &[u8] = include_bytes!("../assets/fonts/DejaVuSans-Symbols.ttf");

/// Put the symbol font into the app's fonts. Synchronous, so the first frame's text can use it.
pub(crate) fn add_symbol_font(app: &mut App) {
    let mut fonts = app.world_mut().resource_mut::<Assets<Font>>();
    if fonts.get(&SYMBOLS).is_none() {
        fonts
            .insert(SYMBOLS.id(), Font::from_bytes(SYMBOLS_TTF.to_vec()))
            .expect("the symbol font's id is a fixed UUID");
    }
}

/// `font` with the symbol font behind it, or `None` when it has a fallback list already (or is
/// the symbol font).
pub fn symbols_behind(font: &FontSource) -> Option<FontSource> {
    match font {
        FontSource::Handle(h) if *h != SYMBOLS => Some(FontSource::List(vec![
            FontSource::Handle(h.clone()),
            FontSource::Handle(SYMBOLS),
        ])),
        _ => None,
    }
}

/// Give every text set in a single font the symbol font as its fallback. Runs after Feathers
/// propagates its fonts and before text is laid out, so the first layout already falls back.
pub fn with_symbols(mut texts: Query<&mut TextFont, Changed<TextFont>>) {
    for mut text in &mut texts {
        if let Some(font) = symbols_behind(&text.font) {
            text.font = font;
        }
    }
}
