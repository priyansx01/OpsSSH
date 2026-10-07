param([Parameter(Mandatory=$true)][string]$Target, [string]$Version = '0.1.0')
$ErrorActionPreference = 'Stop'
if ($Target -notmatch '^[a-z0-9_-]+$' -or $Version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+([.-][A-Za-z0-9.-]+)?$') { throw 'Invalid package target or version' }
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo
$name = "opsssh-$Version-$Target"
$stage = Join-Path $repo "artifacts/packages/$name"
if (Test-Path -LiteralPath $stage) { throw 'Package staging directory already exists; use a fresh workspace' }
New-Item -ItemType Directory -Path $stage -Force | Out-Null
$exe = if ($Target.Contains('windows')) { 'opsssh.exe' } else { 'opsssh' }
$binary = Join-Path $repo "target/$Target/release/$exe"
if (-not (Test-Path -LiteralPath $binary)) { throw "Release binary not found: $binary" }
Copy-Item -LiteralPath $binary -Destination $stage
foreach ($file in @('LICENSE-MIT','LICENSE-APACHE','CHANGELOG.md','README.md')) { Copy-Item -LiteralPath (Join-Path $repo $file) -Destination $stage }
& cargo about generate --locked --all-features --target $Target -o (Join-Path $stage 'THIRD-PARTY-LICENSES.html') scripts/licenses.hbs
if ($LASTEXITCODE -ne 0) { throw 'Third-party license generation failed; refusing to package' }
@{ version=$Version; target=$Target; signing='unsigned-development-artifact'; source_commit=(& git rev-parse HEAD) } | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $stage 'build-info.json')
$extension = if ($Target.Contains('windows')) { 'zip' } else { 'tar.gz' }
$archive = Join-Path $repo "artifacts/packages/$name.$extension"
if ($extension -eq 'zip') { Compress-Archive -LiteralPath $stage -DestinationPath $archive }
else {
    & tar -czf $archive -C (Split-Path -Parent $stage) $name
    if ($LASTEXITCODE -ne 0) { throw 'Archive creation failed' }
}
$hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $archive).Hash.ToLowerInvariant()
"$hash  $name.$extension" | Set-Content -Encoding ascii "$archive.sha256"
Write-Host "Created unsigned development artifact: $archive"
