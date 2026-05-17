Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

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
  $ErrorActionPreference = $previousPreference

  return [pscustomobject]@{
    ExitCode = $LASTEXITCODE
    Lines = @($output | ForEach-Object { [string]$_ })
    Output = (($output | Out-String).Trim())
  }
}

function New-TestRuntime {
  $root = Join-Path ([System.IO.Path]::GetTempPath()) ("prim1-request-id-test-" + [guid]::NewGuid())
  $runtimeDir = Join-Path $root "runtime"
  $sidebandDir = Join-Path $runtimeDir "sideband"
  $null = New-Item -ItemType Directory -Force -Path (Join-Path $sidebandDir "inbox")
  $null = New-Item -ItemType Directory -Force -Path (Join-Path $sidebandDir "outbox")

  $infoPath = Join-Path $runtimeDir "control-plane.json"
  $capturePath = Join-Path $runtimeDir "captured-request.json"
  $status = @{
    transport = "named_pipe"
    endpoint = "\\.\pipe\prim1-request-id-test"
    token = "test-token"
    info_path = $infoPath
  }
  $utf8NoBom = [System.Text.UTF8Encoding]::new($false)
  [System.IO.File]::WriteAllText($infoPath, ($status | ConvertTo-Json -Compress), $utf8NoBom)

  return @{
    Root = $root
    RuntimeDir = $runtimeDir
    InfoPath = $infoPath
    CapturePath = $capturePath
  }
}

function Start-MailboxResponder {
  param(
    [string]$RuntimeDir,
    [string]$CapturePath,
    [string]$Message,
    [string]$RequestId
  )

  return Start-Job -ScriptBlock {
    param($RuntimeDir, $CapturePath, $Message, $RequestId)

    $inboxDir = Join-Path $RuntimeDir "sideband\inbox"
    $outboxDir = Join-Path $RuntimeDir "sideband\outbox"
    $utf8NoBom = [System.Text.UTF8Encoding]::new($false)
    $deadline = [DateTime]::UtcNow.AddSeconds(20)

    while ([DateTime]::UtcNow -lt $deadline) {
      $request = Get-ChildItem -LiteralPath $inboxDir -Filter "*.json" | Sort-Object LastWriteTime | Select-Object -First 1
      if ($request) {
        $raw = Get-Content -LiteralPath $request.FullName -Raw
        [System.IO.File]::WriteAllText($CapturePath, $raw, $utf8NoBom)

        $response = @{
          ok = $true
          message = $Message
          snapshot = $null
          request_id = $RequestId
        } | ConvertTo-Json -Compress
        $responsePath = Join-Path $outboxDir $request.Name
        [System.IO.File]::WriteAllText($responsePath, $response, $utf8NoBom)
        Remove-Item -LiteralPath $request.FullName -Force
        return
      }

      Start-Sleep -Milliseconds 100
    }

    throw "Timed out waiting for mailbox payload."
  } -ArgumentList $RuntimeDir, $CapturePath, $Message, $RequestId
}

function Wait-Responder {
  param($Job)

  Wait-Job -Job $Job -Timeout 25 | Out-Null
  Receive-Job -Job $Job -ErrorAction Stop | Out-Null
  Assert-True ($Job.State -eq "Completed") "Mailbox responder job did not complete cleanly."
}

$repoRoot = Split-Path -Parent $PSScriptRoot
$controlPlaneScript = Join-Path $repoRoot "scripts\control-plane.ps1"
$agentRouteScript = Join-Path $repoRoot "scripts\agent-route.ps1"

$testRuntime = New-TestRuntime
try {
  $job = Start-MailboxResponder -RuntimeDir $testRuntime.RuntimeDir -CapturePath $testRuntime.CapturePath -Message "pong" -RequestId "req-pass"
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

    Wait-Responder -Job $job
    $captured = Get-Content -LiteralPath $testRuntime.CapturePath -Raw | ConvertFrom-Json
    Assert-Equal $captured.kind "ping" "Expected a ping request."
  } finally {
    if ($job) { Remove-Job -Job $job -Force -ErrorAction SilentlyContinue }
  }

  $requestIdPath = Join-Path $testRuntime.RuntimeDir "request-id.txt"
  $job = Start-MailboxResponder -RuntimeDir $testRuntime.RuntimeDir -CapturePath $testRuntime.CapturePath -Message "pong" -RequestId "req-file"
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

    Wait-Responder -Job $job
  } finally {
    if ($job) { Remove-Job -Job $job -Force -ErrorAction SilentlyContinue }
  }

  $routeRequestIdPath = Join-Path $testRuntime.RuntimeDir "route-request-id.txt"
  $job = Start-MailboxResponder -RuntimeDir $testRuntime.RuntimeDir -CapturePath $testRuntime.CapturePath -Message "message routed" -RequestId "req-route"
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

    Wait-Responder -Job $job
    $captured = Get-Content -LiteralPath $testRuntime.CapturePath -Raw | ConvertFrom-Json
    Assert-Equal $captured.kind "route_message" "Expected a route_message request."
    Assert-Equal $captured.request.from "codex" "Expected route source to round-trip."
    Assert-Equal $captured.request.to "claude" "Expected route target to round-trip."
  } finally {
    if ($job) { Remove-Job -Job $job -Force -ErrorAction SilentlyContinue }
  }
} finally {
  Remove-Item -LiteralPath $testRuntime.Root -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "control-plane-request-id tests passed"
