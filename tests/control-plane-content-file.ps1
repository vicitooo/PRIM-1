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

$failedRuntime = New-ControlPlaneTestRuntime -Prefix "prim1-no-mailbox-fallback-test"
try {
  $originalEndpoint = $env:PRIM1_CONTROL_PLANE_ENDPOINT
  try {
    $env:PRIM1_CONTROL_PLANE_ENDPOINT = $null
    $missingEndpointResult = Invoke-ControlPlane -Arguments @(
      "-File", $controlPlaneScript,
      "-Action", "ping",
      "-Quiet"
    )
    Assert-True ($missingEndpointResult.ExitCode -ne 0) "Missing endpoint must fail closed."
    Assert-True ($missingEndpointResult.Output -like "*PRIM1_CONTROL_PLANE_ENDPOINT is not set. External operator control is unavailable; use the PRIM-1 desktop UI.*") "Expected the stable missing-endpoint direction."
  } finally {
    $env:PRIM1_CONTROL_PLANE_ENDPOINT = $originalEndpoint
  }

  $failedResult = Invoke-ControlPlane -Arguments @(
    "-File", $controlPlaneScript,
    "-Action", "ping",
    "-Endpoint", $failedRuntime.Endpoint,
    "-Quiet"
  )
  Assert-True ($failedResult.ExitCode -ne 0) "A missing named-pipe server must fail."
  Assert-True ($failedResult.Output -like "*Control plane named-pipe request failed*") "Expected an explicit named-pipe failure."
} finally {
  Remove-Item -LiteralPath $failedRuntime.Root -Recurse -Force -ErrorAction SilentlyContinue
}

$testRuntime = New-ControlPlaneTestRuntime -Prefix "prim1-content-file-test"
$payloadPath = Join-Path $testRuntime.RuntimeDir "payload.txt"
$lambda = [char]0x03BB
$emoji = [char]::ConvertFromUtf32(0x1F680)
$cjk = "{0}{1}" -f [char]0x6F22, [char]0x5B57
$expected = "/compact keep 'these' quotes, `"those`" quotes, `$var, !bang, @at, #hash, (parens), and D:\path with spaces\file.txt; $lambda $emoji $cjk`r`nsecond line`r`n"
$utf8NoBom = [System.Text.UTF8Encoding]::new($false)
[System.IO.File]::WriteAllText($payloadPath, $expected, $utf8NoBom)

$bothResult = Invoke-ControlPlane -Arguments @(
  "-File", $controlPlaneScript,
  "-Action", "input",
  "-Session", "claude",
  "-Content", "/fast",
  "-ContentFile", $payloadPath,
  "-Endpoint", $testRuntime.Endpoint,
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
  "-Endpoint", $testRuntime.Endpoint,
  "-Quiet"
)
Assert-True ($missingResult.ExitCode -ne 0) "Missing -ContentFile should fail."
Assert-True ($missingResult.Output -like "*failed to resolve -ContentFile*") "Expected the missing-file error for -ContentFile."

$wrongActionResult = Invoke-ControlPlane -Arguments @(
  "-File", $controlPlaneScript,
  "-Action", "key",
  "-Session", "claude",
  "-Key", "enter",
  "-ContentFile", $payloadPath,
  "-Endpoint", $testRuntime.Endpoint,
  "-Quiet"
)
Assert-True ($wrongActionResult.ExitCode -ne 0) "Using -ContentFile with non-input actions should fail."
Assert-True ($wrongActionResult.Output -like "*-ContentFile is only supported for -Action input*") "Expected the action guard for -ContentFile."

$responseJson = @{ ok = $true; message = "input sent" } | ConvertTo-Json -Compress
$responder = Start-ControlPlanePipeResponder -PipeName $testRuntime.PipeName -CapturePath $testRuntime.CapturePath -ResponseJson $responseJson
try {
  $integrationResult = Invoke-ControlPlane -Arguments @(
    "-File", $controlPlaneScript,
    "-Action", "input",
    "-Session", "claude",
    "-ContentFile", $payloadPath,
    "-Endpoint", $testRuntime.Endpoint,
    "-Quiet"
  )
  Assert-Equal $integrationResult.ExitCode 0 "Content-file delivery should succeed."
  Assert-Equal $integrationResult.Output "input sent" "Expected the named-pipe responder success message."

  Wait-ControlPlanePipeResponder -Responder $responder

  $strictUtf8 = [System.Text.UTF8Encoding]::new($false, $true)
  $captured = [System.IO.File]::ReadAllText($testRuntime.CapturePath, $strictUtf8) | ConvertFrom-Json
  Assert-Equal $captured.kind "send_input" "Expected a send_input sideband request."
  Assert-Equal $captured.name "claude" "Expected the target session to remain intact."
  Assert-Equal $captured.input $expected "Expected the payload file to round-trip byte-for-byte through the script."
  Assert-True (-not ($captured.PSObject.Properties.Name -contains "token")) "Token fields must never be emitted."
  Assert-True (-not ($captured.PSObject.Properties.Name -contains "require_idle")) "Legacy require_idle must never be emitted."
} finally {
  Remove-ControlPlanePipeResponder -Responder $responder
  Remove-Item -LiteralPath $testRuntime.Root -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "control-plane-content-file tests passed"
