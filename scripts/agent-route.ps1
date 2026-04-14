param(
  [Parameter(Mandatory = $true)]
  [string]$From,

  [Parameter(Mandatory = $true)]
  [string]$To,

  [Parameter(Mandatory = $true)]
  [string]$Content,

  [ValidateSet("direct", "room", "system", "private")]
  [string]$Scope = "direct",

  [string]$InfoFile
)

$scriptRoot = Split-Path -Parent $PSCommandPath
$controlPlaneScript = Join-Path $scriptRoot "control-plane.ps1"

& $controlPlaneScript `
  -Action route `
  -From $From `
  -To $To `
  -Scope $Scope `
  -Content $Content `
  -InfoFile $InfoFile `
  -Quiet

exit $LASTEXITCODE
