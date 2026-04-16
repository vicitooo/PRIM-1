param(
  [Parameter(Mandatory = $true)]
  [ValidateSet("ping", "list", "start", "stop", "restart", "input", "deliver", "wait_quiet", "key", "route")]
  [string]$Action,

  [string]$Session,
  [ValidateSet("enter", "up", "down", "left", "right", "tab", "esc", "ctrl_c")]
  [string]$Key,
  [string]$From = "victor",
  [string]$To,
  [ValidateSet("direct", "room", "system", "private")]
  [string]$Scope = "direct",
  [string]$Content,
  [string]$ContentFile,
  [int]$QuietSec,
  [int]$TimeoutSec,
  [string]$InfoFile,
  [switch]$Quiet
)

function Invoke-MailboxFallback {
  param(
    [Parameter(Mandatory = $true)]
    [string]$Payload,

    [Parameter(Mandatory = $true)]
    [string]$RuntimeDir
  )

  $sidebandRoot = Join-Path $RuntimeDir "sideband"
  $inboxDir = Join-Path $sidebandRoot "inbox"
  $outboxDir = Join-Path $sidebandRoot "outbox"
  New-Item -ItemType Directory -Force -Path $inboxDir | Out-Null
  New-Item -ItemType Directory -Force -Path $outboxDir | Out-Null

  $requestId = [guid]::NewGuid().ToString()
  $requestPath = Join-Path $inboxDir "$requestId.json"
  $responsePath = Join-Path $outboxDir "$requestId.json"

  $utf8NoBom = [System.Text.UTF8Encoding]::new($false)
  [System.IO.File]::WriteAllText($requestPath, $Payload, $utf8NoBom)

  $deadline = [DateTime]::UtcNow.AddSeconds(10)
  while ([DateTime]::UtcNow -lt $deadline) {
    if (Test-Path -LiteralPath $responsePath) {
      $raw = Get-Content -LiteralPath $responsePath -Raw
      Remove-Item -LiteralPath $responsePath -Force -ErrorAction SilentlyContinue
      return $raw
    }
    Start-Sleep -Milliseconds 100
  }

  throw "No response received from sideband mailbox."
}

function Resolve-MessageContent {
  param(
    [string]$ActionName,
    [string]$InlineContent,
    [string]$ContentFilePath
  )

  if ($InlineContent -and $ContentFilePath) {
    throw "$ActionName accepts either -Content or -ContentFile, not both"
  }

  if (-not $ContentFilePath) {
    return $InlineContent
  }

  $resolvedContentFile = $null
  try {
    $resolvedContentFile = (Resolve-Path -LiteralPath $ContentFilePath -ErrorAction Stop).Path
  } catch {
    throw "failed to resolve -ContentFile '$ContentFilePath'"
  }

  try {
    return Get-Content -LiteralPath $resolvedContentFile -Raw -ErrorAction Stop
  } catch {
    throw "failed to read -ContentFile '$resolvedContentFile': $($_.Exception.Message)"
  }
}

if ($Action -in @("input", "deliver")) {
  $Content = Resolve-MessageContent -ActionName $Action -InlineContent $Content -ContentFilePath $ContentFile
} elseif ($ContentFile) {
  throw "-ContentFile is only supported for -Action input or -Action deliver"
}

if (-not $InfoFile -and $env:PRIM1_PANE_CREDENTIALS -and (Test-Path -LiteralPath $env:PRIM1_PANE_CREDENTIALS)) {
  $InfoFile = $env:PRIM1_PANE_CREDENTIALS
}

if (-not $InfoFile) {
  $scriptRoot = Split-Path -Parent $PSCommandPath
  $InfoFile = Join-Path $scriptRoot "..\\.runtime\\control-plane.json"
}

$resolvedInfoFile = (Resolve-Path $InfoFile).Path
$info = Get-Content $resolvedInfoFile | ConvertFrom-Json

if (-not $info.endpoint) {
  throw "Missing endpoint in $resolvedInfoFile"
}

$pipeName = $info.endpoint -replace '^\\\\\.\\pipe\\', ''
if (-not $pipeName) {
  throw "Failed to parse pipe name from '$($info.endpoint)'"
}

$payload = switch ($Action) {
  "ping" {
    @{ kind = "ping"; token = $info.token }
  }
  "list" {
    @{ kind = "list_sessions"; token = $info.token }
  }
  "start" {
    if (-not $Session) { throw "start requires -Session" }
    @{ kind = "start_session"; token = $info.token; name = $Session }
  }
  "stop" {
    if (-not $Session) { throw "stop requires -Session" }
    @{ kind = "stop_session"; token = $info.token; name = $Session }
  }
  "restart" {
    if (-not $Session) { throw "restart requires -Session" }
    @{ kind = "restart_session"; token = $info.token; name = $Session }
  }
  "input" {
    if (-not $Session) { throw "input requires -Session" }
    if ([string]::IsNullOrEmpty($Content)) { throw "input requires -Content or -ContentFile" }
    @{ kind = "send_input"; token = $info.token; name = $Session; input = $Content }
  }
  "deliver" {
    if (-not $Session) { throw "deliver requires -Session" }
    if ([string]::IsNullOrEmpty($Content)) { throw "deliver requires -Content or -ContentFile" }
    @{ kind = "deliver_message"; token = $info.token; name = $Session; content = $Content }
  }
  "wait_quiet" {
    if (-not $Session) { throw "wait_quiet requires -Session" }
    if ($QuietSec -le 0) { throw "wait_quiet requires -QuietSec > 0" }
    if ($TimeoutSec -le 0) { throw "wait_quiet requires -TimeoutSec > 0" }
    @{
      kind = "wait_quiet"
      token = $info.token
      name = $Session
      quiet_seconds = $QuietSec
      timeout_seconds = $TimeoutSec
    }
  }
  "key" {
    if (-not $Session) { throw "key requires -Session" }
    if (-not $Key) { throw "key requires -Key" }
    @{ kind = "send_key"; token = $info.token; name = $Session; key = $Key }
  }
  "route" {
    if (-not $To) { throw "route requires -To" }
    if (-not $Content) { throw "route requires -Content" }
    @{
      kind = "route_message"
      token = $info.token
      request = @{
        from = $From
        to = $To
        scope = $Scope
        content = $Content
      }
    }
  }
}

$json = $payload | ConvertTo-Json -Depth 8 -Compress
$runtimeDir = Split-Path -Parent $resolvedInfoFile
$response = $null

try {
  $pipe = [System.IO.Pipes.NamedPipeClientStream]::new(".", $pipeName, [System.IO.Pipes.PipeDirection]::InOut)
  try {
    $pipe.Connect(5000)
    $writer = [System.IO.StreamWriter]::new($pipe)
    $writer.AutoFlush = $true
    $reader = [System.IO.StreamReader]::new($pipe)

    $writer.WriteLine($json)
    $response = $reader.ReadLine()
  } finally {
    if ($pipe) {
      $pipe.Dispose()
    }
  }
} catch {
  $response = Invoke-MailboxFallback -Payload $json -RuntimeDir $runtimeDir
}

if (-not $response) {
  throw "No response received from control plane."
}

$parsed = $response | ConvertFrom-Json

if ($Quiet) {
  if (-not $parsed.ok) {
    Write-Error $parsed.message
    exit 1
  }

  $parsed.message
  exit 0
}

$parsed | ConvertTo-Json -Depth 8

if (-not $parsed.ok) {
  exit 1
}
exit 0
