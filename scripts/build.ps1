<#
.SYNOPSIS
    Builds rosetta-desktop.

.DESCRIPTION
    Runs the test suite and builds the optimised binary. Assets are not needed
    to compile, so this works on a fresh clone before setup has been run -- it
    just says so at the end.

.PARAMETER SkipTests
    Skip `cargo test`. Faster, but the binary is unproven.

.PARAMETER Dev
    Build the debug profile instead of release. Much faster to compile and far
    slower to run; the model is the bottleneck, so prefer release for real use.

.PARAMETER Clippy
    Also run clippy, if the component is installed.

.EXAMPLE
    scripts\build.ps1

.EXAMPLE
    scripts\build.ps1 -SkipTests -Dev
#>
[CmdletBinding()]
param(
    [switch]$SkipTests,
    [switch]$Dev,
    [switch]$Clippy
)

$ErrorActionPreference = 'Stop'

if (-not $PSScriptRoot) {
    throw 'Run this as a script file, not by pasting its contents.'
}
. (Join-Path $PSScriptRoot 'common.ps1')

# This script lives in scripts/; everything it touches is one level up.
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Set-Location -LiteralPath $Root
$started = Get-Date

$version = Get-ProjectVersion -Root $Root
$profileName = if ($Dev) { 'debug' } else { 'release' }
Write-Head "Rosetta v$version - build ($profileName)"

Assert-Cargo

if ($Clippy) {
    $has = Invoke-Quiet -Exe 'cargo' -Arguments @('clippy', '--version')
    if ($has.Ok) {
        Invoke-Checked 'Linting...' { cargo clippy --release --all-targets -- -D warnings }
    }
    else {
        Write-Warn 'clippy is not installed (rustup component add clippy); skipping.'
    }
}

if ($SkipTests) {
    Write-Warn 'Skipping tests (-SkipTests).'
}
elseif ($Dev) {
    Invoke-Checked 'Running tests...' { cargo test }
}
else {
    Invoke-Checked 'Running tests...' { cargo test --release }
}

if ($Dev) {
    Invoke-Checked 'Building...' { cargo build }
}
else {
    Invoke-Checked 'Building...' { cargo build --release }
}

$exe = Join-Path $Root "target\$profileName\rosetta-desktop.exe"
if (-not (Test-Path -LiteralPath $exe)) {
    Stop-WithMessage "The build finished but $exe is missing."
}

$sizeMb = [math]::Round((Get-Item -LiteralPath $exe).Length / 1MB, 1)
$elapsed = [int]((Get-Date) - $started).TotalSeconds

Write-Host ''
Write-Good "Built v$version in ${elapsed}s."
Write-Host "  $exe" -ForegroundColor Green
Write-Step "$sizeMb MB"

# Compiling does not need the model or Tesseract, but running does.
$status = Get-AssetStatus -Root $Root
if (-not $status.Ok) {
    Write-Host ''
    Write-Warn 'Runtime assets are not in place yet - run "run.cmd setup" before launching.'
}
Write-Host ''
