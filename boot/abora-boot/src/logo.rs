//! The logo, as produced by `scripts/logo-to-raw.py`: magic `ABR1`, width and
//! height as little-endian u32, then RGBA8 pixels.

use std::fs;
use std::path::Path;

pub struct Logo {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
}

impl Logo {
    pub fn load(path: &Path) -> Result<Self, String> {
        let data = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if data.len() < 12 || &data[..4] != b"ABR1" {
            return Err("not an ABR1 logo file".into());
        }
        let width = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
        let height = u32::from_le_bytes(data[8..12].try_into().unwrap()) as usize;
        if width == 0 || height == 0 || data.len() != 12 + width * height * 4 {
            return Err("logo size does not match its header".into());
        }
        Ok(Self { width, height, rgba: data[12..].to_vec() })
    }

    fn texel(&self, x: usize, y: usize) -> [u32; 4] {
        let i = (y.min(self.height - 1) * self.width + x.min(self.width - 1)) * 4;
        let a = self.rgba[i + 3] as u32;
        // Premultiply so transparent pixels do not bleed their colour.
        [self.rgba[i] as u32 * a, self.rgba[i + 1] as u32 * a, self.rgba[i + 2] as u32 * a, a * 255]
    }

    /// Colour and alpha at destination pixel (`dx`, `dy`) of a `dw`x`dh` box (bilinear).
    pub fn sample(&self, dx: usize, dy: usize, dw: usize, dh: usize) -> (u8, u8, u8, u8) {
        // Map the centre of the destination pixel into source space, in 1/256 units.
        let sx = ((2 * dx + 1) * self.width * 128 / dw) as i64 - 128;
        let sy = ((2 * dy + 1) * self.height * 128 / dh) as i64 - 128;
        let (sx, sy) = (sx.max(0) as usize, sy.max(0) as usize);
        let (x0, y0, fx, fy) = (sx / 256, sy / 256, (sx % 256) as u32, (sy % 256) as u32);
        let (t00, t10, t01, t11) = (self.texel(x0, y0), self.texel(x0 + 1, y0), self.texel(x0, y0 + 1), self.texel(x0 + 1, y0 + 1));
        let mut out = [0u64; 4];
        for c in 0..4 {
            let top = t00[c] as u64 * (256 - fx) as u64 + t10[c] as u64 * fx as u64;
            let bottom = t01[c] as u64 * (256 - fy) as u64 + t11[c] as u64 * fy as u64;
            out[c] = (top * (256 - fy) as u64 + bottom * fy as u64) >> 16;
        }
        // out[0..3] are colour*alpha and out[3] is alpha*255, so divide by alpha to get straight colour.
        let alpha = out[3] / 255;
        if alpha == 0 {
            return (0, 0, 0, 0);
        }
        let un = |v: u64| (v / alpha).min(255) as u8;
        (un(out[0]), un(out[1]), un(out[2]), alpha.min(255) as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn logo(px: &[[u8; 4]], w: usize, h: usize) -> Logo {
        Logo { width: w, height: h, rgba: px.iter().flatten().copied().collect() }
    }

    #[test]
    fn solid_pixels_keep_their_colour_and_alpha_when_scaled() {
        let l = logo(&[[200, 100, 50, 255]; 4], 2, 2);
        assert_eq!(l.sample(3, 3, 8, 8), (200, 100, 50, 255));
    }

    #[test]
    fn transparent_pixels_stay_transparent_and_do_not_bleed_colour() {
        let l = logo(&[[255, 0, 0, 0], [0, 255, 0, 255], [255, 0, 0, 0], [0, 255, 0, 255]], 2, 2);
        let (r, _, _, a) = l.sample(2, 0, 4, 4);
        assert!(a > 0 && r == 0, "edge sample must be green only, got r={r} a={a}");
        assert_eq!(l.sample(0, 0, 4, 4).3, 0);
    }
}
