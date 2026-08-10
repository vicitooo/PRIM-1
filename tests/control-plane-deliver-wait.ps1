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

$testRuntime = New-ControlPlaneTestRuntime -Prefix "prim1-deliver-wait-test"

try {
  Remove-Item -LiteralPath $testRuntime.CapturePath -Force -ErrorAction SilentlyContinue
  $deliverResponse = @{ ok = $true; message = "delivered" } | ConvertTo-Json -Compress
  $responder = Start-ControlPlanePipeResponder -PipeName $testRuntime.PipeName -CapturePath $testRuntime.CapturePath -ResponseJson $deliverResponse
  try {
    $result = Invoke-ControlPlane -Arguments @(
      "-File", $controlPlaneScript,
      "-Action", "deliver",
      "-Session", "claude",
      "-Content", "hello from operator",
      "-InfoFile", $testRuntime.InfoPath,
      "-Quiet"
    )
    Assert-Equal $result.ExitCode 0 "Deliver should succeed."
    Assert-Equal $result.Output "delivered" "Deliver should return the supervisor message in -Quiet mode."

    Wait-ControlPlanePipeResponder -Responder $responder
    $captured = Get-Content -LiteralPath $testRuntime.CapturePath -Raw | ConvertFrom-Json
    Assert-Equal $captured.kind "deliver_message" "Expected a deliver_message sideband request."
    Assert-Equal $captured.name "claude" "Expected deliver to preserve the target session."
    Assert-Equal $captured.content "hello from operator" "Expected deliver to preserve the message body."
  } finally {
    Remove-ControlPlanePipeResponder -Responder $responder
  }

  Remove-Item -LiteralPath $testRuntime.CapturePath -Force -ErrorAction SilentlyContinue
  $slashResponse = @{ ok = $false; message = "deliver_message: slash commands are not supported; use send_input" } | ConvertTo-Json -Compress
  $responder = Start-ControlPlanePipeResponder -PipeName $testRuntime.PipeName -CapturePath $testRuntime.CapturePath -ResponseJson $slashResponse
  try {
    $result = Invoke-ControlPlane -Arguments @(
      "-File", $controlPlaneScript,
      "-Action", "deliver",
      "-Session", "claude",
      "-Content", "/foo",
      "-InfoFile", $testRuntime.InfoPath,
      "-Quiet"
    )
    Assert-True ($result.ExitCode -ne 0) "Slash-command deliver should fail."
  } finally {
    Wait-ControlPlanePipeResponder -Responder $responder
    Remove-ControlPlanePipeResponder -Responder $responder
  }

  Remove-Item -LiteralPath $testRuntime.CapturePath -Force -ErrorAction SilentlyContinue
  $waitQuietResponse = @{
    ok = $true
    message = "session is quiet"
    payload = @{
      kind = "wait_quiet"
      quiet_duration_ms = 1000
    }
  } | ConvertTo-Json -Compress -Depth 6
  $responder = Start-ControlPlanePipeResponder -PipeName $testRuntime.PipeName -CapturePath $testRuntime.CapturePath -ResponseJson $waitQuietResponse -DelayMs 1000
  try {
    $started = [DateTime]::UtcNow
    $result = Invoke-ControlPlane -Arguments @(
      "-File", $controlPlaneScript,
      "-Action", "wait_quiet",
      "-Session", "claude",
      "-QuietSec", "1",
      "-TimeoutSec", "2",
      "-InfoFile", $testRuntime.InfoPath
    )
    $elapsedMs = ([DateTime]::UtcNow - $started).TotalMilliseconds
    Assert-Equal $result.ExitCode 0 "wait_quiet should succeed."
    Assert-True ($elapsedMs -ge 900) "wait_quiet should wait for the named-pipe response."

    Wait-ControlPlanePipeResponder -Responder $responder
    $captured = Get-Content -LiteralPath $testRuntime.CapturePath -Raw | ConvertFrom-Json
    Assert-Equal $captured.kind "wait_quiet" "Expected a wait_quiet sideband request."
    Assert-Equal $captured.quiet_seconds 1 "Expected wait_quiet to preserve QuietSec."
    Assert-Equal $captured.timeout_seconds 2 "Expected wait_quiet to preserve TimeoutSec."

    $parsed = $result.Output | ConvertFrom-Json
    Assert-Equal $parsed.ok $true "Expected the wait_quiet response JSON."
    Assert-Equal $parsed.payload.kind "wait_quiet" "Expected the wait_quiet payload kind."
    Assert-Equal $parsed.payload.quiet_duration_ms 1000 "Expected the wait_quiet payload body."
  } finally {
    Remove-ControlPlanePipeResponder -Responder $responder
  }

  Remove-Item -LiteralPath $testRuntime.CapturePath -Force -ErrorAction SilentlyContinue
  $responder = Start-ControlPlanePipeResponder -PipeName $testRuntime.PipeName -CapturePath $testRuntime.CapturePath -ResponseJson $waitQuietResponse -DelayMs 11000
  try {
    $started = [DateTime]::UtcNow
    $result = Invoke-ControlPlane -Arguments @(
      "-File", $controlPlaneScript,
      "-Action", "wait_quiet",
      "-Session", "claude",
      "-QuietSec", "1",
      "-TimeoutSec", "12",
      "-InfoFile", $testRuntime.InfoPath
    )
    $elapsedMs = ([DateTime]::UtcNow - $started).TotalMilliseconds
    Assert-Equal $result.ExitCode 0 "wait_quiet should honor the extended named-pipe timeout."
    Assert-True ($elapsedMs -ge 10500) "wait_quiet should wait beyond the old 10-second client cap."

    Wait-ControlPlanePipeResponder -Responder $responder
    $captured = Get-Content -LiteralPath $testRuntime.CapturePath -Raw | ConvertFrom-Json
    Assert-Equal $captured.timeout_seconds 12 "Expected the long wait_quiet timeout to be forwarded."
  } finally {
    Remove-ControlPlanePipeResponder -Responder $responder
  }
} finally {
  Remove-Item -LiteralPath $testRuntime.Root -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "control-plane-deliver-wait tests passed"
