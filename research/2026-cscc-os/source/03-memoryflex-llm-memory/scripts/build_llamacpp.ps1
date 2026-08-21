<#
Build llama.cpp from source (CPU + CUDA backends)
Usage: powershell -ExecutionPolicy Bypass -File scripts\build_llamacpp.ps1
Optional: -SkipClone (build only, skip git clone)

NOTE: keep this file ASCII-only. Windows PowerShell 5.1 reads .ps1 without
BOM as ANSI, so non-ASCII comments can break parsing.
#>
param(
    [switch]$SkipClone
)
$ErrorActionPreference = "Stop"

$root  = Split-Path -Parent $PSScriptRoot
$src   = Join-Path $root "tools\llama.cpp"
$build = Join-Path $src "build"
New-Item -ItemType Directory -Force -Path (Join-Path $root "results") | Out-Null

# Workaround for a leftover CUDA 13.0 VS integration on this machine:
# the machine env var CUDA_PATH_V13_0 points to a non-existent v13.0 dir,
# while v12.4 is the installed toolkit. Override whenever v12.4 exists and
# the current value is missing/invalid.
$cuda124 = "C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v12.4"
if ((Test-Path $cuda124) -and (-not $env:CUDA_PATH_V13_0 -or -not (Test-Path $env:CUDA_PATH_V13_0))) {
    $env:CUDA_PATH_V13_0 = $cuda124
    Write-Host "[ok]   CUDA_PATH_V13_0 -> $cuda124 (CUDA 13.0 leftover workaround)"
}

if (-not $SkipClone) {
    # Existence check uses a source marker file (CMakeLists.txt), not .git:
    # this repo renames the nested .git to .git_bak so the outer repo can
    # track the llama.cpp sources, so .git may be absent.
    if (Test-Path (Join-Path $src "CMakeLists.txt")) {
        Write-Host "[skip] llama.cpp source exists: $src (nested git: .git_bak)"
    } else {
        New-Item -ItemType Directory -Force -Path (Join-Path $root "tools") | Out-Null
        $cloneUrls = @(
            "https://gitcode.com/gh_mirrors/ll/llama.cpp.git",
            "https://github.com/ggml-org/llama.cpp",
            "https://gitee.com/mirrors/llama.cpp.git"
        )
        $cloned = $false
        foreach ($url in $cloneUrls) {
            Write-Host "[..] Cloning llama.cpp from $url (depth 1) ..."
            git -c http.schannelCheckRevoke=false clone --depth 1 $url $src 2>$null
            if ($LASTEXITCODE -eq 0) { $cloned = $true; break }
            # Safety: never auto-delete an existing directory (may contain
            # local modifications / patches).
            Write-Host "[warn] clone failed from $url; existing source dir left untouched"
        }
        if (-not $cloned) { throw "git clone failed (network required)" }
    }
}

$commit = (git -C $src rev-parse --short HEAD 2>$null)
if (-not $commit) {
    $commitFile = Join-Path $root "results\llama_cpp_commit.txt"
    if (Test-Path $commitFile) { $commit = (Get-Content $commitFile -Raw).Trim() }
    if (-not $commit) { $commit = "11924d4 (outer-repo tracked)" }
}
Write-Host "[..] llama.cpp commit: $commit"
$commit | Out-File -FilePath (Join-Path $root "results\llama_cpp_commit.txt") -Encoding utf8

Write-Host "[..] CMake configure (VS2022 + CUDA, sm_89) ..."
cmake -S $src -B $build -G "Visual Studio 17 2022" -A x64 `
    -DGGML_CUDA=ON `
    -DCMAKE_CUDA_ARCHITECTURES=89 `
    -DCMAKE_BUILD_TYPE=Release
if ($LASTEXITCODE -ne 0) { throw "cmake configure failed" }

Write-Host "[..] Building (llama-server / llama-cli / llama-bench) ..."
cmake --build $build --config Release --target llama-server llama-cli llama-bench --parallel 8
if ($LASTEXITCODE -ne 0) { throw "cmake build failed" }

$bin = Join-Path $build "bin\Release"
Get-ChildItem $bin -Filter "llama-*.exe" | ForEach-Object { Write-Host "[OK]   $($_.Name)" }
Write-Host "`nBuild done. Binaries: $bin"
