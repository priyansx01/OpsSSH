# Parser fuzzing

On Linux with a nightly Rust toolchain and cargo-fuzz 0.13.1:

```sh
cargo +nightly fuzz run terminal_osc -- -max_total_time=60 -max_len=65536
cargo +nightly fuzz run commands_and_paths -- -max_total_time=60 -max_len=16384
```

The terminal target feeds arbitrary fragmented bytes through the real VT parser
with bounded scrollback. The second target exercises the non-executing SSH command
parser, literal path quoting/joining and strict VPN status messages. No fuzzer
executes proxy commands, connects to a server or reads a user's SSH config.

The CI smoke workflow is a short regression check, not a completed security audit.
Dedicated known_hosts, certificate/key-format and raw SFTP listing fuzz targets
remain outstanding. Crashes and corpora are ignored locally and must be reviewed
for secrets before being shared, even though these targets use generated input.
