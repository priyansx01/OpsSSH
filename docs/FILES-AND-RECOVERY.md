# Files, drops and recovery

The SSH tab's Files pane opens SFTP over the existing verified SSH session. Network
and disk work runs on its own Tokio worker. The job queue is bounded to 32 and the
list renders only visible rows using GPUI's uniform list. The SFTP library itself
currently materializes each directory listing before returning it; 100,000-entry
listing responsiveness and memory remain measurement gates.

Browse with the folder field, breadcrumbs, Places, Back/Forward (Alt+Left/Right or
the mouse's side buttons), Up (Alt+Up or Backspace) or a folder double-click.
Selection is kept by remote path, so a refresh never selects a different item:
Ctrl/Cmd+click toggles, Shift+click selects a range, Ctrl+A selects everything
listed, and dragging on empty space draws a selection rectangle. Keyboard
shortcuts apply only while the listing has focus: F2 renames in place, Delete asks
before deleting the selection, Ctrl+X/C/V cut, copy and paste, Ctrl+Shift+C copies
paths, Ctrl+H shows hidden files, Ctrl+L edits the path, Ctrl+D bookmarks the open
folder and F5 refreshes.

Dragging items onto a folder, breadcrumb, Up or a Place moves them; holding Ctrl
(Option on macOS) copies instead. A folder under the pointer opens after 0.9 s.
Drops onto the item itself, into one of its own subfolders, from another
connection or into the folder that already holds them are refused before anything
is queued. Files dropped from the desktop upload into the folder under the pointer.

Every transfer shares one conflict dialog: Replace, Keep both (a numbered name
such as `report (2).txt` or `backup (2).tar.gz`), Skip or Cancel all, with Apply to
all for the rest of the batch. Same-named folders merge, and each clashing file
inside is asked about again. Items of different types are never replaced. Closing
the dialog skips the item. A file the operating system's save dialog already
confirmed is replaced without a second question; files inside a downloaded folder
were never shown by that dialog and are still asked about. Moves use an atomic
SFTP rename; a move across filesystems copies and then removes the original only
after a clean copy. Replacing during a move uses `posix-rename@openssh.com` when
the server offers it and otherwise sets the original aside until the new file is
in place.

`opsssh-sftp` provides explicit overwrite and resume APIs. Resume binds a transfer
to the verified host fingerprint, canonical path, size and modification time, then
compares every existing prefix byte before continuing. Files with insufficient
metadata, changed identities or mismatched prefixes are rejected. The pane does
not yet expose resume/conflict-resolution buttons. The library caps concurrent
transfers at three; the pane currently processes its queue serially. Pipelining
within each file is supplied by russh-sftp.

Recursive uploads reject symlinks and special files and create every destination
exclusively. Remote filenames containing controls or path separators are rejected.
This deliberately excludes some valid POSIX filenames until a safe representation
and interaction design is implemented.

Dragging items out to the desktop is experimental and off by default; start OpsSSH
with `OPSSSH_EXPERIMENTAL_DRAG_OUT=1` to try it. On Windows the drag starts as soon
as the pointer leaves the window. Releasing over a File Explorer window or the
desktop drops instantly: OpsSSH finds that folder and downloads into it as a normal
transfer, with progress and the usual conflict dialog. A drop onto a subfolder icon
inside Explorer uses the folder that window shows. Other applications need real
files, so only small file selections (32 MiB) that finished preparing while the
drag started can be dropped there. On macOS and Linux Wayland only those prepared
small selections can leave the window. Prepared files live in a private folder
under the system temporary directory, are removed when the pane closes, and
leftovers older than a day are swept at start.

At a positively identified shell prompt, a drop targets the reported current
directory. Otherwise it creates a random `opsssh-drop-<UUID>` folder beneath the
user's canonical SFTP home. Permission 0700 is checked before any content is
uploaded; files are created as 0600. The generated path is quoted as one POSIX
shell argument and inserted after a successful upload. A clipboard PNG of at most
64 MiB uses the same staging flow. Other image encodings are reported as unsupported.
The core also provides a bounded RGBA-to-PNG encoder for platform integrations.

Staged files persist when the pane closes: a terminal application may still need
the inserted path. The upload message reports their location. Explicit cleanup UI,
sudo-assisted moves into protected folders, overwrite decisions and Windows remote
shell path insertion remain unfinished. Upload permission errors never trigger
sudo or password transmission automatically.

`opsssh-net-watch` supplies deterministic exponential retry delays capped at 30 s,
an immediate retry trigger when a network becomes available, permanent blocking
for a changed host identity, and input generations that reject stale keystrokes.
The strict bounded VPN line parser accepts only
`{"state":"connected"}`, `{"state":"reconnecting"}` or `{"state":"down"}`.
It does not authenticate an IPC peer or bind a socket. Native network/sleep event
subscriptions and the authenticated VPN IPC endpoint are still platform work.

Tests cover real SFTP protocol packets over a disposable in-memory server,
transfer cancellation, exclusive creation, upload/download resume with matching
and altered prefixes, private staging permissions and recursive upload. They do
not establish the 1 Gbit throughput target or cross-platform user experience.
