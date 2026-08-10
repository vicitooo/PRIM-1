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

function Assert-ZeroCapturedBytes {
  param(
    [Parameter(Mandatory = $true)]
    [string]$Path,

    [Parameter(Mandatory = $true)]
    [string]$Message
  )

  Assert-True (Test-Path -LiteralPath $Path) "$Message The probe did not publish a capture receipt."
  Assert-Equal ([System.IO.File]::ReadAllBytes($Path).Length) 0 $Message
}

$repoRoot = Split-Path -Parent $PSScriptRoot
$controlPlaneScript = Join-Path $repoRoot "scripts\control-plane.ps1"
$testRuntime = New-ControlPlaneTestRuntime -Prefix "prim1-server-identity-test"
$originalServerPid = [Environment]::GetEnvironmentVariable("PRIM1_CONTROL_PLANE_SERVER_PID", "Process")
$originalServerStartedFiletime = [Environment]::GetEnvironmentVariable("PRIM1_CONTROL_PLANE_SERVER_STARTED_FILETIME", "Process")

try {
  $invalidIdentityCases = @(
    [pscustomobject]@{
      Name = "missing PID"
      Pid = $null
      StartedFiletime = "1"
      Expected = "*PRIM1_CONTROL_PLANE_SERVER_PID is not set*"
    },
    [pscustomobject]@{
      Name = "malformed PID"
      Pid = "not-a-pid"
      StartedFiletime = "1"
      Expected = "*PRIM1_CONTROL_PLANE_SERVER_PID must be a positive unsigned decimal process ID*"
    },
    [pscustomobject]@{
      Name = "missing creation FILETIME"
      Pid = "1"
      StartedFiletime = $null
      Expected = "*PRIM1_CONTROL_PLANE_SERVER_STARTED_FILETIME is not set*"
    },
    [pscustomobject]@{
      Name = "malformed creation FILETIME"
      Pid = "1"
      StartedFiletime = "-1"
      Expected = "*PRIM1_CONTROL_PLANE_SERVER_STARTED_FILETIME must be a positive unsigned decimal FILETIME*"
    }
  )

  foreach ($case in $invalidIdentityCases) {
    [Environment]::SetEnvironmentVariable("PRIM1_CONTROL_PLANE_SERVER_PID", $case.Pid, "Process")
    [Environment]::SetEnvironmentVariable("PRIM1_CONTROL_PLANE_SERVER_STARTED_FILETIME", $case.StartedFiletime, "Process")
    $result = Invoke-ControlPlane -Arguments @(
      "-File", $controlPlaneScript,
      "-Action", "ping",
      "-Endpoint", $testRuntime.Endpoint,
      "-Quiet"
    )
    Assert-True ($result.ExitCode -ne 0) "$($case.Name) must fail closed."
    Assert-True ($result.Output -like $case.Expected) "$($case.Name) did not produce the expected fail-closed error. Output: $($result.Output)"
  }

  [Environment]::SetEnvironmentVariable("PRIM1_CONTROL_PLANE_SERVER_PID", $originalServerPid, "Process")
  [Environment]::SetEnvironmentVariable("PRIM1_CONTROL_PLANE_SERVER_STARTED_FILETIME", $originalServerStartedFiletime, "Process")

  $responseJson = @{ ok = $true; message = "pong"; request_id = "identity-match" } | ConvertTo-Json -Compress
  $responder = Start-ControlPlanePipeResponder `
    -PipeName $testRuntime.PipeName `
    -CapturePath $testRuntime.CapturePath `
    -ResponseJson $responseJson
  try {
    $result = Invoke-ControlPlane -Arguments @(
      "-File", $controlPlaneScript,
      "-Action", "ping",
      "-Endpoint", $testRuntime.Endpoint,
      "-Quiet"
    )
    Assert-Equal $result.ExitCode 0 "Matching server PID and creation FILETIME should succeed."
    Assert-Equal $result.Output "pong" "Matching server identity should preserve the normal response."
    Wait-ControlPlanePipeResponder -Responder $responder
    $captured = Get-Content -LiteralPath $testRuntime.CapturePath -Raw | ConvertFrom-Json
    Assert-Equal $captured.kind "ping" "The verified server should receive the request."
  } finally {
    Remove-ControlPlanePipeResponder -Responder $responder
  }

  $responder = Start-ControlPlanePipeResponder `
    -PipeName $testRuntime.PipeName `
    -CapturePath $testRuntime.CapturePath `
    -ResponseJson $responseJson `
    -AllowClientDisconnectWithoutRequest
  try {
    $trustedPid = [uint32]$PID
    Assert-True ($trustedPid -ne [uint32]$responder.ServerPid) "The rogue-server PID test requires distinct client and server processes."
    [Environment]::SetEnvironmentVariable("PRIM1_CONTROL_PLANE_SERVER_PID", $trustedPid.ToString([Globalization.CultureInfo]::InvariantCulture), "Process")
    $result = Invoke-ControlPlane -Arguments @(
      "-File", $controlPlaneScript,
      "-Action", "ping",
      "-Endpoint", $testRuntime.Endpoint,
      "-Quiet"
    )
    Assert-True ($result.ExitCode -ne 0) "A connected rogue server with the wrong PID must fail."
    Assert-True ($result.Output -like "*Control-plane server identity mismatch: connected PID $($responder.ServerPid), expected PID $trustedPid*") "PID mismatch should identify the connected and trusted processes. Output: $($result.Output)"
    Wait-ControlPlanePipeResponder -Responder $responder
    Assert-ZeroCapturedBytes -Path $testRuntime.CapturePath -Message "No request byte may reach a server before PID verification succeeds."
  } finally {
    Remove-ControlPlanePipeResponder -Responder $responder
  }

  $responder = Start-ControlPlanePipeResponder `
    -PipeName $testRuntime.PipeName `
    -CapturePath $testRuntime.CapturePath `
    -ResponseJson $responseJson `
    -AllowClientDisconnectWithoutRequest
  try {
    [uint64]$wrongStartedFiletime = [uint64]$responder.ServerStartedFiletime + [uint64]1
    [Environment]::SetEnvironmentVariable(
      "PRIM1_CONTROL_PLANE_SERVER_STARTED_FILETIME",
      $wrongStartedFiletime.ToString([Globalization.CultureInfo]::InvariantCulture),
      "Process"
    )
    $result = Invoke-ControlPlane -Arguments @(
      "-File", $controlPlaneScript,
      "-Action", "ping",
      "-Endpoint", $testRuntime.Endpoint,
      "-Quiet"
    )
    Assert-True ($result.ExitCode -ne 0) "A reused or mismatched server PID identity must fail on creation FILETIME."
    Assert-True ($result.Output -like "*Control-plane server identity mismatch: connected PID $($responder.ServerPid) has creation FILETIME*") "Creation-time mismatch should identify the connected server. Output: $($result.Output)"
    Assert-True ($result.Output -like "*$($responder.ServerStartedFiletime)*") "Creation-time mismatch should include the connected server FILETIME. Output: $($result.Output)"
    Assert-True ($result.Output -like "*$wrongStartedFiletime*") "Creation-time mismatch should include the trusted FILETIME. Output: $($result.Output)"
    Wait-ControlPlanePipeResponder -Responder $responder
    Assert-ZeroCapturedBytes -Path $testRuntime.CapturePath -Message "No request byte may reach a server before creation-time verification succeeds."
  } finally {
    Remove-ControlPlanePipeResponder -Responder $responder
  }
} finally {
  [Environment]::SetEnvironmentVariable("PRIM1_CONTROL_PLANE_SERVER_PID", $originalServerPid, "Process")
  [Environment]::SetEnvironmentVariable("PRIM1_CONTROL_PLANE_SERVER_STARTED_FILETIME", $originalServerStartedFiletime, "Process")
  Remove-Item -LiteralPath $testRuntime.Root -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "control-plane-server-identity tests passed"
