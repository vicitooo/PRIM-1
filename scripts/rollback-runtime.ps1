# v3 W7 (Claude PRIM-1): roll a runtime back to its pre-upgrade snapshot in one step. Run only with PRIM-1
# fully stopped. The current (v3) runtime is moved aside, never deleted: it keeps the v3-era room history
# for a later re-upgrade. The snapshot is copied back and itself kept. Then start the previous PRIM-1 build.
# v3 W7 (Reviewer d597f0d1): before anything is moved, the snapshot must be the one taken of THIS runtime
# (its SNAPSHOT.json names both folders, and the runtime's own marker names it when present), still whole
# (its files and bytes are the recorded ones), the two folders must not overlap, and no destination
# folder may exist already.
param(
  [Parameter(Mandatory)][string]$Runtime,
  [Parameter(Mandatory)][string]$Snapshot
)
$ErrorActionPreference = 'Stop'

function Get-TreeSize([string]$Path) {
  $files = @(Get-ChildItem -LiteralPath $Path -Recurse -Force -File -ErrorAction Stop | Where-Object { $_.Name -ne 'SNAPSHOT.json' })
  [pscustomobject]@{ Files = $files.Count; Bytes = [long](($files | Measure-Object -Property Length -Sum).Sum) }
}

# A receipt written by v3 may carry the verbatim \\?\ form of the same path.
function Get-FullPath([string]$Path) {
  if ($Path.StartsWith('\\?\') -and -not $Path.StartsWith('\\?\UNC\')) { $Path = $Path.Substring(4) }
  [IO.Path]::GetFullPath($Path).TrimEnd('\')
}

function Test-SamePath([string]$A, [string]$B) { [string]::Equals((Get-FullPath $A), (Get-FullPath $B), 'OrdinalIgnoreCase') }

function Test-Within([string]$Inner, [string]$Outer) {
  (Get-FullPath $Inner).StartsWith((Get-FullPath $Outer) + '\', [StringComparison]::OrdinalIgnoreCase)
}

$Runtime = (Resolve-Path -LiteralPath $Runtime).Path.TrimEnd('\')
$Snapshot = (Resolve-Path -LiteralPath $Snapshot).Path.TrimEnd('\')
if ((Test-SamePath $Runtime $Snapshot) -or (Test-Within $Runtime $Snapshot) -or (Test-Within $Snapshot $Runtime)) {
  throw "The runtime $Runtime and the snapshot $Snapshot overlap; nothing was moved."
}
$receiptPath = Join-Path $Snapshot 'SNAPSHOT.json'
if (-not (Test-Path -LiteralPath $receiptPath)) { throw "$Snapshot is not a verified runtime snapshot (no SNAPSHOT.json)." }
try { $receipt = Get-Content -Raw -LiteralPath $receiptPath | ConvertFrom-Json }
catch { throw "The snapshot receipt $receiptPath cannot be read; nothing was moved." }
if ($receipt.outcome -ne 'snapshot_taken' -or -not $receipt.runtime -or -not $receipt.snapshot `
    -or $null -eq $receipt.files -or $null -eq $receipt.bytes) {
  throw "The snapshot receipt $receiptPath is not a complete snapshot record; nothing was moved."
}
if (-not (Test-SamePath ([string]$receipt.runtime) $Runtime)) {
  throw "The snapshot $Snapshot was taken of $($receipt.runtime), not of $Runtime; nothing was moved."
}
if (-not (Test-SamePath ([string]$receipt.snapshot) $Snapshot)) {
  throw "The snapshot receipt names $($receipt.snapshot), not $Snapshot; nothing was moved."
}
$expected = [pscustomobject]@{ Files = [long]$receipt.files; Bytes = [long]$receipt.bytes }
$present = Get-TreeSize $Snapshot
if ($present.Files -ne $expected.Files -or $present.Bytes -ne $expected.Bytes) {
  throw ("The snapshot is no longer what was recorded ({0} files / {1} bytes of {2} / {3}); nothing was moved." -f
    $present.Files, $present.Bytes, $expected.Files, $expected.Bytes)
}
# v3 W4 (Claude PRIM-1 8ed4d106, Codex PRIM-1 34795463): a session file that names the Codex App Server
# transport was written by v3, so this is not the pre-v3 snapshot. Stop before anything moves. Changing the
# field would not make a v3 runtime v2-compatible; only the validated pre-v3 snapshot is restored.
$catalog = Join-Path $Snapshot 'session-catalog-v1.json'
if ((Test-Path -LiteralPath $catalog) -and ((Get-Content -Raw -LiteralPath $catalog) -match '"codex_transport"\s*:')) {
  throw ("The snapshot's session file has a member on the Codex App Server transport, so it was written by v3 and is not " +
    'the pre-v3 snapshot; nothing was moved. Roll back to the validated pre-v3 snapshot (<runtime>.snapshot-pre-v3-<UTC>). ' +
    'If there is none, stop here and keep the v3 runtime.')
}
# The runtime's own marker (written by v3 after its snapshot) names the snapshot it took.
$marker = Join-Path $Runtime 'runtime-format.json'
if (Test-Path -LiteralPath $marker) {
  try { $format = Get-Content -Raw -LiteralPath $marker | ConvertFrom-Json }
  catch { throw "The runtime marker $marker cannot be read; nothing was moved." }
  if ($format.pre_v3_snapshot -and -not (Test-SamePath ([string]$format.pre_v3_snapshot) $Snapshot)) {
    throw "This runtime's marker names the snapshot $($format.pre_v3_snapshot), not $Snapshot; nothing was moved."
  }
}
# A running PRIM-1 holds its instance lock open; refuse while anything owns the runtime.
$lock = Join-Path $Runtime 'desktop-instance.lock'
if (Test-Path -LiteralPath $lock) {
  try { $handle = [IO.File]::Open($lock, 'Open', 'ReadWrite', 'None'); $handle.Dispose() }
  catch { throw 'PRIM-1 still owns this runtime (its instance lock is held). Stop PRIM-1 fully, then roll back.' }
}
$stamp = [DateTime]::UtcNow.ToString('yyyyMMddTHHmmssZ')
$aside = "$Runtime.rolled-back-$stamp"
$failed = "$Runtime.failed-restore-$stamp"
foreach ($destination in @($aside, $failed)) {
  if (Test-Path -LiteralPath $destination) { throw "$destination already exists; nothing was moved. Try again in a second." }
}
Move-Item -LiteralPath $Runtime -Destination $aside
try {
  New-Item -ItemType Directory -Path $Runtime | Out-Null
  & robocopy.exe $Snapshot $Runtime /E /COPY:DAT /DCOPY:DAT /XF SNAPSHOT.json /R:2 /W:1 /NFL /NDL /NJH /NJS /NP | Out-Null
  if ($LASTEXITCODE -ge 8) { throw "Restoring the snapshot failed (robocopy exit $LASTEXITCODE)." }
  $restored = Get-TreeSize $Runtime
  if ($restored.Files -ne $expected.Files -or $restored.Bytes -ne $expected.Bytes) {
    throw ("The restored runtime does not match the snapshot ({0} / {1} of {2} / {3})." -f $restored.Files, $restored.Bytes, $expected.Files, $expected.Bytes)
  }
} catch {
  # Put the v3 runtime back exactly as it was; the failed restore is kept aside for inspection.
  if (Test-Path -LiteralPath $Runtime) { Move-Item -LiteralPath $Runtime -Destination $failed }
  Move-Item -LiteralPath $aside -Destination $Runtime
  throw
}
[ordered]@{ outcome = 'rolled_back'; runtime = $Runtime; restored_from = $Snapshot; v3_runtime_kept_at = $aside
  files = $restored.Files; bytes = $restored.Bytes; next = 'start the previous PRIM-1 build on this runtime' } | ConvertTo-Json -Compress
