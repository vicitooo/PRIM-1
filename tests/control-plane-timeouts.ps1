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
$timeoutMessage = "lifecycle op 'stop_session' timed out after 10023ms"
$timeoutResponse = @{
  ok = $false
  timed_out = $true
  message = $timeoutMessage
} | ConvertTo-Json -Compress

try {
  Remove-Item -LiteralPath $testRuntime.CapturePath -Force -ErrorAction SilentlyContinue
  $responder = Start-ControlPlanePipeResponder -PipeName $testRuntime.PipeName -CapturePath $testRuntime.CapturePath -ResponseJson $timeoutResponse
  try {
    $result = Invoke-ControlPlane -Arguments @(
      "-File", $controlPlaneScript,
      "-Action", "stop",
      "-Session", "codex",
      "-InfoFile", $testRuntime.InfoPath
    )
    Assert-Equal $result.ExitCode 124 "Timed-out stop should use the timeout exit code."
    Assert-Equal $result.Output ("TIMED OUT: " + $timeoutMessage) "Timed-out stop should print the timeout banner in non-Quiet mode."

    Wait-ControlPlanePipeResponder -Responder $responder
    $captured = Get-Content -LiteralPath $testRuntime.CapturePath -Raw | ConvertFrom-Json
    Assert-Equal $captured.kind "stop_session" "Expected a stop_session sideband request."
    Assert-Equal $captured.name "codex" "Expected stop to preserve the target session."
  } finally {
    Remove-ControlPlanePipeResponder -Responder $responder
  }

  Remove-Item -LiteralPath $testRuntime.CapturePath -Force -ErrorAction SilentlyContinue
  $responder = Start-ControlPlanePipeResponder -PipeName $testRuntime.PipeName -CapturePath $testRuntime.CapturePath -ResponseJson $timeoutResponse
  try {
    $result = Invoke-ControlPlane -Arguments @(
      "-File", $controlPlaneScript,
      "-Action", "stop",
      "-Session", "codex",
      "-InfoFile", $testRuntime.InfoPath,
      "-Quiet"
    )
    Assert-Equal $result.ExitCode 124 "Timed-out stop should keep exit code 124 in -Quiet mode."
    Assert-Equal $result.Output ("TIMED OUT: " + $timeoutMessage) "Timed-out stop should return one stable timeout line in -Quiet mode."
  } finally {
    Wait-ControlPlanePipeResponder -Responder $responder
    Remove-ControlPlanePipeResponder -Responder $responder
  }
} finally {
  Remove-Item -LiteralPath $testRuntime.Root -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "control-plane-timeouts tests passed"
