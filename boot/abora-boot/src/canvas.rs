//! A software canvas that mirrors a 32-bit linear framebuffer.
//!
//! Geometry comes from sysfs (`/sys/class/graphics/fbN/…`), pixels go out
//! through plain `write()` calls on `/dev/fbN`, so no `mmap` or `ioctl` (and no
//! `unsafe`) is needed. Little-endian XRGB8888 (bytes B, G, R, X) is what the
//! `simpledrm` and `efifb` drivers expose; anything else is refused.

use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};

use crate::font::Font;
use crate::logo::Logo;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rgb(pub u8, pub u8, pub u8);

pub struct Canvas {
    device: File,
    width: usize,
    height: usize,
    stride: usize,
    pixels: Vec<u8>,
}

impl Canvas {
    pub fn open(name: &str) -> Result<Self, String> {
        let sys = format!("/sys/class/graphics/{name}");
        let read = |file: &str| fs::read_to_string(format!("{sys}/{file}")).map_err(|e| format!("{sys}/{file}: {e}"));
        let size = read("virtual_size")?;
        let (w, h) = size.trim().split_once(',').ok_or("bad virtual_size")?;
        let width: usize = w.parse().map_err(|_| "bad width")?;
        let height: usize = h.parse().map_err(|_| "bad height")?;
        let stride: usize = read("stride")?.trim().parse().map_err(|_| "bad stride")?;
        let bpp: usize = read("bits_per_pixel")?.trim().parse().map_err(|_| "bad bits_per_pixel")?;
        if bpp != 32 {
            return Err(format!("{bpp} bits per pixel is not supported (need 32)"));
        }
        if width == 0 || height == 0 || stride < width * 4 {
            return Err("nonsensical framebuffer geometry".into());
        }
        let device = OpenOptions::new()
            .write(true)
            .open(format!("/dev/{name}"))
            .map_err(|e| format!("/dev/{name}: {e}"))?;
        Ok(Self { device, width, height, stride, pixels: vec![0; stride * height] })
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    fn put(&mut self, x: usize, y: usize, c: Rgb) {
        if x < self.width && y < self.height {
            let i = y * self.stride + x * 4;
            self.pixels[i] = c.2;
            self.pixels[i + 1] = c.1;
            self.pixels[i + 2] = c.0;
            self.pixels[i + 3] = 0xFF;
        }
    }

    fn get(&self, x: usize, y: usize) -> Rgb {
        let i = y * self.stride + x * 4;
        Rgb(self.pixels[i + 2], self.pixels[i + 1], self.pixels[i])
    }

    pub fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, c: Rgb) {
        for row in y..(y + h).min(self.height) {
            for col in x..(x + w).min(self.width) {
                self.put(col, row, c);
            }
        }
    }

    pub fn text(&mut self, font: &Font, scale: usize, x: usize, y: usize, text: &str, color: Rgb) {
        let cw = font.width() * scale;
        let mut px = x;
        for c in text.chars() {
            if px + cw > self.width {
                break;
            }
            let rows = font.glyph(c);
            for (gy, bits) in rows.iter().enumerate() {
                for gx in 0..font.width() {
                    if bits & (0x80 >> gx) != 0 {
                        self.fill_rect(px + gx * scale, y + gy * scale, scale, scale, color);
                    }
                }
            }
            px += cw;
        }
    }

    /// Draw `logo` scaled (bilinear) into a `w`x`h` box, alpha-blended over what is there.
    pub fn blit_scaled(&mut self, logo: &Logo, x: usize, y: usize, w: usize, h: usize) {
        for dy in 0..h {
            for dx in 0..w {
                let (r, g, b, a) = logo.sample(dx, dy, w, h);
                if a == 0 {
                    continue;
                }
                let under = if x + dx < self.width && y + dy < self.height { self.get(x + dx, y + dy) } else { continue };
                let blend = |f: u8, u: u8| ((f as u32 * a as u32 + u as u32 * (255 - a as u32)) / 255) as u8;
                self.put(x + dx, y + dy, Rgb(blend(r, under.0), blend(g, under.1), blend(b, under.2)));
            }
        }
    }

    pub fn flush_all(&mut self) {
        self.flush_rows(0, self.height);
    }

    /// Write rows `top..bottom` to the device.
    pub fn flush_rows(&mut self, top: usize, bottom: usize) {
        let bottom = bottom.min(self.height);
        if top >= bottom {
            return;
        }
        let range = top * self.stride..bottom * self.stride;
        if let Err(e) = self.device.seek(SeekFrom::Start(range.start as u64)).and_then(|_| self.device.write_all(&self.pixels[range])) {
            eprintln!("abora-boot: framebuffer write failed: {e}");
        }
    }
}
