param(
  [string]$Session = "claude",
  [string]$ResumeId = "00000000-0000-0000-0000-000000000000",
  [string]$InfoFile
)

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

function Quote-PowerShellLiteral {
  param([string]$Value)
  return "'" + ($Value -replace "'", "''") + "'"
}

function Invoke-ControlPlaneFile {
  param([string[]]$Arguments)

  $output = & powershell.exe -NoProfile -ExecutionPolicy Bypass @Arguments 2>&1
  return [pscustomobject]@{
    ExitCode = $LASTEXITCODE
    Output = ($output | Out-String).Trim()
  }
}

function Invoke-ControlPlaneCommand {
  param([string]$Command)

  $output = & powershell.exe -NoProfile -ExecutionPolicy Bypass -Command $Command 2>&1
  return [pscustomobject]@{
    ExitCode = $LASTEXITCODE
    Output = ($output | Out-String).Trim()
  }
}

$scriptRoot = Split-Path -Parent $PSCommandPath
$wrapperRoot = (Resolve-Path (Join-Path $scriptRoot "..\..")).Path
$controlPlaneScript = Join-Path $wrapperRoot "scripts\control-plane.ps1"

if (-not $InfoFile) {
  $InfoFile = Join-Path $wrapperRoot ".runtime\control-plane.json"
}

$resolvedInfoFile = (Resolve-Path -LiteralPath $InfoFile).Path
$startedBySmoke = $false

Write-Host "start-with-extra-args smoke"
Write-Host "wrapper_root=$wrapperRoot"
Write-Host "info_file=$resolvedInfoFile"
Write-Host "session=$Session"
Write-Host "resume_id=$ResumeId"

$listResult = Invoke-ControlPlaneFile -Arguments @(
  "-File", $controlPlaneScript,
  "-Action", "list",
  "-InfoFile", $resolvedInfoFile
)
Write-Host "list_exit=$($listResult.ExitCode)"
Write-Host $listResult.Output
Assert-True ($listResult.ExitCode -eq 0) "list failed before smoke start"

$listJson = $listResult.Output | ConvertFrom-Json
$target = @($listJson.snapshot.sessions | Where-Object { $_.name -eq $Session }) | Select-Object -First 1
Assert-True ($null -ne $target) "session '$Session' was not registered in the running wrapper"
Assert-True (-not $target.running) "session '$Session' is already running; refusing to stop a pre-existing pane"

$auditPath = $listJson.snapshot.audit_log_path
$quotedScript = Quote-PowerShellLiteral $controlPlaneScript
$quotedInfoFile = Quote-PowerShellLiteral $resolvedInfoFile
$quotedSession = Quote-PowerShellLiteral $Session
$quotedResumeId = Quote-PowerShellLiteral $ResumeId
$startCommand = "& $quotedScript -Action start -Session $quotedSession -ExtraArgs '--resume',$quotedResumeId -InfoFile $quotedInfoFile"

try {
  $startResult = Invoke-ControlPlaneCommand -Command $startCommand
  Write-Host "start_exit=$($startResult.ExitCode)"
  Write-Host $startResult.Output
  Assert-True ($startResult.ExitCode -eq 0) "start with extra args failed"

  $startedBySmoke = $true
  $startJson = $startResult.Output | ConvertFrom-Json
  Assert-True ($startJson.ok -eq $true) "start response was not ok"

  $deadline = [DateTime]::UtcNow.AddSeconds(10)
  $matched = $false
  while ([DateTime]::UtcNow -lt $deadline) {
    $events = Get-Content -LiteralPath $auditPath -Tail 80 |
      Where-Object { $_ } |
      ForEach-Object { $_ | ConvertFrom-Json }

    foreach ($event in $events) {
      $extraArgsProperty = $event.PSObject.Properties["extra_args"]
      if (
        $event.event -eq "sideband_request_lifecycle" -and
        $event.action -eq "start_session" -and
        $event.session -eq $Session -and
        $event.phase -eq "started" -and
        $null -ne $extraArgsProperty -and
        @($extraArgsProperty.Value).Count -eq 2 -and
        $extraArgsProperty.Value[0] -eq "--resume" -and
        $extraArgsProperty.Value[1] -eq $ResumeId
      ) {
        $matched = $true
        break
      }
    }

    if ($matched) {
      break
    }
    Start-Sleep -Milliseconds 200
  }

  Assert-True $matched "audit log did not contain the expected start_session extra_args entry"
  Write-Host "audit_extra_args=matched"
} finally {
  if ($startedBySmoke) {
    $stopResult = Invoke-ControlPlaneFile -Arguments @(
      "-File", $controlPlaneScript,
      "-Action", "stop",
      "-Session", $Session,
      "-InfoFile", $resolvedInfoFile
    )
    Write-Host "stop_exit=$($stopResult.ExitCode)"
    Write-Host $stopResult.Output
    Assert-True ($stopResult.ExitCode -eq 0) "cleanup stop failed"
  }
}

Write-Host "start-with-extra-args smoke passed"
