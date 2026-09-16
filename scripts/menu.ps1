<#
.SYNOPSIS
    The entry point behind run.cmd: a menu of the things you can do here.

.DESCRIPTION
    With no arguments it shows a menu and keeps showing it until you quit, so
    you can build and then package without relaunching.

    With arguments it skips the menu:

        run.cmd run [args]        -> run.ps1
        run.cmd build [args]      -> build.ps1
        run.cmd package [args]    -> package.ps1
        run.cmd setup [args]      -> setup.ps1
        run.cmd bench [args]      -> bench.ps1
        run.cmd publish [args]    -> publish.ps1
        run.cmd -Log              -> run.ps1 -Log

    The last form treats an unrecognised first argument as a run.ps1 switch,
    since running is the common case.

.PARAMETER Action
    run, build, package, publish, setup, bench, or a switch for run.ps1.

.PARAMETER Rest
    Everything else, forwarded to the chosen script untouched.
#>
[CmdletBinding()]
param(
    [Parameter(Position = 0)] [string]$Action,
    [Parameter(ValueFromRemainingArguments = $true)] [string[]]$Rest = @()
)

$ErrorActionPreference = 'Stop'

if (-not $PSScriptRoot) {
    throw 'Run this as a script file, not by pasting its contents.'
}

# This script lives in scripts/; everything it touches is one level up.
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
. (Join-Path $PSScriptRoot 'common.ps1')

# The sub-scripts pause on failure so a double-clicked window stays readable.
# Launched from the menu that is the menu's job, so ask them not to.
$env:ROSETTA_NO_PAUSE = '1'

# The host running this script, so a child gets the same PowerShell.
$psExe = try { (Get-Process -Id $PID).Path } catch { $null }
if (-not $psExe) { $psExe = 'powershell' }

function Invoke-Script {
    param([string]$Name, [string[]]$Arguments = @())

    $path = Join-Path $PSScriptRoot $Name
    if (-not (Test-Path -LiteralPath $path)) {
        Write-Host ''
        Write-Host "  $Name is missing." -ForegroundColor Red
        Write-Host ''
        $global:LASTEXITCODE = 1
        return
    }

    # A child PowerShell rather than `& $path @Arguments`. Splatting an *array*
    # binds positionally, so a forwarded `-Log` would land on the first
    # positional parameter instead of being read as a switch. Passing them to a
    # native command puts them through the normal command-line parser.
    #
    # Nothing is returned on purpose: a function's success-stream output is its
    # return value, so returning the exit code here would swallow everything the
    # child printed. Callers read $LASTEXITCODE instead.
    & $psExe -NoProfile -ExecutionPolicy Bypass -File $path @Arguments
}

# ---------------------------------------------------------------------------
# Non-interactive: a verb, or run.ps1 switches
# ---------------------------------------------------------------------------

if ($Action) {
    switch -Regex ($Action) {
        '^run$' { Invoke-Script 'run.ps1' $Rest; exit $LASTEXITCODE }
        '^build$' { Invoke-Script 'build.ps1' $Rest; exit $LASTEXITCODE }
        '^package$' { Invoke-Script 'package.ps1' $Rest; exit $LASTEXITCODE }
        '^setup$' { Invoke-Script 'setup.ps1' $Rest; exit $LASTEXITCODE }
        '^bench$' { Invoke-Script 'bench.ps1' $Rest; exit $LASTEXITCODE }
        '^publish$' { Invoke-Script 'publish.ps1' $Rest; exit $LASTEXITCODE }
        '^menu$' { break }
        default { Invoke-Script 'run.ps1' (@($Action) + $Rest); exit $LASTEXITCODE }
    }
}

# ---------------------------------------------------------------------------
# The menu
# ---------------------------------------------------------------------------

$choices = @(
    @{ Key = '1'; Name = 'Run'; Detail = 'launch the tray app (release)'; Script = 'run.ps1'; Arguments = @() }
    @{ Key = '2'; Name = 'Run with logging'; Detail = 'print every scan and its translations'; Script = 'run.ps1'; Arguments = @('-Log') }
    @{ Key = '3'; Name = 'Build'; Detail = 'test and build the release binary'; Script = 'build.ps1'; Arguments = @() }
    @{ Key = '4'; Name = 'Build installer'; Detail = 'setup .exe in dist\'; Script = 'package.ps1'; Arguments = @() }
    @{ Key = '5'; Name = 'Publish (dry run)'; Detail = 'show what a release would do, change nothing'; Script = 'publish.ps1'; Arguments = @('-DryRun') }
    @{ Key = '6'; Name = 'Publish'; Detail = 'bump, tag and release to GitHub'; Script = 'publish.ps1'; Arguments = @() }
    @{ Key = '7'; Name = 'Bench'; Detail = 'compare int8/fp32 and greedy/beam'; Script = 'bench.ps1'; Arguments = @() }
    @{ Key = '8'; Name = 'Setup assets'; Detail = 'one-time: fetch the model and Tesseract'; Script = 'setup.ps1'; Arguments = @() }
)

function Show-Menu {
    $version = try { Get-ProjectVersion -Root $Root } catch { $null }

    Write-Host ''
    Write-Host '  Rosetta' -ForegroundColor Cyan -NoNewline
    if ($version) { Write-Host "  v$version" -ForegroundColor DarkGray } else { Write-Host '' }
    Write-Host '  Hebrew to English, on screen, offline' -ForegroundColor DarkGray
    Write-Host ''

    foreach ($choice in $choices) {
        Write-Host '   [' -NoNewline -ForegroundColor DarkGray
        Write-Host $choice.Key -NoNewline -ForegroundColor Cyan
        Write-Host '] ' -NoNewline -ForegroundColor DarkGray
        Write-Host $choice.Name.PadRight(18) -NoNewline
        Write-Host $choice.Detail -ForegroundColor DarkGray
    }
    Write-Host '   [' -NoNewline -ForegroundColor DarkGray
    Write-Host 'Q' -NoNewline -ForegroundColor Cyan
    Write-Host '] ' -NoNewline -ForegroundColor DarkGray
    Write-Host 'Quit'

    # Running before setup is the one failure a first-time user will hit, so
    # say so up front rather than letting option 1 explain it.
    $status = Get-AssetStatus -Root $Root
    if (-not $status.Ok) {
        Write-Host ''
        Write-Host '   Runtime assets are missing - start with [8] Setup assets.' -ForegroundColor Yellow
    }
    Write-Host ''
}

try {
    $Host.UI.RawUI.WindowTitle = 'Rosetta'
}
catch { }

for (;;) {
    Show-Menu
    $answer = (Read-Host '  Choose [1]').Trim()
    if (-not $answer) { $answer = '1' }

    if ($answer -match '^(q|quit|exit|0)$') {
        Write-Host ''
        break
    }

    $choice = $choices | Where-Object { $_.Key -eq $answer } | Select-Object -First 1
    if (-not $choice) {
        Write-Host ''
        Write-Host "  '$answer' is not one of the options." -ForegroundColor Yellow
        continue
    }

    Write-Host ''
    Write-Host "  -> $($choice.Name)" -ForegroundColor DarkGray

    Invoke-Script $choice.Script $choice.Arguments
    $code = $LASTEXITCODE

    Write-Host ''
    if ($code -and $code -ne 0) {
        Write-Host "  $($choice.Name) failed (exit code $code)." -ForegroundColor Red
    }
    else {
        Write-Host "  $($choice.Name) finished." -ForegroundColor Green
    }
    Read-Host '  Press Enter for the menu' | Out-Null
    Clear-Host
}
