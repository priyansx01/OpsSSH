# Changelog

## Unreleased

- Add the four-crate Rust workspace, CI, licensing, and developer tools.
- Add the GPUI home and terminal screens with labelled sample servers.
- Integrate Alacritty VT parsing and portable-pty ConPTY/openpty.
- Add background shell workers, resizing, exit handling, and asynchronous cleanup.
- Render shared row snapshots with fixed cell text, colors/styles, cursor,
  selection, clipboard, mouse, and text/IME commit plumbing.
- Add synchronized-output timeout, input blocking, smart copy, and protocol tests.
- Add real PTY integration tests, parser benchmark, and rendered-frame export.

SSH, vaults, saved servers, and SFTP remain unimplemented. Renderer performance
and all-platform interactive/accessibility acceptance remain pending.
