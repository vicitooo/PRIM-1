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

function Invoke-Script {
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
    Lines = @($output | ForEach-Object { [string]$_ })
    Output = (($output | Out-String).Trim())
  }
}

$repoRoot = Split-Path -Parent $PSScriptRoot
$controlPlaneScript = Join-Path $repoRoot "scripts\control-plane.ps1"
$agentRouteScript = Join-Path $repoRoot "scripts\agent-route.ps1"

$testRuntime = New-ControlPlaneTestRuntime -Prefix "prim1-request-id-test"
try {
  $response = @{ ok = $true; message = "pong"; snapshot = $null; request_id = "req-pass" } | ConvertTo-Json -Compress
  $responder = Start-ControlPlanePipeResponder -PipeName $testRuntime.PipeName -CapturePath $testRuntime.CapturePath -ResponseJson $response
  try {
    $result = Invoke-Script -Arguments @(
      "-File", $controlPlaneScript,
      "-Action", "ping",
      "-InfoFile", $testRuntime.InfoPath,
      "-Quiet",
      "-PassThruJson"
    )
    Assert-Equal $result.ExitCode 0 "Ping with -PassThruJson should succeed."
    Assert-Equal $result.Lines[0] "pong" "Quiet mode should keep the human-readable success message."
    $json = $result.Lines[1] | ConvertFrom-Json
    Assert-Equal $json.request_id "req-pass" "PassThruJson should include request_id."

    Wait-ControlPlanePipeResponder -Responder $responder
    $captured = Get-Content -LiteralPath $testRuntime.CapturePath -Raw | ConvertFrom-Json
    Assert-Equal $captured.kind "ping" "Expected a ping request."
  } finally {
    Remove-ControlPlanePipeResponder -Responder $responder
  }

  $requestIdPath = Join-Path $testRuntime.RuntimeDir "request-id.txt"
  $response = @{ ok = $true; message = "pong"; snapshot = $null; request_id = "req-file" } | ConvertTo-Json -Compress
  $responder = Start-ControlPlanePipeResponder -PipeName $testRuntime.PipeName -CapturePath $testRuntime.CapturePath -ResponseJson $response
  try {
    $result = Invoke-Script -Arguments @(
      "-File", $controlPlaneScript,
      "-Action", "ping",
      "-InfoFile", $testRuntime.InfoPath,
      "-Quiet",
      "-OutRequestIdFile", $requestIdPath
    )
    Assert-Equal $result.ExitCode 0 "Ping with -OutRequestIdFile should succeed."
    Assert-Equal $result.Output "pong" "Quiet mode should remain message-only without -PassThruJson."
    Assert-Equal (Get-Content -LiteralPath $requestIdPath -Raw) "req-file" "Request id file should contain the raw request id."

    Wait-ControlPlanePipeResponder -Responder $responder
  } finally {
    Remove-ControlPlanePipeResponder -Responder $responder
  }

  $routeRequestIdPath = Join-Path $testRuntime.RuntimeDir "route-request-id.txt"
  $response = @{ ok = $true; message = "message routed"; snapshot = $null; request_id = "req-route" } | ConvertTo-Json -Compress
  $responder = Start-ControlPlanePipeResponder -PipeName $testRuntime.PipeName -CapturePath $testRuntime.CapturePath -ResponseJson $response
  try {
    $result = Invoke-Script -Arguments @(
      "-File", $agentRouteScript,
      "-From", "codex",
      "-To", "claude",
      "-Content", "hello",
      "-InfoFile", $testRuntime.InfoPath,
      "-OutRequestIdFile", $routeRequestIdPath,
      "-PassThruJson"
    )
    Assert-Equal $result.ExitCode 0 "agent-route passthrough should succeed."
    Assert-Equal $result.Lines[0] "message routed" "agent-route should preserve quiet message output."
    $routeJson = $result.Lines[1] | ConvertFrom-Json
    Assert-Equal $routeJson.request_id "req-route" "agent-route PassThruJson should expose request_id."
    Assert-Equal (Get-Content -LiteralPath $routeRequestIdPath -Raw) "req-route" "agent-route should forward OutRequestIdFile."

    Wait-ControlPlanePipeResponder -Responder $responder
    $captured = Get-Content -LiteralPath $testRuntime.CapturePath -Raw | ConvertFrom-Json
    Assert-Equal $captured.kind "route_message" "Expected a route_message request."
    Assert-Equal $captured.request.from "codex" "Expected route source to round-trip."
    Assert-Equal $captured.request.to "claude" "Expected route target to round-trip."
  } finally {
    Remove-ControlPlanePipeResponder -Responder $responder
  }
} finally {
  Remove-Item -LiteralPath $testRuntime.Root -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "control-plane-request-id tests passed"
