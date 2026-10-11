# Changelog

## Unreleased

- Add immediate manual reconnection for protected SSH sessions and native tmux scrolling.
- Route Ctrl+V and Ctrl+Shift+V through local text/image clipboard paste, retaining upload destinations.
- Preserve shaped Unicode clusters and use Windows font fallbacks for multilingual terminal output.
- Save connection edits before connecting and keep all setup options visible in one form.
- Confirm local download conflicts and publish complete files atomically, preserving originals on failure.

- Add the four-crate Rust workspace, CI, licensing, and developer tools.
- Add the GPUI home and terminal screens with labelled sample servers.
- Integrate Alacritty VT parsing and portable-pty ConPTY/openpty.
- Add background shell workers, resizing, exit handling, and asynchronous cleanup.
- Render shared row snapshots with fixed cell text, colors/styles, cursor,
  selection, clipboard, mouse, and text/IME commit plumbing.
- Add synchronized-output timeout, input blocking, smart copy, and protocol tests.
- Add real PTY integration tests, parser benchmark, and rendered-frame export.

All-platform interactive/accessibility acceptance remains pending.
