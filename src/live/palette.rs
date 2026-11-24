//! Picks the most vivid colors of an album cover.

use gpui::Rgba;
use image::{GenericImageView, imageops::FilterType};

const HUE_BINS: usize = 24;
const FALLBACK: Rgba = Rgba { r: 1., g: 0.435, b: 0.302, a: 1. };

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub accent: Rgba,
    pub second: Rgba,
}

impl Default for Palette {
    fn default() -> Self {
        Self { accent: FALLBACK, second: Rgba { r: 0.36, g: 0.42, b: 1., a: 1. } }
    }
}

#[derive(Clone, Copy, Default)]
struct Bin {
    weight: f32,
    r: f32,
    g: f32,
    b: f32,
}

fn saturation_value(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let delta = max - min;
    let saturation = if max > 0. { delta / max } else { 0. };
    let hue = if delta <= f32::EPSILON {
        0.
    } else if max == r {
        ((g - b) / delta).rem_euclid(6.)
    } else if max == g {
        (b - r) / delta + 2.
    } else {
        (r - g) / delta + 4.
    } / 6.;
    (hue, saturation, max)
}

/// Lifts dark or muted cover colors so they read as light on a black island.
fn brighten(bin: &Bin) -> Rgba {
    let (r, g, b) = (bin.r / bin.weight, bin.g / bin.weight, bin.b / bin.weight);
    let max = r.max(g).max(b).max(0.01);
    let gain = 0.95 / max;
    Rgba { r: (r * gain).min(1.), g: (g * gain).min(1.), b: (b * gain).min(1.), a: 1. }
}

pub fn from_pixels(pixels: impl Iterator<Item = [u8; 3]>) -> Palette {
    let mut bins = [Bin::default(); HUE_BINS];
    for [r, g, b] in pixels {
        let (r, g, b) = (r as f32 / 255., g as f32 / 255., b as f32 / 255.);
        let (hue, saturation, value) = saturation_value(r, g, b);
        if saturation < 0.22 || value < 0.16 {
            continue;
        }
        let weight = saturation * saturation * value;
        let bin = &mut bins[((hue * HUE_BINS as f32) as usize).min(HUE_BINS - 1)];
        bin.weight += weight;
        bin.r += r * weight;
        bin.g += g * weight;
        bin.b += b * weight;
    }
    let Some(first) = (0..HUE_BINS).max_by(|a, b| bins[*a].weight.total_cmp(&bins[*b].weight)) else {
        return Palette::default();
    };
    if bins[first].weight <= 0.5 {
        return Palette::default();
    }
    let distance = |index: usize| {
        let gap = index.abs_diff(first);
        gap.min(HUE_BINS - gap)
    };
    let second = (0..HUE_BINS)
        .filter(|index| distance(*index) >= 3 && bins[*index].weight > 0.)
        .max_by(|a, b| bins[*a].weight.total_cmp(&bins[*b].weight));
    let accent = brighten(&bins[first]);
    let second = second.map_or(Rgba { r: accent.b, g: accent.r, b: accent.g, a: 1. }, |index| brighten(&bins[index]));
    Palette { accent, second }
}

/// The cover's palette and a square RGBA copy for the particle wallpaper.
pub fn from_image(bytes: &[u8], side: u32) -> Option<(Palette, Vec<u8>)> {
    let decoded = image::load_from_memory(bytes).ok()?;
    let small = decoded.resize_exact(40, 40, FilterType::Triangle);
    let palette = from_pixels(small.pixels().map(|(_, _, pixel)| [pixel.0[0], pixel.0[1], pixel.0[2]]));
    let (width, height) = decoded.dimensions();
    let crop = width.min(height);
    let square = decoded.crop_imm((width - crop) / 2, (height - crop) / 2, crop, crop);
    let rgba = square.resize_exact(side, side, FilterType::Triangle).to_rgba8().into_raw();
    Some((palette, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_vivid_color_over_gray() {
        let mut pixels = vec![[120, 120, 120]; 300];
        pixels.extend(vec![[220, 30, 40]; 100]);
        pixels.extend(vec![[30, 60, 220]; 40]);
        let palette = from_pixels(pixels.into_iter());
        assert!(palette.accent.r > 0.8 && palette.accent.b < 0.4, "{:?}", palette.accent);
        assert!(palette.second.b > 0.8, "{:?}", palette.second);
    }

    #[test]
    fn gray_cover_falls_back() {
        let palette = from_pixels(vec![[90, 90, 90]; 200].into_iter());
        assert_eq!(palette, Palette::default());
    }
}
