param(
    [string]$ToolchainBin
)

$ErrorActionPreference = 'Stop'
$workspacePath = Split-Path -Parent $PSScriptRoot
Push-Location -LiteralPath $workspacePath
$originalPath = $env:PATH
$originalCargoHome = $env:CARGO_HOME
$originalRustc = $env:RUSTC
$originalRustdoc = $env:RUSTDOC
try {
    if ($ToolchainBin) {
        $resolvedBin = (Resolve-Path -LiteralPath $ToolchainBin).Path
        foreach ($binary in @('cargo.exe', 'rustc.exe', 'rustdoc.exe', 'rustfmt.exe', 'cargo-clippy.exe')) {
            if (-not (Test-Path -LiteralPath (Join-Path $resolvedBin $binary))) {
                throw "Missing toolchain binary: $binary"
            }
        }
        $env:PATH = $resolvedBin + [IO.Path]::PathSeparator + $env:PATH
        $env:RUSTC = Join-Path $resolvedBin 'rustc.exe'
        $env:RUSTDOC = Join-Path $resolvedBin 'rustdoc.exe'
    }
    $compilerVersion = & rustc --version
    if ($LASTEXITCODE -ne 0 -or $compilerVersion -notmatch '^rustc 1\.99\.0\b') {
        throw "Expected Rust 1.99.0, got: $compilerVersion"
    }
    & cargo fmt --all -- --check
    if ($LASTEXITCODE -ne 0) { throw 'Formatting check failed' }
    & cargo test --workspace --all-features --locked
    if ($LASTEXITCODE -ne 0) { throw 'Tests failed' }
    & cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw 'Clippy failed' }
    & cargo build --workspace --release --locked
    if ($LASTEXITCODE -ne 0) { throw 'Release build failed' }
} finally {
    $env:PATH = $originalPath
    $env:CARGO_HOME = $originalCargoHome
    $env:RUSTC = $originalRustc
    $env:RUSTDOC = $originalRustdoc
    Pop-Location
}
