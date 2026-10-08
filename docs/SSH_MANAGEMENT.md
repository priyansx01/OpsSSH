# SSH Management

Open a connected VM tab, then choose SSH Management. The registry covers the
authenticated account's default `~/.ssh/authorized_keys`, not all server users.
The page reports the authentication method that actually succeeded. Key and
agent logins protect the known current fingerprint from editing or removal.

Search names, fingerprints and types; inspect key restrictions; copy or export
a public key; import one `.pub` key; edit its comment; or confirm removal.
Private keys are rejected. Existing restrictions, comments, unknown lines,
duplicate rows, ordering and line endings are preserved. Changes compare the
loaded SHA-256 revision with the server file before saving. A conflict requires
Refresh and a reviewed edit rather than silently overwriting another change.

The one-shot Python helper runs on an independent SSH exec channel. It creates
private files, rejects symlinks, locks cooperating writers, saves the previous
document in `.authorized_keys.opsssh-backup`, and atomically replaces the file.
The backup holds one previous version. Documents are limited to 1 MiB. Arbitrary
external writers do not honor the helper's lock; revision revalidation narrows
that race but cannot coordinate every administrator's editor.

## Create a Linux user

Enter a new lowercase username, optional display name, public key and available
login shell. This action requires a root connection or explicit sudo authority.
Non-root connections offer a masked sudo password, or leave it empty to use
passwordless sudo. It is sent only through bounded, redacted, zeroizing exec stdin
and cleared from the form after submission. No credential is typed into the
interactive terminal, stored in profiles, or included in command diagnostics.

The helper uses `useradd -m` and `chpasswd`, installs the supplied public key and
sets private SSH directory/file permissions. It grants no sudo privileges. An
undisclosed random password avoids locked-account behavior that can prevent key
authentication; the app never issues that credential. Server authentication
policy still applies. This does not alter `sshd_config` or password-login policy.

If account creation succeeds but activation or key installation fails, the page
reports the phase and offers Repair for the returned username and UID. Repair
does not run useradd again and avoids reinstalling an identical key. Existing
accounts cannot be adopted through Create. No failure automatically deletes an
account or replays a mutation. Timeout/disconnection results require manual
verification on the server before retrying; reconnect cancels stale view work.

## Requirements and validation

Linux and Python 3 are required. User creation additionally needs `useradd`,
`chpasswd`, an available `/bin/bash` or `/bin/sh`, and root/sudo authorization.
Custom AuthorizedKeysFile paths, certificate-authority configuration and
administration of pre-existing users are deferred.

```sh
cargo test -p opsssh-ssh-management -p opsssh-ssh-core --all-features --locked
PYTHONDONTWRITEBYTECODE=1 python3 crates/ssh-management/tests/test_helper.py -v
```

Linux helper tests use temporary homes and mocked account commands; they do not
create or delete system accounts. They cover atomic file operations, ownership,
permissions, backups, stale revisions, symlinks, partial creation and repair.
SSH loopback tests cover authentication metadata, redacted stdin, EOF, input
limits and isolation from the terminal. A real disposable Linux server remains
necessary to validate distribution-specific useradd/PAM/sudo and sshd policy.

Capture fixtures use example data with remote actions disabled:
`snapshot-session-ssh-management`, `-empty`, `-denied`, `-add`, and `-create`.
