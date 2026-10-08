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

Session entities stay alive while navigating home or settings. Outside terminal focus, Ctrl/Cmd+W cannot
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


## Connected VM workspaces

Entering a VM replaces Home navigation with Terminal, File Manager, and
Infrastructure. Back to servers restores Home. Each tab has a close button;
live SSH sessions offer Stay connected, Disconnect, or Cancel. Hidden sessions
remain in the registry and appear on Home, retaining the same terminal and
transfer state. App exit reports active sessions and transfers. Plain SSH cannot
promise work survives disconnection; optional named tmux workspaces can be
reattached after network recovery or app restart. Missing tmux requires an
explicit choice before connecting without protection.

Files share one SFTP worker between the terminal drawer and full-page grid/list.
Search and hidden-file filtering apply to the current folder. Name, size, and
modified sorting keep folders first. Interrupted transfers retain failure detail
and a Retry action after reconnection. Retry starts a new transfer rather than
resuming byte offsets; partial remote files remain, and CreateNew prevents
silently overwriting them. Choose another folder or remove the partial file.
Download retries ask for a destination again.

Infrastructure currently supports Linux only. It collects bounded read-only
metrics on a separate SSH channel every five seconds while visible. Process CPU
uses 100% per core; overall CPU uses 100% for all cores. Initial CPU readings need
two samples. Missing readings display unavailable, and collection is capped at
1,000 processes. Termination sends SIGTERM only after confirmation and checking
the PID/start-time identity; permission failures never trigger sudo.

Sidebar/page opacity transitions finish in 140?160 ms and do not animate PTY
dimensions. Reduced motion disables these and component transitions.

New capture fixtures (example data, no SSH) are snapshot-session-terminal,
snapshot-session-files, snapshot-session-infra, snapshot-close, and
snapshot-background. Add -light to inspect the light theme. These fixtures check
layout; live tmux and infrastructure behavior still need disposable Linux-server
acceptance in addition to loopback transport and parser tests.


Server workspace verification on Windows: the workspace suite passed 106 tests covering session lifecycle, visible forms,
metrics parsing, and real SSH transport regressions; strict Clippy, formatting, whitespace, and
headless compilation passed. Rendered Terminal, Files grid/list, Infrastructure,
connection form, and close-modal screens were inspected at 125% scaling with
1280x820 and 1024x768 logical windows. File chrome was compacted after review to
leave room for two grid rows or several list entries at the smaller size. Other
OS/DPI and live Linux-server acceptance remain release checks.

## Terminal clipboard

Drag to select text, then right-click for the GPUI Kit Copy, Paste, Select all, and Send Alt+Tab to VM
menu beside the pointer. Selection remains visible while the menu is open;
Copy, Paste, Select all, and Escape restore terminal focus. A focus click does
not select a character, leaving Ctrl+C available to interrupt the shell.
Alt+drag selects a rectangle; Shift+drag bypasses a terminal application's
mouse reporting. Clipboard actions use the right-click menu. All keyboard
shortcuts, including Ctrl/Cmd+C/V, Ctrl/Cmd+Shift+C/V, and Shift+Insert, go to
the terminal application. Ctrl+C is sent even when text is selected. Single-line
menu paste is immediate; multiline text requires review when bracketed paste is
off, and disconnected sessions reject input.

Clipboard GUI regressions cover mouse clicks on Copy and Paste, forward, reverse
and rectangular selection, focus clicks, Escape, multiline review, and a real
local PTY paste followed by Enter. The native Windows right-click
menu was rendered and inspected at 125% scaling. Reproduce that review with
`snapshot-session-clipboard` using the development `capture` feature.


## Harness shortcut priority

The focused terminal and the visible Terminal page have their own GPUI key
contexts. Workspace bindings are excluded from both, including Home, New
connection, Close tab, and Quit. Opening or returning to a terminal focuses it
automatically. If focus returns to the workspace or is lost, the active terminal
reclaims it. Authentication focuses its credential input and preserves the
current field. Dialogs, file inputs, and deliberate keyboard focus on controls
retain their own input handling.
Ctrl+K, Ctrl+N, Ctrl+W, Ctrl+J, Ctrl+R, Ctrl+T, Ctrl+O, Shift+Tab, Escape, and
Alt combinations therefore reach the shell or harness. Workspace buttons remain
available, and the usual workspace bindings still work outside the Terminal page.
The terminal has no local keyboard clipboard handlers. Tab/Shift+Tab and terminal
clipboard chords bypass GPUI Kit's root focus and clipboard bindings. These
overrides apply to terminal input only; authentication fields and paste reviews
retain normal focus traversal. A root-mounted regression checks terminal bytes
and focus, including the legacy backtab and negotiated Kitty Shift+Tab.
Modified key presses and negotiated key releases reach the harness without rewriting them as local
copy/paste actions. Local clipboard operations remain available through the
right-click menu.

Windows VM terminals enable foreground-only keyboard capture by default. The
platform owns a Win32 hook on a dedicated message thread and forwards Alt+Tab,
Alt+Shift+Tab, Alt+Escape, Alt+F4, and Ctrl+Escape through the existing encoder.
Ctrl+Alt+F12 releases capture until the toolbar toggle is enabled again. Capture
suspends during dialogs, menus, paste review, authentication, disconnection,
window deactivation, and navigation away. Secure/Windows-key chords pass through.
Installation/delivery errors fail open; generation checks reject stale events.
Only the audited platform FFI module permits unsafe code.

The right-click Send Alt+Tab action remains available as a fallback. Both paths
use ESC+Tab in legacy mode and the negotiated Kitty encoding otherwise.

Shift+Enter remains distinct from Enter when the application negotiates the
Kitty keyboard protocol. Legacy Enter behavior remains compatible with ordinary
shells. A tmux server can filter extended keys before a harness receives them;
see the [harness terminal configuration](https://code.claude.com/docs/en/terminal-config#configure-tmux)
for configuring that layer. OpsSSH does not modify the VM's tmux configuration.

The workspace suite includes regressions for terminal ownership and native capture. GUI tests exercise the actual workspace bindings, verify that Home shortcuts
still work, inspect the encoded transport bytes for harness shortcuts and
negotiated Shift+Enter, forward former clipboard shortcuts and their releases,
exercise the Send Alt+Tab menu in legacy and Kitty modes, restore
workspace/empty focus, and protect authentication inputs. Live
harness and tmux acceptance on a disposable VM remains a release check.


## Background terminal uploads and motion

Terminal drops initialize SFTP without opening Files. A compact folder picker
validates existing remote directories and remembers the canonical destination in
the server profile after an exclusive empty write probe is closed and removed.
The SSH home button resolves `.` through SFTP; it does not assume `/home/user`.
Failed terminal uploads offer Change folder before retry. SFTP retains the SSH
login's privileges regardless of sudo inside the interactive shell.
Endpoint changes clear the saved preference; an old live session
is detached from the edited profile rather than saving paths for the wrong host.
The destination can be edited in advanced connection session settings or from
the terminal toolbar. Unchecked Remember applies to the current drop only.

The bounded terminal tray shows circular batch progress, Cancel, Retry, and
remote path actions. Preparation scans folders off the UI thread and rejects
symlinks. Cumulative bytes do not reset between files; completion waits for remote
close acknowledgements. Uploads retain exclusive-create semantics. Failed jobs
keep their chosen destination, and Change destination updates explicit retries.
Queued validation replies are tied to their dialog request and worker generation.

Paths are inserted without Enter only into the active original terminal with its
live connection generation, focus, and modal guards satisfied. Drops restore
terminal focus. Successful paths use a single paste transaction, with bracketed
paste framing when negotiated, so terminal applications such as Claude and Codex
receive the reference immediately. No Enter is sent.
If completion occurs while focus is elsewhere, delivery waits until the original
terminal is ready again, then runs once. Reconnect generations cannot replay old
paths automatically. A rejected transport write requires manual insertion.
Insert path and Copy path remain available. Completed feedback disappears after
four seconds unless insertion is pending; transfer history remains in Files.
Every terminal notice has a dismiss ×. Dismissal preserves history and never
cancels a running transfer or deletes a remote file. Dismissing completed pending
feedback also discards its automatic insertion.

An existing upload filename pauses the transfer with its exact remote path and
Replace / Cancel actions in both the terminal notice and Files transfer history.
Replace authorizes only the current colliding file and resumes the original job;
it is never remembered as a blanket overwrite preference. Cancellation or dismissing
the waiting prompt keeps the existing file. Folder uploads merge directories without
removing unrelated files and ask separately for each colliding regular file. Links,
special files, and folders cannot be overwritten as files. Changed destination metadata
while waiting requires a new review; write failures remain failures, not collisions.

GPUI springs animate card entrance/hover and tab indicators. Short underdamped
sidebar/page and upload-card transitions add expressive motion. Terminal page
changes, the grid, cursor, selection, and IME remain immediate. GPUI Kit supplies
dialog/button and progress transitions. Reduced motion resolves springs immediately
and replaces indeterminate rotation with static feedback. Settled animations do
not schedule frames.

Native previews: `snapshot-session-upload-progress`,
`snapshot-session-upload-failed`, and `snapshot-session-upload-destination` with
the `capture` feature; `snapshot-session-upload-complete` also demonstrates a
completed path appearing in a real local PTY without submitting it.
`snapshot-session-upload-replace` previews the repeated `/opt/test.txt` upload prompt.
Windows physical-key and live VM/tmux acceptance should
be exercised interactively; automated tests verify hook routing and encoder bytes.
