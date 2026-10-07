# Files, drops and recovery

The SSH tab's Files pane opens SFTP over the existing verified SSH session. Network
and disk work runs on its own Tokio worker. The job queue is bounded to 32 and the
list renders only visible rows using GPUI's uniform list. The SFTP library itself
currently materializes each directory listing before returning it; 100,000-entry
listing responsiveness and memory remain measurement gates.

Browse with the folder field, Enter, Up, Refresh or a folder double-click. Upload
accepts files and directories, downloads use the native destination picker, and
Create makes a folder. Delete requires a second explicit action and handles files
only. Cancel requests cancellation between transfer chunks. Partial files stay in
place. Existing destinations are never silently replaced. A save dialog's generic
overwrite prompt does not override exclusive-create in this version.

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
and interaction design is implemented. The file listing cannot rename or preview
files from the pane yet; rename exists as a non-overwriting service API.

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
