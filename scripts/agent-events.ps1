param(
  [string]$Consumer = "outside-supervisor",
  [string]$CursorFile,
  [int]$MaxEvents = 200,
  [int]$MaxWaitSeconds = 15,
  [string[]]$IncludeKinds = @("route_delivery", "dispatch_attempt", "session_state", "session_exit", "session_work_state", "supervisor_heartbeat", "supervisor_alert", "system_log", "sideband_request_lifecycle", "request_ack", "request_ack_timeout", "dispatch_no_reaction"),
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
. (Join-Path $scriptRoot "runtime-paths.ps1")

if (-not $CursorFile) {
  $runtimeDir = Resolve-Prim1RuntimeDirectory
  $CursorFile = Join-Path $runtimeDir "cursors\$Consumer.json"
}

if (-not (Test-Path -LiteralPath $CursorFile)) {
  Write-AtomicText -Path $CursorFile -Content "null"
}

$ceArgs = @{
  Action         = 'events_since'
  CursorFile     = $CursorFile
  MaxEvents      = $MaxEvents
  MaxWaitSeconds = $MaxWaitSeconds
  IncludeKinds   = $IncludeKinds
  OutCursorFile  = $CursorFile
  InfoFile       = $InfoFile
}
if ($IncludeSessions) { $ceArgs.IncludeSessions = $IncludeSessions }
if ($IncludeScopes)   { $ceArgs.IncludeScopes   = $IncludeScopes }
$output = & $controlPlaneScript @ceArgs
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
