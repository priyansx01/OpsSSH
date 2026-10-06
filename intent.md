# intent.md: putty-fork (now an open-source, pure-Rust SSH workspace)

An open-source SSH app for everyone who works on remote servers, written entirely in Rust with its own GPU-rendered UI and terminal. It does three jobs better than PuTTY and the tools we looked at:

1. **Servers:** a fast server list and a connect dialog that takes a pasted `ssh …` command, or your existing `~/.ssh/config`, and fills itself in.
2. **Files:** an SFTP file manager that follows the terminal's folder and takes drag and drop from your desktop.
3. **Terminal:** a "harness-first" terminal where Claude Code (and any other full-screen terminal app) running on the server feels as fast and smooth as it does locally, and copy and paste always work.

**Priorities, in order:** performance, then user experience, then breadth of SSH setups supported. There's no deadline, so the plan favours quality over speed.

**Audience:** anyone with SSH servers, not one person's setup. That means every common way people reach and sign in to servers has to work (see "SSH compatibility"), on Windows, macOS and Linux.

History: this started as a personal PuTTY fork, then a Tauri + xterm.js plan. Both are dropped (see "Why this stack"). The folder name stays for continuity; the project needs its own name before the first public release (working name `sshchat` is used in paths below). The VPN integration with `forti-clone/intent.md` is now one optional integration among others.

Versions and licences checked against crates.io and the Rust stable channel on 2026-10-06; russh features checked against its README the same day. The UI was designed with CtrlOps screenshots as **reference only**: no copied layouts, names, artwork or text.

## Scope

**v1 (first public release):**
- Server list (home) and connect dialog, including jump hosts (ProxyJump), proxy commands, and live `~/.ssh/config` support.
- SFTP file manager.
- Terminal: harness-first rendering and input, copy/paste with a hover and selection popup, drag and drop from the desktop to the server.
- Credentials in the OS vault, SSH agents, tmux-based "never lose work" (per server, optional), network-aware auto-reconnect, and the "Feels local" drop behaviour.
- Windows, macOS and Linux releases. Development leads on Windows; 1.0 ships on all three.

**Later (after v1):** port forwarding UI (local, remote, SOCKS), AI assistant, scripts library, deployments, PM2/Docker/cron/log panels, alerts, audit reports, key helper ("make and install a key for me"), PuTTY import, split panes, tmux control mode, mosh-style predictive echo, Kerberos/GSSAPI, X11 forwarding, serial and telnet, plugins.

## Performance budgets

Acceptance criteria, measured from milestone 1 and enforced in CI as regression checks on all three platforms.

| Measure | Budget | How measured |
| --- | --- | --- |
| Cold start to usable server list | < 250 ms | Timestamp from process start to first frame with data |
| Key press to glyph on screen, excluding network | < 8 ms (one 120 Hz frame) | Typometer-style test against a loopback sshd |
| Frame rate while scrolling or during heavy output | Locked to the display refresh (60/120/144 Hz), no dropped frames | Frame-time trace |
| Output throughput (`cat` a large file) | ≥ 100 MB/s parsed with the screen kept responsive | vtebench and a 1 GB `cat` |
| Claude Code full redraw | No flicker, no tearing, no partial frames | Synchronized output test, plus manual soak |
| Idle CPU | ~0% (no redraw unless something changed or the cursor blinks) | OS profiler |
| RAM with 3 sessions + file manager open | < 120 MB | Private working set |
| SFTP download on a 1 Gbit LAN | ≥ 80% of `scp` speed | 1 GB file |
| File manager folder with 100,000 entries | Opens and scrolls without stutter | Virtualized list test |
| Connect through one jump host | < 1.2× the time of two direct connects | Loopback test with two sshd containers |

## Why this stack

| Option | Verdict |
| --- | --- |
| **GPUI (Zed's UI framework) + gpui-component + alacritty_terminal + russh** | **Chosen.** GPUI is a GPU-rendered Rust UI framework built for Zed, which needs exactly this: big text views at high frame rates on macOS, Linux and Windows. Zed's own terminal is GPUI drawing an `alacritty_terminal` grid, so the hardest part has a working reference implementation. `gpui-component` adds inputs, tabs, lists, dialogs, virtualized tables and theming. All Apache-2.0, all Rust. |
| Iced + `iced_term` | **Fallback.** Pure Rust and GPU-rendered (wgpu), with an alacritty-based terminal widget and a more stable API, but a smaller widget set. If GPUI's churn becomes a problem in milestone 1, switch here. |
| egui | Immediate mode redraws every frame; harder to hit the idle-CPU and polish goals. |
| Tauri + xterm.js (earlier plan) | Not pure Rust; WebView adds RAM and input latency. |
| Own renderer on raw wgpu + winit | Most control, but months of work on text shaping, IME and accessibility that GPUI already does. Revisit only if GPUI can't meet the budgets. |

**Terminal emulator core:** `alacritty_terminal` (Apache-2.0) instead of writing our own parser first. It's fast, correct and already supports the protocols full-screen apps need. Our own code goes where users feel it: rendering, input encoding, selection, copy/paste, drops. The parser and grid sit behind our `term-core` interface, so they can be replaced later without touching the UI.

**SSH:** `russh` (Apache-2.0). Its README lists post-quantum hybrid key exchange (`mlkem768x25519-sha256`), curve25519, chacha20-poly1305 and AES-GCM/CTR, ETM MACs, Ed25519/ECDSA/RSA keys, OpenSSH certificates, PuTTY PPK keys, agent forwarding and Pageant support. Gaps to check and fill (see compatibility table): security-key (FIDO) keys and some legacy algorithms.

**Honest risks:**
- GPUI is pre-1.0. `gpui-component` 0.7.1 depends on `gpui-pre` 0.3.8, a published snapshot of Zed's GPUI. Pin both exactly (`=`) and upgrade on purpose.
- GPUI's Windows backend is newer than its macOS one. Milestone 1 tests high-DPI, multiple monitors, IME, and dark/light mode on all three platforms before anything else is built.
- Accessibility (screen readers) in GPUI must be checked early; it's a requirement for 1.0, not an extra.
- A pre-1.0 SSH library carries more risk than OpenSSH. The project follows russh's advisories, runs `cargo-deny` advisories in CI, and fuzzes our own parsing of untrusted input.

## SSH compatibility (works with what people already have)

The goal: if `ssh` from a terminal reaches a server, this app reaches it too, with the same config.

| Area | v1 support | Notes |
| --- | --- | --- |
| **Key types** | Ed25519, ECDSA (P-256/384/521), RSA with `rsa-sha2-256/512` | `ssh-rsa` (SHA-1) is off by default, with a per-server "legacy device" switch for old servers and network gear |
| **Key file formats** | OpenSSH, PEM (PKCS#1, PKCS#8, encrypted or not), PuTTY `.ppk` v2/v3 | Passphrase asked once and stored in the vault if the user agrees |
| **OpenSSH certificates** | Yes | User certs (`id_ed25519-cert.pub`) and `@cert-authority` host CAs in known_hosts |
| **Security keys (FIDO, `sk-ssh-ed25519@openssh.com`)** | Through an SSH agent in v1 | Native signing needs a FIDO library; Later. The agent path covers YubiKey users who already use `ssh-agent` |
| **SSH agents** | OpenSSH agent (`SSH_AUTH_SOCK` on macOS/Linux; `\\.\pipe\openssh-ssh-agent` on Windows), Pageant, and agents that speak the same protocol (1Password, Bitwarden, KeePassXC, Secretive, gpg-agent) | Agent forwarding is off by default and per server, with a warning that the server can use your keys while connected |
| **Sign-in methods** | Public key, password, keyboard-interactive (one-time codes, Duo, Google Authenticator prompts), and chains of them (for example key then OTP) | Prompts appear as a clean dialog, never a raw terminal prompt. One-time codes are never saved |
| **Jump hosts (ProxyJump)** | Yes, including chains (`-J a,b`) | Done inside the app over SSH tunnels (`direct-tcpip`); each hop is a saved server or `user@host:port` with its own sign-in |
| **Proxy commands (ProxyCommand)** | Yes | Runs a local program and speaks SSH over its input/output (for example `ssh -W %h:%p bastion`, `cloudflared access ssh`, AWS SSM `session-manager-plugin`). Tokens `%h %p %r %n %%` are filled in |
| **`~/.ssh/config`** | Read live, not just imported | `Host` patterns, `HostName`, `User`, `Port`, `IdentityFile`, `IdentitiesOnly`, `CertificateFile`, `ProxyJump`, `ProxyCommand`, `ServerAliveInterval`, `Include`, `IdentityAgent`, `ForwardAgent`, `HostKeyAlgorithms`/`PubkeyAcceptedAlgorithms`. `Match` blocks: `host`, `user`, `exec` are Later; unsupported lines are listed, never silently ignored |
| **known_hosts** | Reads and writes `~/.ssh/known_hosts` by default, so the app and `ssh` agree | Hashed entries, `@cert-authority`, `@revoked`, non-standard ports (`[host]:2222`). A setting keeps an app-only file instead |
| **Host key verification** | Strict, trust on first use | First connect shows the fingerprint (SHA256) plainly; a changed key blocks with a red banner and "what this means". Auto-reconnect never accepts a new key. SSHFP DNS records: Later |
| **Algorithms** | Modern defaults (post-quantum hybrid kex first, then curve25519; chacha20-poly1305, AES-GCM) | Per-server "legacy device" switch adds older kex/ciphers/MACs for old servers and routers |
| **Addresses** | IPv4, IPv6, hostnames, `.local` names | Happy-eyeballs style connect for dual-stack hosts |
| **Server types** | OpenSSH (7.4 and newer), Dropbear, Windows OpenSSH server, BusyBox/Alpine (ash shell) | Servers without an SFTP subsystem get a clear message; `scp` fallback is Later |
| **Remote shells** | bash, zsh, fish, ash/sh, PowerShell (Windows servers) | Shell integration snippets per shell; tmux is offered only where it can run |

**Compatibility test matrix in CI:** Docker containers for current OpenSSH, OpenSSH 7.4, Dropbear, and an Alpine/BusyBox server; a two-hop jump chain; keyboard-interactive with a fake OTP; agent with a test key; certificate auth. A Windows OpenSSH server is tested in the Windows CI job.

## Screen 1: Servers (home)

**Layout:** a slim left sidebar (All servers, Favourites, a group per environment or folder, "From SSH config", Settings) and the main area with a search box and the server list. Open sessions appear as tabs along the top.

**Each server card or row shows:** name, `user@host:port`, environment badge, sign-in type (password, key, agent, certificate), a "via bastion" chip when it uses a jump host, a live reachability dot (a cheap TCP probe every 30 s while the list is visible; for jump-host servers it probes the first hop), last connected time, and one big Connect button. Favourite, Edit, Duplicate, Copy SSH command and Delete sit in a hover menu.

**Servers from `~/.ssh/config`** appear automatically under "From SSH config", marked as such. They stay read-only so the app never rewrites your config; "Customize in app" copies one into the app's own list.

**Better than the reference:**
- **Type to find:** search is focused on open; fuzzy matching on name, host, user, environment and tags; Enter connects to the top result; `Ctrl+K` / `Cmd+K` from anywhere.
- **Card, list and compact views,** keyboard-navigable.
- **Environment safety:** user-defined environments with colours; the session header and terminal border carry the colour so you always know where you are.
- **Instant:** the list renders from a local file before any network work.
- **Export / import** of the server list (no secrets) as a file, for sharing a team's servers.

## Screen 2: Connect dialog

One form, not tabs. Fields in order:

1. **Quick connect (paste a command):** accepts any normal `ssh` command line, such as `ssh -i ~/.ssh/key.pem -p 2222 -J admin@bastion user@10.30.7.6`, `user@host:2222`, `-o ProxyCommand=…`, and `-A`. It fills every field below as you type (parsed with `shlex`). Options the app doesn't support are shown with a note, never silently dropped.
2. **Name** (defaults to the host), **Host / IP**, **Port** (22), **Username**.
3. **Environment** (optional): Development, Staging, Production, or your own label and colour.
4. **Sign in with:** a segmented control **Password | Key file | SSH agent**.
   - Password: stored in the OS vault; a "show" eye.
   - Key file: a drop zone ("Drop a .pem, id_rsa, id_ed25519 or .ppk here, or browse") plus a path box. Passphrase asked only if needed. Shows key type and fingerprint once loaded; picks up a matching `-cert.pub` certificate automatically.
   - SSH agent: detects which agent is running and shows its keys; you can pick one or let the server choose.
   - One-time codes and other keyboard-interactive prompts are handled at connect time, whatever is chosen here.
5. **Reach this server through:** **Direct** (default) | **Jump host** | **Proxy command**.
   - **Jump host:** pick a saved server, or type `user@bastion:22`. "Add another hop" builds a chain. Each hop shows its own sign-in method, so a bastion with a key and a target with a password both work.
   - **Proxy command:** a text box with token help (`%h`, `%p`, `%r`). The exact command is shown before it runs the first time.
6. **Keep my work if the connection drops (tmux):** a visible switch, on by default. On save or test the app checks the server:
   - **tmux is there:** the card shows "tmux ✓" and every connect attaches to your session.
   - **tmux is missing:** "tmux isn't installed on this server" with an **Install tmux** button. The app detects the package manager (`apt`, `dnf`, `yum`, `zypper`, `apk`, `pacman`) and installs with sudo, streaming the output into a small log.
   - **Can't install, or switched off:** a plain shell, and the card says work won't survive a drop.
   - The session name (default `work`) is editable.
   - tmux runs on the app's own socket with its own settings file, which loads your `~/.tmux.conf` first, so your personal tmux setup is never edited (see Server setup).
7. **sudo on this server:** **Asks for my password** (default) | **No password needed** | **Not available**. Used only for actions you start (tmux install, uploads into root-owned folders). With the default, the saved login password is sent on sudo's standard input after a one-time confirm.
8. **Advanced (collapsed):** keepalive interval, compression, agent forwarding (off), legacy device algorithms (off), known_hosts file, environment variables to send.
9. **Test connection** and **Save & connect**. Test does the full handshake, hop by hop, and reports in plain words: which hop failed, wrong password, host unreachable ("Is your VPN connected?"), host key needs trusting, key rejected, tmux missing.

## Screen 3: Terminal (harness-first)

### What Claude Code and other full-screen apps need from a terminal

| Protocol / behaviour | Why it matters | Plan |
| --- | --- | --- |
| **Synchronized output** (DEC private mode 2026) | Apps that redraw large parts of the screen at once (Claude Code, editors) otherwise show half-drawn frames. | Supported by `alacritty_terminal`. The renderer holds the frame until the update ends, with a 150 ms safety timeout. |
| **Bracketed paste** (mode 2004) | Large pastes arrive as one paste, not as typed keys. | Always used when enabled; large pastes chunked without breaking the brackets. |
| **Kitty keyboard protocol** (CSI u) | Lets apps tell `Shift+Enter` from `Enter`, `Ctrl+I` from `Tab`. Claude Code uses `Shift+Enter` for a new line. | Our key encoder supports the kitty flags plus classic xterm encodings. |
| **Focus events** (mode 1004) | Apps know when you switch away and back. | Supported. |
| **SGR mouse** (mode 1006) | Clicking and scrolling inside TUIs. | Supported. `Shift`+drag always selects locally. |
| **OSC 52 clipboard** | Copying on the server lands in your desktop clipboard. | Write-only: the server can set your clipboard but never read it. |
| **OSC 8 hyperlinks** | Clickable links and file paths. | `Ctrl/Cmd+click` opens URLs; remote paths open in the file manager. |
| **OSC 9 / OSC 777 notifications, BEL** | "Claude needs your input" while you're in another tab. | Native notification plus an unread dot on the tab. |
| **OSC 7 / OSC 133** | Current folder and prompt start/end. | Drops, "follow folder", jump-to-previous-prompt. |
| **Truecolor, Unicode width, emoji, box drawing** | Rich TUIs use all of these. | 24-bit colour; correct widths; colour emoji via font fallback; box-drawing drawn as pixel-exact shapes so borders never have gaps. |
| **Fast, correct resize** | TUIs reflow on resize. | One window-change message per frame; text reflows locally at once. |

Each item is checked against current Claude Code, vim/neovim, htop and less in milestone 3. Claude Code's use of mode 2026 and the kitty protocol is taken from its public behaviour.

### Speed: where the lag in PuTTY comes from, and the fixes

- **Network:** `TCP_NODELAY` on (no 40 ms Nagle delay on keystrokes). Keystrokes are sent at once, never queued behind output. Round-trip time is shown in the header.
- **Parsing off the UI thread:** SSH bytes are parsed on a background task into the grid; the UI thread only reads what changed when it draws.
- **Draw only what changed:** per-line damage tracking, a GPU glyph atlas, no redraw when idle.
- **Frame pacing:** at most once per display refresh, and immediately after a keystroke echo arrives.
- **tmux tuned for TUIs** (Server setup): `escape-time 0` removes the Esc delay; `extended-keys` and `terminal-features` pass kitty keys, clipboard, sync and hyperlinks through.
- **Measured, not guessed:** vtebench, a loopback latency harness, and a recorded Claude Code session replayed for flicker checks.

### Copy and paste that always works

- **Select, then a small popup:** a pill above the selection: **Copy · Paste · Search · Open** (Open for links and paths). It fades after a few seconds or when you type.
- **Hover popup:** resting the mouse still for about 600 ms with nothing selected shows a small **Paste** pill when the clipboard has something; over a link or path it shows **Open · Copy**. Can be turned off.
- **Keys:** platform-native. Windows/Linux: `Ctrl+C` copies when text is selected, otherwise sends interrupt; `Ctrl+V`, `Ctrl+Shift+V` and `Shift+Insert` paste. macOS: `Cmd+C` / `Cmd+V`. Linux also supports middle-click paste of the primary selection.
- **Right-click:** Copy, Paste, Paste as file (upload), Select all, Clear. A PuTTY-style mode (right-click pastes, select copies) is one switch.
- **Smart copy:** wrapped lines rejoined, trailing spaces trimmed, `Alt`+drag for rectangles, double-click a word or path, triple-click a line.
- **Paste safety:** multi-line paste at a shell prompt shows a preview; no prompt when the program uses bracketed paste.
- **Images:** a screenshot on the clipboard becomes an uploaded file path (see "Feels local").

### Scrollback and tmux

- With tmux, it runs with `mouse off`, so the app owns selection and the mouse wheel.
- 100,000 lines of app-side scrollback per session.
- On reattach, earlier history is preloaded with `tmux capture-pane -p -e -S -50000`.
- **Later:** tmux control mode (`tmux -CC`, as iTerm2 uses) for native scrollback and splits backed by tmux.

### Feels local: dropping and pasting files into a remote session

**Why it fails in other terminals.** Locally, dropping a file types its local path and the program opens it; pasting a screenshot works because the program reads the local clipboard. Over SSH the path points at your computer and the remote program can't see your clipboard.

**The fix.** The app copies the file to the server over SFTP first, then acts on the server-side copy, depending on what's in the foreground:

| Foreground | Mode | What happens on drop |
| --- | --- | --- |
| A shell prompt (bash, zsh, fish, sh, ash, PowerShell) | **Put it here** | Upload into the shell's current folder (for example `/opt`). |
| Any other program (Claude Code, vim, python, an installer asking for a path) | **Hand it to the program** | Upload to the drop folder, then type the remote path(s), quoted, as a bracketed paste. |

Works for any program that takes file paths. While dragging, an overlay says "Upload to **/opt**" or "Give **claude** the file"; `Alt` flips the mode, `Shift` asks for a folder; a per-server setting picks Automatic / Always upload here / Always paste a path.

**How the app knows the foreground program and folder:**
1. **tmux:** a hidden second channel runs `tmux display -p -t <pane> '#{pane_current_command}|#{pane_current_path}'` when the drag enters the window.
2. **Shell integration (no tmux):** an optional one-click snippet per shell that reports the folder with OSC 7 and prompts with OSC 133.
3. **Fallback:** `readlink /proc/<pid>/cwd` of the login shell on Linux servers; else upload to home and say so.

**Drop folder:** `~/.cache/sshchat/drops/<date>/<file>`, permissions `0700`, cleaned after 7 days.

**Screenshot paste:** with an image and no text on the clipboard, the app saves a PNG, uploads it and types the path. Claude Code treats a pasted image path as an image attachment (confirm in milestone 3).

**Root-owned folders like /opt:** the overlay shows a lock; "Upload with sudo" stages the file, then runs `sudo install -m 0644 <staged> /opt/<name>` according to the server's sudo setting; without sudo it offers the home folder; existing files always ask Replace / Keep both / Skip.

**Also:** folders upload recursively with one progress bar and Cancel; right-click a path, then **Download**.

## Screen 4: File manager (SFTP)

Opens beside the terminal (or full width), per session, over the same SSH connection (including through jump hosts).

- **Follows the terminal** via OSC 7 or tmux (on by default), or browses freely.
- **Fast lists:** virtualized rows, streamed directory reads, sort and filter as you type.
- **Breadcrumb path bar** with typing and `Tab` completion.
- **Actions:** upload (button or drag from the desktop), download, new folder/file, rename (`F2`), delete (confirm), copy path, permissions, owner, "Open terminal here".
- **Transfer queue:** progress, speed, time left, pause/resume/cancel; parallel transfers with many SFTP requests in flight; resume after interruption.
- **Root-owned folders:** same sudo flow as drops.
- **Preview:** text, logs and images in place.
- **Later:** edit remotely in your local editor with auto-upload; drag out to the desktop; two-pane local/remote view.

## Connection, vault and reconnect

- **Vault:** passwords and key passphrases only in the OS vault (Credential Manager, macOS Keychain, Secret Service on Linux) via `keyring`, target `sshchat:<user>@<host>:<port>`. Never in settings files, logs or command lines; wiped from memory after use. Linux without a Secret Service: the app says so and asks each time rather than storing in plain text.
- **Never lose work (per server, optional):** with the tmux switch on, every terminal attaches with `tmux -L sshchat -f ~/.config/sshchat/tmux.conf new-session -A -s <name>`; falls back to `screen -dRR <name>`, then a plain shell, and says so.
- **Reconnect:** keepalive plus the OS's network-change and sleep/resume events. During a drop the terminal stays visible, dimmed, with "Connection lost. Waiting for the network…", and typing is held back. When the path is back (network-change event plus a TCP probe of the first hop, backing off 2 s to 60 s), sessions reconnect, staggered, sign in silently and reattach to tmux. The app only stops to ask when a password is rejected (tried once, to avoid lockout), a one-time code is needed, or a host key changed.
- **VPN integrations (optional):** a small documented local protocol, a socket or named pipe sending `{"state":"connected"|"reconnecting"|"down"}` lines, lets any VPN client tell the app the tunnel is back, for an instant reconnect instead of probing. `forti-clone` is the first implementation; others can add it.

## Architecture

One Cargo workspace, one binary per platform.

| Crate | Responsibility |
| --- | --- |
| `term-core` | Wraps `alacritty_terminal`: grid, parser, modes, key encoder (kitty + xterm), selection and smart copy, OSC handlers, scrollback preload. No UI code. |
| `term-view` | GPUI element that draws the grid: glyph atlas, damage tracking, synchronized-output frame holding, cursor, selection, popups, drop overlay. |
| `ssh-core` | `russh` sessions; auth methods and agents; transports (direct TCP, jump-host chain over `direct-tcpip`, proxy command over stdio) behind one trait; keepalive; reconnect state machine; tmux check/install/attach. |
| `ssh-config` | Reads `~/.ssh/config` (via `ssh2-config`, with our handling for `Include` and unsupported lines) and `known_hosts` (hashed entries, markers, ports). |
| `sftp` | `russh-sftp` client, pipelined transfer queue, resume, sudo staging. |
| `drop` | Foreground and folder detection, drop folder, clipboard image to PNG, path quoting. |
| `vault` | `keyring` wrapper. |
| `store` | Servers, groups, environments and settings in TOML (no secrets); export/import. |
| `net-watch` | Network-change and sleep/resume events, TCP probes, VPN status protocol. |
| `platform` | The only crate with OS-specific code (see "Cross-platform"). |
| `app` | GPUI windows: home, connect dialog, session tabs, terminal, file manager, settings. |

**Threads:** UI on GPUI's main thread; all network and parsing on a tokio runtime; each session's grid behind a lock held for microseconds; the UI snapshots changed lines once per frame. No network call ever runs on the UI thread.

## Server setup (done by the app)

The tmux switch checks for tmux, offers to install it, and uploads the app's tmux settings. For reference:

```
sudo apt install tmux          # or: dnf / yum / zypper / apk / pacman
```

`~/.config/sshchat/tmux.conf` (written by the app; used only for the app's sessions on the `-L sshchat` socket):

```
source-file -q ~/.tmux.conf            # your own settings first
set -g history-limit 50000
set -g mouse off                       # the app owns selection and scrolling
set -g status off
set -sg escape-time 0                  # no Esc delay; TUIs feel instant
set -g focus-events on
set -g set-clipboard on                # copy on the server reaches your clipboard (OSC 52)
set -g default-terminal "tmux-256color"
set -g extended-keys on                # pass Shift+Enter etc. through
set -as terminal-features 'xterm*:RGB:clipboard:extkeys:focus:hyperlinks:sync'
```

Option names are from tmux's documentation as remembered for 3.2–3.7; verify against `man tmux` in milestone 3. On older tmux the app skips lines that version doesn't know.

## Cross-platform

A real goal, not a later port: 1.0 ships on Windows, macOS and Linux. Development leads on Windows, and CI keeps the other two green from the first commit.

- **All OS-specific code lives in one crate, `platform`,** with `windows`, `macos` and `linux` modules behind small traits. No other crate uses `cfg(target_os)` or calls OS APIs.

| Concern | Windows | macOS | Linux |
| --- | --- | --- | --- |
| Secrets (`keyring`) | Credential Manager | Keychain | Secret Service (GNOME Keyring / KWallet) |
| SSH agent | `\\.\pipe\openssh-ssh-agent`, Pageant | `SSH_AUTH_SOCK` | `SSH_AUTH_SOCK` |
| Network-change events | `NotifyIpInterfaceChange` | `NWPathMonitor` | netlink route/link events |
| VPN status protocol | Named pipe | Unix socket | Unix socket |
| Clipboard (`arboard`) | Win32 clipboard | NSPasteboard | X11 / Wayland, plus primary selection |
| Drag files out (Later) | OLE `IDataObject` | NSPasteboard file promises | XDND / Wayland DnD |
| Local shell for the renderer spike | ConPTY | `openpty` | `openpty` (via `alacritty_terminal`'s tty module) |
| Sleep/resume events | `WM_POWERBROADCAST` | `NSWorkspace` notifications | logind `PrepareForSleep` |
| Notifications | Toasts | User Notifications | freedesktop notifications |
| Paths (`directories`) | AppData | Application Support | XDG dirs |
| Packages | MSI and winget | Signed, notarized `.dmg` and Homebrew cask | AppImage, `.deb`, `.rpm`, Flatpak |

- **Shortcuts are actions in one keymap file,** mapping `Ctrl` on Windows/Linux to `Cmd` on macOS, and following each platform's conventions.
- **Wayland and X11** are both tested on Linux.
- **UI text says "your computer" and "your clipboard",** never one OS's names, except on OS-specific screens.

## Open source

### Licence

**Recommendation: dual licence, MIT OR Apache-2.0,** the Rust ecosystem's default.
- It's compatible with every dependency here: GPUI (`gpui-pre`), `gpui-component`, `alacritty_terminal`, `russh` and `russh-sftp` are Apache-2.0; most others are MIT or MIT/Apache; `nucleo-matcher` is MPL-2.0 (file-level copyleft, fine to use unmodified).
- Apache-2.0 includes a patent grant; MIT keeps it simple for those who prefer it.
- Contributions come in under the same dual licence via the standard "inbound = outbound" note in CONTRIBUTING.md (no CLA). Commits are signed off (DCO).
- Release binaries include all third-party licence texts, generated with `cargo-about`. `cargo-deny` in CI blocks dependencies with incompatible licences.
- Alternative if you want to stop closed-source forks: GPL-3.0. Possible with these dependencies, but it narrows who can reuse the crates, so it's not recommended here.
- **Name and trademarks:** pick an original name and logo; don't use "PuTTY" or any other product's name or artwork.

### Repository layout

```
/
├─ crates/
│  ├─ app/          term-core/   term-view/   ssh-core/   ssh-config/
│  ├─ sftp/         drop/        vault/       store/      net-watch/   platform/
├─ docs/
│  ├─ ARCHITECTURE.md            how the crates fit, threading, data flow
│  ├─ COMPATIBILITY.md           the SSH compatibility table, kept current
│  ├─ PERFORMANCE.md             budgets, how to run benchmarks
│  └─ design/                    UI specs and screenshots
├─ benches/                      vtebench runner, latency harness, recorded sessions
├─ tests/compat/                 Docker sshd matrix (OpenSSH, Dropbear, Alpine, jump chain)
├─ fuzz/                         cargo-fuzz targets (ssh config, known_hosts, key files, OSC parsing)
├─ i18n/                         Fluent translation files
├─ .github/                      CI workflows, issue and PR templates, CODEOWNERS
├─ README.md   CONTRIBUTING.md   CODE_OF_CONDUCT.md   SECURITY.md   CHANGELOG.md
├─ LICENSE-MIT   LICENSE-APACHE
└─ rust-toolchain.toml   deny.toml   Cargo.toml
```

### Making it easy to contribute

- **Every crate builds and tests on its own** with plain `cargo test`; only `app` needs a GPU. New contributors can work on `ssh-config` or `term-core` without touching UI code.
- **CONTRIBUTING.md:** setup per OS in five commands, how to run the compat matrix locally (`docker compose up`), coding conventions, and how to file a good bug (with a "copy diagnostics" button in the app that collects version, OS, GPU and redacted logs).
- **ARCHITECTURE.md** kept current; every crate has a short top-level doc comment.
- **Labels:** `good first issue`, `help wanted`, `compat` (a server setup that doesn't work), `perf`, `ux`, per-platform labels.
- **Issue templates:** bug, feature, "my server doesn't connect" (asks for `ssh -vvv` output and the app's redacted log).
- **CI on every PR (GitHub Actions):** `cargo fmt --check`, `cargo clippy -D warnings`, tests on Windows/macOS/Linux, the Docker compat matrix, `cargo-deny` (licences, advisories, duplicates), fuzz smoke runs, and performance benchmarks that fail the PR on a regression beyond 5%.
- **MSRV policy:** latest stable Rust minus two releases, stated in the README.
- **Releases:** `cargo-dist` builds signed installers for all three OSes from a tag, with checksums and release notes from CHANGELOG.md; in-app update check against GitHub releases (off-switch in Settings).
- **SECURITY.md:** private vulnerability reporting through GitHub, response targets, and a list of what's in scope (auth, host keys, vault, OSC handling, proxy commands).
- **Governance:** start with you as maintainer and a `CODEOWNERS` file; add maintainers as regular contributors appear.

### Built for everyone

- **Privacy:** no telemetry. Crash reports are saved locally and only sent if the user chooses to attach one to an issue.
- **Accessibility:** keyboard-only use of every screen, screen-reader labels, high-contrast theme, adjustable font size; checked before 1.0.
- **Translations:** all UI strings in Fluent files from the start; English first, community translations welcome.
- **Beginner and expert paths:** the connect dialog stays simple by default, with Advanced collapsed; experts can live entirely in `~/.ssh/config` and `Ctrl+K`.

## Security

- Secrets only in the OS vault; never in files, logs or command lines; zeroed after use. Logs redact hosts and usernames when shared via "copy diagnostics".
- Strict host keys; reconnect never weakens them; known_hosts shared with OpenSSH by default.
- Agent forwarding off by default, per server, with a warning.
- Proxy commands come only from the user's own saved servers or `~/.ssh/config`, and the exact command is shown before its first run. Imported server lists never carry proxy commands silently; they're flagged for review.
- OSC 52 is write-only; terminal reports that could leak data back to the server are answered safely or ignored.
- Drop staging is private (`0700`) and cleaned up. sudo only per the server's sudo setting, only for actions the user started, password on standard input, never a silent overwrite.
- One-time codes are never stored.
- Untrusted input parsers (SSH config, known_hosts, key files, OSC sequences, SFTP listings) are fuzzed in CI.

## Recommended versions

### Toolchain

| Tool | Version | Notes |
| --- | --- | --- |
| Rust | 1.99.0 stable | Pin in `rust-toolchain.toml` |
| Edition | 2024 | |
| Targets | `x86_64-pc-windows-msvc`, `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` | arm64 Windows later |
| `cargo-dist` | 0.32.0 | Release builds and installers for all OSes |
| `cargo-deny` | 0.20.2 | Licence, advisory and duplicate checks in CI |
| `cargo-about` | 0.9.2 | Third-party licence notices in release builds |

### Crates

| Crate | Version | Licence | Used in | Purpose |
| --- | --- | --- | --- | --- |
| `gpui-component` | 0.7.1 (pin `=`) | Apache-2.0 | app, term-view | Widgets, theming |
| `gpui-pre` | 0.3.8 (pin `=`) | Apache-2.0 | app, term-view | GPUI (the Zed snapshot `gpui-component` builds on; not the older `gpui` 0.2.2) |
| `alacritty_terminal` | 0.26.0 | Apache-2.0 | term-core | VT parser, grid, modes |
| `russh` | 0.64.1 | Apache-2.0 | ssh-core | SSH client (pin; API moves between minor versions) |
| `russh-sftp` | 3.0.1 | Apache-2.0 | sftp | SFTP client |
| `ssh-key` | 0.6.7 | Apache-2.0 OR MIT | ssh-core, app | Key parsing, fingerprints, certificates (0.7 is in release candidates; move when stable) |
| `ssh2-config` | 0.8.1 | MIT | ssh-config | `~/.ssh/config` parsing |
| `shlex` | 2.0.1 | MIT OR Apache-2.0 | app | Parse pasted `ssh …` commands |
| `nucleo-matcher` | 0.3.1 | MPL-2.0 | app | Fuzzy search |
| `keyring` | 4.2.0 | MIT OR Apache-2.0 | vault | OS vault (4.x API differs from 3.x) |
| `zeroize` | 1.9.0 | MIT OR Apache-2.0 | vault, ssh-core | Wipe secrets |
| `arboard` | 3.6.1 | MIT OR Apache-2.0 | term-view, drop | Clipboard text and images |
| `image` | 0.25.10 | MIT OR Apache-2.0 | drop, app | Clipboard PNG, previews |
| `rfd` | 0.17.2 | MIT | app | Native file pickers |
| `shell-escape` | 0.1.5 | MIT OR Apache-2.0 | drop | Quote remote paths |
| `fluent-bundle` | 0.16.0 | MIT OR Apache-2.0 | app | Translations |
| `tokio` | 1.53.2 | MIT | all | Async runtime |
| `windows` | 0.62.2 | MIT OR Apache-2.0 | platform (Windows) | Network, power, OLE |
| `serde` / `serde_json` | 1.0.229 / 1.0.151 | MIT OR Apache-2.0 | all | Serialization |
| `toml` | 1.1.6 | MIT OR Apache-2.0 | store | Settings |
| `directories` | 6.0.0 | MIT OR Apache-2.0 | store | Per-OS paths |
| `uuid` | 1.27.0 | MIT OR Apache-2.0 | store | Ids |
| `memchr` | 2.8.3 | MIT OR Unlicense | term-core | Fast byte scanning |
| `tracing` / `tracing-subscriber` | 0.1.44 / 0.3.23 | MIT | all | Logging |
| `anyhow` / `thiserror` | 1.0.104 / 2.0.21 | MIT OR Apache-2.0 | app / libs | Errors |

Licences for the main crates were read from crates.io; those for the smaller utility crates are from memory and are confirmed by `cargo-deny` on the first build. Fallback UI stack: `iced` 0.14.0 (MIT) with `iced_term` 0.8.0 (MIT). Commit `Cargo.lock`.

### Server side

| Tool | Version | Notes |
| --- | --- | --- |
| tmux | 3.7 (2026-06-26); 3.2+ for `terminal-features` and `extended-keys` | Optional, per server |
| OpenSSH server | 7.4 or newer | Oldest version in the compat matrix |

## Milestones

0. **Project setup:** name, repository, licence files, README, CONTRIBUTING, CODE_OF_CONDUCT, SECURITY, CI skeleton on three OSes with fmt/clippy/deny.
1. **Renderer spike (local, no network):** GPUI drawing an `alacritty_terminal` grid fed by a local shell (ConPTY / openpty). Hit the latency, frame and throughput budgets on Windows, then macOS and Linux; high-DPI, multiple monitors, IME, accessibility check. Decide GPUI vs Iced.
2. **SSH terminal:** `ssh-core` with password, key, agent and keyboard-interactive; known_hosts; terminal over SSH; tmux attach; RTT in the header.
3. **Harness-first acceptance:** Claude Code, neovim, htop and less on a server against the protocol table; tune tmux settings; a full-day soak.
4. **Copy/paste UX:** popups, smart copy, platform-native keys, right-click, paste preview.
5. **Connect anywhere:** jump-host chains, proxy commands, `~/.ssh/config` live reading, certificates, legacy switch; the Docker compat matrix in CI.
6. **Servers and connect dialog:** home list, search, environments, probes, quick-connect parsing, tmux switch with install, sudo setting, Test hop by hop; export/import.
7. **File manager:** browse, follow terminal, actions, pipelined queue with resume, sudo flow, preview.
8. **Feels local:** drops in both modes, screenshot paste, root-folder flow.
9. **Reconnect:** drop detection, held input, reattach with scrollback preload, VPN status protocol (with forti-clone as first client).
10. **Polish and 1.0:** themes, translations framework, accessibility pass, installers for all three OSes via cargo-dist, code signing and notarization, update check.

## Done when (1.0)

- The app opens to a usable server list in under 250 ms on all three OSes; typing a name and Enter connects.
- Pasting `ssh -i key.pem -p 2222 -J admin@bastion user@host` fills every field, and connecting through the bastion works first time.
- Every host in an existing `~/.ssh/config` appears and connects the same way `ssh <host>` does, including `ProxyJump`, `ProxyCommand`, `IdentityFile` and `Include`.
- The Docker compat matrix passes: current OpenSSH, OpenSSH 7.4, Dropbear, Alpine, two-hop jump, OTP prompt, agent, certificate.
- A YubiKey user signs in through their agent; a 1Password SSH-agent user signs in with no extra setup.
- Saving a server without tmux offers **Install tmux**; one click (plus the sudo confirm) installs it, and your own `~/.tmux.conf` is untouched.
- Claude Code on the server: no flicker, `Shift+Enter` makes a new line, a 500-line paste arrives as one paste, typing feels like a local terminal on a low-latency link.
- Copy and paste work by popup, keys, right-click and OSC 52, using each platform's conventions.
- Dropping an image onto Claude Code, or pasting a screenshot, attaches the image.
- At a shell prompt in `/opt`, dropping a file puts it in `/opt` (one sudo confirm the first time).
- The file manager follows `cd`, opens a 100,000-file folder smoothly, and a 1 GB download reaches at least 80% of `scp` speed.
- Network off for 10 minutes, then on: every session is back in tmux within 10 s, with scrollback, and no prompt.
- A new contributor can clone, build and run the tests on any of the three OSes by following CONTRIBUTING.md alone.

## Open questions

Resolved by Priyansh on 2026-10-06:
- Open-source project for all users, not a personal tool.
- Jump hosts and proxy commands are in v1, since other users will need them.
- tmux: optional per server, with one-click install when missing.
- sudo: available on Priyansh's servers; the per-server default is "asks for my password".
- Cross-platform: Windows, macOS and Linux, with Windows leading development.
- Company policy allows a third-party SSH client.

Still open:
- Project name (needed before the repository goes public).
- Licence: MIT OR Apache-2.0 recommended; confirm, or choose GPL-3.0 if stopping closed-source forks matters more.
- Where the repository lives (personal GitHub account or a new organization).
