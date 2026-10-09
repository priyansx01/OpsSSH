param(
    [string]$Version = '0.1.0',
    [string]$Binary,
    [string]$Wix = 'wix'
)
$ErrorActionPreference = 'Stop'
if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw 'MSI requires a numeric major.minor.patch version' }
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo
if (-not $Binary) { $Binary = Join-Path $repo 'target/x86_64-pc-windows-msvc/release/opsssh.exe' }
$Binary = (Resolve-Path -LiteralPath $Binary).Path
$binaryReader = [IO.BinaryReader]::new([IO.File]::OpenRead($Binary))
try {
    $binaryReader.BaseStream.Position = 0x3c
    $peOffset = $binaryReader.ReadUInt32()
    $binaryReader.BaseStream.Position = $peOffset
    if ($binaryReader.ReadUInt32() -ne 0x4550 -or $binaryReader.ReadUInt16() -ne 0x8664) {
        throw 'The MSI requires an x64 Windows executable'
    }
    $binaryReader.BaseStream.Position = $peOffset + 24 + 68
    if ($binaryReader.ReadUInt16() -ne 2) {
        throw 'The MSI requires a Windows GUI release build; console builds open an extra terminal window'
    }
} finally {
    $binaryReader.Dispose()
}
$versionParts = $Version.Split('.')
if ([int]$versionParts[0] -gt 255 -or [int]$versionParts[1] -gt 255 -or [int]$versionParts[2] -gt 65535) { throw 'Version exceeds MSI limits' }
$null = Get-Command $Wix -ErrorAction Stop
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$stage = Join-Path $repo "artifacts/packages/msi-stage-$stamp"
New-Item -ItemType Directory -Path $stage -ErrorAction Stop | Out-Null
Copy-Item -LiteralPath $Binary -Destination (Join-Path $stage 'opsssh.exe')
foreach ($file in @('LICENSE-MIT', 'LICENSE-APACHE', 'README.md', 'CHANGELOG.md')) {
    Copy-Item -LiteralPath (Join-Path $repo $file) -Destination $stage
}
$licenseText = "OpsSSH is available under MIT OR Apache-2.0. Both license texts and third-party notices are included in the installation.`r`n`r`n" + (Get-Content -LiteralPath (Join-Path $repo 'LICENSE-MIT') -Raw)
$licenseText = $licenseText.Replace('\', '\\').Replace('{', '\{').Replace('}', '\}').Replace("`r`n", '\par ').Replace("`n", '\par ')
('{\rtf1\ansi\deff0{\fonttbl{\f0 Segoe UI;}}\f0\fs20 ' + $licenseText + '}') | Set-Content -Encoding ascii (Join-Path $stage 'license.rtf')
foreach ($file in @('opsssh.ico', 'opsssh.png', 'opsssh-mascot.svg', 'installer-dialog.bmp', 'installer-banner.bmp')) {
    Copy-Item -LiteralPath (Join-Path $repo "assets/branding/$file") -Destination $stage
}
& cargo about generate --locked --all-features --target x86_64-pc-windows-msvc -o (Join-Path $stage 'THIRD-PARTY-LICENSES.html') scripts/licenses.hbs
if ($LASTEXITCODE -ne 0) { throw 'Third-party license generation failed; refusing to package' }
$commit = & git rev-parse HEAD
$dirty = [bool](& git status --porcelain)
@{ version=$Version; target='x86_64-pc-windows-msvc'; signing='unsigned'; source_commit=$commit; source_dirty=$dirty; binary_sha256=(Get-FileHash -LiteralPath $Binary).Hash.ToLowerInvariant() } |
    ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $stage 'build-info.json')
$output = Join-Path $repo "artifacts/packages/opsssh-$Version-windows-x64.msi"
$buildOutput = Join-Path $stage 'opsssh.msi'
& $Wix build (Join-Path $repo 'installer/windows.wxs') -arch x64 -ext WixToolset.UI.wixext/4.0.6 -ext WixToolset.Util.wixext/4.0.6 -d "Version=$Version" -d "Stage=$stage" -o $buildOutput
if ($LASTEXITCODE -ne 0) { throw 'MSI build or validation failed' }
Copy-Item -LiteralPath $buildOutput -Destination $output -Force
$hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $output).Hash.ToLowerInvariant()
"$hash  $(Split-Path -Leaf $output)" | Set-Content -Encoding ascii "$output.sha256"
Write-Host "Created unsigned MSI: $output"
