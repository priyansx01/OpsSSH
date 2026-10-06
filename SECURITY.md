# Security policy

OpsSSH is pre-release and does not currently implement SSH connections or store
credentials. It is not ready for production use. Only the latest development
revision is supported during this stage.

Security scope includes future authentication, host verification, vault access,
terminal escape handling, clipboard behavior, proxy commands, file transfers,
and privileged remote actions.

Do not open a public issue containing an exploit or any credentials. Before
publication, the maintainer must enable GitHub private vulnerability reporting
and publish the repository's private reporting link here. No public reporting
endpoint exists yet; do not send reports to an unverified address.

For the public project, target acknowledgement within 3 business days and a
triage update within 7 days. Release fixes and advisories according to impact.
Reports should contain a minimal reproduction, affected revision, platform, and
expected impact, using synthetic hosts and keys only.

Current security invariants:

- Non-connected sessions reject input rather than retaining it for replay.
- Bracketed paste rejects embedded escape/control introducers.
- Diagnostics contain no hostnames, usernames, environment variables, or paths.
- Terminal events describe side effects; a parser must not execute them directly.
