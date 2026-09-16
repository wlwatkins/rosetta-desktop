<#
.SYNOPSIS
    One-time setup of the runtime assets: the translation model and Tesseract.

.DESCRIPTION
    Neither asset is committed -- together they are ~490MB, and the model has no
    published ONNX build -- so a fresh clone needs this once.

    Model: exports Helsinki-NLP/opus-mt-tc-big-he-en to ONNX and quantizes it to
    int8. This is the only step that touches Python, in a throwaway venv; the
    app itself has no Python dependency and nothing from the venv ships.

    Tesseract: Windows' own OCR engine has no Hebrew, so Tesseract is vendored
    beside the executable and loaded at runtime, which is why the build needs no
    C++ toolchain.

.PARAMETER SkipModel
    Leave models/ alone.

.PARAMETER SkipTesseract
    Leave vendor/ alone.

.PARAMETER KeepFp32
    Also produce models_fp32/ (1.4GB), which `run.cmd bench` uses to compare
    int8 against full precision.

.PARAMETER Force
    Redo a step even if its output already looks complete.

.EXAMPLE
    scripts\setup.ps1

.EXAMPLE
    scripts\setup.ps1 -SkipModel
#>
[CmdletBinding()]
param(
    [switch]$SkipModel,
    [switch]$SkipTesseract,
    [switch]$KeepFp32,
    [switch]$Force
)

$ErrorActionPreference = 'Stop'

if (-not $PSScriptRoot) {
    throw 'Run this as a script file, not by pasting its contents.'
}
. (Join-Path $PSScriptRoot 'common.ps1')

$Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Set-Location -LiteralPath $Root
$started = Get-Date

Write-Head 'Rosetta - one-time setup'

$status = Get-AssetStatus -Root $Root
$Venv = Join-Path $Root '.venv-export'
$VenvPython = Join-Path $Venv 'Scripts\python.exe'

# ---------------------------------------------------------------------------
# Translation model
# ---------------------------------------------------------------------------

if ($SkipModel) {
    Write-Warn 'Skipping the model (-SkipModel).'
}
elseif ($status.ModelOk -and -not $Force) {
    Write-Step 'Model already present; skipping. Use -Force to rebuild it.'
}
else {
    if (-not (Get-Command uv -ErrorAction SilentlyContinue)) {
        Stop-WithMessage 'uv was not found on your PATH. Install it from https://docs.astral.sh/uv/ and try again.'
    }

    Write-Step 'Creating the export venv (build-time only; nothing here ships)...'
    Invoke-Checked 'uv venv' { uv venv --python 3.12 $Venv }

    # CPU wheels only: the export runs once and the app never uses torch.
    Invoke-Checked 'Installing torch (CPU)...' {
        uv pip install --python $Venv torch --index-url https://download.pytorch.org/whl/cpu
    }
    Invoke-Checked 'Installing exporters...' {
        uv pip install --python $Venv 'transformers>=4.44' 'optimum[onnxruntime]>=1.23' onnx sentencepiece protobuf numpy
    }

    $exportArgs = @((Join-Path $Root 'tools\export_model.py'))
    if ($KeepFp32) { $exportArgs += '--keep-fp32' }

    # The console codepage cannot render the Hebrew probe the script prints.
    $env:PYTHONIOENCODING = 'utf-8'
    Invoke-Checked 'Exporting and quantizing (downloads ~1.2GB)...' {
        & $VenvPython @exportArgs
    }
}

# ---------------------------------------------------------------------------
# Tesseract runtime
# ---------------------------------------------------------------------------

if ($SkipTesseract) {
    Write-Warn 'Skipping Tesseract (-SkipTesseract).'
}
elseif ($status.VendorOk -and -not $Force) {
    Write-Step 'Tesseract already vendored; skipping. Use -Force to redo it.'
}
else {
    $tessDir = Join-Path $env:ProgramFiles 'Tesseract-OCR'
    $tessDll = Join-Path $tessDir 'libtesseract-5.dll'

    if (-not (Test-Path -LiteralPath $tessDll)) {
        if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
            Stop-WithMessage "Tesseract is not installed and winget is unavailable. Install it from https://github.com/UB-Mannheim/tesseract/wiki, then re-run."
        }
        Invoke-Checked 'Installing Tesseract via winget...' {
            winget install --id UB-Mannheim.TesseractOCR `
                --accept-package-agreements --accept-source-agreements --disable-interactivity
        }
    }
    if (-not (Test-Path -LiteralPath $tessDll)) {
        Stop-WithMessage "Still no $tessDll after installing."
    }

    $bin = Join-Path $Root 'vendor\bin'
    $data = Join-Path $Root 'vendor\tessdata'
    New-Item -ItemType Directory -Force -Path $bin, $data | Out-Null

    Write-Step 'Copying the Tesseract runtime...'
    Copy-Item -Path (Join-Path $tessDir '*.dll') -Destination $bin -Force

    # tessdata_best is slower than the default set but noticeably more accurate
    # on the small antialiased text the overlay is pointed at.
    foreach ($lang in @('heb', 'eng')) {
        $out = Join-Path $data "$lang.traineddata"
        if ((Test-Path -LiteralPath $out) -and -not $Force) { continue }
        Write-Step "Downloading $lang.traineddata..."
        Invoke-WebRequest -UseBasicParsing `
            -Uri "https://github.com/tesseract-ocr/tessdata_best/raw/main/$lang.traineddata" `
            -OutFile $out
    }

    # The installer ships the whole pango/cairo/icu stack for its PDF tools;
    # none of it is reachable from the OCR API we call, so it is 46MB of dead
    # weight in every package.
    $python = if (Test-Path -LiteralPath $VenvPython) { $VenvPython } else { (Get-Command python -ErrorAction SilentlyContinue).Source }
    if ($python) {
        Write-Step 'Pruning unreachable DLLs...'
        & $python (Join-Path $Root 'tools\prune_vendor.py') --apply | Select-Object -Last 1 | ForEach-Object { Write-Step $_ }
        Remove-Item -Recurse -Force (Join-Path $Root 'vendor\_unused') -ErrorAction SilentlyContinue
    }
    else {
        Write-Warn 'No Python found; skipping the DLL prune (vendor/ will be ~46MB larger).'
    }
}

# ---------------------------------------------------------------------------

$final = Get-AssetStatus -Root $Root
$elapsed = [int]((Get-Date) - $started).TotalSeconds

Write-Host ''
if ($final.Ok) {
    Write-Good "Setup finished in ${elapsed}s."
    Write-Step ("models   " + (Format-Size (Get-TreeSize (Join-Path $Root 'models'))))
    Write-Step ("vendor   " + (Format-Size (Get-TreeSize (Join-Path $Root 'vendor'))))
    if (Test-Path -LiteralPath (Join-Path $Root 'models_fp32')) {
        Write-Step ("fp32     " + (Format-Size (Get-TreeSize (Join-Path $Root 'models_fp32'))) + '  (bench only)')
    }
}
else {
    Write-Warn 'Setup finished, but some assets are still missing:'
    $final.MissingModel | ForEach-Object { Write-Step "models\$_" }
    $final.MissingVendor | ForEach-Object { Write-Step "vendor\$_" }
}
Write-Host ''
