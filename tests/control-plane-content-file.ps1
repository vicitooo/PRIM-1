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
  $root = Join-Path ([System.IO.Path]::GetTempPath()) ("cli-master-wrapper-content-file-test-" + [guid]::NewGuid())
  $runtimeDir = Join-Path $root "runtime"
  $sidebandDir = Join-Path $runtimeDir "sideband"
  $null = New-Item -ItemType Directory -Force -Path (Join-Path $sidebandDir "inbox")
  $null = New-Item -ItemType Directory -Force -Path (Join-Path $sidebandDir "outbox")

  $infoPath = Join-Path $runtimeDir "control-plane.json"
  $capturePath = Join-Path $runtimeDir "captured-request.json"
  $status = @{
    transport = "named_pipe"
    endpoint = "\\.\pipe\cli-master-wrapper-content-file-test"
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
    [string]$CapturePath
  )

  return Start-Job -ScriptBlock {
    param($RuntimeDir, $CapturePath)

    $inboxDir = Join-Path $RuntimeDir "sideband\inbox"
    $outboxDir = Join-Path $RuntimeDir "sideband\outbox"
    $utf8NoBom = [System.Text.UTF8Encoding]::new($false)
    $deadline = [DateTime]::UtcNow.AddSeconds(20)

    while ([DateTime]::UtcNow -lt $deadline) {
      $request = Get-ChildItem -LiteralPath $inboxDir -Filter "*.json" | Sort-Object LastWriteTime | Select-Object -First 1
      if ($request) {
        $raw = Get-Content -LiteralPath $request.FullName -Raw
        [System.IO.File]::WriteAllText($CapturePath, $raw, $utf8NoBom)

        $response = @{ ok = $true; message = "input sent" } | ConvertTo-Json -Compress
        $responsePath = Join-Path $outboxDir $request.Name
        [System.IO.File]::WriteAllText($responsePath, $response, $utf8NoBom)
        return
      }

      Start-Sleep -Milliseconds 100
    }

    throw "Timed out waiting for mailbox payload."
  } -ArgumentList $RuntimeDir, $CapturePath
}

$repoRoot = Split-Path -Parent $PSScriptRoot
$controlPlaneScript = Join-Path $repoRoot "scripts\control-plane.ps1"

$testRuntime = New-TestRuntime
$payloadPath = Join-Path $testRuntime.RuntimeDir "payload.txt"
$expected = '/compact keep ''these'' quotes, "those" quotes, $var, !bang, @at, #hash, (parens), and D:\path with spaces\file.txt'
$utf8NoBom = [System.Text.UTF8Encoding]::new($false)
[System.IO.File]::WriteAllText($payloadPath, $expected, $utf8NoBom)

$bothResult = Invoke-ControlPlane -Arguments @(
  "-File", $controlPlaneScript,
  "-Action", "input",
  "-Session", "claude",
  "-Content", "/fast",
  "-ContentFile", $payloadPath,
  "-InfoFile", $testRuntime.InfoPath,
  "-Quiet"
)
Assert-True ($bothResult.ExitCode -ne 0) "Providing both -Content and -ContentFile should fail."
Assert-True ($bothResult.Output -like "*input accepts either -Content or -ContentFile, not both*") "Expected the mutual-exclusion error for -Content and -ContentFile."

$missingPath = Join-Path $testRuntime.RuntimeDir "missing.txt"
$missingResult = Invoke-ControlPlane -Arguments @(
  "-File", $controlPlaneScript,
  "-Action", "input",
  "-Session", "claude",
  "-ContentFile", $missingPath,
  "-InfoFile", $testRuntime.InfoPath,
  "-Quiet"
)
Assert-True ($missingResult.ExitCode -ne 0) "Missing -ContentFile should fail."
Assert-True ($missingResult.Output -like "*failed to resolve -ContentFile*") "Expected the missing-file error for -ContentFile."

$wrongActionResult = Invoke-ControlPlane -Arguments @(
  "-File", $controlPlaneScript,
  "-Action", "route",
  "-From", "victor",
  "-To", "claude",
  "-ContentFile", $payloadPath,
  "-InfoFile", $testRuntime.InfoPath,
  "-Quiet"
)
Assert-True ($wrongActionResult.ExitCode -ne 0) "Using -ContentFile with non-input actions should fail."
Assert-True ($wrongActionResult.Output -like "*-ContentFile is only supported for -Action input*") "Expected the action guard for -ContentFile."

$job = Start-MailboxResponder -RuntimeDir $testRuntime.RuntimeDir -CapturePath $testRuntime.CapturePath
try {
  $integrationResult = Invoke-ControlPlane -Arguments @(
    "-File", $controlPlaneScript,
    "-Action", "input",
    "-Session", "claude",
    "-ContentFile", $payloadPath,
    "-InfoFile", $testRuntime.InfoPath,
    "-Quiet"
  )
  Assert-Equal $integrationResult.ExitCode 0 "Content-file delivery should succeed."
  Assert-Equal $integrationResult.Output "input sent" "Expected the mailbox responder success message."

  Wait-Job -Job $job -Timeout 25 | Out-Null
  $jobState = ($job | Receive-Job -ErrorAction Stop | Out-String).Trim()
  Assert-True ($job.State -eq "Completed") "Mailbox responder job did not complete cleanly."

  $captured = Get-Content -LiteralPath $testRuntime.CapturePath -Raw | ConvertFrom-Json
  Assert-Equal $captured.kind "send_input" "Expected a send_input sideband request."
  Assert-Equal $captured.name "claude" "Expected the target session to remain intact."
  Assert-Equal $captured.input $expected "Expected the payload file to round-trip byte-for-byte through the script."
} finally {
  if ($job) {
    Remove-Job -Job $job -Force -ErrorAction SilentlyContinue
  }
  Remove-Item -LiteralPath $testRuntime.Root -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "control-plane-content-file tests passed"
