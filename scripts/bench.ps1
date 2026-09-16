<#
.SYNOPSIS
    Compares decoding strategies and weight precisions.

.DESCRIPTION
    Translates a fixed set of Hebrew sentences under each available
    configuration and reports timings plus whether the outputs agree.

    models_fp32\ is optional: without it the fp32 rows are skipped and you get
    the greedy-vs-beam comparison only. Create it with
    `run.cmd setup -KeepFp32 -Force`.

.PARAMETER Backend
    cpu (default), dml or cuda.

.EXAMPLE
    scripts\bench.ps1

.EXAMPLE
    scripts\bench.ps1 -Backend dml
#>
[CmdletBinding()]
param(
    [ValidateSet('cpu', 'dml', 'cuda')] [string]$Backend = 'cpu'
)

$ErrorActionPreference = 'Stop'

if (-not $PSScriptRoot) {
    throw 'Run this as a script file, not by pasting its contents.'
}
. (Join-Path $PSScriptRoot 'common.ps1')

$Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Set-Location -LiteralPath $Root

$version = Get-ProjectVersion -Root $Root
Write-Head "Rosetta v$version - bench ($Backend)"

Assert-Cargo
Assert-Assets -Root $Root

$exe = Join-Path $Root 'target\release\rosetta-desktop.exe'
if (-not (Test-Path -LiteralPath $exe)) {
    Invoke-Checked 'Building...' { cargo build --release }
}

if (-not (Test-Path -LiteralPath (Join-Path $Root 'models_fp32\model_meta.json'))) {
    Write-Warn 'models_fp32\ is absent; the fp32 rows will be skipped.'
    Write-Step 'Create it with: run.cmd setup -KeepFp32 -Force'
    Write-Host ''
}

$env:ROSETTA_BACKEND = $Backend
$code = Invoke-App -Exe $exe -Arguments @('bench')

Write-Host ''
if ($code -and $code -ne 0) {
    Stop-WithMessage "bench exited with code $code."
}
Write-Host ''
