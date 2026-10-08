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
foreach ($file in @('opsssh.ico', 'opsssh.png', 'opsssh-mascot.svg')) {
    Copy-Item -LiteralPath (Join-Path $repo "assets/branding/$file") -Destination $stage
}
& cargo about generate --locked --all-features --target x86_64-pc-windows-msvc -o (Join-Path $stage 'THIRD-PARTY-LICENSES.html') scripts/licenses.hbs
if ($LASTEXITCODE -ne 0) { throw 'Third-party license generation failed; refusing to package' }
$commit = & git rev-parse HEAD
$dirty = [bool](& git status --porcelain)
@{ version=$Version; target='x86_64-pc-windows-msvc'; signing='unsigned'; source_commit=$commit; source_dirty=$dirty; binary_sha256=(Get-FileHash -LiteralPath $Binary).Hash.ToLowerInvariant() } |
    ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $stage 'build-info.json')
$output = Join-Path $repo "artifacts/packages/opsssh-$Version-windows-x64.msi"
& $Wix build (Join-Path $repo 'installer/windows.wxs') -arch x64 -d "Version=$Version" -d "Stage=$stage" -o $output
if ($LASTEXITCODE -ne 0) { throw 'MSI build or validation failed' }
$hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $output).Hash.ToLowerInvariant()
"$hash  $(Split-Path -Leaf $output)" | Set-Content -Encoding ascii "$output.sha256"
Write-Host "Created unsigned MSI: $output"
