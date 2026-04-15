param(
  [Parameter(Mandatory = $true)]
  [string]$Session,

  [Parameter(Mandatory = $true)]
  [ValidateSet("enter", "up", "down", "left", "right", "tab", "esc", "ctrl_c")]
  [string]$Key,

  [string]$InfoFile
)

$scriptRoot = Split-Path -Parent $PSCommandPath
$controlPlaneScript = Join-Path $scriptRoot "control-plane.ps1"

& $controlPlaneScript `
  -Action key `
  -Session $Session `
  -Key $Key `
  -InfoFile $InfoFile `
  -Quiet

exit $LASTEXITCODE
