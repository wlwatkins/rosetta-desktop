<#
    Shared helpers for the scripts behind run.cmd.

    Kept deliberately small: these are the things every verb needs (consistent
    output, a native-command runner that actually stops on failure, and the
    asset checks that are specific to this project).
#>

function Write-Head {
    param([string]$Text)
    Write-Host ''
    Write-Host "  $Text" -ForegroundColor Cyan
    Write-Host ''
}

function Write-Step {
    param([string]$Text)
    Write-Host "  $Text" -ForegroundColor DarkGray
}

function Write-Good {
    param([string]$Text)
    Write-Host "  $Text" -ForegroundColor Green
}

function Write-Warn {
    param([string]$Text)
    Write-Host "  $Text" -ForegroundColor Yellow
}

function Stop-WithMessage {
    param([string]$Text, [switch]$NoPause)
    Write-Host ''
    Write-Host "  $Text" -ForegroundColor Red
    Write-Host ''
    # The menu does its own pausing, and sets this so the window is not stopped
    # twice on the way out.
    if (-not $NoPause -and -not $env:ROSETTA_NO_PAUSE -and $Host.Name -eq 'ConsoleHost') {
        Read-Host '  Press Enter to close' | Out-Null
    }
    exit 1
}

<#
    Runs a native command and stops the script if it fails. Native commands do
    not raise terminating errors, so without this every call needs its own
    $LASTEXITCODE check and one will eventually be forgotten.
#>
function Invoke-Checked {
    param(
        [Parameter(Mandatory)] [string]$What,
        [Parameter(Mandatory)] [scriptblock]$Command
    )
    Write-Step $What
    & $Command
    if ($LASTEXITCODE -ne 0) {
        Stop-WithMessage "$What failed (exit code $LASTEXITCODE). See the output above."
    }
}

<#
    Runs a command that is allowed to fail, returning its output and exit code
    instead of raising. Native tools write to stderr for ordinary conditions,
    and under $ErrorActionPreference = 'Stop' that becomes a terminating error.
#>
function Invoke-Quiet {
    param(
        [Parameter(Mandatory)] [string]$Exe,
        [Parameter(ValueFromRemainingArguments)] [string[]]$Arguments = @()
    )
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $output = (& $Exe @Arguments 2>&1 | Out-String).Trim()
        return [pscustomobject]@{ Text = $output; ExitCode = $LASTEXITCODE; Ok = ($LASTEXITCODE -eq 0) }
    }
    finally {
        $ErrorActionPreference = $previous
    }
}

<# The version from [package] in Cargo.toml. #>
function Get-ProjectVersion {
    param([string]$Root)
    $manifest = Join-Path $Root 'Cargo.toml'
    if (-not (Test-Path -LiteralPath $manifest)) {
        Stop-WithMessage "No Cargo.toml found in $Root"
    }
    # Only the [package] table: a dependency could carry a version line too.
    $inPackage = $false
    foreach ($line in Get-Content -LiteralPath $manifest) {
        if ($line -match '^\s*\[(.+)\]\s*$') { $inPackage = ($Matches[1] -eq 'package'); continue }
        if ($inPackage -and $line -match '^\s*version\s*=\s*"([^"]+)"') { return $Matches[1] }
    }
    Stop-WithMessage 'Could not read version from [package] in Cargo.toml'
}

function Assert-Cargo {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Stop-WithMessage 'cargo was not found on your PATH. Install Rust from https://rustup.rs and try again.'
    }
}

<#
    The runtime assets the binary loads at startup. Both are generated, never
    committed, so a fresh clone has neither -- reporting them precisely is the
    difference between "run setup" and a confusing crash on first launch.
#>
function Get-AssetStatus {
    param([string]$Root)

    $model = Join-Path $Root 'models'
    $vendor = Join-Path $Root 'vendor'

    $modelFiles = @(
        'model_meta.json', 'tokenizer_source.json', 'vocab_target.json',
        'encoder_model.onnx', 'decoder_model_merged.onnx'
    )
    $missingModel = @($modelFiles | Where-Object { -not (Test-Path -LiteralPath (Join-Path $model $_)) })

    $vendorFiles = @(
        (Join-Path 'bin' 'libtesseract-5.dll'),
        (Join-Path 'tessdata' 'heb.traineddata')
    )
    $missingVendor = @($vendorFiles | Where-Object { -not (Test-Path -LiteralPath (Join-Path $vendor $_)) })

    return [pscustomobject]@{
        ModelOk       = ($missingModel.Count -eq 0)
        VendorOk      = ($missingVendor.Count -eq 0)
        MissingModel  = $missingModel
        MissingVendor = $missingVendor
        Ok            = (($missingModel.Count -eq 0) -and ($missingVendor.Count -eq 0))
    }
}

function Assert-Assets {
    param([string]$Root)
    $status = Get-AssetStatus -Root $Root
    if ($status.Ok) { return }

    Write-Host ''
    if (-not $status.ModelOk) {
        Write-Host '  Translation model is missing:' -ForegroundColor Red
        $status.MissingModel | ForEach-Object { Write-Host "    models\$_" -ForegroundColor DarkGray }
    }
    if (-not $status.VendorOk) {
        Write-Host '  Tesseract runtime is missing:' -ForegroundColor Red
        $status.MissingVendor | ForEach-Object { Write-Host "    vendor\$_" -ForegroundColor DarkGray }
    }
    Stop-WithMessage 'Run "run.cmd setup" first (one-time, downloads ~1.5GB).'
}

function Format-Size {
    param([long]$Bytes)
    if ($Bytes -ge 1GB) { return ('{0:N1} GB' -f ($Bytes / 1GB)) }
    if ($Bytes -ge 1MB) { return ('{0:N0} MB' -f ($Bytes / 1MB)) }
    return ('{0:N0} KB' -f ($Bytes / 1KB))
}

function Get-TreeSize {
    param([string]$Path)
    if (-not (Test-Path -LiteralPath $Path)) { return 0 }
    return (Get-ChildItem -LiteralPath $Path -Recurse -File -ErrorAction SilentlyContinue |
        Measure-Object -Property Length -Sum).Sum
}

<# Where package.ps1 writes the installer, and publish.ps1 looks for it. #>
function Get-InstallerPath {
    param([string]$Root, [string]$Version)
    # No spaces: GitHub rewrites them in asset names.
    return Join-Path $Root "dist/Rosetta-Setup-$Version.exe"
}

<#
    Locates ISCC.exe. winget installs Inno Setup per-user by default, which is
    not on PATH, so look there before giving up.
#>
function Find-InnoSetup {
    $cmd = Get-Command ISCC.exe -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }

    $candidates = @(
        (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe'),
        (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'),
        (Join-Path $env:ProgramFiles 'Inno Setup 6\ISCC.exe'),
        (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 7\ISCC.exe'),
        (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 7\ISCC.exe')
    )
    foreach ($c in $candidates) {
        if ($c -and (Test-Path -LiteralPath $c)) { return $c }
    }
    return $null
}

<# Writes a new version into [package] in Cargo.toml. #>
function Set-ProjectVersion {
    param([string]$Root, [string]$Version)
    $manifest = Join-Path $Root 'Cargo.toml'
    $lines = Get-Content -LiteralPath $manifest
    $inPackage = $false
    $done = $false
    $out = foreach ($line in $lines) {
        if ($line -match '^\s*\[(.+)\]\s*$') { $inPackage = ($Matches[1] -eq 'package') }
        if (-not $done -and $inPackage -and $line -match '^(\s*version\s*=\s*)"[^"]+"(.*)$') {
            $done = $true
            "$($Matches[1])""$Version""$($Matches[2])"
        }
        else { $line }
    }
    if (-not $done) { Stop-WithMessage 'Could not find the version line in Cargo.toml' }
    # Not Set-Content -Encoding UTF8: on PowerShell 5.1 that writes a BOM, which
    # would land in Cargo.toml on every release.
    $utf8NoBom = New-Object System.Text.UTF8Encoding $false
    [System.IO.File]::WriteAllLines($manifest, $out, $utf8NoBom)
}

<#
    Runs the app and waits for it.

    The binary is linked as a GUI application, so no terminal appears when it is
    launched from the Start menu. PowerShell does not wait for those, and `&`
    would return instantly while the tray app was still running. Start-Process
    with -NoNewWindow keeps the inherited console -- so the app's own output
    still lands here -- and -PassThru gives something to wait on.

    Returns the exit code.
#>
function Invoke-App {
    param(
        [Parameter(Mandatory)] [string]$Exe,
        [string[]]$Arguments = @()
    )
    $start = @{ FilePath = $Exe; PassThru = $true; NoNewWindow = $true }
    if ($Arguments.Count) { $start.ArgumentList = $Arguments }
    $proc = Start-Process @start
    $proc.WaitForExit()
    return $proc.ExitCode
}
