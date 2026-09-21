//! PSF1 bitmap font loader (8 pixels wide, `height` rows of one byte each).
//!
//! Only the first 256 glyphs are used, mapped straight from Latin-1; anything
//! else draws as `?`. The Unicode table at the end of the file is ignored.

use std::fs;
use std::path::Path;

pub struct Font {
    height: usize,
    glyphs: Vec<u8>,
    count: usize,
}

impl Font {
    pub fn load(path: &Path) -> Result<Self, String> {
        let data = fs::read(path).map_err(|e| e.to_string())?;
        if data.len() < 4 || data[0] != 0x36 || data[1] != 0x04 {
            return Err("not a PSF1 font".into());
        }
        let count = if data[2] & 1 != 0 { 512 } else { 256 };
        let height = data[3] as usize;
        let need = 4 + count * height;
        if height == 0 || data.len() < need {
            return Err("truncated PSF1 font".into());
        }
        Ok(Self { height, glyphs: data[4..need].to_vec(), count })
    }

    pub fn width(&self) -> usize {
        8
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn glyph(&self, c: char) -> &[u8] {
        let mut i = c as usize;
        if i >= 256.min(self.count) || (i < 32) {
            i = b'?' as usize;
        }
        &self.glyphs[i * self.height..(i + 1) * self.height]
    }
}
