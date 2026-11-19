<#
.SYNOPSIS
  Wakes an idle outside Codex session when its PRIM-1 room has new messages.

.DESCRIPTION
  The PRIM-1 port of the Local Agent Room's subscribe-codex.mjs. It long-polls
  the room through this PRIM-1's guest CLI (`--prim1-guest wait`) and, for a
  message from anyone but this guest (matched by its guest id, not its name),
  queues one prompt into the given Codex thread with `codex queue`. Wakes
  within 30 s are coalesced; a failed queue backs off (1 min, doubling to
  5 min); with no activity a recovery wake still runs every 15 minutes. Every
  child call is time-limited, so the watcher always stops after -Hours.
  A queue receipt is not proof the agent acted: test with a real message.

  Needs PRIM1_GUEST_TOKEN (and PRIM1_RUNTIME_DIR unless the default runtime)
  from the room's invitation. It only reads the room; it never posts.

.EXAMPLE
  $env:PRIM1_GUEST_TOKEN = 'prim1g_…'; $env:PRIM1_RUNTIME_DIR = 'D:\…\runtime'
  .\guest-codex-watch.ps1 -Exe 'D:\…\prim1-experimental.exe' `
    -Thread <this Codex session's id> -Codex "$env:USERPROFILE\…\codex.exe"
#>
param(
  [Parameter(Mandatory = $true)][string]$Exe,
  [Parameter(Mandatory = $true)][string]$Thread,
  [Parameter(Mandatory = $true)][string]$Codex,
  [string]$Remote = 'unix://',
  [double]$Hours = 2,
  [double]$FallbackMinutes = 15,
  [int]$CoalesceSeconds = 30
)
$ErrorActionPreference = 'Stop'
if (-not $env:PRIM1_GUEST_TOKEN) { throw 'Set PRIM1_GUEST_TOKEN from the room invitation first.' }
foreach ($check in @(@('Hours', $Hours, 12), @('FallbackMinutes', $FallbackMinutes, 1440), @('CoalesceSeconds', $CoalesceSeconds, 3600))) {
  if ([double]::IsNaN($check[1]) -or [double]::IsInfinity($check[1]) -or $check[1] -le 0 -or $check[1] -gt $check[2]) {
    throw "-$($check[0]) must be above 0 and at most $($check[2])."
  }
}
foreach ($path in $Exe, $Codex) { if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Not found: $path" } }
$deadline = (Get-Date).AddHours($Hours)

# Runs one child process with a hard time limit; never outlives the deadline.
function Invoke-Bounded([string]$File, [string[]]$Arguments, [int]$LimitMs) {
  $limit = [int][Math]::Min($LimitMs, ($deadline - (Get-Date)).TotalMilliseconds)
  if ($limit -le 0) { return [pscustomobject]@{ code = -1; text = 'deadline reached' } }
  $info = New-Object System.Diagnostics.ProcessStartInfo
  $info.FileName = $File
  $info.Arguments = ($Arguments | ForEach-Object { '"' + ($_ -replace '(\\*)"', '$1$1\"' -replace '(\\+)$', '$1$1') + '"' }) -join ' '
  $info.UseShellExecute = $false
  $info.RedirectStandardOutput = $true
  $info.RedirectStandardError = $true
  $info.CreateNoWindow = $true
  $process = [System.Diagnostics.Process]::Start($info)
  try {
    $stdout = $process.StandardOutput.ReadToEndAsync()
    $stderr = $process.StandardError.ReadToEndAsync()
    $started = Get-Date
    if (-not $process.WaitForExit($limit)) {
      try { $process.Kill() } catch {}
      return [pscustomobject]@{ code = -1; text = "timed out after $limit ms" }
    }
    # A descendant can keep the output pipes open after the process exits:
    # reading them is bounded by the same limit, and unread output is a failure.
    $left = [int][Math]::Max(0, $limit - ((Get-Date) - $started).TotalMilliseconds)
    if (-not [System.Threading.Tasks.Task]::WaitAll(@($stdout, $stderr), $left)) {
      return [pscustomobject]@{ code = -1; text = "output still open after $limit ms" }
    }
    [pscustomobject]@{ code = $process.ExitCode; text = ($stdout.Result + $stderr.Result).Trim() }
  } finally {
    $process.Dispose()
  }
}

function Invoke-Guest([string[]]$GuestArgs, [int]$LimitMs) {
  $r = Invoke-Bounded $Exe (@('--prim1-guest') + $GuestArgs) $LimitMs
  try { return $r.text | ConvertFrom-Json } catch { return [pscustomobject]@{ ok = $false; message = $r.text } }
}

function Send-Wake([string]$Reason, $Cursor) {
  if ((Get-Date) -ge $deadline) { return $false }
  $message = "PRIM-1 room: $Reason. Read the room from your last cursor with your room_read tool " +
    "(or: & '$Exe' --prim1-guest read --epoch $($Cursor.epoch) --sequence <your last sequence>), follow has_more, " +
    "and answer what the room asks you. This wake authorizes no new scope; avoid replies to acknowledgements. " +
    "If the person stopped this session, stop the watcher."
  $r = Invoke-Bounded $Codex @('queue', '--remote', $Remote, '--thread', $Thread, '--message', $message) 15000
  if ($r.code -eq 0) { Write-Host "CODEX-QUEUED $Reason $($r.text). Queue acceptance is not proof the agent acted."; return $true }
  Write-Host "CODEX-QUEUE-ERROR $Reason $($r.text)"
  return $false
}

$start = Invoke-Guest @('rooms') 30000
if (-not $start.ok) { throw "Room not reachable: $($start.message)" }
$selfId = $start.payload.you.guest_id
$cursor = $start.payload.rooms[0].cursor
$lastWake = Get-Date
$notBefore = [datetime]::MinValue
$backoffSeconds = 60
$relevant = $false
$lastError = ''
Write-Host "GUEST-WATCH started as guest $selfId from sequence $($cursor.sequence); stops $deadline"

while ((Get-Date) -lt $deadline) {
  $now = Get-Date
  $fallbackDue = $lastWake.AddMinutes($FallbackMinutes)
  $waitMs = [Math]::Min(25000, [Math]::Min(($deadline - $now).TotalMilliseconds, ($fallbackDue - $now).TotalMilliseconds))
  if ($relevant) { $waitMs = [Math]::Min($waitMs, ($notBefore - $now).TotalMilliseconds) }
  $waitMs = [int][Math]::Max(1000, $waitMs)
  $r = Invoke-Guest @('wait', '--epoch', "$($cursor.epoch)", '--sequence', "$($cursor.sequence)", '--timeout-ms', "$waitMs") ($waitMs + 30000)
  $now = Get-Date
  if (-not $r.ok) {
    if ($r.message -ne $lastError) { Write-Host "GUEST-WATCH-ERROR $($r.message)"; $lastError = $r.message }
    if ($now -ge $lastWake.AddMinutes($FallbackMinutes) -and $now -ge $notBefore) {
      [void](Send-Wake "room unavailable ($($r.message))" $cursor)
      $lastWake = $now
    }
    Start-Sleep -Seconds ([Math]::Max(1, [Math]::Min(5, ($deadline - $now).TotalSeconds)))
    continue
  }
  $lastError = ''
  foreach ($event in @($r.payload.page.events)) {
    $item = $event.item
    if ($item.kind -ne 'message') { continue }
    if ($item.sender.kind -eq 'guest' -and $item.sender.guest_id -eq $selfId) { continue }
    $relevant = $true
  }
  if ($r.payload.page.cursor) { $cursor = $r.payload.page.cursor }
  if ($relevant -and $now -ge $notBefore) {
    $lastWake = $now
    if (Send-Wake 'new room activity' $cursor) {
      $relevant = $false; $backoffSeconds = 60; $notBefore = $now.AddSeconds($CoalesceSeconds)
    } else {
      $notBefore = $now.AddSeconds($backoffSeconds); $backoffSeconds = [Math]::Min(300, $backoffSeconds * 2)
    }
  } elseif (-not $relevant -and $now -ge $lastWake.AddMinutes($FallbackMinutes)) {
    [void](Send-Wake "$FallbackMinutes-minute recovery check" $cursor)
    $lastWake = $now
  }
}
Write-Host "GUEST-WATCH stopped at its $Hours-hour limit; restart it to keep this session subscribed."
