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

function Assert-Equal {
  param(
    $Actual,
    $Expected,
    [string]$Message
  )

  if ($Actual -cne $Expected) {
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
$originalEndpoint = [Environment]::GetEnvironmentVariable("PRIM1_CONTROL_PLANE_ENDPOINT", "Process")
$testRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("PRIM-1 runtime path tests " + [guid]::NewGuid().ToString("N"))
$resolvedTestRoot = [System.IO.Path]::GetFullPath($testRoot)

try {
  New-Item -ItemType Directory -Path $resolvedTestRoot -Force | Out-Null

  $environmentRuntimeDir = Join-Path $resolvedTestRoot "environment runtime root"
  $explicitRuntimeDir = Join-Path $resolvedTestRoot "explicit runtime root"
  $localAppData = Join-Path $resolvedTestRoot "Local App Data"
  $environmentEndpoint = "\\.\pipe\prim1-environment"
  $explicitEndpoint = "\\.\pipe\prim1-explicit"

  [Environment]::SetEnvironmentVariable("PRIM1_RUNTIME_DIR", $environmentRuntimeDir, "Process")
  [Environment]::SetEnvironmentVariable("LOCALAPPDATA", $localAppData, "Process")
  [Environment]::SetEnvironmentVariable("PRIM1_CONTROL_PLANE_ENDPOINT", $environmentEndpoint, "Process")

  Assert-PathEqual `
    -Actual (Resolve-Prim1RuntimeDirectory) `
    -Expected $environmentRuntimeDir `
    -Message "PRIM1_RUNTIME_DIR did not override the platform default"

  Assert-PathEqual `
    -Actual (Resolve-Prim1RuntimeDirectory -RuntimeDir $explicitRuntimeDir) `
    -Expected $explicitRuntimeDir `
    -Message "an explicit runtime test override did not win"

  Assert-Equal `
    -Actual (Resolve-Prim1ControlPlaneEndpoint) `
    -Expected $environmentEndpoint `
    -Message "PRIM1_CONTROL_PLANE_ENDPOINT was not used"

  Assert-Equal `
    -Actual (Resolve-Prim1ControlPlaneEndpoint -Endpoint $explicitEndpoint) `
    -Expected $explicitEndpoint `
    -Message "an explicit endpoint test override did not win"

  [Environment]::SetEnvironmentVariable("PRIM1_CONTROL_PLANE_ENDPOINT", "  ", "Process")
  $missingEndpointMessage = $null
  try {
    Resolve-Prim1ControlPlaneEndpoint | Out-Null
  } catch {
    $missingEndpointMessage = $_.Exception.Message
  }
  Assert-Equal `
    -Actual $missingEndpointMessage `
    -Expected "PRIM1_CONTROL_PLANE_ENDPOINT is not set. External operator control is unavailable; use the PRIM-1 desktop UI." `
    -Message "missing endpoint did not fail with the stable operator direction"

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
  [Environment]::SetEnvironmentVariable("PRIM1_CONTROL_PLANE_ENDPOINT", $originalEndpoint, "Process")

  $tempRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
  if ($resolvedTestRoot.StartsWith($tempRoot, [System.StringComparison]::OrdinalIgnoreCase) -and
      (Split-Path -Leaf $resolvedTestRoot).StartsWith("PRIM-1 runtime path tests ")) {
    Remove-Item -LiteralPath $resolvedTestRoot -Recurse -Force -ErrorAction SilentlyContinue
  }
}

exit 0
