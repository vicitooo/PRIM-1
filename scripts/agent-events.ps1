param(
  [string]$Consumer = "outside-supervisor",
  [string]$CursorFile,
  [int]$MaxEvents = 200,
  [int]$MaxWaitSeconds = 15,
  [string[]]$IncludeKinds = @("routed_message", "session_state", "system_log", "sideband_request_lifecycle"),
  [string[]]$IncludeSessions,
  [string[]]$IncludeScopes,
  [string]$InfoFile
)

function Write-AtomicText {
  param(
    [Parameter(Mandatory = $true)]
    [string]$Path,

    [Parameter(Mandatory = $true)]
    [string]$Content
  )

  $parent = Split-Path -Parent $Path
  if ($parent) {
    New-Item -ItemType Directory -Force -Path $parent | Out-Null
  }

  $tmpPath = "$Path.tmp"
  $utf8NoBom = [System.Text.UTF8Encoding]::new($false)
  [System.IO.File]::WriteAllText($tmpPath, $Content, $utf8NoBom)
  Move-Item -LiteralPath $tmpPath -Destination $Path -Force
}

$scriptRoot = Split-Path -Parent $PSCommandPath
$controlPlaneScript = Join-Path $scriptRoot "control-plane.ps1"

if (-not $CursorFile) {
  $CursorFile = Join-Path $scriptRoot "..\\.runtime\\cursors\\$Consumer.json"
}

if (-not (Test-Path -LiteralPath $CursorFile)) {
  Write-AtomicText -Path $CursorFile -Content "null"
}

$output = & $controlPlaneScript `
  -Action events_since `
  -CursorFile $CursorFile `
  -MaxEvents $MaxEvents `
  -MaxWaitSeconds $MaxWaitSeconds `
  -IncludeKinds $IncludeKinds `
  -IncludeSessions $IncludeSessions `
  -IncludeScopes $IncludeScopes `
  -OutCursorFile $CursorFile `
  -InfoFile $InfoFile
$exitCode = $LASTEXITCODE

if ($exitCode -ne 0) {
  if ($output) {
    $output
  }
  exit $exitCode
}

$rawJson = @($output) -join [Environment]::NewLine
$parsed = $rawJson | ConvertFrom-Json -ErrorAction Stop

foreach ($event in @($parsed.events)) {
  $event | ConvertTo-Json -Depth 12 -Compress
}

exit 0
