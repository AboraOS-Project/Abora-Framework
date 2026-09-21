//! Boot screen for the Abora Framework live ISO.
//!
//! Runs from the initramfs as a background process. It paints the Abora logo on
//! the framebuffer and, underneath, a scrolling log fed by two sources:
//!
//! * the kernel ring buffer (`/dev/kmsg`), and
//! * lines written to a FIFO by the init script (`echo "text" > /run/boot.fifo`).
//!
//! Special FIFO lines: `@status <text>` sets the status line, `@done` marks boot
//! finished (the screen stays as it is). Everything is also echoed to stdout so
//! a serial console sees the same log.
//!
//! No `unsafe` and no dependencies: the framebuffer is described by sysfs and
//! written through the ordinary file API.

mod canvas;
mod font;
mod logo;

use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use canvas::{Canvas, Rgb};
use font::Font;

const BACKGROUND_TOP: Rgb = Rgb(0x0B, 0x0E, 0x1A);
const BACKGROUND_BOTTOM: Rgb = Rgb(0x16, 0x1B, 0x33);
const TITLE: Rgb = Rgb(0xF2, 0xF4, 0xFF);
const MUTED: Rgb = Rgb(0x8A, 0x92, 0xB8);
const KERNEL: Rgb = Rgb(0x7B, 0x84, 0xAE);
const INFO: Rgb = Rgb(0xC9, 0xCF, 0xEA);
const OK: Rgb = Rgb(0x5C, 0xD6, 0x8A);
const WARN: Rgb = Rgb(0xF2, 0xB8, 0x4B);
const FAIL: Rgb = Rgb(0xF2, 0x6B, 0x6B);
const ACCENT: Rgb = Rgb(0x6C, 0x7A, 0xE0);

const MAX_LINES: usize = 400;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Kernel,
    Init,
}

enum Event {
    Line(Source, String),
    Status(String),
    Done,
}

struct Args {
    fifo: PathBuf,
    logo: PathBuf,
    font: PathBuf,
    fb: String,
    version: String,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        fifo: "/run/boot.fifo".into(),
        logo: "/usr/share/abora/logo.rgba".into(),
        font: "/usr/share/abora/font.psf".into(),
        fb: "fb0".into(),
        version: String::new(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match flag.as_str() {
            "--fifo" => args.fifo = value("--fifo")?.into(),
            "--logo" => args.logo = value("--logo")?.into(),
            "--font" => args.font = value("--font")?.into(),
            "--fb" => args.fb = value("--fb")?,
            "--version-text" => args.version = value("--version-text")?,
            "--help" | "-h" => {
                println!("usage: abora-boot [--fifo PATH] [--logo FILE] [--font FILE] [--fb fb0] [--version-text TEXT]");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    Ok(args)
}

/// Turn one `/dev/kmsg` record (`pri,seq,usec,flags;message`) into `[ 1.234] message`.
fn parse_kmsg(record: &str) -> Option<String> {
    let first = record.lines().next()?;
    let (meta, message) = first.split_once(';')?;
    let usec: u64 = meta.split(',').nth(2)?.parse().ok()?;
    Some(format!("[{:>5}.{:03}] {}", usec / 1_000_000, (usec / 1000) % 1000, message.trim_end()))
}

fn spawn_kmsg(tx: Sender<Event>) {
    thread::spawn(move || {
        let Ok(mut file) = OpenOptions::new().read(true).open("/dev/kmsg") else { return };
        // Each read() on /dev/kmsg returns exactly one record.
        let mut chunk = [0u8; 8192];
        loop {
            match file.read(&mut chunk) {
                Ok(0) => return,
                Ok(n) => {
                    if let Some(line) = parse_kmsg(&String::from_utf8_lossy(&chunk[..n])) {
                        if tx.send(Event::Line(Source::Kernel, line)).is_err() {
                            return;
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return, // EPIPE: the ring overtook us; stop rather than spin
            }
        }
    });
}

fn spawn_fifo(path: PathBuf, tx: Sender<Event>) {
    thread::spawn(move || {
        // Read+write keeps a writer open ourselves, so the FIFO never reports EOF
        // when init's short-lived `echo` writers come and go.
        let Ok(file) = OpenOptions::new().read(true).write(true).open(&path) else {
            eprintln!("abora-boot: cannot open {}", path.display());
            return;
        };
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            let event = if let Some(text) = line.strip_prefix("@status ") {
                Event::Status(text.to_owned())
            } else if line == "@done" {
                Event::Done
            } else {
                Event::Line(Source::Init, line)
            };
            if tx.send(event).is_err() {
                return;
            }
        }
    });
}

fn line_color(source: Source, text: &str) -> Rgb {
    if source == Source::Kernel {
        return KERNEL;
    }
    let t = text.trim_start();
    if t.starts_with("[ OK") || t.starts_with("[ok") {
        OK
    } else if t.starts_with("[WARN") {
        WARN
    } else if t.starts_with("[FAIL") {
        FAIL
    } else {
        INFO
    }
}

struct Screen {
    canvas: Canvas,
    font: Font,
    scale: usize,
    panel_x: usize,
    panel_y: usize,
    panel_w: usize,
    panel_h: usize,
    status_y: usize,
    lines: VecDeque<(Rgb, String)>,
    status: String,
    done: bool,
}

impl Screen {
    fn new(mut canvas: Canvas, font: Font, logo: Option<logo::Logo>, version: &str) -> Self {
        let (w, h) = (canvas.width(), canvas.height());
        canvas.vertical_gradient(BACKGROUND_TOP, BACKGROUND_BOTTOM);
        let scale = if h >= 900 { 2 } else { 1 };
        let (cw, ch) = (font.width() * scale, font.height() * scale);

        // Header: logo on the left, product name beside it.
        let logo_size = (h / 5).clamp(96, 200);
        let margin = (w / 24).max(16);
        let top = margin;
        if let Some(logo) = &logo {
            canvas.blit_scaled(logo, margin, top, logo_size, logo_size);
        }
        let text_x = margin + logo_size + margin / 2;
        let title_scale = scale * 3;
        canvas.text(&font, title_scale, text_x, top + logo_size / 2 - font.height() * title_scale, "Abora Framework", TITLE);
        let sub_y = top + logo_size / 2 + font.height() * scale / 2;
        canvas.text(&font, scale, text_x, sub_y, "The shared server foundation behind Abora Cloud and Abora Atlas", MUTED);
        if !version.is_empty() {
            canvas.text(&font, scale, text_x, sub_y + ch + 4, version, ACCENT);
        }

        let rule_y = top + logo_size + margin / 2;
        canvas.fill_rect(margin, rule_y, w - 2 * margin, 2, ACCENT);

        let status_h = ch + 8;
        let panel_y = rule_y + margin / 2 + 2;
        let panel_h = h.saturating_sub(panel_y + margin + status_h);
        let panel_w = w - 2 * margin;
        let status_y = panel_y + panel_h + 6;
        let _ = cw;
        let mut screen = Self {
            canvas,
            font,
            scale,
            panel_x: margin,
            panel_y,
            panel_w,
            panel_h,
            status_y,
            lines: VecDeque::new(),
            status: String::from("Starting"),
            done: false,
        };
        screen.canvas.flush_all();
        screen
    }

    fn rows(&self) -> usize {
        self.panel_h / (self.font.height() * self.scale)
    }

    fn cols(&self) -> usize {
        self.panel_w / (self.font.width() * self.scale)
    }

    fn push(&mut self, color: Rgb, text: &str) {
        let cols = self.cols().max(8);
        let text: String = text.chars().map(|c| if c == '\t' { ' ' } else { c }).collect();
        let mut chars: Vec<char> = text.chars().collect();
        if chars.is_empty() {
            chars.push(' ');
        }
        for (i, chunk) in chars.chunks(cols).enumerate() {
            let s: String = chunk.iter().collect();
            self.lines.push_back((color, if i == 0 { s } else { format!("  {s}") }));
        }
        while self.lines.len() > MAX_LINES {
            self.lines.pop_front();
        }
    }

    fn draw(&mut self) {
        let (cw, ch) = (self.font.width() * self.scale, self.font.height() * self.scale);
        // Panel background: a slightly lighter card.
        self.canvas.fill_rect(self.panel_x, self.panel_y, self.panel_w, self.panel_h, Rgb(0x0F, 0x13, 0x24));
        let rows = self.rows();
        let skip = self.lines.len().saturating_sub(rows);
        for (row, (color, text)) in self.lines.iter().skip(skip).enumerate() {
            let y = self.panel_y + row * ch;
            self.canvas.text(&self.font, self.scale, self.panel_x + cw / 2, y, text, *color);
        }
        // Status line.
        let bar = self.panel_w;
        self.canvas.fill_rect(self.panel_x, self.status_y, bar, ch + 4, Rgb(0x0B, 0x0E, 0x1A));
        let (label, color) = if self.done { ("READY", OK) } else { ("BOOTING", WARN) };
        self.canvas.text(&self.font, self.scale, self.panel_x + cw / 2, self.status_y + 2, label, color);
        let status = self.status.clone();
        self.canvas.text(&self.font, self.scale, self.panel_x + cw * 10, self.status_y + 2, &status, INFO);
        let top = self.panel_y;
        let bottom = self.status_y + ch + 4;
        self.canvas.flush_rows(top, bottom);
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let font = Font::load(&args.font).map_err(|e| format!("font {}: {e}", args.font.display()))?;

    // Without a framebuffer there is nothing to draw; keep echoing so the
    // serial log is still complete. (No framebuffer is normal with -nographic.)
    let canvas = Canvas::open(&args.fb);
    let (tx, rx) = channel();
    spawn_fifo(args.fifo.clone(), tx.clone());
    spawn_kmsg(tx);

    let mut screen = match canvas {
        Ok(c) => {
            let logo = logo::Logo::load(&args.logo).map_err(|e| eprintln!("abora-boot: logo: {e}")).ok();
            Some(Screen::new(c, font, logo, &args.version))
        }
        Err(e) => {
            eprintln!("abora-boot: no framebuffer ({e}); logging to the console only");
            None
        }
    };
    if let Some(s) = screen.as_mut() {
        s.draw();
    }

    loop {
        let mut dirty = false;
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(event) => {
                let mut pending = vec![event];
                pending.extend(rx.try_iter().take(500));
                for event in pending {
                    match event {
                        Event::Line(source, text) => {
                            // The kernel already prints its own log on the serial console.
                            if source == Source::Init {
                                println!("{text}");
                            }
                            if let Some(s) = screen.as_mut() {
                                s.push(line_color(source, &text), &text);
                                dirty = true;
                            }
                        }
                        Event::Status(text) => {
                            if let Some(s) = screen.as_mut() {
                                s.status = text;
                                dirty = true;
                            }
                        }
                        Event::Done => {
                            if let Some(s) = screen.as_mut() {
                                s.done = true;
                                dirty = true;
                            }
                        }
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
        if dirty {
            if let Some(s) = screen.as_mut() {
                s.draw();
            }
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("abora-boot: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kmsg_records_are_formatted_with_a_timestamp() {
        let rec = "6,339,5140900,-;NET: Registered PF_INET\n SUBSYSTEM=net\n";
        assert_eq!(parse_kmsg(rec).unwrap(), "[    5.140] NET: Registered PF_INET");
        assert!(parse_kmsg("garbage").is_none());
    }

    #[test]
    fn init_lines_are_coloured_by_their_tag() {
        assert_eq!(line_color(Source::Init, "[ OK ] mounted"), OK);
        assert_eq!(line_color(Source::Init, "[FAIL] nope"), FAIL);
        assert_eq!(line_color(Source::Kernel, "[ OK ] x"), KERNEL);
    }
}
