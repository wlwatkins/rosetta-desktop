<#
.SYNOPSIS
    Builds the Windows installer.

.DESCRIPTION
    Stages the binary, the model and the Tesseract runtime into dist\stage, then
    compiles installer\rosetta.iss into a single setup executable.

    The install is per-user and needs no admin rights. Only the five runtime
    model files are staged -- not models_fp32\, which exists solely so `bench`
    can compare precisions.

.PARAMETER SkipBuild
    Use the existing release binary instead of rebuilding.

.PARAMETER SkipTests
    Pass -SkipTests through to build.ps1.

.PARAMETER Zip
    Also produce a portable .zip beside the installer.

.PARAMETER StageOnly
    Stage the payload but do not compile the installer.

.EXAMPLE
    scripts\package.ps1

.EXAMPLE
    scripts\package.ps1 -SkipBuild -Zip
#>
[CmdletBinding()]
param(
    [switch]$SkipBuild,
    [switch]$SkipTests,
    [switch]$Zip,
    [switch]$StageOnly
)

$ErrorActionPreference = 'Stop'

if (-not $PSScriptRoot) {
    throw 'Run this as a script file, not by pasting its contents.'
}
. (Join-Path $PSScriptRoot 'common.ps1')

$Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Set-Location -LiteralPath $Root
$started = Get-Date

$version = Get-ProjectVersion -Root $Root
Write-Head "Rosetta v$version - package"

Assert-Cargo
Assert-Assets -Root $Root

if ($SkipBuild) {
    Write-Warn 'Skipping the build (-SkipBuild).'
}
else {
    $buildArgs = @()
    if ($SkipTests) { $buildArgs += '-SkipTests' }
    & (Join-Path $PSScriptRoot 'build.ps1') @buildArgs
    if ($LASTEXITCODE -ne 0) {
        Stop-WithMessage 'The build failed; nothing has been packaged.'
    }
}

$exe = Join-Path $Root 'target\release\rosetta-desktop.exe'
if (-not (Test-Path -LiteralPath $exe)) {
    Stop-WithMessage "No release binary at $exe."
}

# ---------------------------------------------------------------------------
# Stage the payload
# ---------------------------------------------------------------------------

$dist = Join-Path $Root 'dist'
$stage = Join-Path $dist 'stage'

if (Test-Path -LiteralPath $stage) {
    Write-Step 'Clearing the previous staging folder...'
    Remove-Item -Recurse -Force -LiteralPath $stage
}
New-Item -ItemType Directory -Force -Path $stage | Out-Null

Write-Step 'Staging the binary...'
Copy-Item -LiteralPath $exe -Destination $stage

Write-Step 'Staging the model...'
$modelOut = Join-Path $stage 'models'
New-Item -ItemType Directory -Force -Path $modelOut | Out-Null
foreach ($f in @('model_meta.json', 'tokenizer_source.json', 'vocab_target.json',
        'encoder_model.onnx', 'decoder_model_merged.onnx')) {
    Copy-Item -LiteralPath (Join-Path $Root "models\$f") -Destination $modelOut
}

Write-Step 'Staging the Tesseract runtime...'
Copy-Item -LiteralPath (Join-Path $Root 'vendor') -Destination $stage -Recurse
Remove-Item -Recurse -Force (Join-Path $stage 'vendor\_unused') -ErrorAction SilentlyContinue

# A reader on the target machine has no repo to consult.
$readme = @"
Rosetta $version - Hebrew to English screen translation

Rosetta has no window: it sits in the notification area.

  Ctrl+Alt+T          select a region to translate
  Esc                 dismiss the overlay
  double-click tray   select a region
  right-click tray    menu, including Settings

Under the selection there is a toolbar: copy the recognised Hebrew, copy the
English, force a re-scan, or close.

Everything runs locally. Nothing is sent anywhere and no network request is
ever made.

Settings are stored in %APPDATA%\Rosetta\settings.json and can be changed from
the tray menu, or by running:  rosetta-desktop.exe settings

This program is free software under the GPL-3.0; see LICENSE.txt.
It bundles the Helsinki-NLP opus-mt-tc-big-he-en model (CC-BY-4.0) and
Tesseract with tessdata_best (Apache-2.0).
"@
Set-Content -LiteralPath (Join-Path $stage 'README.txt') -Value $readme -Encoding UTF8

$stageSize = Get-TreeSize -Path $stage
Write-Step "Payload: $(Format-Size $stageSize)"

if ($StageOnly) {
    Write-Host ''
    Write-Good 'Staged.'
    Write-Host "  $stage" -ForegroundColor Green
    Write-Host ''
    exit 0
}

# ---------------------------------------------------------------------------
# Compile the installer
# ---------------------------------------------------------------------------

$iscc = Find-InnoSetup
if (-not $iscc) {
    Stop-WithMessage @'
Inno Setup was not found. Install it with:

    winget install --id JRSoftware.InnoSetup

or pass -StageOnly -Zip to produce a portable folder instead.
'@
}

Write-Step 'Compiling the installer (a few minutes; the weights are ~490MB)...'
$iss = Join-Path $Root 'installer\rosetta.iss'
Invoke-Checked 'ISCC' {
    & $iscc /Q "/DAppVersion=$version" "/DPayloadDir=$stage" "/DOutputDir=$dist" $iss
}

$installer = Get-InstallerPath -Root $Root -Version $version
if (-not (Test-Path -LiteralPath $installer)) {
    Stop-WithMessage "ISCC finished but $installer is missing."
}

if ($Zip) {
    $portable = Join-Path $dist "rosetta-desktop-$version"
    if (Test-Path -LiteralPath $portable) { Remove-Item -Recurse -Force -LiteralPath $portable }
    Copy-Item -LiteralPath $stage -Destination $portable -Recurse
    $zipPath = "$portable.zip"
    if (Test-Path -LiteralPath $zipPath) { Remove-Item -Force -LiteralPath $zipPath }
    Write-Step 'Compressing the portable copy...'
    Compress-Archive -Path $portable -DestinationPath $zipPath -CompressionLevel Optimal
    Remove-Item -Recurse -Force -LiteralPath $portable
}

$size = (Get-Item -LiteralPath $installer).Length
$elapsed = [int]((Get-Date) - $started).TotalSeconds

Write-Host ''
Write-Good "Packaged v$version in ${elapsed}s."
Write-Host "  $installer" -ForegroundColor Green
Write-Step "$(Format-Size $size), from a $(Format-Size $stageSize) payload"
if ($Zip) {
    Write-Host "  $(Join-Path $dist "rosetta-desktop-$version.zip")" -ForegroundColor Green
}
Write-Host ''
