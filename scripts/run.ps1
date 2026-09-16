<#
.SYNOPSIS
    Launches the tray app.

.DESCRIPTION
    Builds if the binary is missing, checks that the runtime assets are in
    place, then starts it. The app has no window: it lives in the tray and is
    activated with its hotkey (Ctrl+Alt+T by default).

.PARAMETER Log
    Turn on scan logging: every OCR pass and its translations are printed.

.PARAMETER Theme
    dark (default) or light.

.PARAMETER Hotkey
    Override the activation shortcut, e.g. ctrl+shift+t. Any combination of
    ctrl/alt/shift/win plus a letter, digit or f1-f24.

.PARAMETER Beams
    Beam width for decoding. 4 is the model's own default; 1 is greedy, which is
    faster and slightly worse.

.PARAMETER Backend
    cpu (default), dml or cuda. cpu is the fastest here -- see the README.

.PARAMETER DebugBuild
    Use the debug binary instead of release.

.PARAMETER NoExclude
    Let the overlay appear in screenshots. Only for debugging how it looks; the
    live loop will feed on its own output.

.EXAMPLE
    scripts\run.ps1

.EXAMPLE
    scripts\run.ps1 -Log -Hotkey ctrl+shift+t
#>
[CmdletBinding()]
param(
    [switch]$Log,
    [ValidateSet('dark', 'light')] [string]$Theme,
    [string]$Hotkey,
    [ValidateRange(1, 12)] [int]$Beams,
    [ValidateSet('cpu', 'dml', 'cuda')] [string]$Backend,
    [switch]$DebugBuild,
    [switch]$NoExclude
)

$ErrorActionPreference = 'Stop'

if (-not $PSScriptRoot) {
    throw 'Run this as a script file, not by pasting its contents.'
}
. (Join-Path $PSScriptRoot 'common.ps1')

$Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Set-Location -LiteralPath $Root

$version = Get-ProjectVersion -Root $Root
Write-Head "Rosetta v$version"

Assert-Cargo
Assert-Assets -Root $Root

$profileName = if ($DebugBuild) { 'debug' } else { 'release' }
$exe = Join-Path $Root "target\$profileName\rosetta-desktop.exe"

if (-not (Test-Path -LiteralPath $exe)) {
    Write-Step 'No binary yet; building first.'
    if ($DebugBuild) { Invoke-Checked 'Building...' { cargo build } }
    else { Invoke-Checked 'Building...' { cargo build --release } }
}

# Only set what was asked for, so the binary's own defaults stay authoritative.
if ($Log) { $env:ROSETTA_DEBUG = '1' } else { Remove-Item Env:\ROSETTA_DEBUG -ErrorAction SilentlyContinue }
if ($NoExclude) { $env:ROSETTA_NO_EXCLUDE = '1' } else { Remove-Item Env:\ROSETTA_NO_EXCLUDE -ErrorAction SilentlyContinue }
if ($Theme) { $env:ROSETTA_THEME = $Theme }
if ($Hotkey) { $env:ROSETTA_HOTKEY = $Hotkey }
if ($Beams) { $env:ROSETTA_BEAMS = "$Beams" }
if ($Backend) { $env:ROSETTA_BACKEND = $Backend }

$shown = @()
if ($Hotkey) { $shown += "hotkey $Hotkey" }
if ($Theme) { $shown += "theme $Theme" }
if ($Beams) { $shown += "beams $Beams" }
if ($Backend) { $shown += "backend $Backend" }
if ($Log) { $shown += 'scan logging' }
if ($NoExclude) { $shown += 'visible to capture' }
if ($shown.Count) { Write-Step ($shown -join '   ') }

Write-Step 'Closing this window stops the app. Ctrl+C also works.'
Write-Host ''

& $exe
$code = $LASTEXITCODE

Write-Host ''
if ($code -and $code -ne 0) {
    # A taken hotkey is by far the most common failure, and the binary already
    # prints the fix, so do not bury it under a generic message.
    Stop-WithMessage "Rosetta exited with code $code."
}
Write-Good 'Rosetta exited.'
Write-Host ''
