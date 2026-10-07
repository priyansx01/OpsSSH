param([switch]$KeepRunning)
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo
$fixture = Join-Path $repo 'artifacts/compat/keys'
New-Item -ItemType Directory -Path $fixture -Force | Out-Null
$key = Join-Path $fixture 'client'
if (-not (Test-Path -LiteralPath $key)) {
    # Empty passphrase is intentional for newly generated disposable test keys.
    if ($PSVersionTable.PSVersion.Major -lt 7) { & ssh-keygen -q -t ed25519 -N '""' -C opsssh-compat -f $key }
    else { & ssh-keygen -q -t ed25519 -N '' -C opsssh-compat -f $key }
    if ($LASTEXITCODE -ne 0) { throw 'Fixture key generation failed' }
}
$compose = Join-Path $repo 'tests/compat/compose.yml'
try {
    & docker compose -f $compose up -d --build --wait
    if ($LASTEXITCODE -ne 0) { throw 'Compatibility containers failed to start' }
    & cargo test -p opsssh-ssh-core -p opsssh-sftp --locked
    if ($LASTEXITCODE -ne 0) { throw 'Protocol tests failed' }
    $knownHosts = Join-Path $repo 'artifacts/compat/known_hosts'
    $hostLines = @()
    $targets = @(@{service='openssh';port=22220}, @{service='alpine';port=22221}, @{service='dropbear';port=22222}, @{service='bastion';port=22223})
    foreach ($target in $targets) {
        if ($target.service -eq 'dropbear') {
            $output = & docker compose -f $compose exec -T dropbear dropbearkey -y -f /etc/dropbear/fixture_ed25519
            $publicKey = ($output | Where-Object { $_ -match '^ssh-ed25519 ' } | Select-Object -First 1)
        } else {
            $publicKey = & docker compose -f $compose exec -T $target.service cat /etc/ssh/ssh_host_ed25519_key.pub
        }
        if ($LASTEXITCODE -ne 0 -or -not $publicKey) { throw 'Could not read isolated fixture host key' }
        $hostLines += "[127.0.0.1]:$($target.port) $publicKey"
    }
    $hostLines | Set-Content -Encoding ascii $knownHosts
    foreach ($target in $targets) {
        & cargo run -p opsssh-ssh-core --example compat_client --locked -- 127.0.0.1 $target.port fixture $key $knownHosts
        if ($LASTEXITCODE -ne 0) { throw "OpsSSH compatibility probe failed: $($target.service)" }
    }
    & docker compose -f $compose images --format json | Set-Content -Encoding utf8 'artifacts/compat/images.json'
    Write-Host 'OpsSSH key authentication, SSH command and SFTP roundtrip passed for OpenSSH, Alpine, Dropbear and bastion fixtures.'
} finally {
    if (-not $KeepRunning) { & docker compose -f $compose down --remove-orphans }
}
