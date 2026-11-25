//! Thumbnails and the import preview (`sim_studio_ux.md` §7.11.1, §7.11.3): a small flat-shaded
//! picture of a model in the body frame, drawn on the CPU. No GPU and no Bevy, so the importer,
//! `saber-cli`, the built-in generator and the studio draw the same picture, also headless.

use glam::DVec3;

use crate::inspect::{GltfData, each_triangle};
use crate::manifest::{Bounds, VisualAssetManifest};

/// A model's thumbnail, beside its `model.ron`.
pub const THUMB_FILE: &str = "thumb.png";

/// A thumbnail's edge, px.
pub const THUMB_PX: u32 = 128;

/// One triangle in the body frame, metres, with its linear RGBA colour.
pub type Facet = ([DVec3; 3], [f32; 4]);

/// The colour of a primitive with no material: glTF's default, white.
const DEFAULT_COLOR: [f32; 4] = [0.8, 0.8, 0.8, 1.0];

/// Every drawn triangle of a file with its material's base colour, in the file's units and axes.
pub fn facets(data: &GltfData) -> Vec<Facet> {
    let mut out = Vec::new();
    each_triangle(data, |t, p| {
        let m = p.material();
        let color = if m.index().is_some() {
            m.pbr_metallic_roughness().base_color_factor()
        } else {
            DEFAULT_COLOR
        };
        out.push((t, color));
    });
    out
}

/// The facets of a file in the body frame of its manifest (frame, unit and origin).
pub fn body_facets(data: &GltfData, manifest: &VisualAssetManifest) -> Vec<Facet> {
    facets(data)
        .into_iter()
        .map(|(t, c)| (t.map(|p| manifest.to_body(p)), c))
        .collect()
}

/// An RGBA picture, rows top to bottom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    /// Width, px.
    pub width: u32,
    /// Height, px.
    pub height: u32,
    /// 8-bit sRGB RGBA, straight alpha.
    pub rgba: Vec<u8>,
}

impl Picture {
    /// A transparent picture.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            rgba: vec![0; (width * height * 4) as usize],
        }
    }

    /// The picture as PNG bytes.
    pub fn png(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, self.width, self.height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().expect("a PNG header writes to memory");
        w.write_image_data(&self.rgba)
            .expect("a PNG body writes to memory");
        w.finish().expect("a PNG finishes in memory");
        out
    }

    /// A PNG's picture (8-bit RGBA only, as [`Picture::png`] writes).
    pub fn from_png(bytes: &[u8]) -> Option<Self> {
        let mut r = png::Decoder::new(std::io::Cursor::new(bytes))
            .read_info()
            .ok()?;
        let mut buf = vec![0; r.output_buffer_size()?];
        let info = r.next_frame(&mut buf).ok()?;
        if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
            return None;
        }
        buf.truncate(info.buffer_size());
        Some(Self {
            width: info.width,
            height: info.height,
            rgba: buf,
        })
    }

    /// The share of pixels that are not transparent.
    pub fn coverage(&self) -> f64 {
        let opaque = self.rgba.chunks_exact(4).filter(|p| p[3] > 0).count();
        opaque as f64 / (self.width * self.height).max(1) as f64
    }

    fn put(&mut self, x: i64, y: i64, c: [u8; 4]) {
        if x >= 0 && y >= 0 && (x as u32) < self.width && (y as u32) < self.height {
            let i = ((y as u32 * self.width + x as u32) * 4) as usize;
            self.rgba[i..i + 4].copy_from_slice(&c);
        }
    }

    /// Halve the size, averaging 2×2 blocks (alpha-weighted).
    fn half(&self) -> Self {
        let (w, h) = (self.width / 2, self.height / 2);
        let mut out = Self::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let (mut rgb, mut alpha) = ([0u32; 3], 0u32);
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let i = (((y * 2 + dy) * self.width + x * 2 + dx) * 4) as usize;
                    let a = self.rgba[i + 3] as u32;
                    for (sum, c) in rgb.iter_mut().zip(&self.rgba[i..i + 3]) {
                        *sum += *c as u32 * a;
                    }
                    alpha += a;
                }
                let i = ((y * w + x) * 4) as usize;
                for (o, sum) in out.rgba[i..i + 3].iter_mut().zip(rgb) {
                    *o = sum.checked_div(alpha).unwrap_or(0) as u8;
                }
                out.rgba[i + 3] = (alpha / 4) as u8;
            }
        }
        out
    }
}

/// Where a picture is seen from, in the body frame (x forward, y right, z down).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    /// From the model towards the eye.
    pub eye: DVec3,
    /// Up on the screen.
    pub up: DVec3,
}

impl View {
    /// In front, to the left and above, the body's up at the top: the thumbnails' view.
    pub const THUMB: View = View {
        eye: DVec3::new(1.0, -0.85, -0.6),
        up: DVec3::new(0.0, 0.0, -1.0),
    };
}

/// What else to draw over the model.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Extras {
    /// The body-frame axis triad at the origin: x red (forward), y green (right), z blue (down).
    pub axes: bool,
    /// A reference bar along the bottom edge.
    pub bar: bool,
}

/// What [`draw`] drew.
#[derive(Clone, Debug, PartialEq)]
pub struct Drawn {
    /// The picture.
    pub picture: Picture,
    /// The reference bar's length when drawn, m (1 m unless that does not fit).
    pub bar_m: Option<f64>,
}

/// The light, from the eye's side and above.
const LIGHT: DVec3 = DVec3::new(0.5, -0.3, -0.8);

/// Draw facets in the body frame, seen from `view`, fitted to a `size`-pixel square.
pub fn draw(facets: &[Facet], size: u32, view: View, extras: Extras) -> Drawn {
    // Drawn at twice the size, then halved: cheap anti-aliasing.
    let big = size * 2;
    let mut pic = Picture::new(big, big);
    let dir = view.eye.normalize_or(DVec3::X);
    let up = (view.up - dir * view.up.dot(dir)).normalize_or(DVec3::NEG_Z);
    let right = (-dir).cross(up);
    let light = LIGHT.normalize();
    let screen = |p: DVec3| DVec3::new(p.dot(right), p.dot(up), p.dot(dir));
    let mut points: Vec<DVec3> = facets.iter().flat_map(|(t, _)| t.iter()).copied().collect();
    if extras.axes {
        points.push(DVec3::ZERO);
    }
    let Some(b) = Bounds::around(points.iter().map(|p| screen(*p))) else {
        return Drawn {
            picture: pic.half(),
            bar_m: None,
        };
    };
    let extent = b.size().x.max(b.size().y).max(1e-9);
    let margin = if extras.bar { 0.78 } else { 0.86 };
    let scale = big as f64 * margin / extent;
    let center = b.center();
    let to_px = |p: DVec3| {
        let s = screen(p);
        DVec3::new(
            (s.x - center.x) * scale + big as f64 / 2.0,
            big as f64 / 2.0
                - (s.y - center.y) * scale
                - if extras.bar { big as f64 * 0.05 } else { 0.0 },
            s.z,
        )
    };
    let mut depth = vec![f64::NEG_INFINITY; (big * big) as usize];
    for (t, color) in facets {
        let [a, b2, c] = t.map(to_px);
        let n = (t[1] - t[0]).cross(t[2] - t[0]);
        let Some(n) = n.try_normalize() else { continue };
        let n = if n.dot(dir) < 0.0 { -n } else { n };
        let shade = 0.35 + 0.65 * n.dot(light).abs();
        let rgb = [0, 1, 2].map(|k| to_srgb8(color[k] as f64 * shade));
        let alpha = (color[3].clamp(0.0, 1.0) * 255.0).round() as u8;
        let px = [rgb[0], rgb[1], rgb[2], alpha.max(1)];
        let area = edge(a, b2, c);
        if area.abs() < 1e-12 {
            continue;
        }
        let lo_x = a.x.min(b2.x).min(c.x).floor().max(0.0) as i64;
        let hi_x = a.x.max(b2.x).max(c.x).ceil().min(big as f64 - 1.0) as i64;
        let lo_y = a.y.min(b2.y).min(c.y).floor().max(0.0) as i64;
        let hi_y = a.y.max(b2.y).max(c.y).ceil().min(big as f64 - 1.0) as i64;
        for y in lo_y..=hi_y {
            for x in lo_x..=hi_x {
                let p = DVec3::new(x as f64 + 0.5, y as f64 + 0.5, 0.0);
                let (w0, w1, w2) = (edge(b2, c, p), edge(c, a, p), edge(a, b2, p));
                let inside = if area > 0.0 {
                    w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0
                } else {
                    w0 <= 0.0 && w1 <= 0.0 && w2 <= 0.0
                };
                if !inside {
                    continue;
                }
                let z = (w0 * a.z + w1 * b2.z + w2 * c.z) / area;
                let i = (y as u32 * big + x as u32) as usize;
                if z > depth[i] {
                    depth[i] = z;
                    pic.put(x, y, px);
                }
            }
        }
    }
    if extras.axes {
        let len = b.size().x.max(b.size().y).max(1e-9) * 0.35;
        let o = to_px(DVec3::ZERO);
        for (axis, color) in [
            (DVec3::X, [220, 60, 50, 255]),
            (DVec3::Y, [60, 170, 70, 255]),
            (DVec3::Z, [60, 110, 230, 255]),
        ] {
            line(&mut pic, o, to_px(axis * len), color, 3);
        }
    }
    let mut bar_m = None;
    if extras.bar {
        let fit = |m: f64| (m * scale >= 12.0) && (m * scale <= big as f64 * 0.8);
        let m = [1.0, 10.0, 0.1, 100.0, 0.01, 1000.0]
            .into_iter()
            .find(|m| fit(*m));
        if let Some(m) = m {
            let y = big as f64 - big as f64 * 0.04;
            let x0 = big as f64 * 0.06;
            let x1 = x0 + m * scale;
            let ink = [90, 90, 90, 255];
            line(
                &mut pic,
                DVec3::new(x0, y, 0.0),
                DVec3::new(x1, y, 0.0),
                ink,
                3,
            );
            for x in [x0, x1] {
                line(
                    &mut pic,
                    DVec3::new(x, y - 8.0, 0.0),
                    DVec3::new(x, y + 8.0, 0.0),
                    ink,
                    3,
                );
            }
            bar_m = Some(m);
        }
    }
    Drawn {
        picture: pic.half(),
        bar_m,
    }
}

/// A model's thumbnail as PNG bytes, from its file and manifest.
pub fn thumbnail(data: &GltfData, manifest: &VisualAssetManifest) -> Vec<u8> {
    draw(
        &body_facets(data, manifest),
        THUMB_PX,
        View::THUMB,
        Extras::default(),
    )
    .picture
    .png()
}

fn edge(a: DVec3, b: DVec3, p: DVec3) -> f64 {
    (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x)
}

fn line(pic: &mut Picture, a: DVec3, b: DVec3, color: [u8; 4], width: i64) {
    let steps = (b.x - a.x).abs().max((b.y - a.y).abs()).ceil().max(1.0) as i64;
    for k in 0..=steps {
        let t = k as f64 / steps as f64;
        let x = (a.x + (b.x - a.x) * t).round() as i64;
        let y = (a.y + (b.y - a.y) * t).round() as i64;
        for dy in -(width / 2)..=(width / 2) {
            for dx in -(width / 2)..=(width / 2) {
                pic.put(x + dx, y + dy, color);
            }
        }
    }
}

/// Linear 0–1 to 8-bit sRGB.
fn to_srgb8(c: f64) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round() as u8
}
