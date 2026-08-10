<#
.SYNOPSIS
  PRIM-1 wrapper health check — wrapper process, panes, control plane,
  metadata audit, credentials, and recent error surface in one command.

.DESCRIPTION
  Reports a structured view of PRIM-1 runtime health. Intended as the
  single-command operator check before long-running sessions.

  Exit codes:
    0   all checks passed or only informational notes
    1   at least one hard failure (wrapper down, control-plane unreachable,
        audit log unwritable, or panes in error state)
    2   warnings only, with wrapper and control plane otherwise reachable

.PARAMETER Json
  Emit the full health report as compact JSON instead of human-readable text.
  Useful for integration with other tooling.

.PARAMETER Quiet
  Only emit FAIL and WARN lines. Suppress OK lines and the summary header.
  Combines with -Json to produce nothing when everything is OK.

.PARAMETER InfoFile
  Explicit master control-plane credential file. Its parent directory is also
  used for audit health checks. When omitted, the shared runtime resolver
  applies PRIM1_RUNTIME_DIR and then the platform default.
#>
param(
  [switch]$Json,
  [switch]$Quiet,
  [string]$InfoFile
)

$scriptRoot = Split-Path -Parent $PSCommandPath
. (Join-Path $scriptRoot "runtime-paths.ps1")
$masterInfoFile = Resolve-Prim1ControlPlaneInfoFile -InfoFile $InfoFile
$runtimeDir = Split-Path -Parent $masterInfoFile
$controlPlaneScript = Join-Path $scriptRoot "control-plane.ps1"
$auditDir = Join-Path $runtimeDir "audit"

$report = [ordered]@{
  timestamp = (Get-Date).ToUniversalTime().ToString("o")
  wrapper = $null
  panes = @()
  control_plane = $null
  audit_log = $null
  credentials = $null
  overall = "unknown"
  failures = @()
  warnings = @()
  notes = @()
}

function Add-Failure([string]$msg) { $report.failures += $msg }
function Add-Warning([string]$msg) { $report.warnings += $msg }
function Add-Note([string]$msg) { $report.notes += $msg }

# ---- Wrapper process ----
$wrapperProc = Get-Process -Name cli-master-wrapper-desktop -ErrorAction SilentlyContinue | Select-Object -First 1
if ($wrapperProc) {
  $uptime = (Get-Date) - $wrapperProc.StartTime
  $report.wrapper = [ordered]@{
    pid = $wrapperProc.Id
    start_time = $wrapperProc.StartTime.ToString("o")
    uptime_seconds = [int]$uptime.TotalSeconds
    main_window_title = $wrapperProc.MainWindowTitle
    responding = $wrapperProc.Responding
  }
  if (-not $wrapperProc.Responding) {
    Add-Failure "wrapper process PID $($wrapperProc.Id) is not responding"
  }
  if (-not $wrapperProc.MainWindowTitle) {
    Add-Warning "wrapper has no main window title (headless or still loading)"
  }
} else {
  $report.wrapper = @{ running = $false }
  Add-Failure "cli-master-wrapper-desktop process not running"
}

# ---- Master credentials file ----
if (Test-Path -LiteralPath $masterInfoFile) {
  try {
    $masterCreds = Get-Content -LiteralPath $masterInfoFile -Raw | ConvertFrom-Json
    $report.credentials = [ordered]@{
      master_info_file = $masterInfoFile
      endpoint = $masterCreds.endpoint
      per_session = @{}
    }
    foreach ($pane in @("claude", "codex")) {
      $paneInfo = Join-Path $runtimeDir "control-plane-$pane.json"
      if (Test-Path -LiteralPath $paneInfo) {
        try {
          $paneCreds = Get-Content -LiteralPath $paneInfo -Raw | ConvertFrom-Json
          $report.credentials.per_session[$pane] = [ordered]@{
            info_file = $paneInfo
            distinct_from_master = ($paneCreds.token -ne $masterCreds.token)
          }
          if (-not $report.credentials.per_session[$pane].distinct_from_master) {
            Add-Warning "per-session credentials for '$pane' match the master token"
          }
        } catch {
          Add-Warning "per-session creds file for '$pane' exists but failed to parse"
        }
      } else {
        Add-Note "per-session credentials file for '$pane' is not present"
      }
    }
  } catch {
    Add-Failure "master credentials file at $masterInfoFile failed to parse: $_"
  }
} else {
  Add-Failure "master credentials file not found at $masterInfoFile"
}

# ---- Control plane responsiveness ----
$cpResult = $null
$cpError = $null
if ($wrapperProc) {
  try {
    $cpRaw = & powershell -NoProfile -ExecutionPolicy Bypass -File $controlPlaneScript -Action list -InfoFile $masterInfoFile 2>&1
    if ($LASTEXITCODE -eq 0) {
      $cpResult = $cpRaw | Out-String | ConvertFrom-Json
    } else {
      $cpError = "control-plane.ps1 -Action list exited $LASTEXITCODE`: $cpRaw"
    }
  } catch {
    $cpError = "control-plane.ps1 -Action list threw: $_"
  }
}

if ($cpResult -and $cpResult.ok) {
  $report.control_plane = [ordered]@{
    reachable = $true
    transport = $cpResult.snapshot.control_plane.transport
    endpoint = $cpResult.snapshot.control_plane.endpoint
    runtime_dir = $cpResult.snapshot.runtime_dir
    generated_at = $cpResult.snapshot.generated_at
  }

  foreach ($session in $cpResult.snapshot.sessions) {
    $lastActivitySeconds = if ($session.last_activity_at) {
      $parsed = [datetime]::Parse($session.last_activity_at, $null, [System.Globalization.DateTimeStyles]::AssumeUniversal -bor [System.Globalization.DateTimeStyles]::AdjustToUniversal)
      [int]((Get-Date).ToUniversalTime() - $parsed).TotalSeconds
    } else { $null }

    $paneEntry = [ordered]@{
      name = $session.name
      lifecycle_state = $session.lifecycle_state
      process_id = $session.process_id
      running = $session.running
      last_activity_age_seconds = $lastActivitySeconds
      last_error = $session.last_error
    }
    $report.panes += $paneEntry

    if ($session.last_error) {
      Add-Failure "pane '$($session.name)' has last_error: $($session.last_error)"
    }
    if (-not $session.running -and $session.lifecycle_state -ne "closed") {
      Add-Warning "pane '$($session.name)' not running but lifecycle_state is '$($session.lifecycle_state)'"
    }
    if ($session.running -and $lastActivitySeconds -ne $null -and $lastActivitySeconds -gt 3600) {
      Add-Note "pane '$($session.name)' idle for $lastActivitySeconds seconds (> 1h)"
    }
  }

  if (($cpResult.snapshot.sessions | Where-Object { $_.running }).Count -eq 0) {
    Add-Warning "no panes currently running (wrapper up but all sessions closed)"
  }
} else {
  $report.control_plane = [ordered]@{
    reachable = $false
    error = $cpError
  }
  if ($wrapperProc) {
    Add-Failure "control-plane unreachable despite wrapper process alive: $cpError"
  }
}

# ---- Audit log ----
$today = (Get-Date).ToUniversalTime().ToString("yyyy-MM-dd")
$auditPath = Join-Path $auditDir "$today.jsonl"

if (Test-Path -LiteralPath $auditPath) {
  $auditItem = Get-Item -LiteralPath $auditPath
  $auditAge = [int]((Get-Date) - $auditItem.LastWriteTime).TotalSeconds
  $report.audit_log = [ordered]@{
    path = $auditPath
    exists = $true
    size_bytes = $auditItem.Length
    last_write_age_seconds = $auditAge
  }

  # Writable check: try opening for append
  try {
    $fs = [System.IO.File]::Open($auditPath, [System.IO.FileMode]::Append, [System.IO.FileAccess]::Write, [System.IO.FileShare]::ReadWrite)
    $fs.Close()
    $report.audit_log.writable = $true
  } catch {
    $report.audit_log.writable = $false
    Add-Failure "audit log not writable: $_"
  }
} else {
  $report.audit_log = [ordered]@{
    path = $auditPath
    exists = $false
  }
  if ($wrapperProc) {
    Add-Warning "audit log for today does not exist yet (wrapper may have just started)"
  }
}

# ---- Disk space ----
try {
  $drive = [System.IO.Path]::GetPathRoot($runtimeDir)
  $diskInfo = Get-PSDrive -Name $drive[0] -ErrorAction SilentlyContinue
  if ($diskInfo) {
    $freeGb = [math]::Round($diskInfo.Free / 1GB, 2)
    $report.disk = [ordered]@{
      drive = $drive
      free_gb = $freeGb
    }
    if ($freeGb -lt 1.0) {
      Add-Failure "disk free space critically low: $freeGb GB on $drive"
    } elseif ($freeGb -lt 5.0) {
      Add-Warning "disk free space low: $freeGb GB on $drive"
    }
  }
} catch {
  Add-Note "failed to query disk space: $_"
}

# ---- Overall status ----
if ($report.failures.Count -gt 0) {
  $report.overall = "fail"
  $exitCode = 1
} elseif ($report.warnings.Count -gt 0) {
  $report.overall = "warn"
  $exitCode = 2
} else {
  $report.overall = "ok"
  $exitCode = 0
}

# ---- Output ----
if ($Json) {
  if ($Quiet -and $exitCode -eq 0) {
    # nothing
  } else {
    $report | ConvertTo-Json -Depth 10 -Compress
  }
  exit $exitCode
}

function Write-Line($label, $value, $color = "White") {
  $padded = $label.PadRight(26)
  Write-Host "$padded $value" -ForegroundColor $color
}

if (-not $Quiet) {
  Write-Host ""
  Write-Host "PRIM-1 health check  $($report.timestamp)" -ForegroundColor Cyan
  Write-Host ("-" * 70)

  if ($report.wrapper.pid) {
    Write-Line "wrapper" "PID $($report.wrapper.pid) up $($report.wrapper.uptime_seconds)s  window='$($report.wrapper.main_window_title)'  responding=$($report.wrapper.responding)" Green
  } else {
    Write-Line "wrapper" "NOT RUNNING" Red
  }

  if ($report.control_plane.reachable) {
    Write-Line "control-plane" "reachable  $($report.control_plane.endpoint)" Green
  } else {
    Write-Line "control-plane" "UNREACHABLE  $($report.control_plane.error)" Red
  }

  foreach ($pane in $report.panes) {
    $color = if ($pane.last_error) { "Red" }
             elseif (-not $pane.running) { "Yellow" }
             else { "Green" }
    $ageLabel = if ($pane.last_activity_age_seconds -ne $null) { "last_activity=$($pane.last_activity_age_seconds)s ago" } else { "no activity yet" }
    Write-Line "pane:$($pane.name)" "state=$($pane.lifecycle_state)  pid=$($pane.process_id)  running=$($pane.running)  $ageLabel" $color
  }

  if ($report.audit_log.exists) {
    $auditColor = if ($report.audit_log.writable) { "Green" } else { "Red" }
    Write-Line "audit_log" "$($report.audit_log.size_bytes) bytes  last_write=$($report.audit_log.last_write_age_seconds)s ago  writable=$($report.audit_log.writable)" $auditColor
  } else {
    Write-Line "audit_log" "not yet created for today" Yellow
  }

  if ($report.disk) {
    $diskColor = if ($report.disk.free_gb -lt 1.0) { "Red" } elseif ($report.disk.free_gb -lt 5.0) { "Yellow" } else { "Green" }
    Write-Line "disk" "$($report.disk.free_gb) GB free on $($report.disk.drive)" $diskColor
  }

  if ($report.credentials) {
    $perSessionKeys = @($report.credentials.per_session.Keys)
    $pslabel = if ($perSessionKeys.Count -gt 0) { "per_session=$($perSessionKeys -join ',')" } else { "per_session=none" }
    Write-Line "credentials" "master_file=present  $pslabel" Green
  }

  Write-Host ("-" * 70)
}

if ($report.failures.Count -gt 0) {
  foreach ($f in $report.failures) { Write-Host "FAIL: $f" -ForegroundColor Red }
}
if ($report.warnings.Count -gt 0) {
  foreach ($w in $report.warnings) { Write-Host "WARN: $w" -ForegroundColor Yellow }
}
if ((-not $Quiet) -and $report.notes.Count -gt 0) {
  foreach ($n in $report.notes) { Write-Host "NOTE: $n" -ForegroundColor Gray }
}

if (-not $Quiet) {
  Write-Host ""
  $overallColor = switch ($report.overall) { "ok" {"Green"} "warn" {"Yellow"} "fail" {"Red"} default {"White"} }
  Write-Host "OVERALL: $($report.overall.ToUpper())" -ForegroundColor $overallColor
  Write-Host ""
}

exit $exitCode
