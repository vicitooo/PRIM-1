# v3 W7 (Reviewer d597f0d1): rollback-runtime.ps1 against throwaway folders under %TEMP% only. Each refusal
# must leave both folders untouched; the valid case restores the snapshot and keeps the v3 runtime aside.
#   powershell -NoProfile -File scripts\rollback-runtime.tests.ps1
$ErrorActionPreference = 'Stop'
$script = Join-Path $PSScriptRoot 'rollback-runtime.ps1'
$root = Join-Path $env:TEMP ("prim1-rollback-tests-" + [guid]::NewGuid().ToString('N'))
$failures = 0

function New-Fixture([string]$Name) {
  $base = Join-Path $root $Name
  $runtime = Join-Path $base 'runtime'
  $snapshot = Join-Path $base 'runtime.snapshot-pre-v3-x'
  New-Item -ItemType Directory -Force -Path (Join-Path $runtime 'rooms'), (Join-Path $snapshot 'rooms') | Out-Null
  Set-Content -LiteralPath (Join-Path $runtime 'rooms\r.jsonl') -Value 'v3 history' -NoNewline
  Set-Content -LiteralPath (Join-Path $snapshot 'rooms\r.jsonl') -Value 'v2 history' -NoNewline
  Set-Content -LiteralPath (Join-Path $snapshot 'session-catalog-v1.json') -Value '{}' -NoNewline
  $receipt = [ordered]@{ outcome = 'snapshot_taken'; runtime = $runtime; snapshot = $snapshot; files = 2; bytes = 12; at_utc = 'x' }
  $receipt | ConvertTo-Json | Set-Content -Encoding utf8 -LiteralPath (Join-Path $snapshot 'SNAPSHOT.json')
  [ordered]@{ format = 3; pre_v3_snapshot = $snapshot } | ConvertTo-Json | Set-Content -Encoding utf8 -LiteralPath (Join-Path $runtime 'runtime-format.json')
  [pscustomobject]@{ Base = $base; Runtime = $runtime; Snapshot = $snapshot }
}

function Set-Receipt($Fixture, [scriptblock]$Change) {
  $path = Join-Path $Fixture.Snapshot 'SNAPSHOT.json'
  $receipt = Get-Content -Raw -LiteralPath $path | ConvertFrom-Json
  & $Change $receipt
  $receipt | ConvertTo-Json | Set-Content -Encoding utf8 -LiteralPath $path
}

function Get-State($Fixture) {
  @(Get-ChildItem -LiteralPath $Fixture.Base -Recurse -Force -File | Sort-Object FullName |
    ForEach-Object { "$($_.FullName.Substring($Fixture.Base.Length))=$((Get-Content -Raw -LiteralPath $_.FullName))" }) -join '|'
}

function Assert-Refused([string]$Case, $Fixture, [string]$Runtime, [string]$Snapshot, [string]$Expect) {
  $before = Get-State $Fixture
  try { & $script -Runtime $Runtime -Snapshot $Snapshot | Out-Null; $message = $null }
  catch { $message = $_.Exception.Message }
  $after = Get-State $Fixture
  if (-not $message) { "FAIL $Case : accepted"; $script:failures++ }
  elseif ($message -notlike "*$Expect*") { "FAIL $Case : $message"; $script:failures++ }
  elseif ($before -ne $after) { "FAIL $Case : refused but changed the folders"; $script:failures++ }
  else { "ok   $Case" }
}

try {
  $f = New-Fixture 'wrong-runtime'
  Set-Receipt $f { param($r) $r.runtime = 'D:\some\other\runtime' }
  Assert-Refused 'a snapshot of another runtime' $f $f.Runtime $f.Snapshot 'was taken of'

  $f = New-Fixture 'wrong-snapshot-name'
  Set-Receipt $f { param($r) $r.snapshot = 'D:\some\other\runtime.snapshot' }
  Assert-Refused 'a receipt naming another snapshot' $f $f.Runtime $f.Snapshot 'receipt names'

  $f = New-Fixture 'missing-file'
  Remove-Item -LiteralPath (Join-Path $f.Snapshot 'session-catalog-v1.json')
  Assert-Refused 'a snapshot missing a file since capture' $f $f.Runtime $f.Snapshot 'no longer what was recorded'

  $f = New-Fixture 'app-server-session'
  $catalog = Join-Path $f.Snapshot 'session-catalog-v1.json'
  Set-Content -LiteralPath $catalog -Value '{"sessions":[{"codex_transport":"app_server"}]}' -NoNewline
  Set-Receipt $f { param($r) $r.bytes = 10 + (Get-Item -LiteralPath $catalog).Length }
  Assert-Refused 'a snapshot session file on the App Server transport' $f $f.Runtime $f.Snapshot 'Codex App Server transport'

  $f = New-Fixture 'incomplete-receipt'
  Set-Receipt $f { param($r) $r.outcome = 'started' }
  Assert-Refused 'an incomplete receipt' $f $f.Runtime $f.Snapshot 'not a complete snapshot record'

  $f = New-Fixture 'marker-mismatch'
  [ordered]@{ format = 3; pre_v3_snapshot = 'D:\elsewhere.snapshot' } | ConvertTo-Json |
    Set-Content -Encoding utf8 -LiteralPath (Join-Path $f.Runtime 'runtime-format.json')
  Assert-Refused "a snapshot the runtime's marker does not name" $f $f.Runtime $f.Snapshot "marker names"

  $f = New-Fixture 'equal'
  Assert-Refused 'the same folder twice' $f $f.Runtime $f.Runtime 'overlap'

  $f = New-Fixture 'nested'
  $inner = Join-Path $f.Runtime 'inner-snapshot'
  New-Item -ItemType Directory -Path $inner | Out-Null
  Assert-Refused 'a snapshot inside the runtime' $f $f.Runtime $inner 'overlap'

  $f = New-Fixture 'aside-exists'
  $now = [DateTime]::UtcNow
  foreach ($second in 0..20) {
    New-Item -ItemType Directory -Path ("$($f.Runtime).rolled-back-" + $now.AddSeconds($second).ToString('yyyyMMddTHHmmssZ')) | Out-Null
  }
  Assert-Refused 'an existing aside destination' $f $f.Runtime $f.Snapshot 'already exists'

  $f = New-Fixture 'verbatim-receipt'
  Set-Receipt $f { param($r) $r.runtime = '\\?\' + $r.runtime; $r.snapshot = '\\?\' + $r.snapshot }
  $result = & $script -Runtime $f.Runtime -Snapshot $f.Snapshot | ConvertFrom-Json
  $restored = Get-Content -Raw -LiteralPath (Join-Path $f.Runtime 'rooms\r.jsonl')
  $kept = Get-Content -Raw -LiteralPath (Join-Path $result.v3_runtime_kept_at 'rooms\r.jsonl')
  if ($result.outcome -eq 'rolled_back' -and $restored -eq 'v2 history' -and $kept -eq 'v3 history' -and
      (Test-Path -LiteralPath (Join-Path $f.Snapshot 'SNAPSHOT.json'))) { 'ok   the valid snapshot is restored, the v3 runtime kept aside' }
  else { "FAIL valid rollback: $($result | ConvertTo-Json -Compress) restored=$restored kept=$kept"; $failures++ }
} finally {
  Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}
if ($failures) { "FAILED $failures"; exit 1 } else { 'PASSED' }
