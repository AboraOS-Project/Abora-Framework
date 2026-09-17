//! Ensures the shipped default configuration file
//! (`config/abora.toml.default`) always resolves to exactly `Config::default()`.
//!
//! Running `cargo test` from the repository root runs this check; if defaults
//! change, the shipped file must be updated to match.

use abora_config::{Config, LogFormat};

const SHIPPED_DEFAULT: &str = include_str!("../../../config/abora.toml.default");

#[test]
fn shipped_default_matches_config_default() {
    let shipped = Config::from_str(SHIPPED_DEFAULT).expect("shipped default must parse and validate");
    assert_eq!(shipped, Config::default());
}

#[test]
fn shipped_default_renders_expected_level_and_format() {
    let shipped = Config::from_str(SHIPPED_DEFAULT).unwrap();
    assert_eq!(shipped.logging.level.to_string(), "info");
    assert_eq!(shipped.logging.format, LogFormat::Json);
}