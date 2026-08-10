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
$agentKeyScript = Join-Path $repoRoot "scripts\agent-key.ps1"
$agentSlashScript = Join-Path $repoRoot "scripts\agent-slash.ps1"
$controlPlaneCommand = Get-Command -Name $controlPlaneScript
$actionValidateSet = @(
  $controlPlaneCommand.Parameters["Action"].Attributes |
    Where-Object { $_ -is [System.Management.Automation.ValidateSetAttribute] } |
    Select-Object -First 1
)
Assert-Equal ($actionValidateSet[0].ValidValues -join ",") "ping,wait_quiet,input,key" "The script action surface must stay closed to the four pane-local actions."
Assert-True (-not $controlPlaneCommand.Parameters.ContainsKey("InfoFile")) "Legacy InfoFile discovery must not remain callable."
Assert-True (-not $controlPlaneCommand.Parameters.ContainsKey("RequireIdle")) "Legacy idle compatibility must not remain callable."

$testRuntime = New-ControlPlaneTestRuntime -Prefix "prim1-request-id-test"
try {
  $response = @{ ok = $true; message = "pong"; request_id = "req-pass" } | ConvertTo-Json -Compress
  $responder = Start-ControlPlanePipeResponder -PipeName $testRuntime.PipeName -CapturePath $testRuntime.CapturePath -ResponseJson $response
  try {
    $originalEndpoint = $env:PRIM1_CONTROL_PLANE_ENDPOINT
    try {
      $env:PRIM1_CONTROL_PLANE_ENDPOINT = $testRuntime.Endpoint
      $result = Invoke-Script -Arguments @(
        "-File", $controlPlaneScript,
        "-Action", "ping",
        "-Quiet",
        "-PassThruJson"
      )
    } finally {
      $env:PRIM1_CONTROL_PLANE_ENDPOINT = $originalEndpoint
    }
    Assert-Equal $result.ExitCode 0 "Ping with -PassThruJson should succeed."
    Assert-Equal $result.Lines[0] "pong" "Quiet mode should keep the human-readable success message."
    $json = $result.Lines[1] | ConvertFrom-Json
    Assert-Equal $json.request_id "req-pass" "PassThruJson should include request_id."

    Wait-ControlPlanePipeResponder -Responder $responder
    $captured = Get-Content -LiteralPath $testRuntime.CapturePath -Raw | ConvertFrom-Json
    Assert-Equal $captured.kind "ping" "Expected a ping request."
    Assert-True (-not ($captured.PSObject.Properties.Name -contains "token")) "Ping must not emit a token field."
  } finally {
    Remove-ControlPlanePipeResponder -Responder $responder
  }

  $requestIdPath = Join-Path $testRuntime.RuntimeDir "request-id.txt"
  $response = @{ ok = $true; message = "pong"; request_id = "req-file" } | ConvertTo-Json -Compress
  $responder = Start-ControlPlanePipeResponder -PipeName $testRuntime.PipeName -CapturePath $testRuntime.CapturePath -ResponseJson $response
  try {
    $result = Invoke-Script -Arguments @(
      "-File", $controlPlaneScript,
      "-Action", "ping",
      "-Endpoint", $testRuntime.Endpoint,
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

  $legacyResult = Invoke-Script -Arguments @(
    "-File", $controlPlaneScript,
    "-Action", "route"
  )
  Assert-True ($legacyResult.ExitCode -ne 0) "Legacy route action must be rejected by parameter validation."
  Assert-True ($legacyResult.Output -like "*route*ValidateSet*") "Legacy route rejection must identify the closed action set."

  $response = @{ ok = $true; message = "key sent"; request_id = "req-key" } | ConvertTo-Json -Compress
  $responder = Start-ControlPlanePipeResponder -PipeName $testRuntime.PipeName -CapturePath $testRuntime.CapturePath -ResponseJson $response
  try {
    $result = Invoke-Script -Arguments @(
      "-File", $agentKeyScript,
      "-Session", "claude",
      "-Key", "enter",
      "-Endpoint", $testRuntime.Endpoint
    )
    Assert-Equal $result.ExitCode 0 "agent-key endpoint passthrough should succeed."
    Assert-Equal $result.Output "key sent" "agent-key should preserve quiet message output."

    Wait-ControlPlanePipeResponder -Responder $responder
    $captured = Get-Content -LiteralPath $testRuntime.CapturePath -Raw | ConvertFrom-Json
    Assert-Equal $captured.kind "send_key" "Expected a send_key request."
    Assert-Equal $captured.name "claude" "Expected agent-key to preserve its pane target."
    Assert-Equal $captured.key "enter" "Expected agent-key to preserve its control key."
    Assert-True (-not ($captured.PSObject.Properties.Name -contains "token")) "Key requests must not emit a token field."
  } finally {
    Remove-ControlPlanePipeResponder -Responder $responder
  }

  $response = @{ ok = $true; message = "input sent"; request_id = "req-slash" } | ConvertTo-Json -Compress
  $responder = Start-ControlPlanePipeResponder -PipeName $testRuntime.PipeName -CapturePath $testRuntime.CapturePath -ResponseJson $response
  try {
    $result = Invoke-Script -Arguments @(
      "-File", $agentSlashScript,
      "-Session", "claude",
      "-Slash", "compact",
      "-Args", "keep context",
      "-Endpoint", $testRuntime.Endpoint
    )
    Assert-Equal $result.ExitCode 0 "agent-slash endpoint passthrough should succeed."
    Assert-Equal $result.Output "input sent" "agent-slash should preserve quiet message output."

    Wait-ControlPlanePipeResponder -Responder $responder
    $captured = Get-Content -LiteralPath $testRuntime.CapturePath -Raw | ConvertFrom-Json
    Assert-Equal $captured.kind "send_input" "Expected agent-slash to send raw input."
    Assert-Equal $captured.name "claude" "Expected agent-slash to preserve its pane target."
    Assert-Equal $captured.input "/compact keep context" "Expected agent-slash to preserve its command."
    Assert-True (-not ($captured.PSObject.Properties.Name -contains "token")) "Slash requests must not emit a token field."
  } finally {
    Remove-ControlPlanePipeResponder -Responder $responder
  }

  $argsFile = Join-Path $testRuntime.RuntimeDir "slash-args.txt"
  $lambda = [char]0x03BB
  $emoji = [char]::ConvertFromUtf32(0x1F680)
  $cjk = "{0}{1}" -f [char]0x6F22, [char]0x5B57
  $expectedArgs = "preserve $lambda $emoji $cjk`r`nand trailing newline`r`n"
  [System.IO.File]::WriteAllText(
    $argsFile,
    $expectedArgs,
    [System.Text.UTF8Encoding]::new($false)
  )
  $response = @{ ok = $true; message = "input sent"; request_id = "req-slash-file" } | ConvertTo-Json -Compress
  $responder = Start-ControlPlanePipeResponder -PipeName $testRuntime.PipeName -CapturePath $testRuntime.CapturePath -ResponseJson $response
  try {
    $result = Invoke-Script -Arguments @(
      "-File", $agentSlashScript,
      "-Session", "claude",
      "-Slash", "compact",
      "-ArgsFile", $argsFile,
      "-Endpoint", $testRuntime.Endpoint
    )
    Assert-Equal $result.ExitCode 0 "agent-slash UTF-8 argument-file passthrough should succeed."
    Assert-Equal $result.Output "input sent" "agent-slash argument-file invocation should preserve quiet output."

    Wait-ControlPlanePipeResponder -Responder $responder
    $strictUtf8 = [System.Text.UTF8Encoding]::new($false, $true)
    $captured = [System.IO.File]::ReadAllText($testRuntime.CapturePath, $strictUtf8) | ConvertFrom-Json
    Assert-Equal $captured.kind "send_input" "Expected agent-slash argument-file input."
    Assert-Equal $captured.name "claude" "Expected agent-slash to preserve its pane target."
    Assert-Equal $captured.input "/compact $expectedArgs" "Expected strict UTF-8, CRLF, and trailing-newline fidelity."
  } finally {
    Remove-ControlPlanePipeResponder -Responder $responder
  }
} finally {
  Remove-Item -LiteralPath $testRuntime.Root -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "control-plane-request-id tests passed"
