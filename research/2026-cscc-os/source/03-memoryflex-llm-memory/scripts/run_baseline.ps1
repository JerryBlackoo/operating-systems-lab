<#
Run baseline metrics
Usage:
  powershell -ExecutionPolicy Bypass -File scripts\run_baseline.ps1 -ModelId qwen2.5-1.5b -GpuLayers 99
  powershell -ExecutionPolicy Bypass -File scripts\run_baseline.ps1 -ModelId qwen2.5-3b -GpuLayers 0 -Requests 10 -MaxTokens 48
#>
param(
    [string]$ModelId = "qwen2.5-1.5b",
    [int]$GpuLayers = 99,
    [int]$Requests = 12,
    [int]$MaxTokens = 64,
    [int]$CtxSize = 2048
)
$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
$cfg = Get-Content -Raw -Encoding UTF8 (Join-Path $root "config\models.json") | ConvertFrom-Json
$m = @($cfg.models | Where-Object { $_.id -eq $ModelId } | Select-Object -First 1)
if (-not $m) { Write-Host "Model $ModelId not in config\models.json"; exit 1 }

$modelPath = Join-Path $root ("models\" + $m.file)
if (-not (Test-Path $modelPath)) {
    Write-Host "Model file missing: $modelPath"
    Write-Host "Run first: powershell -ExecutionPolicy Bypass -File scripts\download_models.ps1 -ModelId $ModelId"
    exit 1
}

New-Item -ItemType Directory -Force -Path (Join-Path $root "results") | Out-Null
$env:PYTHONIOENCODING = "utf-8"

python (Join-Path $PSScriptRoot "measure_baseline.py") `
    --model $modelPath `
    --model-id $ModelId `
    --gpu-layers $GpuLayers `
    --ctx-size $CtxSize `
    --n-requests $Requests `
    --max-tokens $MaxTokens `
    --out-dir (Join-Path $root "results")
