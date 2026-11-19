# One bounded Experimental replacement. Never stops a process or restores state.
# -Register only schedules this same staged script; default mode is the outside worker.
[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)][string]$RequestPath,
  [switch]$Register
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Assert-PlainPath([string]$Path, [bool]$Directory = $false) {
  if ($Path.StartsWith('\\?\', [StringComparison]::Ordinal)) {
    if ($Path -notmatch '^\\\\\?\\[A-Za-z]:\\' -or $Path.Contains('/')) { throw 'Only an extended local drive path is supported.' }
    $Path = $Path.Substring(4)
  }
  if ($Path -notmatch '^[A-Za-z]:[\\/]' -or $Path -match '["''\x00-\x1f]' -or
      $Path.Substring(2).Contains(':') -or $Path -match '(^|[\\/])\.\.?([\\/]|$)') { throw 'Expected an unambiguous absolute local path.' }
  if (($Path.Substring(3) -split '[\\/]') | Where-Object { $_.Length -eq 0 -or $_ -match '[. ]$' }) { throw 'Ambiguous path component refused.' }
  $full = [IO.Path]::GetFullPath($Path)
  if ($full.Length -gt [IO.Path]::GetPathRoot($full).Length) { $full = $full.TrimEnd('\') }
  $cursor = $full
  while ($cursor) {
    $item = Get-Item -LiteralPath $cursor -Force -ErrorAction Stop
    if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Reparse paths are not supported for an upgrade.' }
    $cursor = [IO.Path]::GetDirectoryName($cursor)
  }
  if ($Directory -and -not [IO.Directory]::Exists($full)) { throw 'Required upgrade directory is absent.' }
  return $full
}
function Same-Path([string]$A, [string]$B) {
  if ($A -match '^\\\\\?\\[A-Za-z]:\\') { $A = $A.Substring(4) }
  if ($B -match '^\\\\\?\\[A-Za-z]:\\') { $B = $B.Substring(4) }
  return [string]::Equals([IO.Path]::GetFullPath($A).TrimEnd('\'), [IO.Path]::GetFullPath($B).TrimEnd('\'), [StringComparison]::OrdinalIgnoreCase)
}
function Read-BoundedJson([string]$Path, [int]$Maximum = 1048576) {
  $null = Assert-PlainPath $Path
  $file = [IO.File]::Open($Path, 'Open', 'Read', [IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete)
  try {
    if ($file.Length -gt $Maximum) { throw 'Upgrade JSON exceeds its byte limit.' }
    $bytes = New-Object byte[] ($Maximum + 1)
    $count = 0
    while ($count -lt $bytes.Length) {
      $read = $file.Read($bytes, $count, $bytes.Length - $count)
      if ($read -eq 0) { break }
      $count += $read
    }
    if ($count -gt $Maximum) { throw 'Upgrade JSON grew beyond its byte limit.' }
    $text = [Text.UTF8Encoding]::new($false, $true).GetString($bytes, 0, $count)
    $hash = [Security.Cryptography.SHA256]::Create()
    try { $digest = ([BitConverter]::ToString($hash.ComputeHash($bytes, 0, $count))).Replace('-', '').ToLowerInvariant() }
    finally { $hash.Dispose() }
    return @{ Value = ($text | ConvertFrom-Json); Hash = $digest }
  } finally { $file.Dispose() }
}
function Write-ProductJson([string]$Name, $Value, [switch]$Replace) {
  $path = Join-Path $script:attemptDir $Name
  $temporary = $path + '.tmp'
  $bytes = [Text.UTF8Encoding]::new($false).GetBytes(($Value | ConvertTo-Json -Depth 8 -Compress) + "`n")
  $file = [IO.File]::Open($temporary, 'CreateNew', 'Write', 'None')
  try { $file.Write($bytes, 0, $bytes.Length); $file.Flush($true) } finally { $file.Dispose() }
  if ($Replace -and [IO.File]::Exists($path)) { [IO.File]::Replace($temporary, $path, [NullString]::Value) }
  else { [IO.File]::Move($temporary, $path) }
}
function Envelope {
  return [ordered]@{ schema_version = 1; attempt_id = $script:request.attempt_id; request_sha256 = $script:requestHash }
}
function Write-Result([string]$Outcome, [string]$Message, $Candidate = $null) {
  $value = Envelope
  $value.outcome = $Outcome; $value.candidate_process = $Candidate
  $value.at_utc = [DateTimeOffset]::UtcNow.ToString('o'); $value.message = $Message
  Write-ProductJson 'helper-result.json' $value -Replace
}
function Read-State {
  # ReplaceFileW may briefly deny or hide the name while the desktop commits
  # its next phase. Retry only those transient filesystem failures, not an
  # invalid JSON document or mismatched request.
  $retry = [Diagnostics.Stopwatch]::StartNew()
  while ($true) {
    try {
      $state = (Read-BoundedJson (Join-Path $script:attemptDir 'state.json') 16384).Value
      break
    } catch {
      $errorCode = $_.Exception.GetBaseException().HResult -band 0xffff
      $missing = $_.Exception -is [Management.Automation.ItemNotFoundException]
      if (($errorCode -notin @(2, 5, 32) -and -not $missing) -or $retry.Elapsed.TotalSeconds -ge 2) { throw }
      Start-Sleep -Milliseconds 25
    }
  }
  if ($state.schema_version -ne 1 -or $state.attempt_id -cne $script:request.attempt_id -or
      $state.request_sha256 -cne $script:requestHash -or
      $state.phase -cnotin @('prepared', 'closing', 'shutdown_complete', 'cancelled', 'resuming', 'consumed', 'abandoned')) { throw 'Upgrade state does not match this immutable request.' }
  return $state.phase
}
function Check-DeadlineAndCancellation {
  if ([DateTimeOffset]::UtcNow -ge $script:expires) { return 'expired' }
  if ((Read-State) -in @('cancelled', 'abandoned')) { return 'cancelled' }
  if ([DateTimeOffset]::UtcNow -ge $script:expires) { return 'expired' }
  return $null
}
function Assert-TaskSelection {
  $catalog = (Read-BoundedJson (Join-Path $script:runtime 'session-catalog-v1.json') 16777216).Value
  $workspace = Assert-PlainPath ([string]$catalog.workspace_preference.canonical_path) $true
  $settingPath = Join-Path $script:runtime 'task-folder.json'
  $settingExists = [IO.File]::Exists($settingPath)
  if ($null -ne $script:taskSettingExisted -and $script:taskSettingExisted -ne $settingExists) { throw 'Task-folder setting presence changed during upgrade.' }
  $script:taskSettingExisted = $settingExists
  if ($settingExists) {
    $setting = (Read-BoundedJson $settingPath 65536).Value
    if ($setting.schema_version -ne 1) { throw 'Unsupported task-folder setting.' }
    $selected = [string]$setting.folder
  } else {
    # The desktop captured and rechecked its effective selection. Default
    # policy belongs there (personal-repo tasks), not in this process helper.
    $selected = $script:taskFolder
  }
  $null = Assert-PlainPath $selected $true
  if (-not (Same-Path $selected $script:taskFolder)) { throw 'Task-folder selection changed; upgrade remains held.' }
  return $workspace
}
function Ownership-Released {
  foreach ($entry in @(@{ Path = (Join-Path $script:runtime 'desktop-instance.lock'); Missing = $false },
                       @{ Path = (Join-Path $script:taskFolder '.prim1-tasks.lock'); Missing = $true },
                       @{ Path = (Join-Path $script:profile 'EBWebView\lockfile'); Missing = $true })) {
    try {
      # OPEN_EXISTING: never manufactures or deletes an ownership file.
      if ([IO.File]::Exists($entry.Path)) { $null = Assert-PlainPath $entry.Path }
      $probe = [IO.File]::Open($entry.Path, 'Open', 'Read', 'None')
      $probe.Dispose()
    } catch [IO.FileNotFoundException] {
      if (-not $entry.Missing) { throw 'The runtime ownership file is absent.' }
    } catch [IO.DirectoryNotFoundException] {
      if (-not $entry.Missing) { throw 'The runtime ownership directory is absent.' }
    } catch [IO.IOException] {
      $code = $_.Exception.GetBaseException().HResult -band 0xffff
      if ($code -in @(32, 33)) { return $false }
      throw
    }
  }
  return $true
}
function Initialize-ProcessIdentity {
  if ('Prim1Upgrade.ProcessIdentity' -as [type]) { return }
  Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;
namespace Prim1Upgrade {
 public sealed class ProcessIdentity : IDisposable {
  [StructLayout(LayoutKind.Sequential)] struct FT { public uint Low, High; }
  [DllImport("kernel32.dll", SetLastError=true)] static extern SafeProcessHandle OpenProcess(uint access, bool inherit, uint pid);
  [DllImport("kernel32.dll", SetLastError=true, CharSet=CharSet.Unicode)] static extern bool QueryFullProcessImageName(SafeProcessHandle process, uint flags, StringBuilder image, ref uint length);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool GetProcessTimes(SafeProcessHandle process, out FT created, out FT exited, out FT kernel, out FT user);
  [DllImport("kernel32.dll", SetLastError=true)] static extern uint WaitForSingleObject(SafeProcessHandle process, uint milliseconds);
  readonly SafeProcessHandle handle;
  public uint Pid { get; private set; }
  public string Image { get; private set; }
  public ulong Created { get; private set; }
  ProcessIdentity(SafeProcessHandle value) { handle = value; }
  public static ProcessIdentity Open(uint pid) {
   var handle = OpenProcess(0x1000 | 0x100000, false, pid);
   if (handle.IsInvalid) { int error=Marshal.GetLastWin32Error(); handle.Dispose(); throw new Win32Exception(error, "open exact source process"); }
   return FromHandle(handle,pid);
  }
  static ProcessIdentity FromHandle(SafeProcessHandle handle, uint pid) {
   try {
    var result = new ProcessIdentity(handle); result.Pid=pid;
    var image = new StringBuilder(32768); uint length=(uint)image.Capacity;
    if (!QueryFullProcessImageName(handle,0,image,ref length)) throw new Win32Exception(Marshal.GetLastWin32Error(),"query process image");
    FT created, exited, kernel, user;
    if (!GetProcessTimes(handle,out created,out exited,out kernel,out user)) throw new Win32Exception(Marshal.GetLastWin32Error(),"query process creation");
    result.Image=Path.GetFullPath(image.ToString()); result.Created=((ulong)created.High << 32) | created.Low;
    return result;
   } catch { handle.Dispose(); throw; }
  }
  public bool HasExited() {
   uint result=WaitForSingleObject(handle,0);
   if(result==0) return true;
   if(result==258) return false;
   throw new Win32Exception(Marshal.GetLastWin32Error(),"wait for exact source process");
  }
  public void Dispose() { handle.Dispose(); }
 }
}
'@
}
function Register-OutsideHelper {
  # Task Scheduler, not Start-Process from a pane, supplies Job separation.
  $powershell = Join-Path ([Environment]::GetFolderPath('System')) 'WindowsPowerShell\v1.0\powershell.exe'
  $scriptPath = Assert-PlainPath $PSCommandPath
  if (-not (Same-Path $scriptPath (Join-Path $script:attemptDir 'continue-upgrade.ps1'))) { throw 'Register requires the helper staged inside this attempt.' }
  if ([IO.File]::Exists((Join-Path $script:attemptDir 'helper-claim.json'))) { throw 'This attempt already had a helper; it will not be restarted.' }
  $taskName = 'PRIM1-Upgrade-' + $script:request.attempt_id
  $arguments = '-NoProfile -NonInteractive -ExecutionPolicy Bypass -WindowStyle Hidden -File "' + $scriptPath + '" -RequestPath "' + $script:requestPath + '"'
  $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
  $description = 'PRIM1 upgrade v1; request=' + $script:requestHash + '; helper=' + (Get-FileHash -LiteralPath $scriptPath -Algorithm SHA256).Hash.ToLowerInvariant()
  $existing = @(Get-ScheduledTask -TaskPath '\' | Where-Object { $_.TaskName -ceq $taskName })
  if ($existing.Count -gt 1) { throw 'Ambiguous upgrade task.' }
  if ($existing.Count) {
    $task = $existing[0]
    [xml]$xml = Export-ScheduledTask -TaskName $taskName -TaskPath '\'
    $actions = @($task.Actions); $principals = @($xml.Task.Principals.Principal)
    if ($task.Description -cne $description -or $actions.Count -ne 1 -or
        -not (Same-Path $actions[0].Execute $powershell) -or $actions[0].Arguments -cne $arguments -or
        -not (Same-Path $actions[0].WorkingDirectory $script:attemptDir) -or $principals.Count -ne 1 -or
        [string]$principals[0].UserId -cne $identity.User.Value -or [string]$task.Principal.LogonType -ne 'Interactive' -or
        [string]$task.Principal.RunLevel -ne 'Limited' -or [string]$task.Settings.MultipleInstances -ne 'IgnoreNew' -or
        $task.Settings.RestartCount -ne 0 -or $task.Settings.DisallowStartIfOnBatteries -or $task.Settings.StopIfGoingOnBatteries -or
        -not $task.Settings.Hidden -or [Xml.XmlConvert]::ToTimeSpan($task.Settings.ExecutionTimeLimit) -ne [TimeSpan]::FromMinutes(16) -or
        @($task.Triggers | Where-Object { $null -ne $_ }).Count -ne 0 -or
        [string]$task.State -eq 'Disabled') { throw 'Foreign or changed upgrade task refused.' }
  } else {
    $action = New-ScheduledTaskAction -Execute $powershell -Argument $arguments -WorkingDirectory $script:attemptDir
    $principal = New-ScheduledTaskPrincipal -UserId $identity.User.Value -LogonType Interactive -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -Hidden -MultipleInstances IgnoreNew -ExecutionTimeLimit ([TimeSpan]::FromMinutes(16)) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
    $task = Register-ScheduledTask -TaskName $taskName -TaskPath '\' -Action $action -Principal $principal -Settings $settings -Description $description
  }
  Start-ScheduledTask -TaskName $taskName -TaskPath '\'
  Write-Output 'Outside helper requested; readiness and shutdown are separate.'
}
function Register-Candidate([string]$Image, [string]$Workspace, [switch]$ValidateOnly) {
  # A separate direct-executable task has no helper Job or execution deadline.
  # Scheduler supplies the user environment; reject overrides as the existing
  # Experimental launcher does, instead of inheriting the caller's pane secrets.
  foreach ($scope in @('User', 'Machine')) {
    foreach ($key in [Environment]::GetEnvironmentVariables($scope).Keys) {
      if ([string]$key -match '^(PRIM1_|WEBVIEW2_)') { throw "Persistent $scope PRIM1/WEBVIEW2 environment override prevents a deterministic upgrade." }
    }
  }
  $directory = [IO.Path]::GetDirectoryName($Image)
  if (-not (Same-Path $directory (Join-Path $script:attemptDir 'candidate'))) { throw 'Candidate must be staged in this attempt private candidate directory.' }
  $values = [ordered]@{
    PRIM1_RUNTIME_DIR = $script:runtime; WEBVIEW2_USER_DATA_FOLDER = $script:profile
    PRIM1_AGENT_WORKING_ROOT = $Workspace; PRIM1_UPGRADE_ATTEMPT_ID = $script:request.attempt_id
    PRIM1_UPGRADE_REQUEST_FILE = $script:requestPath
  }
  if ($script:request.PSObject.Properties.Name -contains 'personal_repo_override' -and
      $null -ne $script:request.personal_repo_override) {
    $override = [string]$script:request.personal_repo_override
    if ($override -cne 'off') { $override = Assert-PlainPath $override $true }
    $values.PRIM1_PERSONAL_REPO = $override
  }
  $envText = (($values.GetEnumerator() | ForEach-Object { $_.Key + "='" + $_.Value.Replace('\','/') + "'" }) -join "`n") + "`n"
  $envPath = Join-Path $directory '.env'
  $envBytes = [Text.UTF8Encoding]::new($false).GetBytes($envText)
  if ([IO.File]::Exists($envPath)) {
    $null = Assert-PlainPath $envPath
    if ((Get-Item -LiteralPath $envPath).Length -gt 65536 -or [IO.File]::ReadAllText($envPath) -cne $envText) { throw 'Candidate environment differs from this request.' }
  } elseif ($ValidateOnly) { throw 'Prepared candidate environment is absent.' }
  else {
    $envFile = [IO.File]::Open($envPath, 'CreateNew', 'Write', 'None')
    try { $envFile.Write($envBytes, 0, $envBytes.Length); $envFile.Flush($true) } finally { $envFile.Dispose() }
  }
  $taskName = 'PRIM1-Upgrade-' + $script:request.attempt_id + '-Candidate'
  $description = 'PRIM1 upgrade candidate v1; request=' + $script:requestHash + '; sha256=' + $script:request.candidate.exe_sha256
  $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
  $existing = @(Get-ScheduledTask -TaskPath '\' | Where-Object { $_.TaskName -ceq $taskName })
  if ($existing.Count -gt 1) { throw 'Ambiguous candidate task.' }
  if ($existing.Count) {
    $task = $existing[0]; [xml]$xml = Export-ScheduledTask -TaskName $taskName -TaskPath '\'
    $actions = @($task.Actions); $principals = @($xml.Task.Principals.Principal)
    if ($task.Description -cne $description -or $actions.Count -ne 1 -or -not (Same-Path $actions[0].Execute $Image) -or
        $actions[0].Arguments -or -not (Same-Path $actions[0].WorkingDirectory $directory) -or $principals.Count -ne 1 -or
        [string]$principals[0].UserId -cne $identity.User.Value -or [string]$task.Principal.LogonType -ne 'Interactive' -or
        [string]$task.Principal.RunLevel -ne 'Limited' -or [string]$task.Settings.MultipleInstances -ne 'IgnoreNew' -or
        $task.Settings.RestartCount -ne 0 -or $task.Settings.DisallowStartIfOnBatteries -or $task.Settings.StopIfGoingOnBatteries -or
        $task.Settings.Priority -ne 4 -or
        [Xml.XmlConvert]::ToTimeSpan($task.Settings.ExecutionTimeLimit) -ne [TimeSpan]::Zero -or
        @($task.Triggers | Where-Object { $null -ne $_ }).Count -ne 0 -or [string]$task.State -eq 'Disabled') { throw 'Foreign or changed candidate task refused.' }
  } elseif ($ValidateOnly) { throw 'Prepared candidate task is absent.' }
  else {
    $action = New-ScheduledTaskAction -Execute $Image -WorkingDirectory $directory
    $principal = New-ScheduledTaskPrincipal -UserId $identity.User.Value -LogonType Interactive -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -Priority 4 -MultipleInstances IgnoreNew -ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
    $null = Register-ScheduledTask -TaskName $taskName -TaskPath '\' -Action $action -Principal $principal -Settings $settings -Description $description
  }
  return $taskName
}
function Find-Candidate([string]$Image, [uint64]$NotBefore) {
  $name = [IO.Path]::GetFileName($Image)
  $found = New-Object 'System.Collections.Generic.List[object]'
  try {
    foreach ($row in @(Get-CimInstance Win32_Process -Filter ("Name='" + $name + "'"))) {
      if (-not $row.ExecutablePath) { throw 'Cannot identify an existing candidate-image process.' }
      if (-not (Same-Path $row.ExecutablePath $Image)) { continue }
      $bound = [Prim1Upgrade.ProcessIdentity]::Open([uint32]$row.ProcessId)
      if (-not (Same-Path $bound.Image $Image) -or $bound.Created -lt $NotBefore) { $bound.Dispose(); throw 'Candidate discovery did not match the staged launch identity.' }
      if ($bound.HasExited()) { $bound.Dispose(); continue }
      $found.Add($bound)
    }
    if ($found.Count -gt 1) { throw 'More than one process uses the exact candidate image; launch outcome is uncertain.' }
    if ($found.Count -eq 1) { return $found[0] }
    return $null
  } catch { foreach ($bound in $found) { $bound.Dispose() }; throw }
}

$claimed = $false; $source = $null; $candidateFile = $null; $launchAttempted = $false
$script:taskSettingExisted = $null
try {
  $script:requestPath = Assert-PlainPath $RequestPath
  $read = Read-BoundedJson $script:requestPath 2097152
  $script:request = $read.Value; $script:requestHash = $read.Hash
  if ($request.schema_version -ne 1 -or $request.attempt_id -cne ([Guid]$request.attempt_id).ToString() -or
      [Guid]$request.attempt_id -eq [Guid]::Empty) { throw 'Unsupported upgrade request.' }
  $script:runtime = Assert-PlainPath $request.runtime_dir $true
  $script:profile = Assert-PlainPath $request.profile_dir $true
  $script:taskFolder = Assert-PlainPath $request.task_folder $true
  $script:attemptDir = Assert-PlainPath (Join-Path $runtime ('maintenance\upgrades\' + $request.attempt_id)) $true
  if (-not (Same-Path $requestPath (Join-Path $attemptDir 'request.json'))) { throw 'Request is outside its exact runtime attempt.' }
  $sourceImage = Assert-PlainPath $request.source.exe_path
  $candidateImage = Assert-PlainPath $request.candidate.exe_path
  if ($request.source.exe_sha256 -cnotmatch '^[a-f0-9]{64}$' -or $request.candidate.exe_sha256 -cnotmatch '^[a-f0-9]{64}$' -or
      $request.source.creation_filetime -notmatch '^[0-9]{1,20}$' -or [uint32]$request.source.pid -eq 0) { throw 'Invalid pinned process identity.' }
  $script:expires = [DateTimeOffset]::Parse($request.expires_at)
  $notice = [DateTimeOffset]::Parse($request.notice_until); $prepared = [DateTimeOffset]::Parse($request.prepared_at)
  if ($expires.Offset -ne [TimeSpan]::Zero -or $notice.Offset -ne [TimeSpan]::Zero -or $prepared.Offset -ne [TimeSpan]::Zero -or
      $expires -le $notice -or ($notice - $prepared).TotalSeconds -lt 60 -or ($expires - $prepared).TotalMinutes -gt 15 -or
      $prepared -gt [DateTimeOffset]::UtcNow.AddSeconds(5)) { throw 'Invalid bounded UTC upgrade deadline.' }
  $cancel = Check-DeadlineAndCancellation
  if ($cancel) { throw "Upgrade is $cancel; no helper admitted." }
  if ($Register) { Register-OutsideHelper; return }

  $claim = Envelope; $claim.at_utc = [DateTimeOffset]::UtcNow.ToString('o')
  Write-ProductJson 'helper-claim.json' $claim
  $claimed = $true
  Initialize-ProcessIdentity
  $source = [Prim1Upgrade.ProcessIdentity]::Open([uint32]$request.source.pid)
  if ($source.Created -ne [uint64]$request.source.creation_filetime -or -not (Same-Path $source.Image $sourceImage) -or $source.HasExited()) { throw 'Source process identity changed or exited before helper readiness.' }
  if ((Get-FileHash -LiteralPath $sourceImage -Algorithm SHA256).Hash.ToLowerInvariant() -cne $request.source.exe_sha256) { throw 'Source executable hash changed.' }
  # Keep the candidate immutable through the candidate task start (read sharing only).
  $candidateFile = [IO.File]::Open($candidateImage, 'Open', 'Read', 'Read')
  if ($candidateFile.Length -lt 2 -or $candidateFile.Length -gt 536870912) { throw 'Candidate executable exceeds its byte limit.' }
  $sha = [Security.Cryptography.SHA256]::Create()
  try { $candidateHash = ([BitConverter]::ToString($sha.ComputeHash($candidateFile))).Replace('-', '').ToLowerInvariant() } finally { $sha.Dispose() }
  if ($candidateHash -cne $request.candidate.exe_sha256) { throw 'Candidate executable hash mismatch.' }
  $workspace = Assert-TaskSelection
  $taskName = Register-Candidate $candidateImage $workspace
  $cancel = Check-DeadlineAndCancellation
  if ($cancel) { Write-Result $cancel 'Candidate was not started.'; return }
  if ($source.HasExited()) { throw 'Source exited before helper readiness; no upgrade shutdown was admitted.' }
  $self = [Prim1Upgrade.ProcessIdentity]::Open([uint32]$PID)
  try {
    $ready = Envelope; $ready.helper_pid = [uint32]$PID; $ready.helper_creation_filetime = $self.Created.ToString()
    $ready.at_utc = [DateTimeOffset]::UtcNow.ToString('o')
    Write-ProductJson 'helper-ready.json' $ready
  } finally { $self.Dispose() }

  while ($true) {
    $cancel = Check-DeadlineAndCancellation
    if ($cancel) { Write-Result $cancel 'Candidate was not started.'; return }
    if ((Read-State) -eq 'shutdown_complete' -and [DateTimeOffset]::UtcNow -ge $notice -and $source.HasExited() -and (Ownership-Released)) { break }
    Start-Sleep -Milliseconds 200
  }
  $workspace = Assert-TaskSelection
  if ((Read-BoundedJson $requestPath 2097152).Hash -cne $requestHash) { throw 'Immutable upgrade request changed.' }
  $cancel = Check-DeadlineAndCancellation
  if ($cancel) { Write-Result $cancel 'Candidate was not started.'; return }
  $existingCandidate = Find-Candidate $candidateImage 0
  if ($existingCandidate) { $existingCandidate.Dispose(); throw 'The candidate image is already running; no second launch attempted.' }
  $taskName = Register-Candidate $candidateImage $workspace -ValidateOnly
  $cancel = Check-DeadlineAndCancellation
  if ($cancel) { Write-Result $cancel 'Candidate was not started.'; return }
  Write-Result 'launching' 'One candidate task start is about to be requested; this record alone does not prove a process started.'
  $cancel = Check-DeadlineAndCancellation
  if ($cancel) { Write-Result $cancel 'Candidate was not started.'; return }
  $launchAttempted = $true
  $notBefore = [uint64][DateTime]::UtcNow.ToFileTimeUtc()
  Start-ScheduledTask -TaskName $taskName -TaskPath '\'
  $candidateIdentity = $null
  $candidateWatch = [Diagnostics.Stopwatch]::StartNew()
  while ($candidateWatch.Elapsed.TotalSeconds -lt 60) {
    $candidateIdentity = Find-Candidate $candidateImage $notBefore
    if ($candidateIdentity) { break }
    Start-Sleep -Milliseconds 100
  }
  if (-not $candidateIdentity) { throw 'Candidate task was requested but no exact live process was observed within 60 seconds; no second start attempted.' }
  try {
    if (-not (Same-Path $candidateIdentity.Image $candidateImage)) { throw 'Candidate process image differs from the pinned executable.' }
    $process = [ordered]@{ pid = $candidateIdentity.Pid; creation_filetime = $candidateIdentity.Created.ToString(); exe_path = $request.candidate.exe_path; exe_sha256 = $candidateHash }
    Write-Result 'launched' 'Process created; application readiness, conversation resume and continuation are not asserted.' $process
  } finally { $candidateIdentity.Dispose() }
} catch {
  if ($claimed) {
    try { Write-Result $(if ($launchAttempted) { 'uncertain' } else { 'failed' }) $_.Exception.Message } catch { }
  }
  Write-Error $_
  exit 1
} finally {
  if ($candidateFile) { $candidateFile.Dispose() }
  if ($source) { $source.Dispose() }
}
