//! Boot screen for the Abora Framework live ISO.
//!
//! Runs from the initramfs in the background. It shows the boot log as plain
//! text on the framebuffer, with the Abora logo on the right. The log has two
//! sources:
//!
//! * the kernel ring buffer (`/dev/kmsg`), and
//! * lines written to a FIFO by the init script (`echo "text" > /run/boot.fifo`).
//!
//! Init lines are also echoed to stdout so the serial console shows the same log.
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

const BACKGROUND: Rgb = Rgb(0, 0, 0);
const TEXT: Rgb = Rgb(0xD0, 0xD0, 0xD0);
const MAX_LINES: usize = 500;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Kernel,
    Init,
}

enum Event {
    Line(Source, String),
}

struct Args {
    fifo: PathBuf,
    logo: PathBuf,
    font: PathBuf,
    fb: String,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        fifo: "/run/boot.fifo".into(),
        logo: "/usr/share/abora/logo.rgba".into(),
        font: "/usr/share/abora/font.psf".into(),
        fb: "fb0".into(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match flag.as_str() {
            "--fifo" => args.fifo = value("--fifo")?.into(),
            "--logo" => args.logo = value("--logo")?.into(),
            "--font" => args.font = value("--font")?.into(),
            "--fb" => args.fb = value("--fb")?,
            "--help" | "-h" => {
                println!("usage: abora-boot [--fifo PATH] [--logo FILE] [--font FILE] [--fb fb0]");
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
            if tx.send(Event::Line(Source::Init, line)).is_err() {
                return;
            }
        }
    });
}

struct Screen {
    canvas: Canvas,
    font: Font,
    /// Left edge, top edge, width and height of the text area (the logo sits to its right).
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    lines: VecDeque<String>,
}

impl Screen {
    fn new(mut canvas: Canvas, font: Font, logo: Option<logo::Logo>) -> Self {
        let (w, h) = (canvas.width(), canvas.height());
        canvas.fill_rect(0, 0, w, h, BACKGROUND);
        let margin = 16;
        let mut text_w = w - 2 * margin;
        if let Some(logo) = &logo {
            let size = (h / 3).clamp(96, 320).min(w / 3);
            canvas.blit_scaled(logo, w - margin - size, margin, size, size);
            text_w = w - 3 * margin - size;
        }
        canvas.flush_all();
        Self { canvas, font, x: margin, y: margin, w: text_w, h: h - 2 * margin, lines: VecDeque::new() }
    }

    fn push(&mut self, text: &str) {
        let cols = (self.w / self.font.width()).max(8);
        let chars: Vec<char> = text.chars().map(|c| if c == '\t' { ' ' } else { c }).collect();
        if chars.is_empty() {
            self.lines.push_back(String::new());
        }
        for (i, chunk) in chars.chunks(cols).enumerate() {
            let s: String = chunk.iter().collect();
            self.lines.push_back(if i == 0 { s } else { format!("  {s}") });
        }
        while self.lines.len() > MAX_LINES {
            self.lines.pop_front();
        }
    }

    /// Redraw the text area (only the last lines that fit) and send it to the screen.
    fn draw(&mut self) {
        let ch = self.font.height();
        self.canvas.fill_rect(self.x, self.y, self.w, self.h, BACKGROUND);
        let rows = self.h / ch;
        let skip = self.lines.len().saturating_sub(rows);
        for (row, text) in self.lines.iter().skip(skip).enumerate() {
            self.canvas.text(&self.font, 1, self.x, self.y + row * ch, text, TEXT);
        }
        self.canvas.flush_rows(self.y, self.y + self.h);
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let font = Font::load(&args.font).map_err(|e| format!("font {}: {e}", args.font.display()))?;

    let (tx, rx) = channel();
    spawn_fifo(args.fifo.clone(), tx.clone());
    spawn_kmsg(tx);

    // Without a framebuffer (for example `-nographic`) keep echoing to the
    // serial console so its log is still complete.
    let mut screen = match Canvas::open(&args.fb) {
        Ok(canvas) => {
            let logo = logo::Logo::load(&args.logo).map_err(|e| eprintln!("abora-boot: logo: {e}")).ok();
            Some(Screen::new(canvas, font, logo))
        }
        Err(e) => {
            eprintln!("abora-boot: no framebuffer ({e}); logging to the console only");
            None
        }
    };

    loop {
        let first = match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(event) => event,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        };
        // Batch whatever else is already waiting so a burst redraws once.
        for Event::Line(source, text) in std::iter::once(first).chain(rx.try_iter().take(500)) {
            // The kernel already prints its own log on the serial console.
            if source == Source::Init {
                println!("{text}");
            }
            if let Some(screen) = screen.as_mut() {
                screen.push(&text);
            }
        }
        if let Some(screen) = screen.as_mut() {
            screen.draw();
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
}
