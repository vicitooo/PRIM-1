$ErrorActionPreference = "Stop"

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$newTokenScript = Join-Path $repoRoot "scripts\new-smoke-token.ps1"
$handshakeRouteScript = Join-Path $repoRoot "scripts\handshake-route.ps1"
$handshakeWatchdogScript = Join-Path $repoRoot "scripts\handshake-watchdog.ps1"
$powershellExe = (Get-Command powershell).Source
$tempRoot = Join-Path $repoRoot ".runtime-test\handshake-helpers"
New-Item -ItemType Directory -Force -Path $tempRoot | Out-Null

function Assert-True {
  param(
    [bool]$Condition,
    [string]$Message
  )

  if (-not $Condition) {
    throw $Message
  }
}

$firstRun = & $powershellExe -ExecutionPolicy Bypass -File $newTokenScript
Assert-True ($LASTEXITCODE -eq 0) "new-smoke-token.ps1 first run failed"
$secondRun = & $powershellExe -ExecutionPolicy Bypass -File $newTokenScript
Assert-True ($LASTEXITCODE -eq 0) "new-smoke-token.ps1 second run failed"

$first = $firstRun | ConvertFrom-Json
$second = $secondRun | ConvertFrom-Json

Assert-True ($first.token -match "^SMOKE-[0-9A-F]{8}$") "token format is not 8 uppercase hex"
Assert-True ($first.absolute_path -match "handshake-\d{8}T\d{6}Z-SMOKE-[0-9A-F]{8}\.txt$") "absolute path format is wrong"
Assert-True ($first.relative_path -match "^\.runtime/smoke/handshake-\d{8}T\d{6}Z-SMOKE-[0-9A-F]{8}\.txt$") "relative path format is wrong"
Assert-True ($first.token -ne $second.token) "new-smoke-token.ps1 generated the same token twice"

$samplePath = Join-Path $repoRoot ".runtime\smoke\handshake-20260415T120000Z-SMOKE-1A2B3C4D.txt"
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $samplePath) | Out-Null
Set-Content -LiteralPath $samplePath -Value "SMOKE-1A2B3C4D" -Encoding Ascii -NoNewline

$dispatchDryRun = & $powershellExe -ExecutionPolicy Bypass -File $handshakeRouteScript -Actor claude -Action dispatch -Token SMOKE-1A2B3C4D -Path $samplePath -DryRun
Assert-True ($LASTEXITCODE -eq 0) "handshake-route.ps1 dispatch dry run failed"
$dispatch = $dispatchDryRun | ConvertFrom-Json
Assert-True ($dispatch.to -eq "codex") "dispatch target should be codex"
Assert-True ($dispatch.scope -eq "direct") "dispatch scope should be direct"
Assert-True ($dispatch.content.Contains("Handshake test from Claude.")) "dispatch preamble missing"
Assert-True ($dispatch.content.Contains("FILE_READY SMOKE-1A2B3C4D")) "dispatch reply token missing"

$statusDryRun = & $powershellExe -ExecutionPolicy Bypass -File $handshakeRouteScript -Actor codex -Action status -Token SMOKE-1A2B3C4D -Path $samplePath -DryRun
Assert-True ($LASTEXITCODE -eq 0) "handshake-route.ps1 status dry run failed"
$status = $statusDryRun | ConvertFrom-Json
Assert-True ($status.to -eq "room") "codex status should target room"
Assert-True ($status.content.Contains(".runtime/smoke/handshake-20260415T120000Z-SMOKE-1A2B3C4D.txt")) "codex status should use wrapper-relative path"

$readyAuditPath = Join-Path $tempRoot "ready.jsonl"
Set-Content -LiteralPath $readyAuditPath -Value "FILE_READY SMOKE-1A2B3C4D" -Encoding Utf8
& $powershellExe -ExecutionPolicy Bypass -File $handshakeWatchdogScript -Token SMOKE-1A2B3C4D -AuditPath $readyAuditPath -TimeoutSeconds 1 | Out-Null
Assert-True ($LASTEXITCODE -eq 0) "handshake-watchdog.ps1 should exit cleanly when FILE_READY already exists"

$timeoutAuditPath = Join-Path $tempRoot "timeout.jsonl"
Set-Content -LiteralPath $timeoutAuditPath -Value "" -Encoding Utf8
$timeoutDryRun = & $powershellExe -ExecutionPolicy Bypass -File $handshakeWatchdogScript -Token SMOKE-9ABCDEFF -AuditPath $timeoutAuditPath -TimeoutSeconds 0 -DryRun
Assert-True ($LASTEXITCODE -eq 1) "handshake-watchdog.ps1 dry-run timeout should exit 1"
$timeout = $timeoutDryRun | ConvertFrom-Json
Assert-True ($timeout.reason -eq "timeout awaiting FILE_READY") "watchdog timeout reason mismatch"
Assert-True ($timeout.would_route_fail -eq $true) "watchdog dry run should report failure routing"

Write-Output "handshake helper tests passed"
