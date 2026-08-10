$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Assert-PathEqual {
  param(
    [Parameter(Mandatory = $true)]
    [string]$Actual,

    [Parameter(Mandatory = $true)]
    [string]$Expected,

    [Parameter(Mandatory = $true)]
    [string]$Message
  )

  $comparison = if ($env:OS -eq "Windows_NT") {
    [System.StringComparison]::OrdinalIgnoreCase
  } else {
    [System.StringComparison]::Ordinal
  }

  if (-not [string]::Equals(
      [System.IO.Path]::GetFullPath($Actual),
      [System.IO.Path]::GetFullPath($Expected),
      $comparison
    )) {
    throw "$Message`nexpected: $Expected`nactual:   $Actual"
  }
}

$scriptRoot = Split-Path -Parent $PSCommandPath
$wrapperRoot = (Resolve-Path (Join-Path $scriptRoot "..")).Path
$resolverPath = Join-Path $wrapperRoot "scripts\runtime-paths.ps1"
$loadOutput = @(. $resolverPath)
if ($loadOutput.Count -ne 0) {
  throw "runtime-paths.ps1 emitted output while being dot-sourced"
}

$originalRuntimeDir = [Environment]::GetEnvironmentVariable("PRIM1_RUNTIME_DIR", "Process")
$originalLocalAppData = [Environment]::GetEnvironmentVariable("LOCALAPPDATA", "Process")
$originalPaneCredentials = [Environment]::GetEnvironmentVariable("PRIM1_PANE_CREDENTIALS", "Process")
$testRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("PRIM-1 runtime path tests " + [guid]::NewGuid().ToString("N"))
$resolvedTestRoot = [System.IO.Path]::GetFullPath($testRoot)

try {
  New-Item -ItemType Directory -Path $resolvedTestRoot -Force | Out-Null

  $environmentRuntimeDir = Join-Path $resolvedTestRoot "environment runtime root"
  $explicitRuntimeDir = Join-Path $resolvedTestRoot "explicit runtime root"
  $explicitInfoFile = Join-Path $resolvedTestRoot "explicit credentials\custom control-plane.json"
  $paneInfoFile = Join-Path $resolvedTestRoot "pane credentials\control-plane-pane.json"
  $localAppData = Join-Path $resolvedTestRoot "Local App Data"

  [Environment]::SetEnvironmentVariable("PRIM1_RUNTIME_DIR", $environmentRuntimeDir, "Process")
  [Environment]::SetEnvironmentVariable("LOCALAPPDATA", $localAppData, "Process")
  [Environment]::SetEnvironmentVariable("PRIM1_PANE_CREDENTIALS", $paneInfoFile, "Process")

  Assert-PathEqual `
    -Actual (Resolve-Prim1RuntimeDirectory) `
    -Expected $environmentRuntimeDir `
    -Message "PRIM1_RUNTIME_DIR did not override the platform default"

  Assert-PathEqual `
    -Actual (Resolve-Prim1RuntimeDirectory -RuntimeDir $explicitRuntimeDir) `
    -Expected $explicitRuntimeDir `
    -Message "an explicit runtime test override did not win"

  Assert-PathEqual `
    -Actual (Resolve-Prim1ControlPlaneInfoFile) `
    -Expected $paneInfoFile `
    -Message "pane credentials did not win over the operator runtime default"

  Assert-PathEqual `
    -Actual (Resolve-Prim1ControlPlaneInfoFile -InfoFile $explicitInfoFile) `
    -Expected $explicitInfoFile `
    -Message "an explicit InfoFile did not win over pane credentials"

  [Environment]::SetEnvironmentVariable("PRIM1_PANE_CREDENTIALS", "  ", "Process")
  Assert-PathEqual `
    -Actual (Resolve-Prim1ControlPlaneInfoFile) `
    -Expected (Join-Path $environmentRuntimeDir "control-plane.json") `
    -Message "the operator default control-plane path did not use PRIM1_RUNTIME_DIR"

  if (Test-Prim1Windows) {
    [Environment]::SetEnvironmentVariable("PRIM1_RUNTIME_DIR", "  ", "Process")
    Assert-PathEqual `
      -Actual (Resolve-Prim1RuntimeDirectory) `
      -Expected (Join-Path $localAppData "io.prim1.runtime\runtime") `
      -Message "the Windows runtime default did not use LOCALAPPDATA"
  }

  Write-Host "runtime path resolver tests passed"
} finally {
  [Environment]::SetEnvironmentVariable("PRIM1_RUNTIME_DIR", $originalRuntimeDir, "Process")
  [Environment]::SetEnvironmentVariable("LOCALAPPDATA", $originalLocalAppData, "Process")
  [Environment]::SetEnvironmentVariable("PRIM1_PANE_CREDENTIALS", $originalPaneCredentials, "Process")

  $tempRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
  if ($resolvedTestRoot.StartsWith($tempRoot, [System.StringComparison]::OrdinalIgnoreCase) -and
      (Split-Path -Leaf $resolvedTestRoot).StartsWith("PRIM-1 runtime path tests ")) {
    Remove-Item -LiteralPath $resolvedTestRoot -Recurse -Force -ErrorAction SilentlyContinue
  }
}

exit 0
