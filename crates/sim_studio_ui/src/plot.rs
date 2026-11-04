//! Line plots for live channels (`sim_studio_ux.md` §7.5 PLOTS): a series drawn into an image on
//! the CPU and shown with an `ImageNode`. Nothing here needs a GPU, so it runs headless too, and
//! what is drawn is plain bytes a test can read.

use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

/// The colors a plot is drawn with, as RGBA bytes (sRGB).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlotColors {
    /// Behind the line.
    pub background: [u8; 4],
    /// The series.
    pub line: [u8; 4],
    /// The value 0, when it is in range.
    pub zero: [u8; 4],
}

/// An RGBA8 image of `width` × `height` pixels, all `fill`, for a plot to draw into.
pub fn plot_image(width: u32, height: u32, fill: [u8; 4]) -> Image {
    Image::new_fill(
        Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &fill,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    )
}

/// The value range a series is drawn over: its finite minimum and maximum, padded by 5 %
/// (a constant series gets ±1 around its value, or ±|v|·5 %).
pub fn value_range(samples: &[[f64; 2]]) -> Option<(f64, f64)> {
    let (lo, hi) = samples
        .iter()
        .map(|s| s[1])
        .filter(|v| v.is_finite())
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
            (lo.min(v), hi.max(v))
        });
    if lo > hi {
        return None;
    }
    if hi - lo <= f64::EPSILON * hi.abs().max(lo.abs()).max(1.0) {
        let pad = (lo.abs() * 0.05).max(1.0);
        return Some((lo - pad, hi + pad));
    }
    let pad = (hi - lo) * 0.05;
    Some((lo - pad, hi + pad))
}

/// Draw `[t, value]` samples as a line into RGBA8 `pixels` (`width` × `height`, row 0 at the
/// top): time from the first to the last sample across, the value range up the side. Non-finite
/// values break the line.
pub fn draw(pixels: &mut [u8], width: u32, height: u32, samples: &[[f64; 2]], colors: PlotColors) {
    let (w, h) = (width as i64, height as i64);
    for px in pixels.chunks_exact_mut(4) {
        px.copy_from_slice(&colors.background);
    }
    let Some((lo, hi)) = value_range(samples) else {
        return;
    };
    let (t0, t1) = (samples[0][0], samples[samples.len() - 1][0]);
    let span = if t1 > t0 { t1 - t0 } else { 1.0 };
    let x = |t: f64| (((t - t0) / span) * (w - 1) as f64).round() as i64;
    let y = |v: f64| (((hi - v) / (hi - lo)) * (h - 1) as f64).round() as i64;
    let mut put = |px: i64, py: i64, c: [u8; 4]| {
        if (0..w).contains(&px) && (0..h).contains(&py) {
            let i = ((py * w + px) * 4) as usize;
            pixels[i..i + 4].copy_from_slice(&c);
        }
    };
    if lo < 0.0 && hi > 0.0 {
        let zy = y(0.0);
        for px in 0..w {
            put(px, zy, colors.zero);
        }
    }
    let mut last: Option<(i64, i64)> = None;
    for s in samples {
        if !s[1].is_finite() {
            last = None;
            continue;
        }
        let p = (x(s[0]), y(s[1]));
        let from = last.unwrap_or(p);
        line(from, p, |a, b| put(a, b, colors.line));
        last = Some(p);
    }
}

/// Bresenham's line from `a` to `b`, both ends included.
fn line(a: (i64, i64), b: (i64, i64), mut plot: impl FnMut(i64, i64)) {
    let (mut x, mut y) = a;
    let dx = (b.0 - x).abs();
    let dy = -(b.1 - y).abs();
    let sx = if x < b.0 { 1 } else { -1 };
    let sy = if y < b.1 { 1 } else { -1 };
    let mut err = dx + dy;
    loop {
        plot(x, y);
        if (x, y) == b {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
}

/// Spawn an image node of `width` × `height` logical pixels showing `image` under `parent`.
pub fn spawn_plot(
    commands: &mut Commands,
    parent: Entity,
    image: Handle<Image>,
    width: f32,
    height: f32,
) -> Entity {
    commands
        .spawn((
            Node {
                width: px(width),
                height: px(height),
                flex_shrink: 0.0,
                ..default()
            },
            ImageNode::new(image),
            ChildOf(parent),
        ))
        .id()
}
