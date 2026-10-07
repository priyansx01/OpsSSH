# Implementation roadmap

The accepted identity is **OpsSSH**, dual licensed **MIT OR Apache-2.0**.
The full product scope remains in `intent.md`. This file tracks implementation
truth and the decisions that refine that document.

## Status

| Stage | Status | Exit gate |
| --- | --- | --- |
| 0: Foundation | Implemented; repository published; three-platform CI configured | Successful hosted CI on final revision |
| 1: Renderer spike | Local PTY, GPUI/Alacritty, function keys, blink, IME editing and OSC metadata implemented | Controlled performance, physical keypad, all-platform interactive/accessibility results |
| 2: SSH terminal | Transport, authentication, strict host verification, OS vault, PTY resize and tmux implemented | Real-server and interactive acceptance |
| 3: Full-screen acceptance | Protocol tests and fixture tooling available | Claude Code/neovim/htop/less direct and through tmux |
| 4: Compatibility | Config, jump chains, approved proxy, certificates, CA/revocation and hashed known-host support implemented | OpenSSH/Dropbear/platform matrix; agent-backed security keys; forwarding and legacy opt-in |
| 5: Servers | Charcoal/ruby workspace, progressive forms, themes/settings, virtualized servers, retained tabs and import/export implemented | Complete novice, live-config and all-platform UI acceptance |
| 6: Files/drops | Real SFTP, list/transfer/cancel/exclusive-write/resume policy and drop/PNG helpers implemented | Live desktop browser/transfer, drag/drop/image acceptance and explicit sudo workflow |
| 7: Recovery | Tmux reconnect and deterministic recovery/VPN state policy implemented | Native sleep/network/VPN integration, tmux history and reconnect acceptance |
| 8: Release | Three-platform CI, fixture/fuzz hooks and unsigned packages/notices/checksums configured | Accessibility, complete translations, signed installers and all-OS acceptance |

## Current evidence

Dependency access and Rust tooling are available. The exact dependencies are
pinned in Cargo.toml and Cargo.lock. Windows builds/tests exercise real ConPTY,
Alacritty, and the GPUI desktop. Exported home/PowerShell frames were inspected.
The app includes a synthetic parser benchmark and a developer frame exporter.
SSH loopback and SFTP wire fixtures exercise authentication, verification, PTY
requests and transfer policy. They do not accept the renderer or prove product
performance. At the user's request, product phases proceed in parallel while
acceptance remains open.

## Next concrete work

1. Integrate and exercise file-browser/drop/image workflows and recovery policy;
   complete settings, privilege review and missing compatibility options.
2. Measure startup, parser/end-to-end throughput, input-to-presentation timing,
   frame times, idle CPU, and three-session scrollback memory. Test DPI, monitors,
   IME, and screen readers on all platforms.
3. Exercise the disposable server matrix and full-screen programs, native IMEs,
   physical keypad, monitor changes and screen readers. Accept GPUI only after
   those checks; evaluate Iced if framework limitations block acceptance.
4. Generate final notices/packages, then sign and test installers with owner
   signing identities before production release.

## Decisions overriding examples in intent.md

- Use `opsssh` for app-owned paths; do not create `sshchat` paths.
- Treat performance budgets as initial targets, then lock measured gates on
  controlled hardware. No absolute GPU budgets pass yet; parallel feature work
  was explicitly requested without waiving release acceptance.
- Block offline typing and paste; do not retain or automatically replay input.
- Reopen the existing session for the same server and configured tmux name; use different names for distinct protected workspaces.
- Keep the planned MIT/Apache dual licence and human DCO sign-offs.
- Verify the dependency table rather than treating its versions as audited.
- Advertise only input protocol enhancements actually implemented by the adapter.
- Enumerate concrete SSH aliases, with wildcards applying configuration only.
- Surface unsupported connection-affecting config before connecting.
- The repository was published at the user's request. Commits use the configured
  human Git identity and no AI co-author attribution. Signing identities and
  production services require owner setup.

The charcoal/ruby redesign and its validation limits are described in [UX.md](UX.md).


## Server workspace phase

Implemented Terminal / File Manager / Linux Infrastructure navigation, stable
session identities, per-tab close choices, background-session reopening, optional
tmux protection and visible recovery, full-page SFTP grid/list views, interrupted
transfer retry, and independent bounded metrics/process-operation channels.
See [SESSION_RECOVERY.md](SESSION_RECOVERY.md) for persistence behavior. Remote
process termination uses confirmed SIGTERM with identity revalidation and no
sudo. Deployments, AI assistance, PM2, backups, logs, alerts, cron and Docker
remain later phases.
