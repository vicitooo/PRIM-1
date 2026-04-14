param(
  [Parameter(Mandatory = $true)]
  [string]$From,

  [Parameter(Mandatory = $true)]
  [string]$To,

  [Parameter(Mandatory = $true)]
  [string]$Token,

  [string]$InfoFile
)

$scriptRoot = Split-Path -Parent $PSCommandPath
$agentRouteScript = Join-Path $scriptRoot "agent-route.ps1"
$content = "Reply with exactly: $Token"

& $agentRouteScript `
  -From $From `
  -To $To `
  -Content $content `
  -InfoFile $InfoFile

exit $LASTEXITCODE
