# Architecture

## Current implementation

```text
app: GPUI workspace + diagnostics/benchmarks
  platform: native application, shell defaults, ConPTY/openpty
  term-view: GPUI terminal, local session workers, frame scheduler
  term-core: Alacritty adapter, snapshots, selection, events, input encoder
```

Published `gpui-pre` and `gpui-pre-platform` 0.3.8 are pinned together.
`gpui-component` 0.7.1 was checked to use this same GPUI version, but is not
needed for the spike. Alacritty terminal 0.26.0, portable-pty 0.9.0, and
async-channel 2.5.0 are pinned; transitive versions are in Cargo.lock.
OS conditionals stay in `platform`; GUI dependencies can be disabled for tools.

## Data flow

```text
PTY reader worker -> Alacritty parser under a shared lock
  -> shared changed-row snapshot + bounded event notices -> GPUI display callback
Keyboard / IME / paste -> connected-state gate -> encoder -> bounded writer queue
Writer/control worker -> PTY input / resize / child-exit polling
Terminal side effects -> view policy -> clipboard write / title / bell
```

Process creation, PTY reads/writes, and normal output parsing run off the UI thread.
Snapshot extraction and synchronization-expiry flushing currently acquire the
backend lock on the UI thread; lock duration needs profiling before acceptance.
Output bytes are never discarded. Redundant frame notices may be coalesced.
Each paste is one ordered writer command. A saturated input queue reports an
error rather than blocking the UI or retaining input for replay.

Alacritty damage drives immutable shared row reuse; GPUI shapes changed rows
and paints a fixed cell grid. Display callbacks coalesce output. A one-shot timer
flushes unterminated synchronized output after 150 ms even without new bytes.
There is no permanent redraw timer or cursor blink yet. Mouse reports and focus
sequences are conditional on terminal modes; Shift enables local selection.

OSC 52 queries are disabled at the emulator. Clipboard writes, titles, and bells
are explicit side effects. No clipboard contents are returned to the child.
Session destruction cancels workers and delegates process termination/joining to
a cleanup thread. Blocking shutdown is available for integration tests.

Public interfaces are Rust APIs, not a network protocol or stable plugin ABI.
The backend and session interfaces may change during acceptance work.

## Subsequent crates

Introduce `ssh-core`, `ssh-config`, `vault`, `store`, `sftp`, `drop`, and `net-watch`
at their roadmap stages. SSH will feed the same terminal backend/view.
Host-key and authentication prompts must be explicit events; reconnect must not
relax verification. Distinct tabs use distinct tmux identities.

Tmux history preload belongs in historical display rows, not the live escape
parser. File drops require shell-aware quoting; resume requires transfer identity
checks. These are design requirements, not implemented behavior.
