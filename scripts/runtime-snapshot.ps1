# v3 W7 (Claude PRIM-1): the pre-upgrade runtime snapshot that makes a rollback possible. A v2 build
# cannot read room history a v3 build wrote, so a rollback restores this copy instead of reusing the
# runtime. Same disk as the runtime (a sibling folder), size checked before anything is copied, never
# deleted by PRIM-1. Run only while no PRIM-1 owns the runtime (the upgrade helper calls it after the
# old process has shut down and released its ownership files).
#   -Check   : only verify free space for a copy; writes nothing.
#   default  : copy <runtime> to <runtime>.snapshot-<Label>, then verify file count and bytes.
param(
  [Parameter(Mandatory)][string]$Runtime,
  [Parameter(Mandatory)][string]$Label,
  [switch]$Check,
  [long]$MarginBytes = 512MB
)
$ErrorActionPreference = 'Stop'

function Get-TreeSize([string]$Path) {
  $files = @(Get-ChildItem -LiteralPath $Path -Recurse -Force -File -ErrorAction Stop)
  [pscustomobject]@{ Files = $files.Count; Bytes = [long](($files | Measure-Object -Property Length -Sum).Sum) }
}

$Runtime = (Resolve-Path -LiteralPath $Runtime).Path.TrimEnd('\')
if ($Label -cnotmatch '^[A-Za-z0-9._-]{1,80}$') { throw 'Label must be 1-80 letters, digits, dot, dash or underscore.' }
$target = "$Runtime.snapshot-$Label"
$size = Get-TreeSize $Runtime
$drive = [IO.DriveInfo]::new([IO.Path]::GetPathRoot($Runtime))
$needed = $size.Bytes + $MarginBytes
if ($drive.AvailableFreeSpace -lt $needed) {
  throw ("Not enough free space on {0} for the pre-upgrade runtime snapshot: {1:N0} MB free, {2:N0} MB needed. Free some space, then try again." -f
    $drive.Name, ($drive.AvailableFreeSpace / 1MB), ($needed / 1MB))
}
if ($Check) {
  [pscustomobject]@{ outcome = 'space_ok'; runtime = $Runtime; files = $size.Files; bytes = $size.Bytes; target = $target } | ConvertTo-Json -Compress
  return
}
if (Test-Path -LiteralPath $target) { throw "The snapshot $target already exists; it is never overwritten." }
New-Item -ItemType Directory -Path $target | Out-Null
# /E all folders, /COPY:DAT data, attributes, times; no purge, no move: the runtime is left as it is.
& robocopy.exe $Runtime $target /E /COPY:DAT /DCOPY:DAT /R:2 /W:1 /NFL /NDL /NJH /NJS /NP | Out-Null
if ($LASTEXITCODE -ge 8) { throw "Copying the runtime failed (robocopy exit $LASTEXITCODE); the snapshot at $target is incomplete and must not be used." }
$copied = Get-TreeSize $target
if ($copied.Files -ne $size.Files -or $copied.Bytes -ne $size.Bytes) {
  throw ("The snapshot does not match the runtime ({0} files / {1} bytes copied of {2} / {3}); it must not be used." -f
    $copied.Files, $copied.Bytes, $size.Files, $size.Bytes)
}
$receipt = [ordered]@{ outcome = 'snapshot_taken'; runtime = $Runtime; snapshot = $target; files = $copied.Files; bytes = $copied.Bytes
  at_utc = [DateTimeOffset]::UtcNow.ToString('o') }
$receipt | ConvertTo-Json | Set-Content -Encoding utf8 -LiteralPath (Join-Path $target 'SNAPSHOT.json')
$receipt | ConvertTo-Json -Compress
