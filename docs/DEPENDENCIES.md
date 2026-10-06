# Dependency review: local renderer spike

Reviewed 2026-10-06 using Rust 1.99.0 and cargo-deny 0.20.2. Direct third-party
versions are pinned, including matching GPUI core/platform 0.3.8. Component 0.7.1
uses that same core but is not included because the spike needs no component kit.
Cargo.lock fixes the transitive graph across platform targets and capture tools.

## Audit policy

Cargo-deny checks all features. Sources must come from crates.io; wildcard
versions are denied; duplicate transitive versions remain warnings. The license
allowlist includes CC0-1.0 for hexf-parse/tiny-keccak and bzip2-1.0.6 for
libbz2-rs-sys, after reviewing their declared licenses and packaged notices.
Distribution still requires generating and including third-party notices.

The initial audit found maintenance advisories, not reported vulnerabilities,
for these dependencies, with no patched versions in the advisory database:

| Advisory | Dependency | Upstream path |
| --- | --- | --- |
| [RUSTSEC-2024-0436](https://rustsec.org/advisories/RUSTSEC-2024-0436) | paste 1.0.15 | GPUI and pulp/image |
| [RUSTSEC-2026-0206](https://rustsec.org/advisories/RUSTSEC-2026-0206) | rustybuzz 0.20.1 | GPUI -> usvg |
| [RUSTSEC-2026-0192](https://rustsec.org/advisories/RUSTSEC-2026-0192) | ttf-parser 0.25.1 | GPUI, fontdb, rustybuzz |

`deny.toml` contains three advisory-specific, reasoned exceptions so development
checks can pass. These do not disable vulnerability checks or future advisories.
They are a maintenance risk, not evidence that the dependencies are safe forever.

## Release gate

Recheck the advisories and upstream replacements at every GPUI upgrade and before
renderer acceptance. Review no later than 2026-11-06. Remove the exceptions when
upstream adopts maintained alternatives; otherwise record an explicit release
maintenance decision. No production release is accepted by this spike review.
Generate third-party notices for the actual packaged platform/features before
shipping binaries. Do not replace pinned parser/font crates through blind patches
without verifying API and rendering behavior.
