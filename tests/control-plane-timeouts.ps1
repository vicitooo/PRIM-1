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
  $root = Join-Path ([System.IO.Path]::GetTempPath()) ("cli-master-wrapper-timeout-test-" + [guid]::NewGuid())
  $runtimeDir = Join-Path $root "runtime"
  $sidebandDir = Join-Path $runtimeDir "sideband"
  $null = New-Item -ItemType Directory -Force -Path (Join-Path $sidebandDir "inbox")
  $null = New-Item -ItemType Directory -Force -Path (Join-Path $sidebandDir "outbox")

  $infoPath = Join-Path $runtimeDir "control-plane.json"
  $capturePath = Join-Path $runtimeDir "captured-request.json"
  $status = @{
    transport = "named_pipe"
    endpoint = "\\.\pipe\cli-master-wrapper-timeout-test"
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
    [string]$ResponseJson
  )

  return Start-Job -ScriptBlock {
    param($RuntimeDir, $CapturePath, $ResponseJson)

    $inboxDir = Join-Path $RuntimeDir "sideband\inbox"
    $outboxDir = Join-Path $RuntimeDir "sideband\outbox"
    $utf8NoBom = [System.Text.UTF8Encoding]::new($false)
    $deadline = [DateTime]::UtcNow.AddSeconds(20)

    while ([DateTime]::UtcNow -lt $deadline) {
      $request = Get-ChildItem -LiteralPath $inboxDir -Filter "*.json" | Sort-Object LastWriteTime | Select-Object -First 1
      if ($request) {
        $raw = Get-Content -LiteralPath $request.FullName -Raw
        [System.IO.File]::WriteAllText($CapturePath, $raw, $utf8NoBom)
        $responsePath = Join-Path $outboxDir $request.Name
        [System.IO.File]::WriteAllText($responsePath, $ResponseJson, $utf8NoBom)
        Remove-Item -LiteralPath $request.FullName -Force -ErrorAction SilentlyContinue
        return
      }

      Start-Sleep -Milliseconds 100
    }

    throw "Timed out waiting for mailbox payload."
  } -ArgumentList $RuntimeDir, $CapturePath, $ResponseJson
}

$repoRoot = Split-Path -Parent $PSScriptRoot
$controlPlaneScript = Join-Path $repoRoot "scripts\control-plane.ps1"
$testRuntime = New-TestRuntime
$timeoutMessage = "lifecycle op 'stop_session' timed out after 10023ms"
$timeoutResponse = @{
  ok = $false
  timed_out = $true
  message = $timeoutMessage
} | ConvertTo-Json -Compress

try {
  Remove-Item -LiteralPath $testRuntime.CapturePath -Force -ErrorAction SilentlyContinue
  $job = Start-MailboxResponder -RuntimeDir $testRuntime.RuntimeDir -CapturePath $testRuntime.CapturePath -ResponseJson $timeoutResponse
  try {
    $result = Invoke-ControlPlane -Arguments @(
      "-File", $controlPlaneScript,
      "-Action", "stop",
      "-Session", "codex",
      "-InfoFile", $testRuntime.InfoPath
    )
    Assert-Equal $result.ExitCode 124 "Timed-out stop should use the timeout exit code."
    Assert-Equal $result.Output ("TIMED OUT: " + $timeoutMessage) "Timed-out stop should print the timeout banner in non-Quiet mode."

    Wait-Job -Job $job -Timeout 25 | Out-Null
    $captured = Get-Content -LiteralPath $testRuntime.CapturePath -Raw | ConvertFrom-Json
    Assert-Equal $captured.kind "stop_session" "Expected a stop_session sideband request."
    Assert-Equal $captured.name "codex" "Expected stop to preserve the target session."
  } finally {
    if ($job) {
      Remove-Job -Job $job -Force -ErrorAction SilentlyContinue
    }
  }

  Remove-Item -LiteralPath $testRuntime.CapturePath -Force -ErrorAction SilentlyContinue
  $job = Start-MailboxResponder -RuntimeDir $testRuntime.RuntimeDir -CapturePath $testRuntime.CapturePath -ResponseJson $timeoutResponse
  try {
    $result = Invoke-ControlPlane -Arguments @(
      "-File", $controlPlaneScript,
      "-Action", "stop",
      "-Session", "codex",
      "-InfoFile", $testRuntime.InfoPath,
      "-Quiet"
    )
    Assert-Equal $result.ExitCode 124 "Timed-out stop should keep exit code 124 in -Quiet mode."
    Assert-True ($result.Output.Contains("TIMED OUT: $timeoutMessage")) "Timed-out stop should surface the timeout banner in -Quiet mode."
  } finally {
    if ($job) {
      Remove-Job -Job $job -Force -ErrorAction SilentlyContinue
    }
  }
} finally {
  Remove-Item -LiteralPath $testRuntime.Root -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "control-plane-timeouts tests passed"
