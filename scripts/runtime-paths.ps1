function ConvertTo-Prim1AbsolutePath {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory = $true)]
    [string]$Path
  )

  if ([string]::IsNullOrWhiteSpace($Path)) {
    throw "PRIM-1 path cannot be empty"
  }

  if ([System.IO.Path]::IsPathRooted($Path)) {
    return [System.IO.Path]::GetFullPath($Path)
  }

  $currentDirectory = (Get-Location).ProviderPath
  return [System.IO.Path]::GetFullPath((Join-Path $currentDirectory $Path))
}

function Test-Prim1Windows {
  [CmdletBinding()]
  param()

  return $env:OS -eq "Windows_NT" -or $PSVersionTable.PSEdition -eq "Desktop"
}

function Resolve-Prim1RuntimeDirectory {
  [CmdletBinding()]
  param(
    [string]$RuntimeDir
  )

  if (-not [string]::IsNullOrWhiteSpace($RuntimeDir)) {
    return ConvertTo-Prim1AbsolutePath -Path $RuntimeDir
  }

  if (-not [string]::IsNullOrWhiteSpace($env:PRIM1_RUNTIME_DIR)) {
    return ConvertTo-Prim1AbsolutePath -Path $env:PRIM1_RUNTIME_DIR
  }

  if (Test-Prim1Windows) {
    if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
      throw "Cannot resolve the PRIM-1 runtime directory: LOCALAPPDATA is not set. Set PRIM1_RUNTIME_DIR explicitly."
    }

    return ConvertTo-Prim1AbsolutePath -Path (Join-Path $env:LOCALAPPDATA "io.prim1.runtime\runtime")
  }

  $userProfile = [Environment]::GetFolderPath([Environment+SpecialFolder]::UserProfile)
  if ([string]::IsNullOrWhiteSpace($userProfile)) {
    throw "Cannot resolve the PRIM-1 runtime directory: the user profile is unavailable. Set PRIM1_RUNTIME_DIR explicitly."
  }

  $isMacOSVariable = Get-Variable -Name IsMacOS -ErrorAction SilentlyContinue
  if ($isMacOSVariable -and [bool]$isMacOSVariable.Value) {
    return ConvertTo-Prim1AbsolutePath -Path (Join-Path $userProfile "Library/Application Support/io.prim1.runtime/runtime")
  }

  $dataRoot = $env:XDG_DATA_HOME
  if ([string]::IsNullOrWhiteSpace($dataRoot)) {
    $dataRoot = Join-Path $userProfile ".local/share"
  }

  return ConvertTo-Prim1AbsolutePath -Path (Join-Path $dataRoot "io.prim1.runtime/runtime")
}

function Resolve-Prim1ControlPlaneInfoFile {
  [CmdletBinding()]
  param(
    [string]$InfoFile,
    [string]$RuntimeDir
  )

  if (-not [string]::IsNullOrWhiteSpace($InfoFile)) {
    return ConvertTo-Prim1AbsolutePath -Path $InfoFile
  }

  if (-not [string]::IsNullOrWhiteSpace($env:PRIM1_PANE_CREDENTIALS)) {
    return ConvertTo-Prim1AbsolutePath -Path $env:PRIM1_PANE_CREDENTIALS
  }

  $resolvedRuntimeDir = Resolve-Prim1RuntimeDirectory -RuntimeDir $RuntimeDir
  return Join-Path $resolvedRuntimeDir "control-plane.json"
}
