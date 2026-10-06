# Performance and acceptance

## Current executable measurements

```sh
cargo run --release -p opsssh -- bench 1000000
cargo run --release -p opsssh -- bench-parser 64
```

One JSON record contains schema version, OS/architecture, workload/iteration
count, encoded byte count, total input encoding nanoseconds, total scheduler
nanoseconds, and the number of simulated frames. Use release mode, the same
compiler, hardware, iteration count, and power profile for comparisons.

The simulated clock submits damage every 100 microseconds at 120 Hz. This
measures scheduler CPU cost, not real display frames. `gpu_measured` and
`vt_parser_measured` are explicitly false for `bench`.

`bench-parser` replays a deterministic block of ANSI truecolor ASCII lines into
Alacritty at 120x40 with history disabled. Its JSON records exact bytes, elapsed
nanoseconds, and MiB/s; `vt_parser_measured` is true and `gpu_measured` is false.
The argument selects approximately 1-1024 MiB. This omits snapshot extraction,
shaping, painting, PTY transport, Unicode shaping, and retained history costs.

The desktop logs its first display callback time for development. This is not a
GPU presentation/readiness measurement. Windows home/local-terminal frame exports
were visually inspected, but no GPU memory, SSH, or key-to-glyph budgets pass yet.

CI uploads this record for inspection. It does not fail PRs against noisy hosted
runner timings. Actual 5% regression checks require fixed benchmark machines
and a recorded baseline after the renderer spike.

## Renderer baseline protocol

Preserve the targets in `intent.md`, then measure at least 20 warmed runs and
report the median and p95 with raw artifacts. Record CPU/GPU/driver, OS, compiler,
screen refresh, DPI, font/size, terminal dimensions, workload hash, power profile,
and commit. Startup runs are cold process starts with a local fixture server list,
with cache conditions documented separately. Measure readiness at first usable
presented frame, not merely window construction.

Measure local echo and loopback SSH separately; report network RTT separately.
Use high-resolution input/echo/presentation timestamps and confirm display
latency with an external measurement when needed. A frame-rate average alone
does not prove the absence of missed frames.

Keep parser-only replay separate from live PTY/SSH 1 GB throughput. Exercise
text and full-screen workloads. Measure private/RSS memory with three sessions
and 100,000 populated scrollback lines, then the file manager when implemented.
Compare SFTP to `scp` on the same host, file, cipher, and link.

## Required spike scenarios

- 60/120/144 Hz output and scrolling; resize storms and monitor/DPI changes.
- Unicode, combining/wide characters, emoji and font fallback, box drawing.
- Correct full-frame output synchronization and unterminated-sequence timeout.
- No idle redraws beyond intentional cursor blinking; blink timers pause for
  hidden/inactive windows as appropriate.
- Keyboard/IME composition, focus, clipboard, mouse, selection, and accessibility.
- Shutdown during heavy output and a blocked reader/writer, with no orphan shell.
