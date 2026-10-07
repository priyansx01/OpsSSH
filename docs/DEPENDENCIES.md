# Dependency review: local renderer spike

Reviewed 2026-10-07 using Rust 1.99.0 and cargo-deny 0.20.2. Direct third-party
versions are pinned, including matching GPUI core/platform 0.3.8 and component
0.7.1. Cargo.lock fixes the transitive graph across platform targets and tools.
The russh optional RSA feature is disabled because the current RustCrypto RSA
implementation has a timing side-channel advisory; RSA private keys are not
supported. Prefer Ed25519 or another supported non-RSA key type.

## Audit policy

Cargo-deny checks all features. Sources must come from crates.io; wildcard
versions are denied; duplicate transitive versions remain warnings. The license
allowlist includes CC0-1.0 for hexf-parse/tiny-keccak, bzip2-1.0.6 for
libbz2-rs-sys and 0BSD for enum-iterator, after reviewing their declared licenses.
Distribution still requires generating and including third-party notices.

The initial audit found maintenance advisories, not reported vulnerabilities,
for these dependencies, with no patched versions in the advisory database:

| Advisory | Dependency | Upstream path |
| --- | --- | --- |
| [RUSTSEC-2024-0436](https://rustsec.org/advisories/RUSTSEC-2024-0436) | paste 1.0.15 | GPUI and pulp/image |
| [RUSTSEC-2026-0206](https://rustsec.org/advisories/RUSTSEC-2026-0206) | rustybuzz 0.20.1 | GPUI -> usvg |
| [RUSTSEC-2026-0192](https://rustsec.org/advisories/RUSTSEC-2026-0192) | ttf-parser 0.25.1 | GPUI, fontdb, rustybuzz |
| [RUSTSEC-2024-0384](https://rustsec.org/advisories/RUSTSEC-2024-0384) | instant 0.1.13 | GPUI component 0.7.1 |
| [RUSTSEC-2025-0134](https://rustsec.org/advisories/RUSTSEC-2025-0134) | rustls-pemfile 2.2.0 | GPUI optional wasm HTTP client; not used on desktop |

`deny.toml` contains three advisory-specific, reasoned exceptions so development
checks can pass. RSA is disabled rather than excepted. These maintenance
exceptions do not disable vulnerability checks or future advisories.
They are a maintenance risk, not evidence that the dependencies are safe forever.

## Release gate

Recheck the advisories and upstream replacements at every GPUI upgrade and before
renderer acceptance. Review no later than 2026-11-06. Remove the exceptions when
upstream adopts maintained alternatives; otherwise record an explicit release
maintenance decision. No production release is accepted by this spike review.
Generate third-party notices for the actual packaged platform/features before
shipping binaries. Do not replace pinned parser/font crates through blind patches
without verifying API and rendering behavior.
