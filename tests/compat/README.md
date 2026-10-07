# Disposable compatibility fixtures

These containers use a newly generated test key and the public fixture password
`fixture-only-not-a-secret`. Ports bind only to loopback. They never mount a user's
SSH directory or agent. Do not use the fixtures as public servers.

From the repository root, run `pwsh scripts/compat.ps1`. This generates an isolated
key in ignored `artifacts/compat/keys`, builds OpenSSH, Alpine/ash, Dropbear and a
bastion, and runs the probe named in the script. `-KeepRunning` keeps the containers
available for manual TUI testing; `docker compose -f tests/compat/compose.yml down`
stops them. Host keys are read from each isolated container, not trusted from an
unauthenticated network scan. Keys are fixture-only and regenerated only explicitly.

The in-process `cargo test -p opsssh-sftp --test protocol` exercises real SFTP packets,
300 KB transfers spanning multiple packets, listing, exclusive create, cancellation,
upload/download resume, changed remote metadata and incorrect partial contents.
It does not establish SSH authentication or measure LAN throughput.

The Docker set covers the packages available from Debian Bookworm and Alpine 3.23,
not the complete promised matrix. OpenSSH 7.4, OTP/PAM, certificate authentication,
FIDO/third-party agents, Windows sshd, two-hop chains and full-day TUI soak remain
separate acceptance jobs. Fixture images currently track distro packages; record
image digests with acceptance results before a release.
