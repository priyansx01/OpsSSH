# OpsSSH mascot

The ruby terminal badger is the OpsSSH application mark. The editable source is
`opsssh-mascot.svg`; `opsssh.png` is embedded in the sidebar and `opsssh.ico`
contains 16, 24, 32, 48, 64, 128 and 256 pixel Windows icons. The Windows
executable embeds icon resource 1, which GPUI uses for the window and taskbar.
The MSI uses the same icon for its Start menu shortcut and installed-app entry.
Its welcome and completion artwork and progress banner use the mascot instead
of WiX's generic graphics. The generator also updates `installer-dialog.bmp`
(493 by 312) and `installer-banner.bmp` (493 by 58).

Regenerate the committed PNG and ICO after changing the SVG:

```powershell
python -m pip install pillow==12.3.0 resvg-py==0.5.0
python scripts/generate-branding.py
```

Normal builds use the committed images and do not require Python.

Build the Windows MSI with WiX 4.0.6 and cargo-about 0.9.2 installed:

```powershell
wix extension add WixToolset.UI.wixext/4.0.6
wix extension add WixToolset.Util.wixext/4.0.6
cargo build -p opsssh --release --locked --target x86_64-pc-windows-msvc
./scripts/package-msi.ps1
```

For a host release build, pass `-Binary target/release/opsssh.exe`. Use `-Wix`
to specify a local WiX executable. The MSI installs for the current user in
`%LOCALAPPDATA%\OpsSSH`, includes license notices and a Start menu shortcut,
and leaves application settings and connection data intact on uninstall.
The visible setup wizard offers a checked Launch OpsSSH option on completion.
Windows GUI release builds launch without a separate console window. Debug
builds and builds using `--no-default-features` keep console output for the
development commands.
Artifacts and SHA-256 checksums are written to `artifacts/packages`. They are
unsigned until a release signing process is configured.
