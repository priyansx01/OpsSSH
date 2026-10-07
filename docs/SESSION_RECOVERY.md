# Keeping remote work running

Enable **Protect work with tmux** in the connection form before connecting. It is
off for ordinary SSH profiles. Choose a session name containing 1–100 letters,
numbers, hyphens or underscores. The default name is `ops`.

The server must have tmux installed. OpsSSH uses a dedicated `opsssh` tmux socket,
so it does not attach to sessions on the server's ordinary default socket. It
creates an initial configuration at `~/.config/opsssh/tmux.conf`; that
configuration also sources `~/.tmux.conf` if present.

With protection enabled, commands inside the remote tmux session can continue
when the network drops or OpsSSH exits. Reconnecting with the same server,
username and saved session name attaches to that workspace again. Use different
names for independent workspaces on the same server and account. Protection
does not keep commands alive through a server restart or a terminated tmux
session.

OpsSSH automatically retries a lost protected connection with bounded backoff.
It continues to verify host keys and authenticate normally. Keystrokes entered
while disconnected are discarded; they are never replayed after reconnecting.

An ordinary SSH shell does not have these persistence guarantees. Disabling the
switch preserves the name in the open form for convenience, but saves a profile
without tmux protection. SSH keepalive remains configurable under **Advanced →
Session**; keepalive detects connectivity problems and does not preserve work.

File transfers are separate from the tmux terminal. Tmux protection does not
resume an interrupted upload or download.


Closing a live server tab offers **Stay connected**, **Disconnect**, and
**Cancel**. Stay connected keeps that SSH transport and its file transfers alive
while OpsSSH remains open. Reopen it from the background-session strip on Home.
Disconnect cancels transfers and closes SSH; it does not kill a protected tmux
workspace. Quitting disconnects all SSH transports and exits local shells.

Protected reconnects retry transient network and handshake failures with capped
backoff. Authentication rejection and host-key problems stop recovery for your
attention. **Stop reconnecting** ends attempts without replaying input. If tmux
is missing, the terminal offers an explicit **Connect without protection**
action; OpsSSH never installs tmux or silently changes the saved profile.

After a network interruption, Files retains transfer failures and offers Retry
using the new connection. Retry starts anew rather than resuming partial byte
ranges. Remote partial files are not overwritten automatically. Choose another
folder or remove the partial file before retrying an upload.
