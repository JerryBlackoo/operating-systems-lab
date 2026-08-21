<#
Download GGUF models (network required; supports resume)
Usage:
  powershell -ExecutionPolicy Bypass -File scripts\download_models.ps1
  powershell -ExecutionPolicy Bypass -File scripts\download_models.ps1 -ModelId qwen2.5-1.5b
#>
param(
    [string]$ModelId = "",
    [switch]$IncludeOptional
)
$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
$cfgPath = Join-Path $root "config\models.json"
$modelDir = Join-Path $root "models"
New-Item -ItemType Directory -Force -Path $modelDir | Out-Null

$cfg = Get-Content -Raw -Encoding UTF8 $cfgPath | ConvertFrom-Json
$models = @($cfg.models | Where-Object {
    ($ModelId -eq "" -and $_.enabled) -or ($ModelId -ne "" -and $_.id -eq $ModelId) -or ($IncludeOptional -and $_.id -eq $ModelId)
})

if ($models.Count -eq 0) {
    Write-Host "No matching model. Available: $((@($cfg.models) | ForEach-Object { $_.id }) -join ', ')"
    exit 1
}

foreach ($m in $models) {
    $base = if ($env:HF_ENDPOINT) { $env:HF_ENDPOINT } else { "https://hf-mirror.com" }
    $url = "$base/$($m.repo)/resolve/main/$($m.file)"
    $out = Join-Path $modelDir $m.file
    $exists = if (Test-Path $out) { (Get-Item $out).Length } else { 0 }
    if ($exists -ge ($m.size_bytes_expected * 0.95)) {
        Write-Host "[skip] $($m.id) already exists ($([math]::Round($exists/1MB,0)) MB)"
        continue
    }
    Write-Host "[..] Downloading $($m.id): $url"
    curl.exe -4 --ssl-no-revoke -L --retry 3 -C - --fail --output $out $url
    if ($LASTEXITCODE -ne 0) { Write-Host "[FAIL] $($m.id) download failed"; exit 1 }
    $size = (Get-Item $out).Length
    if ($size -ge ($m.size_bytes_expected * 0.95)) {
        Write-Host "[OK]   $($m.id) $([math]::Round($size/1MB,0)) MB"
    } else {
        Write-Host "[FAIL] $($m.id) incomplete: $([math]::Round($size/1MB,0)) MB (expected ~$([math]::Round($m.size_bytes_expected/1MB,0)) MB)"
        exit 1
    }
}
Write-Host "`nModel dir: $modelDir"
