Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
. "$PSScriptRoot\control-plane-test-helpers.ps1"

function Assert-True {
  param(
    [bool]$Condition,
    [string]$Message
  )

  if (-not $Condition) {
    throw $Message
  }
}

function Assert-Equal {
  param(
    $Actual,
    $Expected,
    [string]$Message
  )

  if ($Actual -cne $Expected) {
    throw "$Message`nExpected: $Expected`nActual:   $Actual"
  }
}

function Invoke-ControlPlane {
  param(
    [string[]]$Arguments
  )

  $previousPreference = $ErrorActionPreference
  $ErrorActionPreference = "Continue"
  $output = & powershell.exe -NoProfile -ExecutionPolicy Bypass @Arguments 2>&1
  $exitCode = $LASTEXITCODE
  $ErrorActionPreference = $previousPreference
  $combined = ($output | Out-String).Trim()

  return [pscustomobject]@{
    ExitCode = $exitCode
    Output = $combined
  }
}

$repoRoot = Split-Path -Parent $PSScriptRoot
$controlPlaneScript = Join-Path $repoRoot "scripts\control-plane.ps1"
$testRuntime = New-ControlPlaneTestRuntime -Prefix "prim1-timeout-test"
$timeoutMessage = "wait_quiet timed out after 10023ms"
$timeoutResponse = @{
  ok = $false
  timed_out = $true
  message = $timeoutMessage
} | ConvertTo-Json -Compress

try {
  $quietOverLimit = Invoke-ControlPlane -Arguments @(
    "-File", $controlPlaneScript,
    "-Action", "wait_quiet",
    "-Session", "codex",
    "-QuietSec", "61",
    "-TimeoutSec", "300",
    "-Endpoint", $testRuntime.Endpoint,
    "-Quiet"
  )
  Assert-True ($quietOverLimit.ExitCode -ne 0) "Quiet windows above the server cap must fail before connecting."
  Assert-True ($quietOverLimit.Output -like "*wait_quiet requires -QuietSec <= 60*") "Expected the quiet-window cap error."

  $timeoutOverLimit = Invoke-ControlPlane -Arguments @(
    "-File", $controlPlaneScript,
    "-Action", "wait_quiet",
    "-Session", "codex",
    "-QuietSec", "1",
    "-TimeoutSec", "301",
    "-Endpoint", $testRuntime.Endpoint,
    "-Quiet"
  )
  Assert-True ($timeoutOverLimit.ExitCode -ne 0) "Wait timeouts above the server cap must fail before connecting."
  Assert-True ($timeoutOverLimit.Output -like "*wait_quiet requires -TimeoutSec <= 300*") "Expected the wait-timeout cap error."

  $invertedWait = Invoke-ControlPlane -Arguments @(
    "-File", $controlPlaneScript,
    "-Action", "wait_quiet",
    "-Session", "codex",
    "-QuietSec", "2",
    "-TimeoutSec", "1",
    "-Endpoint", $testRuntime.Endpoint,
    "-Quiet"
  )
  Assert-True ($invertedWait.ExitCode -ne 0) "Quiet windows longer than the wait budget must fail before connecting."
  Assert-True ($invertedWait.Output -like "*wait_quiet requires -QuietSec <= -TimeoutSec*") "Expected the quiet-vs-timeout ordering error."

  Remove-Item -LiteralPath $testRuntime.CapturePath -Force -ErrorAction SilentlyContinue
  $responder = Start-ControlPlanePipeResponder -PipeName $testRuntime.PipeName -CapturePath $testRuntime.CapturePath -ResponseJson $timeoutResponse
  try {
    $result = Invoke-ControlPlane -Arguments @(
      "-File", $controlPlaneScript,
      "-Action", "wait_quiet",
      "-Session", "codex",
      "-QuietSec", "1",
      "-TimeoutSec", "2",
      "-Endpoint", $testRuntime.Endpoint
    )
    Assert-Equal $result.ExitCode 124 "Timed-out wait should use the timeout exit code."
    Assert-Equal $result.Output ("TIMED OUT: " + $timeoutMessage) "Timed-out wait should print the timeout banner in non-Quiet mode."

    Wait-ControlPlanePipeResponder -Responder $responder
    $captured = Get-Content -LiteralPath $testRuntime.CapturePath -Raw | ConvertFrom-Json
    Assert-Equal $captured.kind "wait_quiet" "Expected a wait_quiet request."
    Assert-Equal $captured.name "codex" "Expected wait_quiet to preserve the target session."
    Assert-True (-not ($captured.PSObject.Properties.Name -contains "token")) "Timed-out waits must not emit a token field."
  } finally {
    Remove-ControlPlanePipeResponder -Responder $responder
  }

  Remove-Item -LiteralPath $testRuntime.CapturePath -Force -ErrorAction SilentlyContinue
  $responder = Start-ControlPlanePipeResponder -PipeName $testRuntime.PipeName -CapturePath $testRuntime.CapturePath -ResponseJson $timeoutResponse
  try {
    $result = Invoke-ControlPlane -Arguments @(
      "-File", $controlPlaneScript,
      "-Action", "wait_quiet",
      "-Session", "codex",
      "-QuietSec", "1",
      "-TimeoutSec", "2",
      "-Endpoint", $testRuntime.Endpoint,
      "-Quiet"
    )
    Assert-Equal $result.ExitCode 124 "Timed-out wait should keep exit code 124 in -Quiet mode."
    Assert-Equal $result.Output ("TIMED OUT: " + $timeoutMessage) "Timed-out wait should return one stable timeout line in -Quiet mode."
  } finally {
    Wait-ControlPlanePipeResponder -Responder $responder
    Remove-ControlPlanePipeResponder -Responder $responder
  }

  Remove-Item -LiteralPath $testRuntime.CapturePath -Force -ErrorAction SilentlyContinue
  $lateWriteMessage = "send_input exceeded its server-side write budget after cancellation"
  $lateWriteResponse = @{
    ok = $false
    timed_out = $true
    message = $lateWriteMessage
  } | ConvertTo-Json -Compress
  $responder = Start-ControlPlanePipeResponder -PipeName $testRuntime.PipeName -CapturePath $testRuntime.CapturePath -ResponseJson $lateWriteResponse -DelayMs 36000
  try {
    $startedAt = [DateTime]::UtcNow
    $result = Invoke-ControlPlane -Arguments @(
      "-File", $controlPlaneScript,
      "-Action", "input",
      "-Session", "codex",
      "-Content", "delayed timeout response",
      "-Endpoint", $testRuntime.Endpoint
    )
    $elapsed = [DateTime]::UtcNow - $startedAt
    Assert-True ($elapsed.TotalSeconds -ge 35.5) "Input timeout receipt returned before the delayed server response."
    Assert-True ($elapsed.TotalSeconds -lt 45) "Input client deadline did not leave room for the truthful server response."
    Assert-Equal $result.ExitCode 124 "A late truthful input timeout should use exit code 124, not the client's own read timeout."
    Assert-Equal $result.Output ("TIMED OUT: " + $lateWriteMessage) "The client must preserve the server's late timeout detail."
    Wait-ControlPlanePipeResponder -Responder $responder -TimeoutSec 5
    $captured = Get-Content -LiteralPath $testRuntime.CapturePath -Raw | ConvertFrom-Json
    Assert-Equal $captured.kind "send_input" "Expected the delayed timeout receipt to carry send_input."
    Assert-Equal $captured.input "delayed timeout response" "Expected the delayed timeout receipt to preserve input."
  } finally {
    Remove-ControlPlanePipeResponder -Responder $responder
  }
} finally {
  Remove-Item -LiteralPath $testRuntime.Root -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "control-plane-timeouts tests passed"
