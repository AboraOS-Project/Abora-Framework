//! Structured logging for Abora Framework services.
//!
//! Design goals (see `docs/security.md` and `docs/architecture.md`):
//!
//! * **Structured**: logs are emitted either as one JSON object per line
//!   (default, `systemd`/`journald` friendly) or as readable single-line
//!   text records.
//! * **Severities**: `off/error/warn/info/debug/trace`, mapped to
//!   journald priorities when running under systemd.
//! * **No secret leakage**: nothing is logged automatically; use
//!   [`Redacted`] when a value must be recorded but never printed.
//! * **Component/module names**: every record carries a `component`, e.g.
//!   `aborad`, `update::provider` — the future Cloud log viewer will filter
//!   on this field.
//!
//! The logger is deliberately dependency-light: no `tracing`/`log` facade,
//! no async machinery, so it can be used by every framework binary and test.

#![forbid(unsafe_code)]

// Allows `abora_log::...` paths inside this crate's own tests/macros.
extern crate self as abora_log;

use std::collections::BTreeMap;
use std::fmt;
use std::io::Write;
use std::str::FromStr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

/// Log severity threshold. Strongly ordered: a record is emitted when its
/// level is "at or below" the configured threshold (i.e. `info` emits
/// `info`/`warn`/`error`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Off,
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl Default for Level {
    fn default() -> Self {
        Level::Info
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Level::Off => "off",
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Info => "info",
            Level::Debug => "debug",
            Level::Trace => "trace",
        })
    }
}

impl FromStr for Level {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "off" => Ok(Level::Off),
            "error" => Ok(Level::Error),
            "warn" => Ok(Level::Warn),
            "info" => Ok(Level::Info),
            "debug" => Ok(Level::Debug),
            "trace" => Ok(Level::Trace),
            other => Err(format!(
                "unknown log level `{other}` (expected one of: off, error, warn, info, debug, trace)"
            )),
        }
    }
}

/// Record output encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// One JSON object per line. Best for systemd/journald and the future
    /// Cloud log viewer.
    Json,
    /// Human friendly single-line records, for interactive development.
    Text,
}

impl Default for Format {
    fn default() -> Self {
        Format::Json
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Format::Json => "json",
            Format::Text => "text",
        })
    }
}

/// Wrapper that always logs as `<redacted>`, regardless of the wrapped value.
///
/// ```rust
/// # use abora_log::Redacted;
/// let token = "super-secret-token";
/// let shown = Redacted(token).to_string();
/// assert_eq!(shown, "<redacted>");
/// ```
pub struct Redacted<T>(pub T);

impl<T: fmt::Display> fmt::Display for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl<T> serde::Serialize for Redacted<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str("<redacted>")
    }
}

struct LoggerInner {
    level: Level,
    format: Format,
    writer: Mutex<Box<dyn Write + Send>>,
}

/// A structured logger. Cheap to clone and share across threads.
#[derive(Clone)]
pub struct Logger(Arc<LoggerInner>);

impl Logger {
    /// Start building a logger. Defaults: `info`, JSON, stderr.
    pub fn builder() -> LoggerBuilder {
        LoggerBuilder::default()
    }

    /// Configure from a framework config [`Level`]/[`Format`].
    pub fn new(level: Level, format: Format) -> Self {
        Self::builder().level(level).format(format).build()
    }

    /// The configured threshold.
    pub fn level(&self) -> Level {
        self.0.level
    }

    /// Whether a record at `level` would be emitted.
    pub fn enabled(&self, level: Level) -> bool {
        self.0.level != Level::Off && level != Level::Off && level <= self.0.level
    }

    /// Start a record at `level`, then add fields and `.emit(...)` a message.
    pub fn record(&self, level: Level) -> RecordBuilder<'_> {
        RecordBuilder::new(self, level)
    }

    /// Convenience for error records with no extra fields.
    pub fn error(&self, msg: impl fmt::Display) {
        self.record(Level::Error).emit(format_args!("{msg}"));
    }

    /// Convenience for warn records with no extra fields.
    pub fn warn(&self, msg: impl fmt::Display) {
        self.record(Level::Warn).emit(format_args!("{msg}"));
    }

    /// Convenience for info records with no extra fields.
    pub fn info(&self, msg: impl fmt::Display) {
        self.record(Level::Info).emit(format_args!("{msg}"));
    }

    /// Convenience for debug records with no extra fields.
    pub fn debug(&self, msg: impl fmt::Display) {
        self.record(Level::Debug).emit(format_args!("{msg}"));
    }

    /// Convenience for trace records with no extra fields.
    pub fn trace(&self, msg: impl fmt::Display) {
        self.record(Level::Trace).emit(format_args!("{msg}"));
    }

    fn write_record(&self, level: Level, component: &str, fields: &BTreeMap<String, Value>, msg: fmt::Arguments<'_>) {
        let ts = utc_rfc3339_millis(now_since_epoch());
        let mut writer = self.0.writer.lock().expect("log writer poisoned");
        let result = match self.0.format {
            Format::Json => write_json(&mut *writer, &ts, level, component, fields, msg),
            Format::Text => write_text(&mut *writer, &ts, level, component, fields, msg),
        };
        match result {
            Ok(()) => {}
            Err(_) => {
                // Writing to the log sink failed; there is nowhere useful to
                // report it, so we silently drop the record. Use a direct
                // sink (file, journald, ...) in production, never /dev/null.
            }
        }
    }
}

fn write_json<W: Write>(
    w: &mut W,
    ts: &str,
    level: Level,
    component: &str,
    fields: &BTreeMap<String, Value>,
    msg: fmt::Arguments<'_>,
) -> std::io::Result<()> {
    let mut obj = json!({
        "timestamp": ts,
        "level": level,
        "component": component,
        "message": format!("{msg}"),
    });
    for (k, v) in fields {
        obj.as_object_mut().expect("object just built").insert(k.clone(), v.clone());
    }
    let mut buf = serde_json::to_string(&obj).unwrap_or_else(|_| "{}".into());
    buf.push('\n');
    w.write_all(buf.as_bytes())
}

fn write_text<W: Write>(
    w: &mut W,
    ts: &str,
    level: Level,
    component: &str,
    fields: &BTreeMap<String, Value>,
    msg: fmt::Arguments<'_>,
) -> std::io::Result<()> {
    let mut line = format!("{ts} {level:<5} [{component}] {msg}");
    for (k, v) in fields {
        line.push_str(&format!(" {k}={v}"));
    }
    line.push('\n');
    w.write_all(line.as_bytes())
}

/// Ongoing record construction. Message is [`fmt::Arguments`] so nothing is
/// rendered unless the record will actually be emitted.
pub struct RecordBuilder<'a> {
    logger: &'a Logger,
    level: Level,
    component: Option<String>,
    fields: BTreeMap<String, Value>,
}

impl<'a> RecordBuilder<'a> {
    fn new(logger: &'a Logger, level: Level) -> Self {
        Self {
            logger,
            level,
            component: None,
            fields: BTreeMap::new(),
        }
    }

    /// Set the component/module name (e.g. `aborad`, `update::provider`).
    pub fn component(mut self, component: impl Into<String>) -> Self {
        self.component = Some(component.into());
        self
    }

    /// Attach a structured field. Values are JSON-serialized; wrap secrets
    /// in [`Redacted`].
    pub fn field(mut self, key: impl Into<String>, value: impl serde::Serialize) -> Self {
        if let Ok(v) = serde_json::to_value(&value) {
            self.fields.insert(key.into(), v);
        }
        self
    }

    /// Emit the record with the given message. Formats lazily.
    pub fn emit(self, msg: fmt::Arguments<'_>) {
        if !self.logger.enabled(self.level) {
            return;
        }
        let component = self.component.as_deref().unwrap_or("framework");
        self.logger.write_record(self.level, component, &self.fields, msg);
    }

    /// Convenience: emit an owned string message.
    pub fn emit_string(self, msg: String) {
        self.emit(format_args!("{msg}"));
    }
}

/// Builder for [`Logger`].
#[derive(Default)]
pub struct LoggerBuilder {
    level: Level,
    format: Format,
    writer: Option<Box<dyn Write + Send>>,
}

impl LoggerBuilder {
    pub fn level(mut self, level: Level) -> Self {
        self.level = level;
        self
    }

    pub fn format(mut self, format: Format) -> Self {
        self.format = format;
        self
    }

    /// Override the sink (default: process stderr, which systemd/journald
    /// capture).
    pub fn writer(mut self, writer: Box<dyn Write + Send>) -> Self {
        self.writer = Some(writer);
        self
    }

    pub fn build(self) -> Logger {
        let boxed = self
            .writer
            .unwrap_or_else(|| Box::new(std::io::stderr()));
        Logger(Arc::new(LoggerInner {
            level: self.level,
            format: self.format,
            writer: Mutex::new(boxed),
        }))
    }
}

static GLOBAL: OnceLock<Logger> = OnceLock::new();

/// Install a global default logger for the current process.
pub fn set_global(logger: Logger) -> Result<(), Logger> {
    GLOBAL.set(logger)
}

/// Access the global default logger, if one was installed.
pub fn global() -> Option<&'static Logger> {
    GLOBAL.get()
}

/// Macro: emit an error record on the given logger.
#[macro_export]
macro_rules! error {
    ($logger:expr, $($arg:tt)*) => {{
        $logger.record($crate::Level::Error).emit(format_args!($($arg)*))
    }};
}

/// Macro: emit a warn record on the given logger.
#[macro_export]
macro_rules! warn {
    ($logger:expr, $($arg:tt)*) => {{
        $logger.record($crate::Level::Warn).emit(format_args!($($arg)*))
    }};
}

/// Macro: emit an info record on the given logger.
#[macro_export]
macro_rules! info {
    ($logger:expr, $($arg:tt)*) => {{
        $logger.record($crate::Level::Info).emit(format_args!($($arg)*))
    }};
}

/// Macro: emit a debug record on the given logger.
#[macro_export]
macro_rules! debug {
    ($logger:expr, $($arg:tt)*) => {{
        $logger.record($crate::Level::Debug).emit(format_args!($($arg)*))
    }};
}

/// Macro: emit a trace record on the given logger.
#[macro_export]
macro_rules! trace {
    ($logger:expr, $($arg:tt)*) => {{
        $logger.record($crate::Level::Trace).emit(format_args!($($arg)*))
    }};
}

fn now_since_epoch() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
}

/// Current wall clock as an RFC 3339 UTC timestamp with millisecond
/// precision, e.g. `2026-09-16T22:00:00.123Z`. Reused by services that need
/// a consistent timestamp format (health payloads, update records, ...).
pub fn rfc3339_now() -> String {
    utc_rfc3339_millis(now_since_epoch())
}

/// Format an epoch duration as an RFC 3339 UTC timestamp with millisecond
/// precision, e.g. `2026-09-16T22:00:00.123Z`. Implemented locally to keep
/// the logger dependency-free; covered by unit tests against known epochs.
pub fn utc_rfc3339_millis(epoch: Duration) -> String {
    let secs = epoch.as_secs() as i64;
    let millis = epoch.subsec_millis();
    let (days, day_secs) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    let (year, month, day) = civil_from_days(days);
    let (h, mi, s) = (
        day_secs / 3600,
        (day_secs % 3600) / 60,
        day_secs % 60,
    );
    format!(
        "{year:04}-{month:02}-{day:02}T{h:02}:{mi:02}:{s:02}.{millis:03}Z"
    )
}

/// Convert days since `1970-01-01` into a `(year, month, day)` UTC civil date.
/// Standard proleptic Gregorian algorithm (Howard Hinnant).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Owned, shareable in-memory log sink so the logger can outlive the test.
    #[derive(Clone)]
    struct TestSink(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for TestSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn capture(level: Level, format: Format, f: impl FnOnce(&Logger)) -> String {
        let sink = TestSink(Arc::new(Mutex::new(Vec::new())));
        let logger = Logger::builder()
            .level(level)
            .format(format)
            .writer(Box::new(sink.clone()))
            .build();
        f(&logger);
        let bytes = sink.0.lock().unwrap().clone();
        String::from_utf8(bytes).expect("log output must be valid UTF-8")
    }

    #[test]
    fn json_record_has_expected_shape() {
        let out = capture(Level::Info, Format::Json, |l| {
            l.record(Level::Info)
                .component("aborad")
                .field("event", "started")
                .field("port", 7360)
                .emit(format_args!("listening on {}", "127.0.0.1:7360"));
        });
        let v: Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(v["level"], "info");
        assert_eq!(v["component"], "aborad");
        assert_eq!(v["message"], "listening on 127.0.0.1:7360");
        assert_eq!(v["event"], "started");
        assert_eq!(v["port"], 7360);
        assert!(v["timestamp"].as_str().unwrap().ends_with('Z'));
    }

    #[test]
    fn level_filtering() {
        let out = capture(Level::Info, Format::Json, |l| {
            l.warn("kept");
            l.debug("dropped");
            abora_log::info!(l, "kept too");
        });
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "output was:\n{out}");
        for line in &lines {
            assert!(line.contains("\"level\":\"info\"") || line.contains("\"level\":\"warn\""));
        }
    }

    #[test]
    fn off_level_emits_nothing() {
        let out = capture(Level::Off, Format::Text, |l| {
            l.error("quiet");
        });
        assert!(out.is_empty());
    }

    #[test]
    fn text_format_is_human_readable() {
        let out = capture(Level::Info, Format::Text, |l| {
            l.record(Level::Info)
                .component("cli")
                .field("channel", "stable")
                .emit(format_args!("config ok"));
        });
        let line = out.trim();
        assert!(line.starts_with("20"), "got: {line}");
        assert!(line.contains(" [cli] config ok channel=\"stable\""), "got: {line}");
    }

    #[test]
    fn redacted_values_never_leak() {
        let secret = "hunter2-secret";
        let out = capture(Level::Info, Format::Json, |l| {
            l.record(Level::Info)
                .field("token", Redacted(secret))
                .emit(format_args!("auth ok"));
        });
        assert!(!out.contains(secret), "secret leaked into log: {out}");
        assert!(out.contains("<redacted>"), "got: {out}");
    }

    #[test]
    fn text_and_json_print_lowercase_levels() {
        for format in [Format::Json, Format::Text] {
            let out = capture(Level::Debug, format, |l| l.debug("x"));
            assert!(out.contains("debug"), "format {format}: {out}");
        }
    }

    #[test]
    fn known_epochs_format_correctly() {
        assert_eq!(
            utc_rfc3339_millis(Duration::from_secs(0)),
            "1970-01-01T00:00:00.000Z"
        );
        assert_eq!(
            utc_rfc3339_millis(Duration::from_secs(1_700_000_000)),
            "2023-11-14T22:13:20.000Z"
        );
        assert_eq!(
            utc_rfc3339_millis(Duration::from_millis(1_700_000_000_123)),
            "2023-11-14T22:13:20.123Z"
        );
        assert_eq!(
            utc_rfc3339_millis(Duration::from_millis(1)),
            "1970-01-01T00:00:00.001Z"
        );
    }

    #[test]
    fn nullable_field_and_integer_field_round_trip() {
        let out = capture(Level::Info, Format::Json, |l| {
            l.record(Level::Info).field("n", 7_i64).field("f", 2.5_f64).emit(format_args!("m"));
        });
        let v: Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(v["n"], 7);
        assert_eq!(v["f"], 2.5);
    }

    #[test]
    fn level_from_str_round_trips() {
        for lv in [Level::Off, Level::Error, Level::Warn, Level::Info, Level::Debug, Level::Trace] {
            assert_eq!(lv.to_string().parse::<Level>().unwrap(), lv);
        }
        assert!(Level::from_str("loud").is_err());
    }

    #[test]
    fn redacted_is_serialize_safe() {
        let a = json!({ "token": Redacted("abc") });
        assert_eq!(a["token"], "<redacted>");
    }
}