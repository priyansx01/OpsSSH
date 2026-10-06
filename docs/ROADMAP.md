# Implementation roadmap

The accepted identity is **OpsSSH**, dual licensed **MIT OR Apache-2.0**.
The full product scope remains in `intent.md`. This file tracks implementation
truth and the decisions that refine that document.

## Status

| Stage | Status | Exit gate |
| --- | --- | --- |
| 0: Foundation | Local workspace implemented; remote/public setup pending | Tests/build locally; three-platform CI after repository hosting |
| 1: Renderer spike | Real local PTY and GPUI/Alacritty implemented; Windows visual smoke check passed | Controlled performance and all-platform interactive/accessibility results |
| 2: SSH terminal | Not started | Direct auth, host verification, vault, resize, tmux |
| 3: Full-screen acceptance | Not started | Claude Code/neovim/htop/less direct and through tmux |
| 4: Compatibility | Not started | Config, jump/proxy, certificates, agent-backed security keys, matrix |
| 5: Servers | Not started | Home/search/connect dialog, persistence setup, import/export |
| 6: Files/drops | Not started | SFTP queue/resume, browser, drops/images, explicit sudo staging |
| 7: Recovery | Not started | Sleep/network reconnect, tmux/history, optional VPN protocol |
| 8: Release | Not started | Accessibility, translations, signed packages, all-OS acceptance |

## Current evidence

Dependency access and Rust tooling are available. The exact dependencies are
pinned in Cargo.toml and Cargo.lock. Windows builds/tests exercise real ConPTY,
Alacritty, and the GPUI desktop. Exported home/PowerShell frames were inspected.
The app includes a synthetic parser benchmark and a developer frame exporter.
These results do not yet accept the renderer or prove the product performance
targets. SSH/product work remains behind the acceptance gate.

## Next concrete work

1. Complete function/keypad input, cursor blinking, and native IME composition
   editing; test full-screen programs and failure/shutdown under sustained output.
2. Measure startup, parser/end-to-end throughput, input-to-presentation timing,
   frame times, idle CPU, and three-session scrollback memory. Test DPI, monitors,
   IME, and screen readers on all platforms.
3. Accept GPUI if those results pass. If framework limitations block acceptance,
   evaluate Iced with the same backend and platform interfaces. Record the
   decision before starting SSH/product features.

## Decisions overriding examples in intent.md

- Use `opsssh` for app-owned paths; do not create `sshchat` paths.
- Treat performance budgets as initial targets, then lock measured gates on
  controlled hardware before feature work. No absolute GPU budgets pass yet.
- Block offline typing and paste; do not retain or automatically replay input.
- Give distinct terminal tabs unique tmux session identities.
- Keep the planned MIT/Apache dual licence and human DCO sign-offs.
- Verify the dependency table rather than treating its versions as audited.
- Advertise only input protocol enhancements actually implemented by the adapter.
- Enumerate concrete SSH aliases, with wildcards applying configuration only.
- Surface unsupported connection-affecting config before connecting.
- No repository publication, signing identity, remote service, or commit is
  created automatically. Future commits must not add AI co-author attribution.
