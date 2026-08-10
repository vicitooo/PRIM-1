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

  return [pscustomobject]@{
    ExitCode = $exitCode
    Output = (($output | Out-String).Trim())
  }
}

$repoRoot = Split-Path -Parent $PSScriptRoot
$controlPlaneScript = Join-Path $repoRoot "scripts\control-plane.ps1"
$testRuntime = New-ControlPlaneTestRuntime -Prefix "prim1-wait-test"

try {
  $waitQuietResponse = @{
    ok = $true
    message = "session is quiet"
    request_id = "req-wait"
    payload = @{
      kind = "wait_quiet"
      quiet_duration_ms = 250
    }
  } | ConvertTo-Json -Compress -Depth 6
  $responder = Start-ControlPlanePipeResponder `
    -PipeName $testRuntime.PipeName `
    -CapturePath $testRuntime.CapturePath `
    -ResponseJson $waitQuietResponse `
    -DelayMs 250
  try {
    $started = [DateTime]::UtcNow
    $result = Invoke-ControlPlane -Arguments @(
      "-File", $controlPlaneScript,
      "-Action", "wait_quiet",
      "-Session", "claude",
      "-QuietSec", "1",
      "-TimeoutSec", "12",
      "-Endpoint", $testRuntime.Endpoint
    )
    $elapsedMs = ([DateTime]::UtcNow - $started).TotalMilliseconds
    Assert-Equal $result.ExitCode 0 "wait_quiet should succeed."
    Assert-True ($elapsedMs -ge 200) "wait_quiet should wait for the named-pipe response."

    Wait-ControlPlanePipeResponder -Responder $responder
    $captured = Get-Content -LiteralPath $testRuntime.CapturePath -Raw | ConvertFrom-Json
    Assert-Equal $captured.kind "wait_quiet" "Expected a wait_quiet request."
    Assert-Equal $captured.name "claude" "Expected wait_quiet to preserve its pane target."
    Assert-Equal $captured.quiet_seconds 1 "Expected wait_quiet to preserve QuietSec."
    Assert-Equal $captured.timeout_seconds 12 "Expected wait_quiet to preserve TimeoutSec."
    Assert-True (-not ($captured.PSObject.Properties.Name -contains "token")) "Wait requests must not emit a token field."

    $parsed = $result.Output | ConvertFrom-Json
    Assert-Equal $parsed.ok $true "Expected the wait_quiet response JSON."
    Assert-Equal $parsed.request_id "req-wait" "Expected request_id to remain visible."
    Assert-Equal $parsed.payload.kind "wait_quiet" "Expected the wait_quiet payload kind."
    Assert-Equal $parsed.payload.quiet_duration_ms 250 "Expected the minimal wait payload."
  } finally {
    Remove-ControlPlanePipeResponder -Responder $responder
  }
} finally {
  Remove-Item -LiteralPath $testRuntime.Root -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "control-plane-wait tests passed"
