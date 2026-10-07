# Charcoal and ruby workspace

OpsSSH uses GPUI Component 0.7.1 with application-owned semantic colors. Ruby marks
primary actions; neutral charcoal surfaces separate navigation, content, and
dialogs. Light and system-following themes use the same component hierarchy.
System UI typography and monospace endpoints remain native to each platform.

The server page has virtualized cards and a list alternative, search, environment
groups, favorites, and name/recent sorting. Recency is recorded only after a
saved connection reaches Connected; exported profiles omit local timestamps.
There are no synthetic reachability indicators in the application.

Connection setup starts with host, username, and authentication. Details and
advanced routing/security/session settings are collapsed. Existing advanced
values survive editing while collapsed. Passwords and encrypted-key passphrases
are requested during authentication. Unsupported SSH options require explicit
review. Save & connect validates connection options before saving or closing
the dialog; failed checks retain entered values.

Session entities stay alive while navigating home or settings. Ctrl/Cmd+W cannot
close a hidden terminal from settings, help, or a connection dialog. Files use
a resizable panel with per-task cancellation and current-file byte progress.
Click the file list's Name heading, or activate it with the keyboard, to use
arrow keys and Enter. Enter opens a directory or presents the download picker.
Selection actions overlay the terminal instead of changing its PTY dimensions.

Appearance, terminal size, navigation state, and view/sort choices are persisted
in the existing version-1 store with defaults for older files. UI text is served
from the shared embedded Fluent catalog; additional languages remain future
work. The security prompts retain explicit host trust and credential-saving
choices; terminal ANSI colors are unchanged by the application theme.

## Verification

The workspace all-features suite includes real GPUI connection-form regressions,
settings compatibility, semantic color contrast, SSH loopback, SFTP wire tests,
and ConPTY tests. CI and the local check script run the all-features suite.

The `capture` feature provides in-memory visual fixtures. Capture processes
disable persistence and use example.test endpoints; fixture profiles never
replace saved user profiles. `snapshot-files` shows static file/transfer data
without opening SSH. These captures check layout, not live-server compatibility.

```powershell
cargo run -p opsssh --features capture -- snapshot-servers target/servers.png
cargo run -p opsssh --features capture -- snapshot-servers-light target/light.png
cargo run -p opsssh --features capture -- snapshot-connection target/connection.png
cargo run -p opsssh --features capture -- snapshot-key target/key.png
cargo run -p opsssh --features capture -- snapshot-settings target/settings.png
cargo run -p opsssh --features capture -- snapshot-files target/files.png
$env:OPSSSH_CAPTURE_WIDTH = '1024'
$env:OPSSSH_CAPTURE_HEIGHT = '768'
cargo run -p opsssh --features capture -- snapshot-many target/many.png
```

Rendered Windows screens were inspected at the current desktop scale (125%),
including a 1024x768 logical window and a 500-profile fixture. Additional OS/DPI,
screen-reader, live file-browser, and novice usability acceptance remain release
checks; these results do not establish performance or cross-platform acceptance.
