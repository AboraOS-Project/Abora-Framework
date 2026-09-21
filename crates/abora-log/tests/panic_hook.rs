//! The panic hook is process-global, so this lives in its own test binary
//! where no other test can panic underneath it.

use std::io::Write;
use std::sync::{Arc, Mutex};

use abora_log::{install_panic_hook, Format, Level, Logger};

#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);

impl Write for Buffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn records(buffer: &Buffer) -> Vec<serde_json::Value> {
    let bytes = buffer.0.lock().unwrap().clone();
    String::from_utf8(bytes)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).expect("every line is JSON"))
        .collect()
}

#[test]
fn panics_become_structured_error_records() {
    let buffer = Buffer::default();
    let logger = Logger::builder()
        .level(Level::Info)
        .format(Format::Json)
        .writer(Box::new(buffer.clone()))
        .build();
    install_panic_hook(logger);

    // A panic with a &str payload on a named thread.
    let handle = std::thread::Builder::new()
        .name("worker-7".into())
        .spawn(|| panic!("disk on fire"))
        .unwrap();
    assert!(handle.join().is_err());

    // A formatted (String) payload on this thread, caught so the test survives.
    let n = 42;
    let _ = std::panic::catch_unwind(|| panic!("bad value {n}"));

    let recs = records(&buffer);
    assert_eq!(recs.len(), 2, "one record per panic: {recs:?}");

    assert_eq!(recs[0]["level"], "error");
    assert_eq!(recs[0]["component"], "panic");
    assert_eq!(recs[0]["message"], "panic: disk on fire");
    assert_eq!(recs[0]["thread"], "worker-7");
    assert!(recs[0]["location"]
        .as_str()
        .unwrap()
        .contains("panic_hook.rs:"));

    assert_eq!(recs[1]["message"], "panic: bad value 42");
}
