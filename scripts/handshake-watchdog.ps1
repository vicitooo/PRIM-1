param(
  [Parameter(Mandatory = $true)]
  [string]$Token,

  [string]$AuditPath,
  [int]$TimeoutSeconds = 120,
  [int]$PollMilliseconds = 500,
  [string]$InfoFile,
  [switch]$DryRun
)

$scriptRoot = Split-Path -Parent $PSCommandPath
$wrapperRoot = (Resolve-Path (Join-Path $scriptRoot "..")).Path
$handshakeRouteScript = Join-Path $scriptRoot "handshake-route.ps1"

if (-not $AuditPath) {
  $auditPath = Join-Path $wrapperRoot (Join-Path ".runtime\audit" "$((Get-Date).ToUniversalTime().ToString('yyyy-MM-dd')).jsonl")
}

$terminalMarkers = @(
  "FILE_READY $Token",
  "HANDSHAKE PASS token=$Token",
  "HANDSHAKE FAIL token=$Token",
  "HANDSHAKE FAIL (codex side) token=$Token"
)
$deadline = (Get-Date).ToUniversalTime().AddSeconds($TimeoutSeconds)

while ((Get-Date).ToUniversalTime() -lt $deadline) {
  if (Test-Path -LiteralPath $auditPath) {
    $raw = Get-Content -LiteralPath $auditPath -Raw -ErrorAction SilentlyContinue
    if ($raw) {
      foreach ($marker in $terminalMarkers) {
        if ($raw.Contains($marker)) {
          Write-Output "observed $marker"
          exit 0
        }
      }
    }
  }
  Start-Sleep -Milliseconds $PollMilliseconds
}

$reason = "timeout awaiting FILE_READY"
if ($DryRun) {
  @{
    token = $Token
    would_route_fail = $true
    reason = $reason
    audit_path = $auditPath
    timeout_seconds = $TimeoutSeconds
  } | ConvertTo-Json -Compress
  exit 1
}

& $handshakeRouteScript `
  -Actor claude `
  -Action fail `
  -Token $Token `
  -Reason $reason `
  -InfoFile $InfoFile

exit $LASTEXITCODE
