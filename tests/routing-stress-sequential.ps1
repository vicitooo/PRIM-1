<#
.SYNOPSIS
  Sequential routing stress test — fire N routed messages back-to-back
  from claude to codex and verify all N arrive in the audit log.

.PARAMETER Count
  Number of messages to fire. Default 10.

.PARAMETER Marker
  Unique marker prefix to make this run distinguishable from others.
  Default uses a timestamp.
#>
param(
  [int]$Count = 10,
  [string]$Marker = ""
)

if (-not $Marker) {
  $Marker = "STRESS-" + (Get-Date).ToUniversalTime().ToString("yyyyMMddTHHmmss")
}

$scriptRoot = Split-Path -Parent $PSCommandPath
$routeScript = Join-Path (Split-Path -Parent $scriptRoot) "scripts\agent-route.ps1"
$auditLog = Join-Path (Split-Path -Parent $scriptRoot) ".runtime\audit"
$today = (Get-Date).ToUniversalTime().ToString("yyyy-MM-dd")
$auditFile = Join-Path $auditLog "$today.jsonl"

$start = Get-Date

Write-Host "firing $Count messages with marker prefix '$Marker' claude -> codex direct..."

$results = @()
for ($i = 1; $i -le $Count; $i++) {
  $content = "$Marker-$i : concurrent routing stress test marker $i of $Count"
  $output = & $routeScript -From claude -To codex -Scope direct -Content $content 2>&1
  $results += [pscustomobject]@{
    Index = $i
    ExitCode = $LASTEXITCODE
    Output = $output
  }
}

$elapsedMs = [int]((Get-Date) - $start).TotalMilliseconds
Write-Host "dispatch phase: $elapsedMs ms (avg $([math]::Round($elapsedMs / $Count, 1)) ms per message)"

$failedDispatch = $results | Where-Object { $_.ExitCode -ne 0 }
if ($failedDispatch) {
  Write-Host "DISPATCH FAILURES:" -ForegroundColor Red
  foreach ($f in $failedDispatch) {
    Write-Host "  msg $($f.Index): exit=$($f.ExitCode) output=$($f.Output)"
  }
}

Write-Host "sleeping 2s for audit record settle..."
Start-Sleep -Seconds 2

Write-Host "scanning audit log for routed_message delivery records..."

# Verification uses routed_message events as the authoritative transport record.
# session_output is NOT used because it captures current-frame redraw state,
# not scrollback — rapid-fire content scrolls past before a redraw chunk can
# capture it, so session_output is an unreliable signal for message-content
# verification at stress rates.

$routedHits = 0
$busyInputForwarded = 0
$found = @{}
for ($i = 1; $i -le $Count; $i++) { $found[$i] = $false }

Select-String -Path $auditFile -Pattern $Marker -SimpleMatch | ForEach-Object {
  try {
    $j = $_.Line | ConvertFrom-Json
  } catch { return }

  if ($j.event -eq "routed_message" -and $j.content -and $j.content.Contains($Marker)) {
    $routedHits++
    for ($i = 1; $i -le $Count; $i++) {
      if ($j.content.Contains("$Marker-$i ")) {
        $found[$i] = $true
      }
    }
  }
}

$foundCount = ($found.Values | Where-Object { $_ -eq $true }).Count
$missing = @()
for ($i = 1; $i -le $Count; $i++) {
  if (-not $found[$i]) { $missing += $i }
}

Write-Host ""
Write-Host "=== RESULT ==="
Write-Host "routed_message events matching marker: $routedHits"
Write-Host "distinct message indexes found: $foundCount / $Count"

if ($missing.Count -gt 0) {
  Write-Host "MISSING MESSAGE INDEXES: $($missing -join ',')" -ForegroundColor Red
  exit 1
}

if ($routedHits -lt $Count) {
  Write-Host "MESSAGE COUNT MISMATCH: expected $Count, got $routedHits" -ForegroundColor Red
  exit 1
}

Write-Host "PASS: all $Count messages routed with distinct indexes" -ForegroundColor Green
exit 0
