param(
  [Parameter(Mandatory = $true)]
  [ValidateSet("ping", "wait_quiet", "input", "key", "room_read", "room_post", "room_deliver")]
  [string]$Action,

  [string]$Session,
  [ValidateSet("enter", "up", "down", "left", "right", "tab", "esc", "ctrl_c")]
  [string]$Key,
  [string]$Content,
  [string]$ContentFile,
  [string]$Recipient,
  [int]$QuietSec,
  [int]$TimeoutSec,
  [string]$CursorEpoch,
  [Nullable[UInt64]]$CursorSequence,
  [string]$Endpoint,
  [string]$OutRequestIdFile,
  [switch]$PassThruJson,
  [switch]$Quiet
)

$scriptRoot = Split-Path -Parent $PSCommandPath
. (Join-Path $scriptRoot "runtime-paths.ps1")

function Initialize-Prim1ControlPlaneNativeMethods {
  if ($null -ne ("Prim1.ControlPlane.NativeMethods" -as [type])) {
    return
  }

  Add-Type -Language CSharp -ErrorAction Stop -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

namespace Prim1.ControlPlane
{
    [StructLayout(LayoutKind.Sequential)]
    public struct NativeFileTime
    {
        public uint LowDateTime;
        public uint HighDateTime;
    }

    public static class NativeMethods
    {
        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool GetNamedPipeServerProcessId(
            SafePipeHandle pipe,
            out uint serverProcessId);

        [DllImport("kernel32.dll", SetLastError = true)]
        public static extern IntPtr OpenProcess(
            uint desiredAccess,
            [MarshalAs(UnmanagedType.Bool)] bool inheritHandle,
            uint processId);

        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool GetProcessTimes(
            IntPtr process,
            out NativeFileTime creationTime,
            out NativeFileTime exitTime,
            out NativeFileTime kernelTime,
            out NativeFileTime userTime);

        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool CloseHandle(IntPtr handle);
    }
}
"@
}

function Resolve-Prim1ExpectedControlPlaneServerIdentity {
  $expectedPidRaw = [Environment]::GetEnvironmentVariable("PRIM1_CONTROL_PLANE_SERVER_PID", "Process")
  if ([string]::IsNullOrEmpty($expectedPidRaw)) {
    throw "PRIM1_CONTROL_PLANE_SERVER_PID is not set; refusing an unauthenticated control-plane connection."
  }
  if ($expectedPidRaw -notmatch '^[1-9][0-9]*$') {
    throw "PRIM1_CONTROL_PLANE_SERVER_PID must be a positive unsigned decimal process ID."
  }

  [uint32]$expectedPid = 0
  if (-not [uint32]::TryParse($expectedPidRaw, [ref]$expectedPid)) {
    throw "PRIM1_CONTROL_PLANE_SERVER_PID must be a positive unsigned decimal process ID."
  }

  $expectedStartedRaw = [Environment]::GetEnvironmentVariable("PRIM1_CONTROL_PLANE_SERVER_STARTED_FILETIME", "Process")
  if ([string]::IsNullOrEmpty($expectedStartedRaw)) {
    throw "PRIM1_CONTROL_PLANE_SERVER_STARTED_FILETIME is not set; refusing an unauthenticated control-plane connection."
  }
  if ($expectedStartedRaw -notmatch '^[1-9][0-9]*$') {
    throw "PRIM1_CONTROL_PLANE_SERVER_STARTED_FILETIME must be a positive unsigned decimal FILETIME."
  }

  [uint64]$expectedStartedFiletime = 0
  if (-not [uint64]::TryParse($expectedStartedRaw, [ref]$expectedStartedFiletime)) {
    throw "PRIM1_CONTROL_PLANE_SERVER_STARTED_FILETIME must be a positive unsigned decimal FILETIME."
  }

  return [pscustomobject]@{
    ProcessId = $expectedPid
    StartedFiletime = $expectedStartedFiletime
  }
}

function Get-Prim1ProcessCreationFiletime {
  param(
    [Parameter(Mandatory = $true)]
    [uint32]$ProcessId
  )

  $processQueryLimitedInformation = [uint32]0x1000
  $processHandle = [Prim1.ControlPlane.NativeMethods]::OpenProcess(
    $processQueryLimitedInformation,
    $false,
    $ProcessId
  )
  if ($processHandle -eq [IntPtr]::Zero) {
    $errorCode = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
    throw "OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION) failed for control-plane server PID $ProcessId (Win32 $errorCode)."
  }

  try {
    $creationTime = New-Object Prim1.ControlPlane.NativeFileTime
    $exitTime = New-Object Prim1.ControlPlane.NativeFileTime
    $kernelTime = New-Object Prim1.ControlPlane.NativeFileTime
    $userTime = New-Object Prim1.ControlPlane.NativeFileTime
    $succeeded = [Prim1.ControlPlane.NativeMethods]::GetProcessTimes(
      $processHandle,
      [ref]$creationTime,
      [ref]$exitTime,
      [ref]$kernelTime,
      [ref]$userTime
    )
    if (-not $succeeded) {
      $errorCode = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
      throw "GetProcessTimes failed for control-plane server PID $ProcessId (Win32 $errorCode)."
    }

    return ([uint64]$creationTime.HighDateTime * [uint64]4294967296) + [uint64]$creationTime.LowDateTime
  } finally {
    $null = [Prim1.ControlPlane.NativeMethods]::CloseHandle($processHandle)
  }
}

function Assert-Prim1ControlPlaneServerIdentity {
  param(
    [Parameter(Mandatory = $true)]
    [System.IO.Pipes.NamedPipeClientStream]$Pipe,

    [Parameter(Mandatory = $true)]
    $ExpectedIdentity
  )

  Initialize-Prim1ControlPlaneNativeMethods

  [uint32]$connectedServerPid = 0
  $succeeded = [Prim1.ControlPlane.NativeMethods]::GetNamedPipeServerProcessId(
    $Pipe.SafePipeHandle,
    [ref]$connectedServerPid
  )
  if (-not $succeeded) {
    $errorCode = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
    throw "GetNamedPipeServerProcessId failed for the connected control-plane pipe (Win32 $errorCode)."
  }

  if ($connectedServerPid -ne [uint32]$ExpectedIdentity.ProcessId) {
    throw "Control-plane server identity mismatch: connected PID $connectedServerPid, expected PID $($ExpectedIdentity.ProcessId)."
  }

  [uint64]$connectedStartedFiletime = Get-Prim1ProcessCreationFiletime -ProcessId $connectedServerPid
  if ($connectedStartedFiletime -ne [uint64]$ExpectedIdentity.StartedFiletime) {
    throw "Control-plane server identity mismatch: connected PID $connectedServerPid has creation FILETIME $connectedStartedFiletime, expected $($ExpectedIdentity.StartedFiletime)."
  }
}

function Resolve-InputContent {
  param(
    [string]$InlineContent,
    [string]$ContentFilePath
  )

  if ($InlineContent -and $ContentFilePath) {
    throw "input accepts either -Content or -ContentFile, not both"
  }

  if (-not $ContentFilePath) {
    return $InlineContent
  }

  try {
    $resolvedContentFile = (Resolve-Path -LiteralPath $ContentFilePath -ErrorAction Stop).Path
  } catch {
    throw "failed to resolve -ContentFile '$ContentFilePath'"
  }

  try {
    $strictUtf8 = [System.Text.UTF8Encoding]::new($false, $true)
    return [System.IO.File]::ReadAllText($resolvedContentFile, $strictUtf8)
  } catch {
    throw "failed to read -ContentFile '$resolvedContentFile': $($_.Exception.Message)"
  }
}

function Write-AtomicText {
  param(
    [Parameter(Mandatory = $true)]
    [string]$Path,

    [Parameter(Mandatory = $true)]
    [string]$Content
  )

  $parent = Split-Path -Parent $Path
  if ($parent) {
    New-Item -ItemType Directory -Force -Path $parent | Out-Null
  }

  $tmpPath = "$Path.tmp"
  $utf8NoBom = [System.Text.UTF8Encoding]::new($false)
  [System.IO.File]::WriteAllText($tmpPath, $Content, $utf8NoBom)
  Move-Item -LiteralPath $tmpPath -Destination $Path -Force
}

if ($Action -in @("input", "room_post", "room_deliver")) {
  $Content = Resolve-InputContent -InlineContent $Content -ContentFilePath $ContentFile
} elseif ($ContentFile) {
  throw "-ContentFile is only supported for -Action input, room_post or room_deliver"
}

if ($Action -in @("room_read", "room_post", "room_deliver")) {
  if ($Session) { throw "$Action derives room membership from the calling pane; -Session is not accepted" }
  if ($Key) { throw "$Action does not accept -Key" }
}
if ($Action -ne "room_deliver" -and $Recipient) {
  throw "-Recipient is supported only for -Action room_deliver"
}
if ($Action -eq "room_deliver" -and [string]::IsNullOrWhiteSpace($Recipient)) {
  throw "room_deliver requires -Recipient (a member label, a session id, or all)"
}
if ($Action -eq "room_read" -and -not [string]::IsNullOrEmpty($Content)) {
  throw "room_read does not accept -Content"
}
if ($Action -ne "room_read" -and ($CursorEpoch -or $null -ne $CursorSequence)) {
  throw "cursor parameters are supported only for -Action room_read"
}

$resolvedEndpoint = Resolve-Prim1ControlPlaneEndpoint -Endpoint $Endpoint
$pipePrefix = "\\.\pipe\"
if (-not $resolvedEndpoint.StartsWith($pipePrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
  throw "Control plane endpoint must use the Windows named-pipe form '\\.\pipe\<name>'."
}

$pipeName = $resolvedEndpoint.Substring($pipePrefix.Length)
if ([string]::IsNullOrWhiteSpace($pipeName)) {
  throw "Control plane endpoint must include a named-pipe name."
}

$payload = switch ($Action) {
  "ping" {
    [ordered]@{ kind = "ping" }
  }
  "wait_quiet" {
    if (-not $Session) { throw "wait_quiet requires -Session" }
    if ($QuietSec -le 0) { throw "wait_quiet requires -QuietSec > 0" }
    if ($QuietSec -gt 60) { throw "wait_quiet requires -QuietSec <= 60" }
    if ($TimeoutSec -le 0) { throw "wait_quiet requires -TimeoutSec > 0" }
    if ($TimeoutSec -gt 300) { throw "wait_quiet requires -TimeoutSec <= 300" }
    if ($QuietSec -gt $TimeoutSec) { throw "wait_quiet requires -QuietSec <= -TimeoutSec" }
    [ordered]@{
      kind = "wait_quiet"
      name = $Session
      quiet_seconds = $QuietSec
      timeout_seconds = $TimeoutSec
    }
  }
  "input" {
    if (-not $Session) { throw "input requires -Session" }
    if ([string]::IsNullOrEmpty($Content)) { throw "input requires -Content or -ContentFile" }
    [ordered]@{
      kind = "send_input"
      name = $Session
      input = $Content
    }
  }
  "key" {
    if (-not $Session) { throw "key requires -Session" }
    if (-not $Key) { throw "key requires -Key" }
    [ordered]@{
      kind = "send_key"
      name = $Session
      key = $Key
    }
  }
  "room_read" {
    if ([string]::IsNullOrEmpty($CursorEpoch) -xor ($null -eq $CursorSequence)) {
      throw "room_read requires -CursorEpoch and -CursorSequence together"
    }
    $request = [ordered]@{ kind = "room_read" }
    if (-not [string]::IsNullOrEmpty($CursorEpoch)) {
      [guid]$parsedCursorEpoch = [guid]::Empty
      if (-not [guid]::TryParse($CursorEpoch, [ref]$parsedCursorEpoch)) {
        throw "room_read requires -CursorEpoch to be a UUID"
      }
      $request.cursor = [ordered]@{
        epoch = $parsedCursorEpoch.ToString()
        sequence = [uint64]$CursorSequence
      }
    }
    $request
  }
  "room_post" {
    if ([string]::IsNullOrEmpty($Content)) { throw "room_post requires -Content or -ContentFile" }
    [ordered]@{
      kind = "room_post"
      content = $Content
    }
  }
  "room_deliver" {
    if ([string]::IsNullOrWhiteSpace($Recipient)) { throw "room_deliver requires -Recipient (a member label, a session id, or all)" }
    if ([string]::IsNullOrEmpty($Content)) { throw "room_deliver requires -Content or -ContentFile" }
    [ordered]@{
      kind = "room_deliver"
      recipient = $Recipient
      content = $Content
    }
  }
}

$json = $payload | ConvertTo-Json -Depth 4 -Compress
$pipeReadTimeoutSec = if ($Action -eq "wait_quiet") { $TimeoutSec + 10 } else { 45 }
$response = $null
$pipe = $null

try {
  $expectedServerIdentity = Resolve-Prim1ExpectedControlPlaneServerIdentity
  $pipe = [System.IO.Pipes.NamedPipeClientStream]::new(".", $pipeName, [System.IO.Pipes.PipeDirection]::InOut)
  try {
    $pipe.Connect(5000)
    Assert-Prim1ControlPlaneServerIdentity -Pipe $pipe -ExpectedIdentity $expectedServerIdentity

    $writer = [System.IO.StreamWriter]::new($pipe)
    $writer.AutoFlush = $true
    $reader = [System.IO.StreamReader]::new($pipe)

    # The pane secret (PRIM1_PANE_SECRET) opens the connection when this shell
    # runs outside the pane's Windows Job; the supervisor still prefers the Job.
    $paneSecret = [Environment]::GetEnvironmentVariable("PRIM1_PANE_SECRET", "Process")
    if (-not [string]::IsNullOrWhiteSpace($paneSecret)) {
      $writer.WriteLine(([ordered]@{ secret = $paneSecret } | ConvertTo-Json -Compress))
    }
    $writer.WriteLine($json)
    $readTask = $reader.ReadLineAsync()
    if (-not $readTask.Wait($pipeReadTimeoutSec * 1000)) {
      throw "Control plane response timed out after $pipeReadTimeoutSec seconds."
    }
    $response = $readTask.GetAwaiter().GetResult()
  } finally {
    if ($pipe) {
      $pipe.Dispose()
    }
  }
} catch {
  throw "Control plane named-pipe request failed: $($_.Exception.Message)"
}

if (-not $response) {
  throw "No response received from control plane."
}

$parsed = $response | ConvertFrom-Json

if ($OutRequestIdFile -and $parsed.request_id) {
  Write-AtomicText -Path $OutRequestIdFile -Content ([string]$parsed.request_id)
}

if ($Quiet) {
  $jsonOutput = $null
  if ($PassThruJson) {
    $jsonOutput = $parsed | ConvertTo-Json -Depth 10 -Compress
  }

  if ($parsed.timed_out) {
    Write-Output ("TIMED OUT: " + $parsed.message)
    if ($jsonOutput) { $jsonOutput }
    exit 124
  }

  if (-not $parsed.ok) {
    Write-Output $parsed.message
    if ($jsonOutput) { $jsonOutput }
    exit 1
  }

  $parsed.message
  if ($jsonOutput) { $jsonOutput }
  exit 0
}

if ($parsed.timed_out) {
  Write-Host ("TIMED OUT: " + $parsed.message)
  if ($PassThruJson) {
    $parsed | ConvertTo-Json -Depth 10 -Compress
  }
  exit 124
}

$parsed | ConvertTo-Json -Depth 10

if (-not $parsed.ok) {
  exit 1
}
exit 0
