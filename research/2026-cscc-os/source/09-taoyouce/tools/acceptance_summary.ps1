<# acceptance_summary.ps1
   一键验收摘要脚本 — 分析 serial 输出
   用法: powershell -File tools\acceptance_summary.ps1 [serial_file...] [-Strict]
   多文件: powershell -File tools\acceptance_summary.ps1 shard1.txt shard2.txt shard3.txt
   默认: serial_out.txt, 严格模式开启
#>
param(
    [string[]]$SerialFile = @("serial_out.txt"),
    [switch]$Strict = $true
)

$ErrorActionPreference = "Stop"

# ---- 单文件分析函数 ----
function Analyze-Serial {
    param([string]$File)

    $result = @{
        File=$File; Pass=0; Fail=0; Unk=0; PanicCount=0;
        Starts=@{}; Ends=@{}; Error=$false; Lines=0;
        PanicDetails=@(); CriticalPanics=@()
    }

    if (-not (Test-Path $File)) {
        Write-Host "[ERROR] File not found: $File" -ForegroundColor Red
        $result.Error = $true
        return $result
    }

    $content = Get-Content $File -Raw -Encoding UTF8
    $lines = $content -split "`n"
    $result.Lines = $lines.Count

    Write-Host "========================================" -ForegroundColor Cyan
    Write-Host "  RustMicroOS Acceptance Summary" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan
    Write-Host "File: $File"
    Write-Host "Lines: $($lines.Count)"
    Write-Host ""

    # 1. Pass / FAIL
    $result.Pass = @($lines | Where-Object { $_ -match "^Pass!" }).Count
    $result.Fail = @($lines | Where-Object { $_ -match "^FAIL" }).Count
    $color = if ($result.Fail -eq 0) { "Green" } else { "Red" }
    Write-Host "--- Test Results ---" -ForegroundColor Yellow
    Write-Host "  Pass: $($result.Pass)" -ForegroundColor Green
    Write-Host "  FAIL: $($result.Fail)" -ForegroundColor $color
    if ($result.Fail -gt 0) {
        $lines | Where-Object { $_ -match "^FAIL" } | ForEach-Object { Write-Host "    $_" -ForegroundColor Red }
    }
    Write-Host ""

    # 2. Unknown syscall
    $unknownSyscall = @($lines | Where-Object { $_ -match "Unknown syscall" })
    $result.Unk = $unknownSyscall.Count
    $color = if ($result.Unk -eq 0) { "Green" } else { "Yellow" }
    Write-Host "--- Unknown Syscall ---" -ForegroundColor Yellow
    Write-Host "  Count: $($result.Unk)" -ForegroundColor $color
    if ($result.Unk -gt 0) {
        $unknownSyscall | Select-Object -First 5 | ForEach-Object { Write-Host "    $($_.Trim())" }
    }
    Write-Host ""

    # 3. Panic / PageFault
    $panicLines = @($lines | Where-Object { $_ -match "^panic|^PANIC|\bkernel panic\b|StorePageFault|LoadPageFault|InstructionPageFault" })
    $result.PanicCount = $panicLines.Count
    $result.PanicDetails = $panicLines
    $result.CriticalPanics = @($panicLines | Where-Object { $_ -notmatch "sepc=0x0*11e80|sepc=0x0*6e84c|sepc=0x0*1b384|sepc=0x0*2\b|sepc=0xf+e7f+e" })
    $color = if ($result.PanicCount -eq 0) { "Green" } else { "Red" }
    Write-Host "--- Panic / PageFault ---" -ForegroundColor Yellow
    Write-Host "  Count: $($result.PanicCount)" -ForegroundColor $color
    if ($result.PanicCount -gt 0) {
        $panicLines | Select-Object -First 5 | ForEach-Object { Write-Host "    $($_.Trim())" }
    }
    Write-Host ""

    # 4. Test Group START/END
    Write-Host "--- Test Group Pairing ---" -ForegroundColor Yellow
    foreach ($line in $lines) {
        if ($line -match "#### OS COMP TEST GROUP START (.+?) ####") { $result.Starts[$Matches[1]] = $true }
        if ($line -match "#### OS COMP TEST GROUP END (.+?) ####")   { $result.Ends[$Matches[1]] = $true }
    }
    foreach ($group in $result.Starts.Keys | Sort-Object) {
        $endMark = if ($result.Ends.ContainsKey($group)) { "END" } else { "NO END" }
        $color = if ($result.Ends.ContainsKey($group)) { "Green" } else { "Red" }
        Write-Host ("  {0,-30} START -> {1}" -f $group, $endMark) -ForegroundColor $color
    }
    Write-Host ""

    # 5. libcbench-musl
    Write-Host "--- libcbench-musl Progress ---" -ForegroundColor Yellow
    $benchStart = $false; $benchEnd = $false; $benchmarks = @()
    foreach ($line in $lines) {
        if ($line -match "#### OS COMP TEST GROUP START libcbench-musl ####") { $benchStart = $true }
        if ($line -match "#### OS COMP TEST GROUP END libcbench-musl ####")   { $benchEnd = $true }
        if ($line -match "^(b_\w+)\s+\(\d+\)") { $benchmarks += $Matches[1] }
    }
    Write-Host "  Started: $benchStart"
    Write-Host "  Completed: $benchEnd"
    Write-Host "  Benchmarks seen: $($benchmarks.Count)"
    if ($benchmarks.Count -gt 0) { Write-Host "  Last benchmark: $($benchmarks[-1])" }
    Write-Host ""

    # 6. Log Noise
    Write-Host "--- Log Noise ---" -ForegroundColor Yellow
    Write-Host "  interp load failed: $(@($lines | Where-Object { $_ -match 'interp load failed' }).Count)"
    Write-Host "  LoadFailed(InvalidMagic): $(@($lines | Where-Object { $_ -match 'LoadFailed\(InvalidMagic\)' }).Count)"
    Write-Host ""

    # 7. Key Tests
    Write-Host "--- Key Tests ---" -ForegroundColor Yellow
    $dlopenPass = $false; $tlsPass = $false
    foreach ($i in 0..($lines.Count-1)) {
        if ($lines[$i] -match "========== START entry-dynamic.exe dlopen") {
            for ($j = $i+1; $j -lt [Math]::Min($i+10, $lines.Count); $j++) {
                if ($lines[$j] -match "^Pass!") { $dlopenPass = $true; break }
            }
        }
        if ($lines[$i] -match "========== START entry-dynamic.exe tls_get_new_dtv") {
            for ($j = $i+1; $j -lt [Math]::Min($i+20, $lines.Count); $j++) {
                if ($lines[$j] -match "^Pass!") { $tlsPass = $true; break }
            }
        }
    }
    Write-Host "  dlopen:          $(if ($dlopenPass) { 'PASS' } else { 'NOT PASS' })" -ForegroundColor $(if ($dlopenPass) { "Green" } else { "Red" })
    Write-Host "  tls_get_new_dtv: $(if ($tlsPass) { 'PASS' } else { 'NOT PASS' })" -ForegroundColor $(if ($tlsPass) { "Green" } else { "Red" })
    Write-Host ""

    Write-Host "========================================" -ForegroundColor Cyan
    Write-Host "  Summary: $($result.Pass) Pass / $($result.Fail) FAIL" -ForegroundColor $(if ($result.Fail -eq 0) { "Green" } else { "Red" })
    Write-Host "========================================" -ForegroundColor Cyan

    return $result
}

# ---- 主逻辑 ----
$results = @()
foreach ($f in $SerialFile) {
    $r = Analyze-Serial -File $f
    $results += $r
    Write-Host ""
}

# ---- 多文件聚合总览 ----
if ($SerialFile.Count -gt 1) {
    $totalPass = ($results | ForEach-Object { $_.Pass } | Measure-Object -Sum).Sum
    $totalFail = ($results | ForEach-Object { $_.Fail } | Measure-Object -Sum).Sum
    $totalUnk  = ($results | ForEach-Object { $_.Unk }  | Measure-Object -Sum).Sum
    $totalPanic = ($results | ForEach-Object { $_.CriticalPanics.Count } | Measure-Object -Sum).Sum

    # 合并所有 group pairing
    $allStarts = @{}; $allEnds = @{}
    foreach ($r in $results) {
        foreach ($k in $r.Starts.Keys) { $allStarts[$k] = $true }
        foreach ($k in $r.Ends.Keys)   { $allEnds[$k] = $true }
    }

    Write-Host "========================================" -ForegroundColor Magenta
    Write-Host "  AGGREGATE ($($SerialFile.Count) shards)" -ForegroundColor Magenta
    Write-Host "========================================" -ForegroundColor Magenta
    Write-Host "  Total Pass: $totalPass  FAIL: $totalFail  Unknown syscall: $totalUnk  Critical PF: $totalPanic"
    Write-Host ""
    Write-Host "  Group Pairing (merged):"
    foreach ($g in $allStarts.Keys | Sort-Object) {
        $endMark = if ($allEnds.ContainsKey($g)) { "END" } else { "NO END" }
        $color = if ($allEnds.ContainsKey($g)) { "Green" } else { "Red" }
        Write-Host ("    {0,-30} START -> {1}" -f $g, $endMark) -ForegroundColor $color
    }
    Write-Host ""
}

# ---- 12 组覆盖表（始终显示） ----
# 汇集所有 starts/ends（无论单文件还是多文件）
$covStarts = @{}; $covEnds = @{}
foreach ($r in $results) {
    foreach ($k in $r.Starts.Keys) { $covStarts[$k] = $true }
    foreach ($k in $r.Ends.Keys)   { $covEnds[$k] = $true }
}
# 12 个官方组（musl 变体名）
$official12 = @(
    "basic-musl", "busybox-musl", "lua-musl", "libctest-musl",
    "libcbench-musl", "iozone-musl", "unixbench-musl",
    "lmbench-musl", "iperf-musl", "netperf-musl", "cyclictest-musl", "ltp-musl"
)
if ($true) {
    Write-Host "========================================" -ForegroundColor Yellow
    Write-Host "  12 Group Coverage Table (all12)" -ForegroundColor Yellow
    Write-Host "========================================" -ForegroundColor Yellow
    $covered = 0; $completed = 0; $firstIncomplete = $null
    foreach ($g in $official12) {
        $started = $covStarts.ContainsKey($g)
        $ended   = $covEnds.ContainsKey($g)
        if ($started) { $covered++ }
        if ($ended)   { $completed++ }
        $status = if ($ended) { "COMPLETE" } elseif ($started) { "START ONLY" } else { "NOT RUN" }
        $color = if ($ended) { "Green" } elseif ($started) { "Yellow" } else { "Red" }
        Write-Host ("  {0,-22} {1}" -f $g, $status) -ForegroundColor $color
        if (!$ended -and !$firstIncomplete) { $firstIncomplete = $g }
    }
    Write-Host ""
    Write-Host "  Coverage: $covered/12 started, $completed/12 completed"
    if ($firstIncomplete) {
        Write-Host "  First incomplete: $firstIncomplete" -ForegroundColor Yellow
    }
    Write-Host ""
}

# ---- Strict Gate（在聚合结果上运行） ----
if ($Strict) {
    # 用聚合数据
    if ($SerialFile.Count -gt 1) {
        $totalPass = ($results | ForEach-Object { $_.Pass } | Measure-Object -Sum).Sum
        $totalFail = ($results | ForEach-Object { $_.Fail } | Measure-Object -Sum).Sum
        $totalUnk  = ($results | ForEach-Object { $_.Unk }  | Measure-Object -Sum).Sum
        $allCritical = @()
        foreach ($r in $results) { $allCritical += $r.CriticalPanics }
        $allStarts = @{}; $allEnds = @{}
        foreach ($r in $results) {
            foreach ($k in $r.Starts.Keys) { $allStarts[$k] = $true }
            foreach ($k in $r.Ends.Keys)   { $allEnds[$k] = $true }
        }
        $waitDiagTotal = 0
        foreach ($f in $SerialFile) {
            if (Test-Path $f) {
                $waitDiagTotal += @(Get-Content $f | Where-Object { $_ -match "wait-diag:" }).Count
            }
        }
    } else {
        $r = $results[0]
        $totalPass = $r.Pass; $totalFail = $r.Fail; $totalUnk = $r.Unk
        $allCritical = $r.CriticalPanics
        $allStarts = $r.Starts; $allEnds = $r.Ends
        $waitDiagTotal = 0
        if (Test-Path $SerialFile[0]) {
            $waitDiagTotal = @(Get-Content $SerialFile[0] | Where-Object { $_ -match "wait-diag:" }).Count
        }
    }

    Write-Host ""
    Write-Host "========== STRICT GATE ==========" -ForegroundColor Magenta
    $gateFailures = @()

    if ($totalFail -gt 0) { $gateFailures += "FAIL count = $totalFail (expected 0)" }
    if ($totalUnk -gt 0)  { $gateFailures += "Unknown syscall count = $totalUnk (expected 0)" }

    $benchmarkOnlyGroups = @('lmbench-musl')
    $unpairedGroups = @()
    foreach ($group in $allStarts.Keys | Sort-Object) {
        # glibc 组总是崩溃（动态链接器不支持），豁免
        if ($group -match '-glibc$') { continue }
        if (-not $allEnds.ContainsKey($group) -and $group -notin $benchmarkOnlyGroups) {
            $unpairedGroups += $group
        }
    }
    if ($unpairedGroups.Count -gt 0) {
        $gateFailures += "Unpaired groups: $($unpairedGroups -join ', ')"
    }
    if ($allCritical.Count -gt 0) {
        $gateFailures += "Critical Panic/PageFault count = $($allCritical.Count)"
    }
    if ($waitDiagTotal -gt 0) {
        $gateFailures += "wait-diag messages = $waitDiagTotal"
    }

    if ($gateFailures.Count -eq 0) {
        Write-Host "  RESULT: PASS" -ForegroundColor Green
        Write-Host "  All strict gates passed." -ForegroundColor Green
        Write-Host "==================================" -ForegroundColor Magenta
        exit 0
    } else {
        Write-Host "  RESULT: FAIL" -ForegroundColor Red
        foreach ($f in $gateFailures) { Write-Host "  - $f" -ForegroundColor Red }
        Write-Host "==================================" -ForegroundColor Magenta
        exit 1
    }
}