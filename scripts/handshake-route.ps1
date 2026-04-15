param(
  [Parameter(Mandatory = $true)]
  [ValidateSet("claude", "codex")]
  [string]$Actor,

  [Parameter(Mandatory = $true)]
  [ValidateSet("start", "dispatch", "pass", "fail", "ready", "status")]
  [string]$Action,

  [string]$Token,
  [string]$Path,
  [string]$Reason,
  [string]$InfoFile,
  [switch]$DryRun
)

$scriptRoot = Split-Path -Parent $PSCommandPath
$wrapperRoot = (Resolve-Path (Join-Path $scriptRoot "..")).Path
$agentRouteScript = Join-Path $scriptRoot "agent-route.ps1"

function Resolve-RequiredPath {
  param([string]$InputPath)

  if (-not $InputPath) {
    throw "this action requires -Path"
  }

  return [System.IO.Path]::GetFullPath($InputPath)
}

function Convert-ToWrapperRelativePath {
  param([string]$AbsolutePath)

  $normalizedAbsolute = [System.IO.Path]::GetFullPath($AbsolutePath)
  $normalizedRoot = [System.IO.Path]::GetFullPath($wrapperRoot)

  if ($normalizedAbsolute.StartsWith($normalizedRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
    $suffix = $normalizedAbsolute.Substring($normalizedRoot.Length).TrimStart('\')
    if ($suffix) {
      return ($suffix -replace '\\', '/').Insert(0, '.')
    }
  }

  return $AbsolutePath -replace '\\', '/'
}

if (-not $Token) {
  throw "all handshake route actions require -Token"
}

$route = switch ("${Actor}:${Action}") {
  "claude:start" {
    $resolvedPath = Resolve-RequiredPath -InputPath $Path
    @{
      from = "claude"
      to = "room"
      scope = "room"
      content = "HANDSHAKE START token=$Token target=$(Convert-ToWrapperRelativePath -AbsolutePath $resolvedPath) dispatching codex"
    }
  }
  "claude:dispatch" {
    $resolvedPath = Resolve-RequiredPath -InputPath $Path
    @{
      from = "claude"
      to = "codex"
      scope = "direct"
      content = "Handshake test from Claude. Create file at $resolvedPath with its entire contents being exactly the token $Token (no newline, no quotes, no surrounding whitespace). When the file is written, reply to me with exactly: FILE_READY $Token. Do not do anything else. Do not touch any other file. Stop after replying."
    }
  }
  "claude:pass" {
    $resolvedPath = Resolve-RequiredPath -InputPath $Path
    @{
      from = "claude"
      to = "room"
      scope = "room"
      content = "HANDSHAKE PASS token=$Token file verified at $(Convert-ToWrapperRelativePath -AbsolutePath $resolvedPath)"
    }
  }
  "claude:fail" {
    if (-not $Reason) {
      throw "claude fail requires -Reason"
    }
    @{
      from = "claude"
      to = "room"
      scope = "room"
      content = "HANDSHAKE FAIL token=$Token reason=$Reason"
    }
  }
  "codex:ready" {
    @{
      from = "codex"
      to = "claude"
      scope = "direct"
      content = "FILE_READY $Token"
    }
  }
  "codex:status" {
    $resolvedPath = Resolve-RequiredPath -InputPath $Path
    @{
      from = "codex"
      to = "room"
      scope = "room"
      content = "Handshake file written at $(Convert-ToWrapperRelativePath -AbsolutePath $resolvedPath) token=$Token replied to claude"
    }
  }
  "codex:fail" {
    if (-not $Reason) {
      throw "codex fail requires -Reason"
    }
    @{
      from = "codex"
      to = "room"
      scope = "room"
      content = "HANDSHAKE FAIL (codex side) token=$Token reason=$Reason"
    }
  }
  default {
    throw "unsupported handshake route action '${Actor}:${Action}'"
  }
}

if ($DryRun) {
  $route | ConvertTo-Json -Compress
  exit 0
}

& $agentRouteScript `
  -From $route.from `
  -To $route.to `
  -Scope $route.scope `
  -Content $route.content `
  -InfoFile $InfoFile

exit $LASTEXITCODE
