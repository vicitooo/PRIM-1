param(
  [Parameter(Mandatory = $true)]
  [ValidateSet("ping", "list", "start", "stop", "restart", "input", "route")]
  [string]$Action,

  [string]$Session,
  [string]$From = "victor",
  [string]$To,
  [ValidateSet("direct", "room", "system", "private")]
  [string]$Scope = "direct",
  [string]$Content,
  [string]$InfoFile
)

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
    if (-not $Content) { throw "input requires -Content" }
    @{ kind = "send_input"; token = $info.token; name = $Session; input = $Content }
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

$pipe = [System.IO.Pipes.NamedPipeClientStream]::new(".", $pipeName, [System.IO.Pipes.PipeDirection]::InOut)
$pipe.Connect(5000)

try {
  $writer = [System.IO.StreamWriter]::new($pipe)
  $writer.AutoFlush = $true
  $reader = [System.IO.StreamReader]::new($pipe)

  $writer.WriteLine($json)
  $response = $reader.ReadLine()

  if (-not $response) {
    throw "No response received from control plane."
  }

  $response | ConvertFrom-Json | ConvertTo-Json -Depth 8
} finally {
  $pipe.Dispose()
}
