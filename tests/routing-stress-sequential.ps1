<#
.SYNOPSIS
  Sequential routing stress test — fire N routed messages back-to-back
  from claude to codex and verify metadata-only delivery receipts.

.PARAMETER Count
  Number of messages to fire. Default 10.

.PARAMETER Marker
  Unique marker prefix to make this run distinguishable from others.
  Default uses a timestamp.

.PARAMETER InfoFile
  Explicit control-plane credential file for fixture/live overrides. Its
  parent directory anchors audit verification. When omitted, the shared
  runtime resolver selects the product runtime directory.
#>
param(
  [int]$Count = 10,
  [string]$Marker = "",
  [string]$InfoFile
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ($Count -lt 1) {
  throw "-Count must be at least 1"
}

if (-not $Marker) {
  $Marker = "STRESS-" + (Get-Date).ToUniversalTime().ToString("yyyyMMddTHHmmss")
}

$scriptRoot = Split-Path -Parent $PSCommandPath
$wrapperRoot = Split-Path -Parent $scriptRoot
$routeScript = Join-Path $wrapperRoot "scripts\agent-route.ps1"
. (Join-Path $wrapperRoot "scripts\runtime-paths.ps1")
$runtimeDir = if ($InfoFile) {
  Split-Path -Parent (Resolve-Prim1ControlPlaneInfoFile -InfoFile $InfoFile)
} else {
  Resolve-Prim1RuntimeDirectory
}
$auditLog = Join-Path $runtimeDir "audit"
$startedAtUtc = [datetime]::UtcNow

$start = Get-Date

Write-Host "firing $Count messages with marker prefix '$Marker' claude -> codex direct..."

$results = @()
for ($i = 1; $i -le $Count; $i++) {
  $content = "$Marker-$i : concurrent routing stress test marker $i of $Count"
  $output = @(& $routeScript -From claude -To codex -Scope direct -Content $content -InfoFile $InfoFile -PassThruJson 2>&1)
  $exitCode = $LASTEXITCODE
  $response = $null
  foreach ($line in $output) {
    try {
      $candidate = ([string]$line) | ConvertFrom-Json -ErrorAction Stop
      if ($candidate.request_id) {
        $response = $candidate
      }
    } catch { }
  }
  $results += [pscustomobject]@{
    Index = $i
    ExitCode = $exitCode
    RequestId = if ($response) { [string]$response.request_id } else { $null }
    Output = ($output -join [Environment]::NewLine)
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

$missingRequestId = $results | Where-Object { $_.ExitCode -eq 0 -and -not $_.RequestId }
if ($missingRequestId) {
  Write-Host "MISSING REQUEST IDS:" -ForegroundColor Red
  foreach ($result in $missingRequestId) {
    Write-Host "  msg $($result.Index): output=$($result.Output)"
  }
}

Write-Host "sleeping 1s for audit record settle..."
Start-Sleep -Seconds 1

Write-Host "scanning metadata audit for route_delivery receipts..."

$endedAtUtc = [datetime]::UtcNow
$auditFiles = @(
  $startedAtUtc.ToString("yyyy-MM-dd")
  $endedAtUtc.ToString("yyyy-MM-dd")
) | Select-Object -Unique | ForEach-Object { Join-Path $auditLog "$_.jsonl" }
$existingAuditFiles = @($auditFiles | Where-Object { Test-Path -LiteralPath $_ })
if ($existingAuditFiles.Count -eq 0) {
  Write-Host "AUDIT FILE NOT FOUND: $($auditFiles -join ', ')" -ForegroundColor Red
  exit 1
}

$auditEvents = @(Get-Content -LiteralPath $existingAuditFiles | ForEach-Object {
  try { $_ | ConvertFrom-Json -ErrorAction Stop } catch { }
})
$receiptFailures = @()
$receiptPasses = 0

foreach ($result in $results | Where-Object { $_.ExitCode -eq 0 -and $_.RequestId }) {
  $events = @($auditEvents | Where-Object {
    $_.event -eq "route_delivery" -and $_.request_id -eq $result.RequestId
  })
  $resolved = @($events | Where-Object { $_.phase -eq "resolved" })
  $written = @($events | Where-Object { $_.phase -eq "written" -and $_.recipient -eq "codex" })
  $failed = @($events | Where-Object { $_.phase -eq "failed" })

  if ($resolved.Count -eq 1 -and
      [int]$resolved[0].recipient_count -eq 1 -and
      $written.Count -eq 1 -and
      $failed.Count -eq 0) {
    $receiptPasses++
  } else {
    $receiptFailures += "msg $($result.Index) request=$($result.RequestId) resolved=$($resolved.Count) written_to_codex=$($written.Count) failed=$($failed.Count)"
  }
}

Write-Host ""
Write-Host "=== RESULT ==="
Write-Host "successful dispatches: $($Count - @($failedDispatch).Count) / $Count"
Write-Host "complete route_delivery receipts: $receiptPasses / $Count"

if ($failedDispatch -or $missingRequestId -or $receiptFailures.Count -gt 0) {
  foreach ($failure in $receiptFailures) {
    Write-Host "RECEIPT FAILURE: $failure" -ForegroundColor Red
  }
  exit 1
}

Write-Host "PASS: all $Count requests have one resolved and one written metadata receipt" -ForegroundColor Green
Write-Host "NOTE: content fidelity requires the separate child-input/receiver-side oracle." -ForegroundColor Gray
exit 0
