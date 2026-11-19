<#
.SYNOPSIS
Run the PRIM-1 v2 preview beside the live app, outside the live app's job.

.DESCRIPTION
A process started from inside a live PRIM-1 pane joins that app's
kill-on-close Windows job. This script therefore never starts the v2
executable itself. "start" registers a per-user Task Scheduler task and asks
the Schedule service to run it, so the service creates the v2 process. The
live app's job breakaway is never used.

  start       Stage the built exe as <ScratchRoot>\bin\prim1-v2.exe, write the
              .env it loads from its own folder, register or update the task
              "PRIM1-v2-preview", and run it now.
  stop        Force-stop only verified staged-image processes through retained
              native handles. Close the preview window yourself for a graceful exit.
  status      Read-only. PID, start time and parent chain of every v2 process,
              and whether the main process was created by the Schedule
              service rather than by the live app or one of its panes.
  screenshot  PrintWindow capture of the v2 main window to -Out. A minimized
              window is restored without activation for the capture, then
              minimized again.

The task runs the staged exe directly, so no wrapper sits in its parent
chain. Its environment is the user's default environment, which holds none of
the variables a live pane exports (PRIM1_*, WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS).
The staged .env sets PRIM1_RUNTIME_DIR, PRIM1_AGENT_WORKING_ROOT and
PRIM1_START_MINIMIZED. Remote debugging stays off (PLAN R9) unless
-DebugCdpPort names a loopback QA port; 9222 belongs to the live app and is
refused.

Held trials additionally require -TrialAttemptId and -TrialHealthWindowSeconds
together. ScratchRoot must end in that canonical UUID and already contain
runtime catalogs and a profile directory; WorkingRoot must be outside the
attempt. No checkpoint copying or compatibility claim is made by this script.
#>
[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [ValidateSet("start", "stop", "status", "screenshot")]
  [string]$Action,

  [string]$SourceExe = "D:\All Repos\PRIM-1-new-target\s0-isolation\release\cli-master-wrapper-desktop.exe",
  [string]$ScratchRoot = "D:\prim1-v2-scratch",
  [string]$WorkingRoot,
  [switch]$ShowWindow,
  [int]$DebugCdpPort = 0,
  [string]$TrialAttemptId,
  [string]$TrialHealthWindowSeconds,
  [string]$Out,
  [int]$SettleSeconds = 3
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$TaskName = "PRIM1-v2-preview"
$ScratchRoot = [System.IO.Path]::GetFullPath($ScratchRoot)
$BinDir = Join-Path $ScratchRoot "bin"
$StagedExe = Join-Path $BinDir "prim1-v2.exe"
$RuntimeDir = Join-Path $ScratchRoot "runtime"
if ([string]::IsNullOrWhiteSpace($WorkingRoot)) {
  $WorkingRoot = Join-Path $ScratchRoot "work"
}
$WorkingRoot = [System.IO.Path]::GetFullPath($WorkingRoot)
$LiveExe = Join-Path $env:LOCALAPPDATA "Programs\PRIM-1\cli-master-wrapper-desktop.exe"
$LiveRuntimeRoot = Join-Path $env:LOCALAPPDATA "io.prim1.runtime"
$V1ImageName = "cli-master-wrapper-desktop.exe"
$HasTrialAttempt = $PSBoundParameters.ContainsKey("TrialAttemptId")
$HasTrialWindow = $PSBoundParameters.ContainsKey("TrialHealthWindowSeconds")
if ($Action -ne "start" -and ($HasTrialAttempt -or $HasTrialWindow)) {
  throw "trial parameters are only valid with -Action start"
}

function Test-SamePath([string]$A, [string]$B) {
  if ([string]::IsNullOrWhiteSpace($A) -or [string]::IsNullOrWhiteSpace($B)) {
    return $false
  }
  return [string]::Equals(
    [System.IO.Path]::GetFullPath($A).TrimEnd("\"),
    [System.IO.Path]::GetFullPath($B).TrimEnd("\"),
    [System.StringComparison]::OrdinalIgnoreCase)
}

function Test-PathInside([string]$Path, [string]$Root) {
  $full = [System.IO.Path]::GetFullPath($Path).TrimEnd("\") + "\"
  $rootFull = [System.IO.Path]::GetFullPath($Root).TrimEnd("\") + "\"
  return $full.StartsWith($rootFull, [System.StringComparison]::OrdinalIgnoreCase)
}

function Assert-Isolated {
  if (Test-SamePath $StagedExe $LiveExe) {
    throw "refusing: the staged v2 exe would be the live app's exe ($LiveExe)"
  }
  foreach ($path in @($ScratchRoot, $RuntimeDir, $WorkingRoot)) {
    if ((Test-PathInside $path $LiveRuntimeRoot) -or (Test-PathInside $path (Split-Path -Parent $LiveExe))) {
      throw "refusing: $path is inside the live app's runtime or install directory"
    }
    if ($path.Contains("'")) {
      throw "refusing: $path contains a single quote, which the staged .env cannot carry"
    }
  }
  if ((Test-PathInside $RuntimeDir $WorkingRoot) -or (Test-PathInside $WorkingRoot $RuntimeDir)) {
    throw "refusing: the runtime directory and the agent working root must be disjoint"
  }
}

function New-TrialEnvironmentLines(
  [bool]$HasAttempt, [bool]$HasWindow, [string]$AttemptId, [string]$HealthWindowSeconds,
  [string]$Root, [string]$AgentWorkingRoot,
  [scriptblock]$PathExists = { param($Path, $Kind) Test-Path -LiteralPath $Path -PathType $Kind }
) {
  if (-not $HasAttempt -and -not $HasWindow) { return }
  if (-not $HasAttempt -or -not $HasWindow) {
    throw "held trial requires both -TrialAttemptId and -TrialHealthWindowSeconds"
  }
  $attempt = [guid]::Empty
  if (-not [guid]::TryParseExact($AttemptId, "D", [ref]$attempt) -or
      $attempt -eq [guid]::Empty -or $attempt.ToString("D") -cne $AttemptId) {
    throw "TrialAttemptId must be a canonical non-nil UUID"
  }
  $seconds = 0
  if ($HealthWindowSeconds -notmatch '^[0-9]+$' -or
      -not [int]::TryParse($HealthWindowSeconds, [ref]$seconds) -or $seconds -lt 1 -or $seconds -gt 300) {
    throw "TrialHealthWindowSeconds must be an integer in 1..300"
  }
  if ([System.IO.Path]::GetFileName($Root.TrimEnd('\', '/')) -cne $AttemptId) {
    throw "held trial ScratchRoot must end in its exact TrialAttemptId"
  }
  if ((Test-PathInside $Root $AgentWorkingRoot) -or (Test-PathInside $AgentWorkingRoot $Root)) {
    throw "held trial requires an explicit WorkingRoot outside the attempt directory"
  }
  foreach ($required in @(
    @{ Path = (Join-Path $Root "runtime"); Kind = "Container" },
    @{ Path = (Join-Path $Root "profile"); Kind = "Container" },
    @{ Path = (Join-Path $Root "runtime\session-catalog-v1.json"); Kind = "Leaf" },
    @{ Path = (Join-Path $Root "runtime\room-catalog-v1.json"); Kind = "Leaf" }
  )) {
    if (-not (& $PathExists $required.Path $required.Kind)) {
      throw "held trial requires existing staged $($required.Path)"
    }
  }
  # Literal values are written before task registration/start, never patched
  # into an already-running Normal desktop.
  "PRIM1_TRIAL_ATTEMPT_ID='$AttemptId'"
  "PRIM1_TRIAL_ROOT='$($Root.Replace('\', '/'))'"
  "PRIM1_TRIAL_HEALTH_WINDOW_SECONDS='$seconds'"
}

# Matched by the exact staged image path, never by name: the live app may run
# an exe of any name, and a v1 build has the same name as a fresh v2 build.
function Get-V2Processes {
  $found = @(Get-CimInstance Win32_Process -Filter "Name = 'prim1-v2.exe'" |
      Where-Object { $_.ExecutablePath -and (Test-SamePath $_.ExecutablePath $StagedExe) })
  foreach ($process in $found) {
    if (Test-SamePath $process.ExecutablePath $LiveExe) {
      throw "refusing: PID $($process.ProcessId) is the live app's exe"
    }
  }
  return $found
}

function Get-V2Role($Process) {
  $commandLine = [string]$Process.CommandLine
  if ($commandLine -match "--prim1-pane-mcp") { return "pane-mcp" }
  if ($commandLine -match "--prim1-room") { return "room-cli" }
  return "main"
}

function Get-ParentChain($Process) {
  $chain = @()
  $child = $Process
  for ($depth = 0; $depth -lt 8; $depth++) {
    $parentId = [uint32]$child.ParentProcessId
    if ($parentId -eq 0) { break }
    $parent = Get-CimInstance Win32_Process -Filter "ProcessId = $parentId"
    if (-not $parent) {
      $chain += [pscustomobject]@{ Id = $parentId; Name = "(exited)"; Process = $null }
      break
    }
    if ($parent.CreationDate -gt $child.CreationDate) {
      # The PID was reused after the real parent exited.
      $chain += [pscustomobject]@{ Id = $parentId; Name = "(exited; PID reused)"; Process = $null }
      break
    }
    $chain += [pscustomobject]@{ Id = $parentId; Name = $parent.Name; Process = $parent }
    $child = $parent
  }
  return $chain
}

function Test-InAnyJob([uint32]$ProcessId) {
  # Returns $true / $false from kernel32 IsProcessInJob(handle, NULL), or $null
  # when the process cannot be opened for a limited query.
  if (-not ("Prim1V2.JobQuery" -as [type])) {
    Add-Type -Namespace Prim1V2 -Name JobQuery -MemberDefinition @'
[DllImport("kernel32.dll", SetLastError = true)]
public static extern System.IntPtr OpenProcess(uint access, bool inherit, uint pid);
[DllImport("kernel32.dll", SetLastError = true)]
public static extern bool IsProcessInJob(System.IntPtr process, System.IntPtr job, out bool result);
[DllImport("kernel32.dll", SetLastError = true)]
public static extern bool CloseHandle(System.IntPtr handle);
'@
  }
  $PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
  $handle = [Prim1V2.JobQuery]::OpenProcess($PROCESS_QUERY_LIMITED_INFORMATION, $false, $ProcessId)
  if ($handle -eq [System.IntPtr]::Zero) { return $null }
  try {
    $inJob = $false
    if (-not [Prim1V2.JobQuery]::IsProcessInJob($handle, [System.IntPtr]::Zero, [ref]$inJob)) { return $null }
    return $inJob
  } finally {
    [void][Prim1V2.JobQuery]::CloseHandle($handle)
  }
}

function Show-Status {
  $task = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
  if ($task) {
    $info = Get-ScheduledTaskInfo -TaskName $TaskName
    Write-Output ("task {0}: {1}; last run {2:yyyy-MM-dd HH:mm:ss}; last result 0x{3:X}" -f
      $TaskName, $task.State, $info.LastRunTime, $info.LastTaskResult)
  } else {
    Write-Output "task ${TaskName}: not registered"
  }

  if (Test-Path -LiteralPath $StagedExe -PathType Leaf) {
    $hash = (Get-FileHash -LiteralPath $StagedExe -Algorithm SHA256).Hash
    $size = (Get-Item -LiteralPath $StagedExe).Length
    Write-Output "staged exe: $StagedExe ($size bytes, sha256 $hash)"
  } else {
    Write-Output "staged exe: $StagedExe (absent)"
  }

  $scheduleService = Get-CimInstance Win32_Service -Filter "Name = 'Schedule'"
  $v2 = @(Get-V2Processes)
  if ($v2.Count -eq 0) {
    Write-Output "v2: not running"
  }
  foreach ($process in $v2) {
    $role = Get-V2Role $process
    $priority = (Get-Process -Id $process.ProcessId -ErrorAction SilentlyContinue).PriorityClass
    Write-Output ("v2 {0}: pid {1}, started {2:yyyy-MM-dd HH:mm:ss.fff}, priority {3}" -f
      $role, $process.ProcessId, $process.CreationDate, $priority)
    $chain = @(Get-ParentChain $process)
    $rendered = ($chain | ForEach-Object {
        $label = "$($_.Name) pid $($_.Id)"
        if ($scheduleService -and $_.Id -eq $scheduleService.ProcessId -and $_.Process) {
          $label += " [Schedule service]"
        }
        $label
      }) -join " <- "
    Write-Output "  parent chain: $rendered"
    if ($role -ne "main") { continue }

    # The parent chain is launch-origin evidence only. Job membership is
    # proven by IsProcessInJob(process, NULL): FALSE means it is in no job.
    $inAnyJob = Test-InAnyJob $process.ProcessId
    if ($inAnyJob -eq $false) {
      Write-Output "  verdict: outside every job (IsProcessInJob(pid, NULL) = FALSE)."
    } elseif ($inAnyJob -eq $true) {
      Write-Output "  verdict: NOT PROVEN outside the live job: IsProcessInJob(pid, NULL) = TRUE (it is in some job)."
    } else {
      Write-Output "  verdict: NOT PROVEN: IsProcessInJob could not be queried."
    }
  }

  $live = @(Get-CimInstance Win32_Process -Filter "Name = '$V1ImageName'" |
      Where-Object { $_.ExecutablePath -and (Test-SamePath $_.ExecutablePath $LiveExe) -and
        ([string]$_.CommandLine -notmatch "--prim1-") })
  if ($live.Count -eq 0) {
    Write-Output "live v1: not running"
  }
  foreach ($process in $live) {
    $title = (Get-Process -Id $process.ProcessId -ErrorAction SilentlyContinue).MainWindowTitle
    Write-Output ("live v1: pid {0}, started {1:yyyy-MM-dd HH:mm:ss}, window title '{2}' (read-only)" -f
      $process.ProcessId, $process.CreationDate, $title)
  }
}

function Start-V2 {
  Assert-Isolated
  $trialEnvironment = @(New-TrialEnvironmentLines $HasTrialAttempt $HasTrialWindow `
    $TrialAttemptId $TrialHealthWindowSeconds $ScratchRoot $WorkingRoot)
  if ($DebugCdpPort -ne 0 -and ($DebugCdpPort -lt 1024 -or $DebugCdpPort -gt 65535 -or $DebugCdpPort -eq 9222)) {
    throw "refusing -DebugCdpPort ${DebugCdpPort}: use a free port in 1024-65535 other than the live app's 9222"
  }
  $running = @(Get-V2Processes)
  if ($running.Count -gt 0) {
    throw "the v2 preview is already running (PID $($running[0].ProcessId)); run -Action stop first"
  }
  if (-not (Test-Path -LiteralPath $SourceExe -PathType Leaf)) {
    throw "built exe not found: $SourceExe"
  }

  New-Item -ItemType Directory -Force -Path $BinDir, $RuntimeDir, $WorkingRoot | Out-Null
  # A preview stopped just before may still hold the staged image for a moment.
  $copyDeadline = (Get-Date).AddSeconds(10)
  while ($true) {
    try {
      Copy-Item -LiteralPath $SourceExe -Destination $StagedExe -Force
      break
    } catch [System.IO.IOException] {
      if ((Get-Date) -ge $copyDeadline) { throw }
      Start-Sleep -Milliseconds 250
    }
  }
  $sourceHash = (Get-FileHash -LiteralPath $SourceExe -Algorithm SHA256).Hash
  $stagedHash = (Get-FileHash -LiteralPath $StagedExe -Algorithm SHA256).Hash
  if ($sourceHash -ne $stagedHash) {
    throw "the staged copy differs from $SourceExe"
  }

  # Single-quoted dotenv values are literal; forward slashes avoid escapes.
  $startMinimized = if ($ShowWindow) { "0" } else { "1" }
  $envLines = @(
    "# Written by scripts/v2-preview.ps1 start. The v2 exe loads it from its own folder.",
    ("PRIM1_RUNTIME_DIR='{0}'" -f $RuntimeDir.Replace("\", "/")),
    ("PRIM1_AGENT_WORKING_ROOT='{0}'" -f $WorkingRoot.Replace("\", "/")),
    ("PRIM1_START_MINIMIZED='{0}'" -f $startMinimized)
  )
  if ($DebugCdpPort -ne 0) {
    $envLines += ("PRIM1_DEBUG_CDP_PORT='{0}'" -f $DebugCdpPort)
  }
  $envLines += $trialEnvironment
  $envEncoding = if ($trialEnvironment.Count -gt 0) { [System.Text.UTF8Encoding]::new($false) } else { [System.Text.Encoding]::ASCII }
  [System.IO.File]::WriteAllText((Join-Path $BinDir ".env"), (($envLines -join "`n") + "`n"),
    $envEncoding)

  $execute = if ($StagedExe.Contains(" ")) { '"{0}"' -f $StagedExe } else { $StagedExe }
  $identity = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
  $taskAction = New-ScheduledTaskAction -Execute $execute -WorkingDirectory $BinDir
  $principal = New-ScheduledTaskPrincipal -UserId $identity -LogonType Interactive -RunLevel Limited
  # Priority 4 is the normal priority class; the Task Scheduler default (7) is
  # below normal and would skew every performance number taken on the preview.
  $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
    -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew -Priority 4
  Register-ScheduledTask -TaskName $TaskName -Action $taskAction -Principal $principal -Settings $settings `
    -Description "PRIM-1 v2 preview, started outside the live app's job. Managed by scripts/v2-preview.ps1." `
    -Force | Out-Null
  Start-ScheduledTask -TaskName $TaskName

  $deadline = (Get-Date).AddSeconds(30)
  do {
    Start-Sleep -Milliseconds 250
    $main = @(Get-V2Processes | Where-Object { (Get-V2Role $_) -eq "main" })
  } while ($main.Count -eq 0 -and (Get-Date) -lt $deadline)
  if ($main.Count -eq 0) {
    Show-Status
    throw "the v2 process did not appear within 30 s; see the task result above and $RuntimeDir\desktop-events.jsonl"
  }
  Show-Status
}

function Initialize-V2StopHandle {
  if ("Prim1V2.OwnedPreviewProcess" -as [type]) { return }
  Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;

namespace Prim1V2 {
  public sealed class OwnedPreviewProcess : IDisposable {
    [StructLayout(LayoutKind.Sequential)]
    struct FileTime { public uint Low, High; }

    [DllImport("kernel32.dll", SetLastError = true)]
    static extern SafeProcessHandle OpenProcess(uint access, bool inherit, uint pid);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern bool QueryFullProcessImageName(SafeProcessHandle process, uint flags,
      StringBuilder image, ref uint length);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool GetProcessTimes(SafeProcessHandle process, out FileTime created,
      out FileTime exited, out FileTime kernel, out FileTime user);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern uint WaitForSingleObject(SafeProcessHandle process, uint milliseconds);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool TerminateProcess(SafeProcessHandle process, uint exitCode);

    readonly SafeProcessHandle handle;
    public uint ProcessId { get; private set; }
    public string ImagePath { get; private set; }
    public ulong CreationFileTime { get; private set; }

    OwnedPreviewProcess(SafeProcessHandle retained, uint pid) {
      handle = retained;
      ProcessId = pid;
    }

    string ReadImage() {
      var image = new StringBuilder(32768);
      uint length = (uint)image.Capacity;
      if (!QueryFullProcessImageName(handle, 0, image, ref length))
        throw new Win32Exception(Marshal.GetLastWin32Error(), "query owned process image");
      return Path.GetFullPath(image.ToString());
    }

    ulong ReadCreation() {
      FileTime created, exited, kernel, user;
      if (!GetProcessTimes(handle, out created, out exited, out kernel, out user))
        throw new Win32Exception(Marshal.GetLastWin32Error(), "query owned process creation");
      return ((ulong)created.High << 32) | created.Low;
    }

    bool Wait(uint milliseconds) {
      uint result = WaitForSingleObject(handle, milliseconds);
      if (result == 0) return true;
      if (result == 258) return false;
      throw new Win32Exception(Marshal.GetLastWin32Error(), "wait for owned process");
    }

    public static OwnedPreviewProcess Open(uint pid, string expectedImage) {
      // CIM is only candidate discovery. Authority starts at this kernel handle;
      // CIM's lower-precision CreationDate is never treated as a native identity.
      var retained = OpenProcess(0x1000 | 0x100000 | 0x0001, false, pid);
      if (retained.IsInvalid) {
        int error = Marshal.GetLastWin32Error();
        retained.Dispose();
        if (error == 87) return null; // Candidate exited before binding.
        throw new Win32Exception(error, "open candidate preview process");
      }
      try {
        var owned = new OwnedPreviewProcess(retained, pid);
        if (owned.Wait(0)) { owned.Dispose(); return null; }
        owned.ImagePath = owned.ReadImage();
        owned.CreationFileTime = owned.ReadCreation();
        if (!String.Equals(owned.ImagePath, Path.GetFullPath(expectedImage),
                           StringComparison.OrdinalIgnoreCase))
          throw new InvalidOperationException("candidate image is no longer the exact staged executable");
        if (owned.Wait(0)) { owned.Dispose(); return null; }
        return owned;
      } catch {
        retained.Dispose();
        throw;
      }
    }

    public bool TerminateAndWait() {
      if (Wait(0)) return false;
      if (ReadCreation() != CreationFileTime ||
          !String.Equals(ReadImage(), ImagePath, StringComparison.OrdinalIgnoreCase))
        throw new InvalidOperationException("retained preview process identity changed");
      if (!TerminateProcess(handle, 1)) {
        int error = Marshal.GetLastWin32Error();
        if (Wait(0)) return false; // It exited between the query and termination.
        throw new Win32Exception(error, "terminate owned preview process");
      }
      if (!Wait(5000)) throw new TimeoutException("owned preview process did not signal exit within 5 seconds");
      return true;
    }

    public void Dispose() { handle.Dispose(); }
  }
}
'@
}

function Stop-V2 {
  $v2 = @(Get-V2Processes)
  if ($v2.Count -eq 0) {
    Write-Output "v2: not running"
    return
  }
  Initialize-V2StopHandle
  $owned = [System.Collections.Generic.List[object]]::new()
  $failures = [System.Collections.Generic.List[string]]::new()
  try {
    # Bind all candidates before stopping the main process: its Job can retire
    # helpers during cleanup. Later PID reuse cannot redirect any retained handle.
    foreach ($process in $v2) {
      try {
        $retained = [Prim1V2.OwnedPreviewProcess]::Open([uint32]$process.ProcessId, $StagedExe)
        if ($null -ne $retained) { $owned.Add($retained) }
      } catch {
        $failures.Add("bind candidate PID $($process.ProcessId): $($_.Exception.Message)")
      }
    }

    # CloseMainWindow cannot atomically bind an HWND lifetime to our process
    # handle. Omit that unsafe grace path; normal window close remains available.
    Write-Output "v2: stopping through verified process handles (forced; use normal window close for graceful exit)"
    foreach ($retained in $owned) {
      try {
        $forced = $retained.TerminateAndWait()
        Write-Output "v2 PID $($retained.ProcessId): exit confirmed; forced=$forced; creation FILETIME=$($retained.CreationFileTime)"
      } catch {
        $failures.Add("stop owned PID $($retained.ProcessId): $($_.Exception.Message)")
      }
    }

    # Status is observation only. Newly discovered candidates never inherit the
    # earlier handle's authority, and a failed query cannot report success.
    try {
      $left = @(Get-V2Processes)
      if ($left.Count -gt 0) {
        $failures.Add("v2 processes still running: $(($left | ForEach-Object { $_.ProcessId }) -join ', ')")
      }
    } catch {
      $failures.Add("final owned-image query: $($_.Exception.Message)")
    }
    if ($failures.Count -gt 0) { throw ($failures -join "; ") }
    Write-Output "v2: stopped"
  } finally {
    foreach ($retained in $owned) { $retained.Dispose() }
  }
}

function Save-V2Screenshot {
  if ([string]::IsNullOrWhiteSpace($Out)) {
    throw "-Out <file.png> is required for screenshot"
  }
  $main = @(Get-V2Processes | Where-Object { (Get-V2Role $_) -eq "main" })
  if ($main.Count -ne 1) {
    throw "expected one running v2 main process, found $($main.Count)"
  }
  Add-Type -ReferencedAssemblies System.Drawing -TypeDefinition @"
using System;
using System.Collections.Generic;
using System.Drawing;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;
using System.Text;

public static class Prim1V2Window {
  delegate bool EnumProc(IntPtr hwnd, IntPtr lParam);
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc callback, IntPtr lParam);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
  [DllImport("user32.dll")] static extern bool IsIconic(IntPtr hwnd);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr hwnd, StringBuilder text, int max);
  [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr hwnd, out RECT rect);
  [DllImport("user32.dll")] static extern bool ShowWindow(IntPtr hwnd, int command);
  [DllImport("user32.dll")] static extern bool PrintWindow(IntPtr hwnd, IntPtr hdc, uint flags);
  [DllImport("user32.dll")] static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
  [StructLayout(LayoutKind.Sequential)] struct RECT { public int Left, Top, Right, Bottom; }

  const int SW_SHOWNOACTIVATE = 4;
  const int SW_SHOWMINNOACTIVE = 7;
  const uint PW_RENDERFULLCONTENT = 2;

  public static string Capture(uint pid, string title, string path, int settleMs) {
    IntPtr previous = SetThreadDpiAwarenessContext(new IntPtr(-4));
    IntPtr hwnd = IntPtr.Zero;
    EnumWindows(delegate (IntPtr candidate, IntPtr unused) {
      uint owner;
      GetWindowThreadProcessId(candidate, out owner);
      if (owner != pid || !IsWindowVisible(candidate)) return true;
      var text = new StringBuilder(256);
      GetWindowText(candidate, text, text.Capacity);
      if (text.ToString() == title) { hwnd = candidate; return false; }
      return true;
    }, IntPtr.Zero);
    if (hwnd == IntPtr.Zero) throw new InvalidOperationException("no visible window titled '" + title + "' for PID " + pid);

    bool wasMinimized = IsIconic(hwnd);
    try {
      if (wasMinimized) {
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        System.Threading.Thread.Sleep(settleMs);
      }
      RECT rect;
      GetWindowRect(hwnd, out rect);
      int width = rect.Right - rect.Left, height = rect.Bottom - rect.Top;
      using (var bitmap = new Bitmap(width, height, PixelFormat.Format32bppArgb)) {
        using (var graphics = Graphics.FromImage(bitmap)) {
          IntPtr hdc = graphics.GetHdc();
          bool printed = PrintWindow(hwnd, hdc, PW_RENDERFULLCONTENT);
          graphics.ReleaseHdc(hdc);
          if (!printed) throw new InvalidOperationException("PrintWindow failed");
        }
        bitmap.Save(path, ImageFormat.Png);
        var colours = new HashSet<int>();
        for (int y = 0; y < height; y += Math.Max(1, height / 48))
          for (int x = 0; x < width; x += Math.Max(1, width / 48))
            colours.Add(bitmap.GetPixel(x, y).ToArgb());
        return "hwnd 0x" + hwnd.ToInt64().ToString("X") + ", " + width + "x" + height +
          ", sampled colours " + colours.Count + ", restored from minimized: " + wasMinimized;
      }
    } finally {
      if (wasMinimized) ShowWindow(hwnd, SW_SHOWMINNOACTIVE);
      if (previous != IntPtr.Zero) SetThreadDpiAwarenessContext(previous);
    }
  }
}
"@
  $target = [System.IO.Path]::GetFullPath($Out)
  New-Item -ItemType Directory -Force -Path (Split-Path -Parent $target) | Out-Null
  $result = [Prim1V2Window]::Capture([uint32]$main[0].ProcessId, "PRIM-1 v2", $target, $SettleSeconds * 1000)
  Write-Output "screenshot: $target ($result)"
}

switch ($Action) {
  "start" { Start-V2 }
  "stop" { Stop-V2 }
  "status" { Show-Status }
  "screenshot" { Save-V2Screenshot }
}
