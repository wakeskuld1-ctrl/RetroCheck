# Verify gRPC Distributed Transaction with SQLite
# Author: TraeAI
# Date: 2026-02-17
# Description: This script automates the verification of the distributed transaction system.
# It starts one master (SQLite) and two SQLite slaves and runs the client.

# ### Change Log (2026-02-17)
# - Reason: add multi-scenario parameters for fault injection validation
# - Goal: provide configurable scenario/mode/pause window/cleanup control
[CmdletBinding()]
param(
    [string]$Scenario = "full",
    [string]$Engine = "sqlite",
    [int]$PauseBeforeCommitMs = 4000,
    [string]$PauseBeforeCommitMsList = "1000,4000,8000",
    [string]$Mode = "quorum",
    [int]$VerifyRepeatCount = 3,
    [bool]$ResetDbFiles = $true
)

$env:CARGO_TARGET_DIR = "target_new"

# ### Change Log (2026-02-17)
# - Reason: unify address configuration
# - Goal: align script and client parameters
$MasterAddr = "http://127.0.0.1:50051"
$SlaveAddrs = "http://127.0.0.1:50052,http://127.0.0.1:50053"

function Reset-DbFiles {
    # ### Change Log (2026-02-17)
    # - Reason: production-like runs should avoid data cleanup
    # - Goal: control DB deletion via parameter
    if (-not $ResetDbFiles) {
        Write-Host ">>> Skip DB cleanup (ResetDbFiles=false)" -ForegroundColor Yellow
        return
    }
    Write-Host ">>> Cleaning DB files..." -ForegroundColor Yellow
    Remove-Item -Path "master.db","slave1.db","slave2.db" -ErrorAction SilentlyContinue
    Remove-Item -Path "master.db-wal","slave1.db-wal","slave2.db-wal" -ErrorAction SilentlyContinue
    Remove-Item -Path "master.db-shm","slave1.db-shm","slave2.db-shm" -ErrorAction SilentlyContinue
}

function Stop-AllServers {
    Write-Host ">>> Stopping existing server processes..." -ForegroundColor Yellow
    Get-Process server -ErrorAction SilentlyContinue | Stop-Process
}

function Start-Servers {
    # ### Change Log (2026-02-17)
    # - Reason: keep process handles for fault injection
    # - Goal: return process objects to caller
    Write-Host ">>> Starting Master on port 50051..." -ForegroundColor Green
    $master = Start-Process -FilePath "target_new/debug/server.exe" -ArgumentList "--port", "50051", "--db", "master.db", "--engine", $Engine -NoNewWindow -PassThru
    Start-Sleep -Seconds 2

    Write-Host ">>> Starting Slave 1 on port 50052..." -ForegroundColor Green
    $slave1 = Start-Process -FilePath "target_new/debug/server.exe" -ArgumentList "--port", "50052", "--db", "slave1.db", "--engine", $Engine -NoNewWindow -PassThru

    # ### Change Log (2026-02-17)
    # - Reason: use SQLite as default verification engine
    # - Goal: avoid missing dependency failures
    Write-Host ">>> Starting Slave 2 on port 50053..." -ForegroundColor Green
    $slave2 = Start-Process -FilePath "target_new/debug/server.exe" -ArgumentList "--port", "50053", "--db", "slave2.db", "--engine", $Engine -NoNewWindow -PassThru

    Start-Sleep -Seconds 5
    return @{
        Master = $master
        Slave1 = $slave1
        Slave2 = $slave2
    }
}

function Run-ClientFull {
    param(
        [int]$PauseMs = 0
    )
    # ### Change Log (2026-02-17)
    # - Reason: allow pause parameter for full scenario
    # - Goal: reuse full flow for fault injection
    $clientArgs = @(
        "run","--bin","client","--",
        "--master-addr",$MasterAddr,
        "--slave-addrs",$SlaveAddrs,
        "--mode",$Mode,
        "--scenario","full"
    )
    if ($PauseMs -gt 0) {
        $clientArgs += @("--pause-before-commit-ms",$PauseMs)
    }
    Write-Host ">>> Running Client (full)..." -ForegroundColor Cyan
    cargo @clientArgs
}

function Run-ClientVerifyOnly {
    # ### Change Log (2026-02-17)
    # - Reason: verify-only after restart is sufficient
    # - Goal: reduce write noise during repeats
    $clientArgs = @(
        "run","--bin","client","--",
        "--master-addr",$MasterAddr,
        "--slave-addrs",$SlaveAddrs,
        "--mode",$Mode,
        "--scenario","verify-only"
    )
    Write-Host ">>> Running Client (verify-only)..." -ForegroundColor Cyan
    cargo @clientArgs
}

function Wait-ForClientPauseLog {
    param(
        [string]$LogFile,
        [int]$TimeoutMs = 8000
    )
    # ### Change Log (2026-02-17)
    # - Reason: fixed sleep is unstable across load
    # - Goal: use log signal to detect pause window
    $elapsed = 0
    while ($elapsed -lt $TimeoutMs) {
        if (Test-Path $LogFile) {
            $found = Select-String -Path $LogFile -Pattern "Pause Before Commit" -Quiet -ErrorAction SilentlyContinue
            if ($found) {
                return $true
            }
        }
        Start-Sleep -Milliseconds 200
        $elapsed += 200
    }
    return $false
}

function Run-ScenarioFull {
    Reset-DbFiles
    Stop-AllServers
    $procs = Start-Servers
    Run-ClientFull
    Stop-AllServers
}

function Run-ScenarioRestartSingleNode {
    Reset-DbFiles
    Stop-AllServers
    $procs = Start-Servers
    Run-ClientFull
    Write-Host ">>> Stopping Slave 2 for restart scenario..." -ForegroundColor Yellow
    Stop-Process -Id $procs["Slave2"].Id -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 2
    Write-Host ">>> Restarting Slave 2..." -ForegroundColor Yellow
    $procs["Slave2"] = Start-Process -FilePath "target_new/debug/server.exe" -ArgumentList "--port", "50053", "--db", "slave2.db", "--engine", $Engine -NoNewWindow -PassThru
    Start-Sleep -Seconds 3
    # ### Change Log (2026-02-17)
    # - Reason: repeat checks to detect version rollback
    # - Goal: capture non-deterministic failures
    for ($i = 1; $i -le $VerifyRepeatCount; $i++) {
        Write-Host ">>> Repeat verify-only ($i/$VerifyRepeatCount)..." -ForegroundColor Cyan
        Run-ClientVerifyOnly
    }
    Stop-AllServers
}

function Run-ScenarioPrepareCommitKill {
    Reset-DbFiles
    Stop-AllServers
    $procs = Start-Servers
    $logFileOut = "client_prepare_commit_kill_out.log"
    $logFileErr = "client_prepare_commit_kill_err.log"
    Remove-Item -Path $logFileOut,$logFileErr -ErrorAction SilentlyContinue
    Write-Host ">>> Running Client with pause-before-commit ($PauseBeforeCommitMs ms)..." -ForegroundColor Cyan
    $clientArgs = @(
        "run","--bin","client","--",
        "--master-addr",$MasterAddr,
        "--slave-addrs",$SlaveAddrs,
        "--mode",$Mode,
        "--scenario","full",
        "--pause-before-commit-ms",$PauseBeforeCommitMs
    )
    $clientProc = Start-Process -FilePath "cargo" -ArgumentList $clientArgs -NoNewWindow -PassThru -RedirectStandardOutput $logFileOut -RedirectStandardError $logFileErr
    if (-not $clientProc) {
        Write-Error "Client process failed to start."
        Stop-AllServers
        exit 1
    }
    $pauseHit = Wait-ForClientPauseLog -LogFile $logFileOut -TimeoutMs ([Math]::Max($PauseBeforeCommitMs, 3000))
    if (-not $pauseHit) {
        Write-Host ">>> Pause window log not found, fallback to timed wait..." -ForegroundColor Yellow
        Start-Sleep -Milliseconds ([int]($PauseBeforeCommitMs / 2))
    }
    Write-Host ">>> Killing Slave 2 during pause window..." -ForegroundColor Yellow
    Stop-Process -Id $procs["Slave2"].Id -ErrorAction SilentlyContinue
    Wait-Process -Id $clientProc.Id
    Write-Host ">>> Restarting Slave 2 for verification..." -ForegroundColor Yellow
    $procs["Slave2"] = Start-Process -FilePath "target_new/debug/server.exe" -ArgumentList "--port", "50053", "--db", "slave2.db", "--engine", $Engine -NoNewWindow -PassThru
    Start-Sleep -Seconds 3
    for ($i = 1; $i -le $VerifyRepeatCount; $i++) {
        Write-Host ">>> Repeat verify-only ($i/$VerifyRepeatCount)..." -ForegroundColor Cyan
        Run-ClientVerifyOnly
    }
    Stop-AllServers
}

function Run-ScenarioPrepareCommitKillMatrix {
    # ### Change Log (2026-02-17)
    # - Reason: validate stability across multiple pause values
    # - Goal: run a pause matrix in one pass
    $values = $PauseBeforeCommitMsList.Split(",") | ForEach-Object { $_.Trim() } | Where-Object { $_ -ne "" }
    foreach ($value in $values) {
        $PauseBeforeCommitMs = [int]$value
        Write-Host ">>> Matrix Run: PauseBeforeCommitMs=$PauseBeforeCommitMs" -ForegroundColor Cyan
        Run-ScenarioPrepareCommitKill
    }
}

function Run-ScenarioChaosMultiRestart {
    Reset-DbFiles
    Stop-AllServers
    $procs = Start-Servers
    Run-ClientFull
    Write-Host ">>> Restarting Slave 1 (chaos)..." -ForegroundColor Yellow
    Stop-Process -Id $procs["Slave1"].Id -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 2
    $procs["Slave1"] = Start-Process -FilePath "target_new/debug/server.exe" -ArgumentList "--port", "50052", "--db", "slave1.db", "--engine", $Engine -NoNewWindow -PassThru
    Start-Sleep -Seconds 3
    Write-Host ">>> Restarting Slave 2 (chaos)..." -ForegroundColor Yellow
    Stop-Process -Id $procs["Slave2"].Id -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 2
    $procs["Slave2"] = Start-Process -FilePath "target_new/debug/server.exe" -ArgumentList "--port", "50053", "--db", "slave2.db", "--engine", $Engine -NoNewWindow -PassThru
    Start-Sleep -Seconds 3
    for ($i = 1; $i -le $VerifyRepeatCount; $i++) {
        Write-Host ">>> Repeat verify-only ($i/$VerifyRepeatCount)..." -ForegroundColor Cyan
        Run-ClientVerifyOnly
    }
    Stop-AllServers
}

function Run-ScenarioChaosMixedFaults {
    Reset-DbFiles
    Stop-AllServers
    $procs = Start-Servers
    $logFileOut = "client_chaos_mixed_faults_out.log"
    $logFileErr = "client_chaos_mixed_faults_err.log"
    Remove-Item -Path $logFileOut,$logFileErr -ErrorAction SilentlyContinue
    Write-Host ">>> Running Client with pause-before-commit ($PauseBeforeCommitMs ms)..." -ForegroundColor Cyan
    $clientArgs = @(
        "run","--bin","client","--",
        "--master-addr",$MasterAddr,
        "--slave-addrs",$SlaveAddrs,
        "--mode",$Mode,
        "--scenario","full",
        "--pause-before-commit-ms",$PauseBeforeCommitMs
    )
    $clientProc = Start-Process -FilePath "cargo" -ArgumentList $clientArgs -NoNewWindow -PassThru -RedirectStandardOutput $logFileOut -RedirectStandardError $logFileErr
    if (-not $clientProc) {
        Write-Error "Client process failed to start."
        Stop-AllServers
        exit 1
    }
    $pauseHit = Wait-ForClientPauseLog -LogFile $logFileOut -TimeoutMs ([Math]::Max($PauseBeforeCommitMs, 3000))
    if (-not $pauseHit) {
        Write-Host ">>> Pause window log not found, fallback to timed wait..." -ForegroundColor Yellow
        Start-Sleep -Milliseconds ([int]($PauseBeforeCommitMs / 2))
    }
    Write-Host ">>> Killing Slave 2 during pause window (chaos)..." -ForegroundColor Yellow
    Stop-Process -Id $procs["Slave2"].Id -ErrorAction SilentlyContinue
    Wait-Process -Id $clientProc.Id
    Write-Host ">>> Restarting Slave 2 (chaos)..." -ForegroundColor Yellow
    $procs["Slave2"] = Start-Process -FilePath "target_new/debug/server.exe" -ArgumentList "--port", "50053", "--db", "slave2.db", "--engine", $Engine -NoNewWindow -PassThru
    Start-Sleep -Seconds 3
    Write-Host ">>> Killing Slave 1 after commit (chaos)..." -ForegroundColor Yellow
    Stop-Process -Id $procs["Slave1"].Id -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 2
    Write-Host ">>> Restarting Slave 1 (chaos)..." -ForegroundColor Yellow
    $procs["Slave1"] = Start-Process -FilePath "target_new/debug/server.exe" -ArgumentList "--port", "50052", "--db", "slave1.db", "--engine", $Engine -NoNewWindow -PassThru
    Start-Sleep -Seconds 3
    for ($i = 1; $i -le $VerifyRepeatCount; $i++) {
        Write-Host ">>> Repeat verify-only ($i/$VerifyRepeatCount)..." -ForegroundColor Cyan
        Run-ClientVerifyOnly
    }
    Stop-AllServers
}

function Run-ScenarioChaosPrepareCommitAlternate {
    # ### Change Log (2026-02-18)
    # - Reason: add alternating prepare/commit kill scenario
    # - Goal: validate replay consistency under alternating slave failures
    Reset-DbFiles
    for ($round = 1; $round -le $VerifyRepeatCount; $round++) {
        # ### Change Log (2026-02-18)
        # - Reason: ensure clean process handles per round
        # - Goal: isolate each alternating failure cycle
        Stop-AllServers
        $procs = Start-Servers
        # ### Change Log (2026-02-18)
        # - Reason: PowerShell 不允许 stdout/stderr 指向同一文件
        # - Goal: 分离日志文件避免 Start-Process 失败
        $logFileOut = "client_prepare_commit_alternate_${round}_out.log"
        $logFileErr = "client_prepare_commit_alternate_${round}_err.log"
        Remove-Item -Path $logFileOut,$logFileErr -ErrorAction SilentlyContinue
        # ### Change Log (2026-02-18)
        # - Reason: PowerShell 变量拼接导致解析错误
        # - Goal: 使用 ${} 明确变量边界
        Write-Host ">>> Round ${round}/${VerifyRepeatCount}: Running Client with pause-before-commit ($PauseBeforeCommitMs ms)..." -ForegroundColor Cyan
        $clientArgs = @(
            "run","--bin","client","--",
            "--master-addr",$MasterAddr,
            "--slave-addrs",$SlaveAddrs,
            "--mode",$Mode,
            "--scenario","full",
            "--pause-before-commit-ms",$PauseBeforeCommitMs
        )
        $clientProc = Start-Process -FilePath "cargo" -ArgumentList $clientArgs -NoNewWindow -PassThru -RedirectStandardOutput $logFileOut -RedirectStandardError $logFileErr
        # ### Change Log (2026-02-18)
        # - Reason: Start-Process 失败会返回空对象
        # - Goal: 及时中止以避免后续空引用
        if (-not $clientProc) {
            Write-Error "Client process failed to start."
            Stop-AllServers
            exit 1
        }
        $pauseHit = Wait-ForClientPauseLog -LogFile $logFileOut -TimeoutMs ([Math]::Max($PauseBeforeCommitMs, 3000))
        if (-not $pauseHit) {
            Write-Host ">>> Pause window log not found, fallback to timed wait..." -ForegroundColor Yellow
            Start-Sleep -Milliseconds ([int]($PauseBeforeCommitMs / 2))
        }
        if ($round % 2 -eq 1) {
            # ### Change Log (2026-02-18)
            # - Reason: alternate kill target each round
            # - Goal: validate both slaves can recover via replay
            Write-Host ">>> Killing Slave 1 during pause window (alternate)..." -ForegroundColor Yellow
            Stop-Process -Id $procs["Slave1"].Id -ErrorAction SilentlyContinue
        } else {
            # ### Change Log (2026-02-18)
            # - Reason: alternate kill target each round
            # - Goal: validate both slaves can recover via replay
            Write-Host ">>> Killing Slave 2 during pause window (alternate)..." -ForegroundColor Yellow
            Stop-Process -Id $procs["Slave2"].Id -ErrorAction SilentlyContinue
        }
        Wait-Process -Id $clientProc.Id
        if ($round % 2 -eq 1) {
            # ### Change Log (2026-02-18)
            # - Reason: restart killed slave for replay
            # - Goal: allow verify-only to observe recovery
            Write-Host ">>> Restarting Slave 1 (alternate)..." -ForegroundColor Yellow
            $procs["Slave1"] = Start-Process -FilePath "target_new/debug/server.exe" -ArgumentList "--port", "50052", "--db", "slave1.db", "--engine", $Engine -NoNewWindow -PassThru
        } else {
            # ### Change Log (2026-02-18)
            # - Reason: restart killed slave for replay
            # - Goal: allow verify-only to observe recovery
            Write-Host ">>> Restarting Slave 2 (alternate)..." -ForegroundColor Yellow
            $procs["Slave2"] = Start-Process -FilePath "target_new/debug/server.exe" -ArgumentList "--port", "50053", "--db", "slave2.db", "--engine", $Engine -NoNewWindow -PassThru
        }
        Start-Sleep -Seconds 3
        # ### Change Log (2026-02-18)
        # - Reason: re-check consistency after restart
        # - Goal: detect replay gaps early
        Run-ClientVerifyOnly
    }
    Stop-AllServers
}

function Run-ScenarioRepeatVerify3x {
    Reset-DbFiles
    Stop-AllServers
    $procs = Start-Servers
    Run-ClientFull
    for ($i = 1; $i -le $VerifyRepeatCount; $i++) {
        Write-Host ">>> Repeat verify-only ($i/$VerifyRepeatCount)..." -ForegroundColor Cyan
        Run-ClientVerifyOnly
    }
    Stop-AllServers
}

Write-Host ">>> Building Project..." -ForegroundColor Cyan
cargo build
if ($LASTEXITCODE -ne 0) {
    Write-Error "Build failed!"
    exit 1
}

switch ($Scenario) {
    "full" { Run-ScenarioFull }
    "restart_single_node" { Run-ScenarioRestartSingleNode }
    "prepare_commit_kill" { Run-ScenarioPrepareCommitKill }
    "prepare_commit_kill_matrix" { Run-ScenarioPrepareCommitKillMatrix }
    "chaos_prepare_commit_kill_matrix" { Run-ScenarioPrepareCommitKillMatrix }
    "chaos_prepare_commit_alternate" { Run-ScenarioChaosPrepareCommitAlternate }
    "repeat_verify_3x" { Run-ScenarioRepeatVerify3x }
    "chaos_multi_restart" { Run-ScenarioChaosMultiRestart }
    "chaos_mixed_faults" { Run-ScenarioChaosMixedFaults }
    default {
        Write-Host "Unknown scenario '$Scenario', fallback to full." -ForegroundColor Yellow
        Run-ScenarioFull
    }
}
