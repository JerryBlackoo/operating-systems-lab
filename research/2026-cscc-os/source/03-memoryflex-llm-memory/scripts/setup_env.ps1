<#
Environment self-check: GPU / driver / CUDA / compiler / git / cmake / python / disk
Usage: powershell -ExecutionPolicy Bypass -File scripts\setup_env.ps1
#>
$ErrorActionPreference = "Stop"

function Check([string]$name, [scriptblock]$block) {
    try {
        $v = & $block 2>$null
        if ($LASTEXITCODE -eq 0 -and $null -ne $v -and "$v" -ne "") {
            Write-Host ("[OK]   {0,-16} {1}" -f $name, $v)
            return $true
        }
        Write-Host ("[WARN] {0,-16} not found" -f $name)
    } catch {
        Write-Host ("[WARN] {0,-16} not found" -f $name)
    }
    return $false
}

Write-Host "===== Environment self-check =====" -ForegroundColor Cyan

# GPU
$gpu = nvidia-smi --query-gpu=name,memory.total,driver_version --format=csv,noheader 2>$null
if ($gpu) { Write-Host "[OK]   GPU            $gpu" } else { Write-Host "[WARN] GPU            nvidia-smi not found" }

# CUDA
$nvcc = nvcc --version 2>$null | Select-String "release" | ForEach-Object { $_.Line.Trim() }
if ($nvcc) { Write-Host "[OK]   CUDA           $nvcc" } else { Write-Host "[WARN] CUDA           nvcc not found" }

# Compiler (VS2022)
$vs = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$vc = $null
if (Test-Path $vs) {
    $vc = & $vs -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath 2>$null
}
if ($vc) { Write-Host "[OK]   VS2022         $vc" } else { Write-Host "[WARN] VS2022         C++ toolchain not found" }

# Tools
$null = Check "git"    { git --version }
$null = Check "cmake"  { cmake --version | Select-Object -First 1 }
$null = Check "python" { python --version }
$null = Check "pip"    { pip --version | Select-Object -First 1 }

# Python deps
try {
    python -c "import psutil, requests; print('psutil', psutil.__version__, '/ requests', requests.__version__)"
} catch {
    Write-Host "[WARN] Python deps   psutil/requests missing; run: pip install psutil requests"
}

# Disk
try {
    $d = [System.IO.DriveInfo]::GetDrives() | Where-Object { $_.IsReady -and $_.Name -eq "D:\" }
    Write-Host ("[OK]   Disk           D: free {0} GB / {1} GB" -f [math]::Round($d.AvailableFreeSpace/1GB,1), [math]::Round($d.TotalSize/1GB,1))
} catch { Write-Host "[WARN] Disk           cannot read" }

Write-Host "`n===== Self-check done =====" -ForegroundColor Cyan
