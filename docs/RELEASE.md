# Release readiness and development packages

`Build development packages` is a manual GitHub Actions workflow. It tests and
builds Windows x64, macOS arm64/x64 and Linux x64/arm64, includes the project and
third-party license texts using pinned cargo-about, emits SHA-256 checksums, and
uploads unsigned ZIP/tar.gz artifacts. It never publishes a GitHub release, updates a
package registry, installs certificates, or signs with developer credentials.

For a local package, install `cargo-about` 0.9.2, build with
`cargo build -p opsssh --release --locked --target <target>`, then run
`pwsh scripts/package.ps1 -Target <target>`. Existing staging directories are
rejected so old contents cannot accidentally enter a new package.

These archives are development artifacts, not 1.0 installers. Signing/notarization
requires the maintainer's Windows signing identity, Apple Developer certificates
and notarization credentials in protected release environments. No such credentials
are configured by this repository. MSI/DMG/AppImage/deb/rpm/Flatpak, cargo-dist
installer integration and installer upgrade/uninstall testing remain acceptance
work. Do not tag a 1.0 release based solely on a successful build.

Release acceptance must also attach the three-platform renderer/IME/accessibility
results, compatibility matrix, full-day TUI soak, measured latency/throughput/RAM,
dependency review and license report. Unit tests and fixture tests establish only
the behavior each test exercises; unmeasured performance budgets remain unverified.

The application has no telemetry. An update notifier must use explicit settings
and never execute downloaded code without verification; automatic update delivery
is not implemented by these package scripts.
