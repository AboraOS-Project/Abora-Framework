# Contributing

Guidelines for working on Abora Framework.

## Code of conduct

Be respectful; this is a small, security-focused codebase and review is a
trust exercise. Report interpersonal issues to the maintainers privately.

## Workspace & conventions

This is a Rust 2021 workspace. See [docs/architecture.md](architecture.md)
for layout.

* `crates/` — libraries. Depends outward only; `abora-core` is the leaf.
* `services/aborad` — the daemon (binary).
* `cli/abora` — the CLI (binary).
* No crate may depend on a web framework except where HTTP serves (daemon).
* `#![forbid(unsafe_code)]` is enforced workspace-wide — pull requests adding
  `unsafe` will not be accepted without an exceptional security review.

## Getting started

```sh
cargo test --workspace
cargo build --workspace
./scripts/check.sh          # build + test + (fmt/clippy when installed)
cargo run -p aborad -- --config config/abora.toml.default
cargo run -p abora -- status
```

Note: on distro-packaged Rust (`apt install cargo`), `cargo fmt` / `cargo
clippy` / `rustdoc` may be missing or broken (e.g. rustdoc fails to load
`libLLVM`). The project scripts detect this and degrade gracefully; prefer
rustup-managed toolchains for full checks.

## What is expected of a PR

1. One logical change; small diffs; no unrelated reformatting.
2. Tests for new behaviour (unit tests in the owning crate; a parity test
   exists for the shipped default config — keep it passing).
3. `cargo test --workspace` green.
4. Windows/format/validation mistakes rejected loudly in new config fields.
5. No new dependencies without a reason in the PR body.

## Security-sensitive changes

Anything touching auth, remote, execution, secrets, or bind addresses goes
through [docs/security.md](security.md). In doubt, ask first — reviewers
erred toward "not yet" repeatedly during the design of this codebase.

## Documentation

* API behaviour must be reflected in [docs/daemon-api.md](daemon-api.md).
* Config changes must update [docs/configuration.md](configuration.md) **and**
  [`config/abora.toml.default`](../config/abora.toml.default) (keep parity).
* Update-system changes belong in [docs/update.md](update.md).

## Versioning

Follow [SemVer](https://semver.org) at the workspace level. The API has its
own additive-only policy documented in [docs/daemon-api.md](daemon-api.md).