//! Original abstract wallpapers, generated in code so the launcher ships
//! real default backgrounds without bundling any copyrighted image. Each
//! is rendered once into the cache and reused; the picker under
//! Settings → Appearance → Default background cycles them.

use std::path::PathBuf;

use image::{ImageBuffer, Rgba};

/// Bump when a generator changes so old cached PNGs are ignored.
const VERSION: u32 = 1;
const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;

pub struct Wallpaper {
    pub name: String,
    pub path: PathBuf,
}

type Generator = fn(f32, f32) -> [u8; 3];

/// Name, filename slug, and pixel generator (u,v in 0..1 → RGB) for each
/// shipped wallpaper, in display order.
fn catalog() -> [(&'static str, &'static str, Generator); 6] {
    [
        ("Aurora", "aurora", aurora),
        ("Nebula", "nebula", nebula),
        ("Mesh", "mesh", mesh),
        ("Dusk", "dusk", dusk),
        ("Tide", "tide", tide),
        ("Vanishing", "vanishing", vanishing),
    ]
}

fn backgrounds_dir() -> Option<PathBuf> {
    let dir = crate::config::paths()?.cache.join("backgrounds");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn file_name(slug: &str) -> String {
    format!("{slug}-v{VERSION}.png")
}

/// Generate any missing wallpapers and return the full list. Idempotent:
/// an existing file for this version is left untouched.
pub fn ensure_defaults() -> Vec<Wallpaper> {
    let Some(dir) = backgrounds_dir() else {
        return Vec::new();
    };
    let mut wallpapers = Vec::new();
    for (name, slug, generator) in catalog() {
        let path = dir.join(file_name(slug));
        if !path.is_file() {
            render(generator, &path);
        }
        if path.is_file() {
            wallpapers.push(Wallpaper {
                name: name.to_owned(),
                path,
            });
        }
    }
    wallpapers
}

/// The wallpaper list without rendering (for the settings subtitle); the
/// paths may not exist yet on the very first frame.
pub fn list() -> Vec<Wallpaper> {
    let Some(dir) = backgrounds_dir() else {
        return Vec::new();
    };
    catalog()
        .into_iter()
        .map(|(name, slug, _)| Wallpaper {
            name: name.to_owned(),
            path: dir.join(file_name(slug)),
        })
        .collect()
}

fn render(generator: Generator, path: &std::path::Path) {
    let image = ImageBuffer::from_fn(WIDTH, HEIGHT, |x, y| {
        let u = x as f32 / (WIDTH - 1) as f32;
        let v = y as f32 / (HEIGHT - 1) as f32;
        let [r, g, b] = generator(u, v);
        Rgba([r, g, b, 255])
    });
    let _ = image.save(path);
}

// ---- helpers ---------------------------------------------------------

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        lerp(a[0], b[0], t),
        lerp(a[1], b[1], t),
        lerp(a[2], b[2], t),
    ]
}

fn rgb(color: [f32; 3]) -> [u8; 3] {
    [
        (color[0].clamp(0.0, 1.0) * 255.0) as u8,
        (color[1].clamp(0.0, 1.0) * 255.0) as u8,
        (color[2].clamp(0.0, 1.0) * 255.0) as u8,
    ]
}

/// Cheap deterministic value noise in [0,1) from a 2D point.
fn hash2(x: f32, y: f32) -> f32 {
    ((x * 127.1 + y * 311.7).sin() * 43758.547).fract().abs()
}

fn value_noise(x: f32, y: f32) -> f32 {
    let (xi, yi) = (x.floor(), y.floor());
    let (xf, yf) = (x - xi, y - yi);
    let tl = hash2(xi, yi);
    let tr = hash2(xi + 1.0, yi);
    let bl = hash2(xi, yi + 1.0);
    let br = hash2(xi + 1.0, yi + 1.0);
    let sx = xf * xf * (3.0 - 2.0 * xf);
    let sy = yf * yf * (3.0 - 2.0 * yf);
    lerp(lerp(tl, tr, sx), lerp(bl, br, sx), sy)
}

fn fbm(x: f32, y: f32) -> f32 {
    let mut sum = 0.0;
    let mut amp = 0.5;
    let mut freq = 1.0;
    for _ in 0..4 {
        sum += value_noise(x * freq, y * freq) * amp;
        freq *= 2.0;
        amp *= 0.5;
    }
    sum
}

// ---- generators ------------------------------------------------------

fn aurora(u: f32, v: f32) -> [u8; 3] {
    let top = [0.03, 0.05, 0.12];
    let bottom = [0.01, 0.02, 0.05];
    let mut color = mix(top, bottom, v);
    let green = [0.20, 0.85, 0.55];
    let violet = [0.55, 0.35, 0.90];
    // Two sine-warped curtains of light in the upper half.
    for (band, hue, phase) in [(0.34_f32, green, 0.0_f32), (0.5, violet, 2.1)] {
        let wobble = (u * 7.0 + phase).sin() * 0.05 + (u * 17.0).sin() * 0.015;
        let center = band + wobble;
        let dist = ((v - center) / 0.14).abs();
        let glow = (1.0 - dist).max(0.0).powf(2.2);
        let fade = (1.0 - v * 0.6).max(0.0);
        color = mix(color, hue, glow * 0.8 * fade);
    }
    rgb(color)
}

fn nebula(u: f32, v: f32) -> [u8; 3] {
    let cx = u - 0.5;
    let cy = v - 0.5;
    let radial = (1.0 - (cx * cx + cy * cy).sqrt() * 1.3).max(0.0);
    let base = mix([0.02, 0.01, 0.06], [0.10, 0.05, 0.20], radial);
    let clouds = fbm(u * 4.0, v * 4.0);
    let tint = [0.45, 0.20, 0.55];
    let mut color = mix(base, tint, clouds * radial * 0.9);
    // A few brighter cores.
    let hot = (fbm(u * 8.0 + 3.0, v * 8.0) - 0.55).max(0.0) * 2.0;
    color = mix(color, [0.85, 0.55, 0.75], hot * radial);
    rgb(color)
}

fn mesh(u: f32, v: f32) -> [u8; 3] {
    // Bilinear blend of four corner colors.
    let tl = [0.11, 0.16, 0.34];
    let tr = [0.35, 0.14, 0.32];
    let bl = [0.06, 0.09, 0.18];
    let br = [0.10, 0.22, 0.28];
    let top = mix(tl, tr, u);
    let bottom = mix(bl, br, u);
    rgb(mix(top, bottom, v))
}

fn dusk(u: f32, v: f32) -> [u8; 3] {
    // Diagonal gradient with fine grain.
    let d = (u * 0.4 + v * 0.6).clamp(0.0, 1.0);
    let base = mix([0.40, 0.22, 0.16], [0.06, 0.05, 0.12], d);
    let sun = (1.0 - ((u - 0.72).powi(2) + (v - 0.28).powi(2)).sqrt() * 2.2).max(0.0);
    let mut color = mix(base, [0.95, 0.72, 0.45], sun.powf(2.0) * 0.7);
    let grain = (hash2(u * 1920.0, v * 1080.0) - 0.5) * 0.03;
    color = [color[0] + grain, color[1] + grain, color[2] + grain];
    rgb(color)
}

fn tide(u: f32, v: f32) -> [u8; 3] {
    // Static wave-ribbon field over a gradient, echoing the live waves.
    let base = mix([0.05, 0.13, 0.22], [0.02, 0.05, 0.11], v);
    let mut color = base;
    let accent = [0.24, 0.66, 0.92];
    for (anchor, amp, cycles, phase) in [
        (0.62_f32, 0.05_f32, 1.2_f32, 0.0_f32),
        (0.72, 0.038, 1.7, 2.0),
    ] {
        let crest = anchor + (u * std::f32::consts::TAU * cycles + phase).sin() * amp;
        if v > crest {
            let depth = ((v - crest) / (1.0 - crest)).clamp(0.0, 1.0);
            color = mix(color, accent, (1.0 - depth) * 0.28);
        }
        let line = (1.0 - ((v - crest) / 0.006).abs()).max(0.0);
        color = mix(color, [0.7, 0.9, 1.0], line * 0.5);
    }
    rgb(color)
}

fn vanishing(u: f32, v: f32) -> [u8; 3] {
    let base = mix([0.08, 0.10, 0.20], [0.02, 0.02, 0.06], v);
    let mut color = base;
    let horizon = 0.62;
    let accent = [0.35, 0.75, 0.95];
    if v > horizon {
        let t = (v - horizon) / (1.0 - horizon);
        // Perspective rungs: exponential spacing toward the horizon.
        let rung = ((t * t * 16.0).fract() - 0.5).abs();
        let line_h = (1.0 - rung / 0.04).max(0.0);
        // Rails converging on the centre.
        let cx = (u - 0.5) / (0.05 + t * 0.9);
        let rail = ((cx * 8.0).fract() - 0.5).abs();
        let line_v = (1.0 - rail / 0.06).max(0.0);
        let grid = line_h.max(line_v) * t;
        color = mix(color, accent, grid * 0.6);
    } else {
        // A soft glow sitting on the horizon.
        let glow = (1.0 - (horizon - v) / horizon).max(0.0).powf(3.0);
        color = mix(color, accent, glow * 0.18);
    }
    rgb(color)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_names_and_files_are_unique_and_stable() {
        let entries = catalog();
        let names: Vec<_> = entries.iter().map(|(name, ..)| *name).collect();
        let files: Vec<_> = entries.iter().map(|(_, slug, _)| file_name(slug)).collect();
        for i in 0..entries.len() {
            for j in (i + 1)..entries.len() {
                assert_ne!(names[i], names[j]);
                assert_ne!(files[i], files[j]);
            }
        }
        assert!(
            files
                .iter()
                .all(|f| f.ends_with(&format!("-v{VERSION}.png")))
        );
    }

    #[test]
    fn generators_stay_in_gamut_across_the_frame() {
        for (_, _, generator) in catalog() {
            for &(u, v) in &[(0.0, 0.0), (0.5, 0.5), (1.0, 1.0), (0.72, 0.28), (0.1, 0.9)] {
                let _ = generator(u, v); // clamped in rgb(); must not panic
            }
        }
    }
}
