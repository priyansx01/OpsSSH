# OpsSSH

A Rust SSH workspace for people who work on remote servers: a fast server list,
a responsive terminal, and SFTP with remote file drops.

**Status: development preview with local/SSH terminals and SFTP.**
The desktop app uses GPUI, Alacritty's VT emulator, and real ConPTY/openpty.
See [the roadmap](docs/ROADMAP.md) and [the original product intent](intent.md).

## Run

Install Rust 1.99.0 and [native build prerequisites](CONTRIBUTING.md), then:

```sh
cargo run -p opsssh
cargo run -p opsssh -- local
```

The home screen reads saved servers and concrete aliases from SSH config. Add or
edit a server, select authentication, and connect. Unknown host keys require an
explicit trust decision; changed or revoked keys stop the connection. Passwords
and passphrases are saved in the OS credential store only when you opt in. OTP
responses are never saved.

Ctrl/Cmd+Enter opens a local shell. Ctrl/Cmd+K opens home/search, Ctrl/Cmd+N adds
a server, and Ctrl/Cmd+W closes the active tab. Ctrl/Cmd+Shift+H returns home while
retaining sessions. Ctrl+Shift+Q (Cmd+Q on macOS) quits. These workspace shortcuts
apply outside the Terminal page. Opening or returning to a terminal focuses it
automatically. Inside a terminal, keys go to the shell or
harness, including Ctrl+K, Ctrl+N, Ctrl+W, Shift+Tab, Escape, and Alt shortcuts.
Use the workspace buttons to navigate, close a tab, or quit while using a harness.

Select text by dragging, then right-click for Copy, Paste, or Select all beside
the pointer. Alt+drag selects a rectangle. Keyboard shortcuts, including
Ctrl/Cmd+C/V, Ctrl/Cmd+Shift+C/V, and Shift+Insert, go directly to the terminal
application. Ctrl+C reaches the remote process even when text is selected.
Use right-click Copy/Paste for the local clipboard. On Windows, Keyboard captured
forwards Alt+Tab, Alt+Shift+Tab, Alt+Escape, Alt+F4, and Ctrl+Escape while the
connected VM terminal owns focus. Ctrl+Alt+F12 releases capture; click the toolbar
control to enable it again. Dialogs, menus, other pages, and inactive windows
suspend capture. Windows-key combinations and Ctrl+Alt+Delete remain local.
Right-click Send Alt+Tab to VM is also available when capture is released.
Shift bypasses application
mouse reporting for local selection/scrollback. Input is blocked while disconnected
and is never queued for later replay. Multiline paste without bracketed-paste mode
requires review. Ctrl/Cmd-click opens HTTP/HTTPS terminal hyperlinks.

Dropping a file keeps the terminal open. Choose a remote upload folder once and
remember it for that server; future drops use that folder automatically. Change it
with Upload destination or the connection's advanced session settings. The
folder picker checks write access before remembering a destination. Use SSH home
selects the login user's home, avoiding unwritable parents such as `/home`.
SFTP uses the SSH login's permissions even if the shell is running under sudo.
A circular progress tray shows preparation, upload, cancellation, and failure. Successful
uploads immediately paste quoted remote paths into the active terminal's prompt,
including Claude and Codex, without pressing Enter. If you move away, delivery
waits until the original terminal is focused again. Insert path and Copy path
remain available. Each notice has an × to dismiss it; active transfers continue
and history remains in Files.
If an upload finds an existing filename, it pauses and shows the remote path with
Replace and Cancel. Replace overwrites that file only; Cancel keeps it. Folder
uploads merge existing folders and ask for each colliding file. Symbolic links
and folders cannot be replaced with files. Dismissing a replacement prompt cancels
the waiting upload; other active uploads keep running when their notice is dismissed.

Native spring motion animates server cards, tabs, navigation, dialogs, and upload
feedback. Terminal typing and output stay immediate. Reduced motion removes
movement and animated loading indicators.

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

- Persisted server editor, fuzzy search, environments, favorites, import/export,
  SSH config aliases, quick-connect parsing, and retained terminal tabs.
- GPUI terminal grid, colors/styles, cursor blinking, selection and clipboard actions.
- Alacritty VT parsing, alternate screen, Unicode snapshots, 100,000-line history.
- Shared unchanged rows, display-frame coalescing, 150 ms synchronized-output hold.
- Background PTY/parser/writer workers, resize, process exit, and shell cleanup.
- Cursor/control/function keys, selected Kitty enhancements, focus, bracketed paste,
  UTF-16 IME composition editing, OSC 7/133 metadata and OSC 8 links. OSC 52 can
  write but cannot read the clipboard.
- Background SSH transport, password/key/agent/keyboard-interactive authentication,
  certificates, strict known-host verification, jump chains and reviewed proxies.
- SFTP listing/transfers, cancellation, exclusive creation, explicit overwrite and
  identity-checked resume; drop quoting, private staging and PNG encoding.
- Per-session Linux SSH key management: search, import, inspect, copy/export,
  comment edits and confirmed removal; explicit privileged user creation with repair.
  See [SSH Management](docs/SSH_MANAGEMENT.md) for requirements and scope.
- Tmux reconnect with bounded backoff and disconnected input rejection.
- Real PTY, SSH loopback and SFTP protocol tests, three-platform CI, compatibility
  fixtures and unsigned package tooling.

GPU performance, screen readers, native IMEs, monitor changes, full-screen
application acceptance and signed installers still need validation. This preview
does not establish the compatibility or performance targets in `intent.md`.

See [architecture](docs/ARCHITECTURE.md), [compatibility](docs/COMPATIBILITY.md),
and [performance methodology](docs/PERFORMANCE.md). Windows leads development;
Windows, macOS, and Linux are required for 1.0. No telemetry is planned.

## Licence

MIT OR Apache-2.0, at your option. See [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE). Contributions use the same dual licence.
