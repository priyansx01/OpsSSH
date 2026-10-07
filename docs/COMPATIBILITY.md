# Compatibility status

The development preview connects over SSH and offers SFTP. Compatibility remains
partial until the disposable-server and interactive application matrix passes.

## Local terminal spike

Implemented: real ConPTY/openpty, Alacritty VT parsing, alternate screen,
Unicode/wide/combining cells, scrollback, ANSI/truecolor, selected text styles,
resize, cursor shapes, smart copy, bracketed paste, focus, and basic legacy/SGR
mouse reports. Automated tests cover fragmented streams, alternate-screen
restoration, synchronized output, clipboard query denial, and a real shell
command/resize/shutdown round trip.

Input covers cursor/navigation and F1-F35 encodings, xterm control/Alt encodings,
and selected Kitty enhancements (disambiguation, event types, all-key reporting for
represented keys). Alacritty handles mode negotiation. Physical keypad distinction,
alternate-key/associated-text enhancements, and extended mouse encodings are not
implemented. This is partial protocol support, not full Kitty keyboard compatibility.

GPUI text/IME handlers keep editable preedit text local until commit. UTF-16 range
edits are covered by unit tests. Native candidate windows, complex-script shaping/
fallback, and screen readers still require hands-on testing; the tests do not prove
native integration.

SSH has loopback coverage for password and private-key auth, keyboard-interactive,
unknown/changed/revoked host keys, PTY resize, and a two-hop chain. OpenSSH config
parsing, hashed known_hosts, host CAs, and explicit proxy approval are implemented.
RSA authentication is disabled because the enabled upstream implementation has a
timing side-channel advisory. Agent-backed security keys, forwarding, legacy
algorithms, host-certificate live server tests and real proxy processes remain open.

The SFTP wire fixture covers listing, upload/download, resume identity/content
checks, cancellation and conflicts. Desktop file browser/drop integration is still
being completed; see the roadmap for its acceptance gate.

## Pending SSH matrix

OpenSSH, OpenSSH 7.4, Dropbear, Alpine/BusyBox, Windows OpenSSH, and live proxy
subprocess testing remain unverified. Docker Desktop's daemon was unavailable in
this Windows run. Use disposable fixture keys, never personal SSH settings or
credentials. CI configuration exercises disposable servers on Linux when hosted.
Use disposable fixture keys, never personal SSH settings or credentials.

## Platform evidence

Windows: native debug desktop built and launched; home and local PowerShell
frames exported from the renderer and visually inspected. Unit and real ConPTY
integration tests pass locally. The exports do not prove input-to-presentation
latency, accessibility, or complete interactive application compatibility.

Windows/macOS/Linux builds and PTY tests are configured in CI. macOS, Linux,
X11/Wayland, packaging, signing, native IMEs, monitor/DPI changes, and screen
readers have not been executed in this Windows workspace. The renderer acceptance
gate remains open until those checks and controlled performance measurements pass.
