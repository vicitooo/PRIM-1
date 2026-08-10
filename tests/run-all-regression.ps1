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

function Invoke-RunnerProcess {
  param(
    [string]$PowerShellExe,
    [string[]]$Arguments,
    [string]$PathValue
  )

  $previousPath = $env:PATH
  try {
    $env:PATH = $PathValue
    $output = & $PowerShellExe @Arguments 2>&1 | Out-String
    $exitCode = $LASTEXITCODE
  } finally {
    $env:PATH = $previousPath
  }

  return [pscustomobject]@{
    ExitCode = $exitCode
    Output = $output
  }
}

$repoRoot = Split-Path -Parent $PSScriptRoot
$runnerSource = Join-Path $PSScriptRoot "run-all.ps1"
$powerShellExe = (Get-Command powershell.exe -CommandType Application).Source
$testRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("prim1-runner-regression-" + [guid]::NewGuid())
$stubBin = Join-Path $testRoot "stub-bin"
$testDir = Join-Path $testRoot "tests"
$scriptsDir = Join-Path $testRoot "scripts"
$utf8NoBom = [System.Text.UTF8Encoding]::new($false)

try {
  $null = New-Item -ItemType Directory -Force -Path $stubBin, $testDir, $scriptsDir
  Copy-Item -LiteralPath $runnerSource -Destination (Join-Path $testDir "run-all.ps1")

  foreach ($relativePath in @(
    "tests\runtime-paths.ps1",
    "tests\agent-events-summary.test.py",
    "tests\control-plane-content-file.ps1",
    "tests\control-plane-request-id.ps1",
    "tests\control-plane-server-identity.ps1",
    "tests\control-plane-wait.ps1",
    "tests\control-plane-timeouts.ps1",
    "tests\control-plane-room.ps1"
  )) {
    $path = Join-Path $testRoot $relativePath
    [System.IO.File]::WriteAllText($path, "", $utf8NoBom)
  }

  [System.IO.File]::WriteAllText(
    (Join-Path $stubBin "python.cmd"),
    "@exit /b 0`r`n",
    $utf8NoBom
  )
  [System.IO.File]::WriteAllText(
    (Join-Path $stubBin "powershell.cmd"),
    "@echo %* | %SystemRoot%\System32\findstr.exe /c:`"control-plane-timeouts.ps1`" >nul && exit /b 1`r`n@exit /b 0`r`n",
    $utf8NoBom
  )

  $runnerPath = Join-Path $testDir "run-all.ps1"
  $stubPath = $stubBin + [System.IO.Path]::PathSeparator + $env:PATH
  $singleFailure = Invoke-RunnerProcess -PowerShellExe $powerShellExe -Arguments @(
    "-NoProfile",
    "-ExecutionPolicy", "Bypass",
    "-File", $runnerPath
  ) -PathValue $stubPath

  Assert-True ($singleFailure.ExitCode -eq 1) "A single failing suite must make the runner exit 1."
  Assert-True ($singleFailure.Output.Contains("PRIM-1 test runner: 7 pass, 1 fail")) "The single failing suite must be counted as an integer 1."
  Assert-True ($singleFailure.Output.Contains("control-plane timeouts (exit 1)")) "The failing suite must be identified."
  Assert-True ($singleFailure.Output.Contains("OVERALL: FAIL")) "The runner must render the failing boundary."
  Assert-True (-not $singleFailure.Output.Contains("OVERALL: PASS")) "A failing suite must never render an overall pass."

  $escapedRunnerPath = $runnerPath.Replace("'", "''")
  $noNativeCommand = @"
function global:python { 'synthetic python output' }
function global:powershell { 'synthetic powershell output' }
`$global:LASTEXITCODE = 0
& '$escapedRunnerPath'
"@
  $noNativeExit = Invoke-RunnerProcess -PowerShellExe $powerShellExe -Arguments @(
    "-NoProfile",
    "-ExecutionPolicy", "Bypass",
    "-Command", $noNativeCommand
  ) -PathValue $env:PATH

  Assert-True ($noNativeExit.ExitCode -eq 1) "A suite without a fresh native exit code must fail closed."
  Assert-True ($noNativeExit.Output.Contains("PRIM-1 test runner: 0 pass, 8 fail")) "Every no-exit suite must be classified as failed."
  Assert-True ($noNativeExit.Output.Contains("suite completed without an external process exit code")) "The fail-closed reason must be visible."
  Assert-True ($noNativeExit.Output.Contains("OVERALL: FAIL")) "The no-exit case must render the failing boundary."
  Assert-True (-not $noNativeExit.Output.Contains("OVERALL: PASS")) "The no-exit case must never render an overall pass."
} finally {
  Remove-Item -LiteralPath $testRoot -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "run-all regression tests passed"
exit 0
