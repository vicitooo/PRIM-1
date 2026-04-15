<#
.SYNOPSIS
  PRIM-001 heartbeat — runs health-check.ps1, writes a one-line status record
  to .runtime/heartbeat.log, optionally routes a BLOCKED message to the room
  when N consecutive heartbeats fail.

.DESCRIPTION
  Intended as the loop body for autonomous PRIM-001 monitoring. Call it every
  30-120 seconds from a scheduled task, cron equivalent, or a simple while loop:

    while ($true) { .\scripts\heartbeat.ps1; Start-Sleep -Seconds 60 }

  Each invocation:
    1. Runs scripts/health-check.ps1 -Json
    2. Parses the result
    3. Writes one JSONL line to .runtime/heartbeat.log with timestamp, overall
       status, pane states, and failure/warning counts
    4. If overall is "fail" for -AlertThreshold consecutive heartbeats, routes a
       BLOCKED room message to surface the problem to Victor

  Heartbeat log lines are append-only. Rotation / archival is the operator's
  job (pick a cron/retention strategy that fits).

.PARAMETER AlertThreshold
  Number of consecutive FAIL heartbeats before routing a BLOCKED message to the
  room. Default 3. Set to 0 to disable room alerts entirely (log-only mode).

.PARAMETER RoomFrom
  The -From actor for alert routing. Default "heartbeat". Chosen to be visually
  distinct from claude/codex/victor in the audit log.
#>
param(
  [int]$AlertThreshold = 3,
  [string]$RoomFrom = "heartbeat"
)

$scriptRoot = Split-Path -Parent $PSCommandPath
$wrapperRoot = (Resolve-Path (Join-Path $scriptRoot "..")).Path
$runtimeDir = Join-Path $wrapperRoot ".runtime"
$heartbeatLog = Join-Path $runtimeDir "heartbeat.log"
$healthCheckScript = Join-Path $scriptRoot "health-check.ps1"
$agentRouteScript = Join-Path $scriptRoot "agent-route.ps1"

if (-not (Test-Path -LiteralPath $runtimeDir)) {
  New-Item -ItemType Directory -Path $runtimeDir -Force | Out-Null
}

# Run health check in JSON mode, capture output
$healthJson = $null
$healthExit = 0
try {
  $rawOutput = & powershell -NoProfile -ExecutionPolicy Bypass -File $healthCheckScript -Json 2>&1
  $healthExit = $LASTEXITCODE
  if ($rawOutput) {
    $healthJson = $rawOutput | Out-String | ConvertFrom-Json
  }
} catch {
  $healthExit = -1
}

$timestamp = (Get-Date).ToUniversalTime().ToString("o")
$overall = if ($healthJson) { $healthJson.overall } else { "unknown" }
$failCount = if ($healthJson) { $healthJson.failures.Count } else { 0 }
$warnCount = if ($healthJson) { $healthJson.warnings.Count } else { 0 }
$wrapperPid = if ($healthJson -and $healthJson.wrapper) { $healthJson.wrapper.pid } else { $null }
$paneStates = @{}
if ($healthJson -and $healthJson.panes) {
  foreach ($p in $healthJson.panes) {
    $paneStates[$p.name] = $p.lifecycle_state
  }
}

$logRecord = [ordered]@{
  timestamp = $timestamp
  overall = $overall
  health_exit = $healthExit
  wrapper_pid = $wrapperPid
  pane_states = $paneStates
  fail_count = $failCount
  warn_count = $warnCount
}

# Append to heartbeat log
$logLine = $logRecord | ConvertTo-Json -Compress -Depth 5
Add-Content -LiteralPath $heartbeatLog -Value $logLine -Encoding UTF8

# Consecutive-fail logic: count trailing fail records in the log
$consecutiveFails = 0
if ($overall -eq "fail") {
  $tailLines = Get-Content -LiteralPath $heartbeatLog -Tail 20 -ErrorAction SilentlyContinue
  if ($tailLines) {
    $reversed = @($tailLines)
    [array]::Reverse($reversed)
    foreach ($line in $reversed) {
      try {
        $r = $line | ConvertFrom-Json
        if ($r.overall -eq "fail") {
          $consecutiveFails++
        } else {
          break
        }
      } catch { break }
    }
  }
}

# Emit to stdout for operator visibility (-Quiet-style: only if not OK)
if ($overall -ne "ok") {
  Write-Host "heartbeat $timestamp  overall=$overall  fails=$failCount  warns=$warnCount  consecutive_fails=$consecutiveFails"
} else {
  # Silent on OK unless explicitly verbose — logfile only
}

# Alert if consecutive fails reach threshold and threshold > 0
if ($AlertThreshold -gt 0 -and $consecutiveFails -ge $AlertThreshold -and $overall -eq "fail") {
  $alertContent = "BLOCKED: heartbeat detected $consecutiveFails consecutive failures. Latest failures: " + (
    ($healthJson.failures | Select-Object -First 3) -join "; "
  )
  try {
    & powershell -NoProfile -ExecutionPolicy Bypass -File $agentRouteScript -From $RoomFrom -To room -Scope room -Content $alertContent 2>&1 | Out-Null
    Write-Host "heartbeat: routed BLOCKED alert to room after $consecutiveFails consecutive fails"
  } catch {
    Write-Host "heartbeat: failed to route alert: $_" -ForegroundColor Red
  }
}

# Exit code: 0 if ok or warn, 1 if fail
if ($overall -eq "fail") { exit 1 }
exit 0
