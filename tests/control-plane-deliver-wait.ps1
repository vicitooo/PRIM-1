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

function Invoke-ControlPlane {
  param(
    [string[]]$Arguments
  )

  $previousPreference = $ErrorActionPreference
  $ErrorActionPreference = "Continue"
  $output = & powershell.exe -NoProfile -ExecutionPolicy Bypass @Arguments 2>&1
  $ErrorActionPreference = $previousPreference
  $combined = ($output | Out-String).Trim()

  return [pscustomobject]@{
    ExitCode = $LASTEXITCODE
    Output = $combined
  }
}

function New-TestRuntime {
  $root = Join-Path ([System.IO.Path]::GetTempPath()) ("cli-master-wrapper-deliver-wait-test-" + [guid]::NewGuid())
  $runtimeDir = Join-Path $root "runtime"
  $sidebandDir = Join-Path $runtimeDir "sideband"
  $null = New-Item -ItemType Directory -Force -Path (Join-Path $sidebandDir "inbox")
  $null = New-Item -ItemType Directory -Force -Path (Join-Path $sidebandDir "outbox")

  $infoPath = Join-Path $runtimeDir "control-plane.json"
  $capturePath = Join-Path $runtimeDir "captured-request.json"
  $status = @{
    transport = "named_pipe"
    endpoint = "\\.\pipe\cli-master-wrapper-deliver-wait-test"
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
    [string]$ResponseJson,
    [int]$DelayMs = 0
  )

  return Start-Job -ScriptBlock {
    param($RuntimeDir, $CapturePath, $ResponseJson, $DelayMs)

    $inboxDir = Join-Path $RuntimeDir "sideband\inbox"
    $outboxDir = Join-Path $RuntimeDir "sideband\outbox"
    $utf8NoBom = [System.Text.UTF8Encoding]::new($false)
    $deadline = [DateTime]::UtcNow.AddSeconds(20)

    while ([DateTime]::UtcNow -lt $deadline) {
      $request = Get-ChildItem -LiteralPath $inboxDir -Filter "*.json" | Sort-Object LastWriteTime | Select-Object -First 1
      if ($request) {
        $raw = Get-Content -LiteralPath $request.FullName -Raw
        [System.IO.File]::WriteAllText($CapturePath, $raw, $utf8NoBom)

        if ($DelayMs -gt 0) {
          Start-Sleep -Milliseconds $DelayMs
        }

        $responsePath = Join-Path $outboxDir $request.Name
        [System.IO.File]::WriteAllText($responsePath, $ResponseJson, $utf8NoBom)
        Remove-Item -LiteralPath $request.FullName -Force -ErrorAction SilentlyContinue
        return
      }

      Start-Sleep -Milliseconds 100
    }

    throw "Timed out waiting for mailbox payload."
  } -ArgumentList $RuntimeDir, $CapturePath, $ResponseJson, $DelayMs
}

$repoRoot = Split-Path -Parent $PSScriptRoot
$controlPlaneScript = Join-Path $repoRoot "scripts\control-plane.ps1"

$testRuntime = New-TestRuntime

try {
  Remove-Item -LiteralPath $testRuntime.CapturePath -Force -ErrorAction SilentlyContinue
  $deliverResponse = @{ ok = $true; message = "delivered" } | ConvertTo-Json -Compress
  $job = Start-MailboxResponder -RuntimeDir $testRuntime.RuntimeDir -CapturePath $testRuntime.CapturePath -ResponseJson $deliverResponse
  try {
    $result = Invoke-ControlPlane -Arguments @(
      "-File", $controlPlaneScript,
      "-Action", "deliver",
      "-Session", "claude",
      "-Content", "hello from victor",
      "-InfoFile", $testRuntime.InfoPath,
      "-Quiet"
    )
    Assert-Equal $result.ExitCode 0 "Deliver should succeed."
    Assert-Equal $result.Output "delivered" "Deliver should return the supervisor message in -Quiet mode."

    Wait-Job -Job $job -Timeout 25 | Out-Null
    $captured = Get-Content -LiteralPath $testRuntime.CapturePath -Raw | ConvertFrom-Json
    Assert-Equal $captured.kind "deliver_message" "Expected a deliver_message sideband request."
    Assert-Equal $captured.name "claude" "Expected deliver to preserve the target session."
    Assert-Equal $captured.content "hello from victor" "Expected deliver to preserve the message body."
  } finally {
    if ($job) {
      Remove-Job -Job $job -Force -ErrorAction SilentlyContinue
    }
  }

  Remove-Item -LiteralPath $testRuntime.CapturePath -Force -ErrorAction SilentlyContinue
  $slashResponse = @{ ok = $false; message = "deliver_message: slash commands are not supported; use send_input" } | ConvertTo-Json -Compress
  $job = Start-MailboxResponder -RuntimeDir $testRuntime.RuntimeDir -CapturePath $testRuntime.CapturePath -ResponseJson $slashResponse
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
    if ($job) {
      Remove-Job -Job $job -Force -ErrorAction SilentlyContinue
    }
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
  $job = Start-MailboxResponder -RuntimeDir $testRuntime.RuntimeDir -CapturePath $testRuntime.CapturePath -ResponseJson $waitQuietResponse -DelayMs 1000
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
    Assert-True ($elapsedMs -ge 900) "wait_quiet should wait for the mailbox response."

    Wait-Job -Job $job -Timeout 25 | Out-Null
    $captured = Get-Content -LiteralPath $testRuntime.CapturePath -Raw | ConvertFrom-Json
    Assert-Equal $captured.kind "wait_quiet" "Expected a wait_quiet sideband request."
    Assert-Equal $captured.quiet_seconds 1 "Expected wait_quiet to preserve QuietSec."
    Assert-Equal $captured.timeout_seconds 2 "Expected wait_quiet to preserve TimeoutSec."

    $parsed = $result.Output | ConvertFrom-Json
    Assert-Equal $parsed.ok $true "Expected the wait_quiet response JSON."
    Assert-Equal $parsed.payload.kind "wait_quiet" "Expected the wait_quiet payload kind."
    Assert-Equal $parsed.payload.quiet_duration_ms 1000 "Expected the wait_quiet payload body."
  } finally {
    if ($job) {
      Remove-Job -Job $job -Force -ErrorAction SilentlyContinue
    }
  }
} finally {
  Remove-Item -LiteralPath $testRuntime.Root -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "control-plane-deliver-wait tests passed"
