param(
  [Parameter(Mandatory = $true)]
  [string]$Session,

  [Parameter(Mandatory = $true)]
  [ValidatePattern("^[A-Za-z0-9:_-]+$")]
  [string]$Slash,

  [string]$Args,
  [string]$ArgsFile,
  [string]$InfoFile
)

if ($Args -and $ArgsFile) {
  throw "agent-slash accepts either -Args or -ArgsFile, not both"
}

$resolvedArgs = $Args
if ($ArgsFile) {
  $resolvedArgsPath = (Resolve-Path -LiteralPath $ArgsFile -ErrorAction Stop).Path
  $resolvedArgs = Get-Content -LiteralPath $resolvedArgsPath -Raw -ErrorAction Stop
}

$content = "/$Slash"
if (-not [string]::IsNullOrEmpty($resolvedArgs)) {
  $content = "$content $resolvedArgs"
}

$scriptRoot = Split-Path -Parent $PSCommandPath
$controlPlaneScript = Join-Path $scriptRoot "control-plane.ps1"

& $controlPlaneScript `
  -Action input `
  -Session $Session `
  -Content $content `
  -InfoFile $InfoFile `
  -Quiet

exit $LASTEXITCODE
