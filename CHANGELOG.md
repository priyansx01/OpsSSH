# Changelog

All notable changes to OpsSSH are documented here. The project follows
[Semantic Versioning](https://semver.org/).

## 0.1.1 (2026-11-10)

### Fixed
- Splitting a terminal on an SSH connection no longer disconnects the whole connection, and the new pane connects instead of staying on "Connecting".

## 0.1.0 (2026-11-10)

First public release.

### Terminal
- Split terminals right or down into nested, resizable panes. Remote panes open extra shells on the existing SSH connection without signing in again.
- Reconnect tmux-protected sessions within seconds after the computer wakes from sleep, and retry transient network errors while Wi-Fi or a VPN comes back.
- Add immediate manual reconnection for protected SSH sessions and native tmux scrolling.
- Route Ctrl+V and Ctrl+Shift+V through local text/image clipboard paste, retaining upload destinations.
- Preserve shaped Unicode clusters and use Windows font fallbacks for multilingual terminal output.

### File manager
- Rework the remote file manager: multi-select, rubber-band selection, drag and drop between folders with Ctrl to copy, desktop drops onto any folder, cut/copy/paste, inline rename, Back/Forward history, Places with per-server bookmarks, and keyboard shortcuts.
- Add one conflict dialog for uploads, downloads, copies and moves with Replace, Keep both, Skip, Cancel all and Apply to all; same-named folders merge item by item.
- Stop asking twice when downloading over a file the save dialog already confirmed.
- Confirm local download conflicts and publish complete files atomically, preserving originals on failure.
- Use icon navigation controls and chevron breadcrumbs.
- Add experimental drag-out to the desktop behind `OPSSSH_EXPERIMENTAL_DRAG_OUT=1`.

### Servers and infrastructure
- Show processor and per-process CPU usage within a second of opening Infrastructure.
- Save connection edits before connecting and keep all setup options visible in one form.

### Foundation
- Add the Rust workspace, CI, licensing, and developer tools.
- Add the GPUI home and terminal screens.
- Integrate Alacritty VT parsing and portable-pty ConPTY/openpty.
- Add background shell workers, resizing, exit handling, and asynchronous cleanup.
- Render shared row snapshots with fixed cell text, colors/styles, cursor,
  selection, clipboard, mouse, and text/IME commit plumbing.
- Add synchronized-output timeout, input blocking, smart copy, and protocol tests.
- Add real PTY integration tests, parser benchmark, and rendered-frame export.

Builds are unsigned. All-platform interactive and accessibility acceptance remains pending for 1.0.
