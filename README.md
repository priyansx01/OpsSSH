# OpsSSH

A Rust SSH workspace for people who work on remote servers: a fast server list,
a responsive terminal, and SFTP with remote file drops.

**Status: runnable local terminal spike; SSH and SFTP are not implemented.**
The desktop app uses GPUI, Alacritty's VT emulator, and real ConPTY/openpty.
See [the roadmap](docs/ROADMAP.md) and [the original product intent](intent.md).

## Run

Install Rust 1.99.0 and [native build prerequisites](CONTRIBUTING.md), then:

```sh
cargo run -p opsssh
cargo run -p opsssh -- local
```

The home screen contains labelled sample servers and an **Open local terminal**
button. Ctrl/Cmd+Enter opens the local shell from home. Ctrl/Cmd+Shift+H returns
home and closes the shell. Ctrl+Shift+Q (Cmd+Q on macOS) quits.

Select text by dragging; Alt+drag selects a rectangle. Ctrl/Cmd+C copies a
selection, and Ctrl/Cmd+V or Shift+Insert pastes. Shift bypasses application
mouse reporting for local selection/scrollback. Input is blocked after exit.

## Development tools

```sh
cargo test --workspace --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo run -p opsssh -- diagnostics
cargo run --release -p opsssh -- bench-parser 64
```

Headless backend tests/tools use `--no-default-features`. `bench` measures input
encoding and simulated frame scheduling; `bench-parser` measures synthetic VT
replay. Neither measures GPU latency or SSH speed.

Development-only frame exports require a working GPU/windowing environment:

```sh
cargo run -p opsssh --features capture -- snapshot home.png
cargo run -p opsssh --features capture -- snapshot-local terminal.png
```

The app exports its rendered frame after two seconds and exits. Terminal exports
can contain local paths and shell output; review them before sharing.

## Implemented

- GPUI home and terminal screens, fixed cell text, colors/styles, cursor, selection.
- Alacritty VT parsing, alternate screen, Unicode snapshots, 100,000-line history.
- Shared unchanged rows, display-frame coalescing, 150 ms synchronized-output hold.
- Background PTY/parser/writer workers, resize, process exit, and shell cleanup.
- Cursor/control keys, selected Kitty enhancements, focus, bracketed paste, and
  text/IME commit plumbing. OSC 52 can write but cannot read the clipboard.
- Real PTY integration and protocol tests; three-platform CI configuration.

GPU performance, screen readers, native IMEs, monitor changes, and full-screen
application acceptance still need validation. Function/keypad input, cursor
blinking, persisted servers, SSH, vaults, and SFTP remain future work.

See [architecture](docs/ARCHITECTURE.md), [compatibility](docs/COMPATIBILITY.md),
and [performance methodology](docs/PERFORMANCE.md). Windows leads development;
Windows, macOS, and Linux are required for 1.0. No telemetry is planned.

## Licence

MIT OR Apache-2.0, at your option. See [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE). Contributions use the same dual licence.
