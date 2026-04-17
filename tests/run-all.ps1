<#
.SYNOPSIS
  PRIM-001 test runner — executes every PRIM-001 test suite in sequence and
  reports pass/fail. One-command gate for trusting the wrapper before a long run.

.DESCRIPTION
  Runs the following suites in order:
    1. tools/tests/test_watcher.py       (Python unit tests)
    2. tests/handshake-helpers.ps1       (PowerShell helper tests)
    3. tests/control-plane-content-file.ps1 (PowerShell content-file harness)
    4. tests/control-plane-deliver-wait.ps1 (PowerShell deliver/wait harness)
    5. tests/control-plane-timeouts.ps1  (PowerShell timeout harness)
    6. tests/routing-stress-sequential.ps1  (live transport stress, requires
                                             wrapper + panes running)
    7. scripts/health-check.ps1          (live runtime state check)

  Uses direct invocation with $LASTEXITCODE capture. Output is only shown on
  failure unless -Verbose is set.

.PARAMETER SkipLive
  Skip suites that require a running wrapper + panes (stress test + health check).
  Useful for CI-style pre-commit checks.

.PARAMETER Verbose
  Print output from all suites, not just failing ones.
#>
[CmdletBinding()]
param(
  [switch]$SkipLive
)

$ErrorActionPreference = "Continue"
$scriptRoot = Split-Path -Parent $PSCommandPath
$wrapperRoot = (Resolve-Path (Join-Path $scriptRoot "..")).Path

function Invoke-Suite {
  param(
    [string]$Name,
    [scriptblock]$Block,
    [switch]$Live
  )

  if ($Live -and $script:SkipLiveFlag) {
    Write-Host "[SKIP] $Name  (live suite, -SkipLive set)" -ForegroundColor Gray
    return [pscustomobject]@{ Name = $Name; Status = "skip"; ExitCode = 0; Duration = 0; Output = "" }
  }

  Write-Host "[RUN ] $Name" -NoNewline
  $suiteStart = Get-Date

  Push-Location $wrapperRoot
  $captured = $null
  $exitCode = 0
  try {
    $captured = & $Block 2>&1 | Out-String
    $exitCode = $LASTEXITCODE
    if ($null -eq $exitCode) { $exitCode = 0 }
  } catch {
    $captured = $_.Exception.Message
    $exitCode = 1
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

$script:SkipLiveFlag = $SkipLive.IsPresent

Write-Host ""
Write-Host "PRIM-001 test runner" -ForegroundColor Cyan
Write-Host ("=" * 70)
Write-Host "wrapper root: $wrapperRoot"
if ($SkipLive) { Write-Host "mode: skip live suites (unit-only gate)" }
Write-Host ""

$results = @()
$start = Get-Date

$results += Invoke-Suite -Name "watcher unit tests" -Block {
  & python tools/tests/test_watcher.py
}

$results += Invoke-Suite -Name "handshake helpers" -Block {
  & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $scriptRoot "handshake-helpers.ps1")
}

$results += Invoke-Suite -Name "control-plane content file" -Block {
  & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $scriptRoot "control-plane-content-file.ps1")
}

$results += Invoke-Suite -Name "control-plane deliver + wait" -Block {
  & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $scriptRoot "control-plane-deliver-wait.ps1")
}

$results += Invoke-Suite -Name "control-plane timeouts" -Block {
  & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $scriptRoot "control-plane-timeouts.ps1")
}

$results += Invoke-Suite -Name "routing stress sequential" -Live -Block {
  & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $scriptRoot "routing-stress-sequential.ps1") -Count 5
}

$results += Invoke-Suite -Name "health check" -Live -Block {
  & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $wrapperRoot "scripts/health-check.ps1")
}

$totalMs = [int](((Get-Date) - $start).TotalMilliseconds)

Write-Host ""
Write-Host ("=" * 70)
$passCount = ($results | Where-Object { $_.Status -eq "pass" }).Count
$failCount = ($results | Where-Object { $_.Status -eq "fail" }).Count
$skipCount = ($results | Where-Object { $_.Status -eq "skip" }).Count

Write-Host "PRIM-001 test runner: $passCount pass, $failCount fail, $skipCount skip  (total $totalMs ms)"

if ($failCount -gt 0) {
  Write-Host ""
  Write-Host "FAILED SUITES:" -ForegroundColor Red
  foreach ($r in $results | Where-Object { $_.Status -eq "fail" }) {
    Write-Host "  - $($r.Name) (exit $($r.ExitCode))" -ForegroundColor Red
    if ($r.Output) {
      Write-Host "    output:" -ForegroundColor Gray
      $r.Output -split "`n" | Where-Object { $_.Trim() } | Select-Object -First 20 | ForEach-Object { Write-Host "    $_" -ForegroundColor Gray }
    }
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
