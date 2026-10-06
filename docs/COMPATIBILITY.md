# Compatibility status

No SSH server can be connected to with this build. The original compatibility
table remains a target.

## Local terminal spike

Implemented: real ConPTY/openpty, Alacritty VT parsing, alternate screen,
Unicode/wide/combining cells, scrollback, ANSI/truecolor, selected text styles,
resize, cursor shapes, smart copy, bracketed paste, focus, and basic legacy/SGR
mouse reports. Automated tests cover fragmented streams, alternate-screen
restoration, synchronized output, clipboard query denial, and a real shell
command/resize/shutdown round trip.

Input covers cursor/navigation keys, xterm control/Alt encodings, and selected
Kitty enhancements (disambiguation, event types, all-key reporting for represented
keys). Alacritty handles mode negotiation. Function/keypad/modifier keys,
alternate-key/associated-text enhancements, extended mouse encodings, and cursor
blinking are not implemented. This is partial protocol support, not full Kitty
keyboard compatibility.

GPUI text/IME handlers keep preedit text local until commit. Native candidate
windows, composition edits, complex-script shaping/fallback, and screen readers
still require hands-on testing; text-handler plumbing alone does not prove them.

## Pending SSH matrix

Current OpenSSH, OpenSSH 7.4, Dropbear, Alpine/BusyBox, Windows OpenSSH, jump chains,
proxy subprocesses, keyboard-interactive/OTP, agents, certificates, hashed
known_hosts, host CAs/revocations, and agent-backed security keys remain untested.
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
