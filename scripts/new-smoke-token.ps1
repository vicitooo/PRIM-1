param(
  [string]$Prefix = "SMOKE"
)

$scriptRoot = Split-Path -Parent $PSCommandPath
$wrapperRoot = (Resolve-Path (Join-Path $scriptRoot "..")).Path
$timestampUtc = (Get-Date).ToUniversalTime().ToString("yyyyMMddTHHmmssZ")
$bytes = [byte[]]::new(4)
$rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()
$rng.GetBytes($bytes)
$hex = (($bytes | ForEach-Object { $_.ToString("X2") }) -join "")
$token = "$Prefix-$hex"
$relativePath = ".runtime/smoke/handshake-$timestampUtc-$token.txt"
$absolutePath = Join-Path $wrapperRoot (Join-Path ".runtime\smoke" "handshake-$timestampUtc-$token.txt")

@{
  token = $token
  timestamp_utc = $timestampUtc
  relative_path = $relativePath
  absolute_path = $absolutePath
} | ConvertTo-Json -Compress
