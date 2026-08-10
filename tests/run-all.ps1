<#
.SYNOPSIS
  PRIM-1 deterministic control-script test runner.

.DESCRIPTION
  Runs the following deterministic control-script suites in order. This is not
  the complete production gate; Rust, TypeScript, packaging, browser, privacy,
  process, harness-admission, and performance verification are aggregated
  separately.
    1. tests/runtime-paths.ps1           (PowerShell path resolver tests)
    2. tests/agent-events-summary.test.py (metadata summary fixture tests)
    3. tests/control-plane-content-file.ps1 (PowerShell content-file harness)
    4. tests/control-plane-request-id.ps1 (PowerShell request-id helper harness)
    5. tests/control-plane-server-identity.ps1 (named-pipe server authenticity)
    6. tests/control-plane-wait.ps1        (PowerShell wait harness)
    7. tests/control-plane-timeouts.ps1   (PowerShell timeout harness)
    8. tests/control-plane-room.ps1       (kernel-derived room request shapes)

  Uses direct invocation with fail-closed $LASTEXITCODE capture. Output is only
  shown on failure unless -Verbose is set.

.PARAMETER Verbose
  Print output from all suites, not just failing ones.
#>
[CmdletBinding()]
param()

$ErrorActionPreference = "Continue"
$scriptRoot = Split-Path -Parent $PSCommandPath
$wrapperRoot = (Resolve-Path (Join-Path $scriptRoot "..")).Path

function Invoke-Suite {
  param(
    [string]$Name,
    [scriptblock]$Block
  )

  Write-Host "[RUN ] $Name" -NoNewline
  $suiteStart = Get-Date

  Push-Location $wrapperRoot
  $captured = $null
  $exitCode = 1
  try {
    # Native exit state is sticky in Windows PowerShell 5.1. Clear it so a
    # scriptblock that never launches a native process cannot inherit success
    # from the preceding suite.
    $global:LASTEXITCODE = $null
    $captured = & $Block 2>&1 | Out-String
    $observedExitCode = $global:LASTEXITCODE
    if ($null -eq $observedExitCode) {
      $captured = ($captured + "`nrunner error: suite completed without an external process exit code").Trim()
    } else {
      $exitCode = [int]$observedExitCode
    }
  } catch {
    $observedExitCode = $global:LASTEXITCODE
    if ($null -ne $observedExitCode -and [int]$observedExitCode -ne 0) {
      $exitCode = [int]$observedExitCode
    }
    $captured = ($_ | Out-String).Trim()
  } finally {
    Pop-Location
  }

  $duration = [int](((Get-Date) - $suiteStart).TotalMilliseconds)

  if ($exitCode -eq 0) {
    Write-Host "  PASS  ($duration ms)" -ForegroundColor Green
  } else {
    Write-Host "  FAIL  ($duration ms, exit=$exitCode)" -ForegroundColor Red
  }

  return [pscustomobject]@{
    Name = $Name
    Status = if ($exitCode -eq 0) { "pass" } else { "fail" }
    ExitCode = $exitCode
    Duration = $duration
    Output = $captured
  }
}

Write-Host ""
Write-Host "PRIM-1 test runner" -ForegroundColor Cyan
Write-Host ("=" * 70)
Write-Host "wrapper root: $wrapperRoot"
Write-Host "scope: control-script suites only; not the complete production gate"
Write-Host ""

$results = @()
$start = Get-Date

$results += Invoke-Suite -Name "runtime path resolver" -Block {
  & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $scriptRoot "runtime-paths.ps1")
}

$results += Invoke-Suite -Name "agent events summary" -Block {
  & python (Join-Path $scriptRoot "agent-events-summary.test.py")
}

$results += Invoke-Suite -Name "control-plane content file" -Block {
  & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $scriptRoot "control-plane-content-file.ps1")
}

$results += Invoke-Suite -Name "control-plane request id" -Block {
  & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $scriptRoot "control-plane-request-id.ps1")
}

$results += Invoke-Suite -Name "control-plane server identity" -Block {
  & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $scriptRoot "control-plane-server-identity.ps1")
}

$results += Invoke-Suite -Name "control-plane wait" -Block {
  & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $scriptRoot "control-plane-wait.ps1")
}

$results += Invoke-Suite -Name "control-plane timeouts" -Block {
  & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $scriptRoot "control-plane-timeouts.ps1")
}

$results += Invoke-Suite -Name "control-plane room" -Block {
  & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $scriptRoot "control-plane-room.ps1")
}

$totalMs = [int](((Get-Date) - $start).TotalMilliseconds)

Write-Host ""
Write-Host ("=" * 70)
$resultCount = @($results).Count
$passCount = @($results | Where-Object { $_.Status -eq "pass" }).Count
$failCount = @($results | Where-Object { $_.Status -eq "fail" }).Count
$classifiedCount = $passCount + $failCount

Write-Host "PRIM-1 test runner: $passCount pass, $failCount fail  (total $totalMs ms)"

if ($resultCount -eq 0 -or $classifiedCount -ne $resultCount -or $failCount -gt 0) {
  Write-Host ""
  Write-Host "FAILED SUITES:" -ForegroundColor Red
  foreach ($r in $results | Where-Object { $_.Status -eq "fail" }) {
    Write-Host "  - $($r.Name) (exit $($r.ExitCode))" -ForegroundColor Red
    if ($r.Output) {
      Write-Host "    output:" -ForegroundColor Gray
      $r.Output -split "`n" | Where-Object { $_.Trim() } | Select-Object -First 20 | ForEach-Object { Write-Host "    $_" -ForegroundColor Gray }
    }
  }
  if ($resultCount -eq 0) {
    Write-Host "  - runner produced no suite results" -ForegroundColor Red
  } elseif ($classifiedCount -ne $resultCount) {
    Write-Host "  - runner produced $($resultCount - $classifiedCount) unclassified suite result(s)" -ForegroundColor Red
  }
  Write-Host ""
  Write-Host "OVERALL: FAIL" -ForegroundColor Red
  exit 1
}

if ($VerbosePreference -eq "Continue") {
  Write-Host ""
  Write-Host "PASSING SUITES (verbose):" -ForegroundColor Cyan
  foreach ($r in $results | Where-Object { $_.Status -eq "pass" }) {
    Write-Host "  - $($r.Name) ($($r.Duration) ms)" -ForegroundColor Green
    if ($r.Output) {
      $r.Output -split "`n" | Where-Object { $_.Trim() } | Select-Object -First 10 | ForEach-Object { Write-Host "    $_" -ForegroundColor Gray }
    }
  }
}

Write-Host ""
Write-Host "OVERALL: PASS" -ForegroundColor Green
exit 0
