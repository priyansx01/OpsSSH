# Contributing to OpsSSH

Read [the roadmap](docs/ROADMAP.md) before starting product features;
renderer acceptance comes first.

## Prerequisites

- Rust 1.99.0 with rustfmt and Clippy (the current declared minimum).
- Windows: Visual Studio Build Tools, Desktop development with C++, Windows SDK.
- macOS: Xcode command-line tools and Metal tooling for the selected GPUI release.
- Linux: C/C++ linker, pkg-config, fontconfig, FreeType, xkbcommon including X11,
  Wayland, XCB, and OpenSSL development libraries. CI installs Ubuntu packages.
- A working GPU/windowing environment to run the desktop or export frames.

Backend tests need no GPU, remote server, or credentials; they start disposable
local shells. Use `cargo test --workspace --no-default-features --locked`.
Native IME/accessibility/GPU acceptance must run on each supported desktop.

## Checks

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo build --workspace --release --locked
cargo run -p opsssh -- local
```

The first build needs dependency downloads. Offline builds require a populated
Cargo cache. `scripts/check.ps1` runs the four checks on Windows. Its optional
`-ToolchainBin 'C:\path\to\toolchain\bin'` selects already installed binaries
without changing global settings; Cargo continues using its existing cache.

`cargo deny check advisories licenses bans sources` checks the dependency graph;
CI pins cargo-deny 0.20.2. Keep Cargo.lock committed and upgrades deliberate.
Development frame export commands and benchmarks are described in README.

## Review

- Keep OS conditionals in `platform` and process/network I/O off the UI thread.
- Test protocol boundaries and failures; don't infer GPU timings from parser tests.
- Never include credentials, OTPs, keys, personal server details, or sensitive
  local shell output in fixtures, logs, issues, or shared screenshots.
- Fluent integration remains planned; the spike currently uses English literals.
- Follow [the code of conduct](CODE_OF_CONDUCT.md).

Contributions are MIT OR Apache-2.0 (inbound = outbound), without a CLA.
Use DCO sign-off only with your own valid Git identity and when you can certify
the [Developer Certificate of Origin](https://developercertificate.org/).
Do not manufacture author identities, sign-offs, or AI co-author trailers.

Include the command, expected/actual result, and `opsssh diagnostics` when reporting
bugs. Report vulnerabilities according to [SECURITY.md](SECURITY.md).
