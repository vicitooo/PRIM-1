# Stage/start an isolated pinned build; -Check reads only, -PrepareOnly never starts.
[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)][string]$SourceExe,
  [Parameter(Mandatory = $true)][ValidatePattern('^[a-fA-F0-9]{64}$')][string]$Sha256,
  [Parameter(Mandatory = $true)][string]$Root,
  [string]$WorkingRoot = 'D:\All Repos\victor',
  [int]$DebugCdpPort = 0,
  [switch]$Check,
  [switch]$PrepareOnly,
  [switch]$CreateShortcut
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
function Assert-Path([string]$Path) {
  if ($Path -notmatch '^[A-Za-z]:\\' -or $Path.Substring(2).Contains(':') -or
      $Path -match "['`r`n]" -or $Path -match '(^|\\)\.\.(\\|$)') { throw "Unsafe absolute path: $Path" }
  $full = [IO.Path]::GetFullPath($Path).TrimEnd('\')
  $cursor = $full
  while ($cursor) {
    $item = Get-Item -Force -LiteralPath $cursor -ErrorAction SilentlyContinue
    if ($item) {
      if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
        throw "Reparse path refused: $cursor"
      }
    }
    $cursor = [IO.Path]::GetDirectoryName($cursor)
  }
  return $full
}
function Test-Overlap([string]$A, [string]$B) {
  $aPrefix = $A.TrimEnd('\') + '\'; $bPrefix = $B.TrimEnd('\') + '\'
  return $aPrefix.StartsWith($bPrefix, [StringComparison]::OrdinalIgnoreCase) -or
    $bPrefix.StartsWith($aPrefix, [StringComparison]::OrdinalIgnoreCase)
}
function Assert-Hash([string]$Path) {
  if (-not (Test-Path -LiteralPath $Path -PathType Leaf) -or
      (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash -ne $Sha256) { throw "Pinned executable hash mismatch: $Path" }
}
function Assert-PersistentEnvironment([System.Collections.IDictionary]$Values, [string]$Scope) {
  foreach ($name in $Values.Keys) {
    if ([string]$name -match '^(PRIM1_|WEBVIEW2_)') { throw "Persistent $Scope override refused: $name" }
  }
}
$Root = Assert-Path $Root
$isQa = $Root -match '^D:\\prim1-qa-20261019-[a-z0-9][a-z0-9-]*$'
# MAINPC's clean main root (Victor, room 3606, 21 Oct 2026): a shipping root
# like the Experimental one, whose shortcut is the desktop's only PRIM-1.
$isMain = $Root -ieq 'D:\prim1-v2-20261021'
if ($Root -ine 'D:\prim1-v2-experimental-20261019' -and -not $isMain -and -not $isQa) {
  throw 'Root must be the dedicated Experimental root, the MAINPC main root or a dated QA root.'
}
if ($DebugCdpPort -ne 0 -and (-not $isQa -or $DebugCdpPort -lt 1024 -or $DebugCdpPort -gt 65535 -or $DebugCdpPort -eq 9222)) {
  throw 'CDP is QA-only: choose a loopback port in 1024..65535 other than 9222.'
}
$SourceExe = Assert-Path $SourceExe
if ([IO.Path]::GetExtension($SourceExe) -ine '.exe') { throw 'SourceExe must be an executable artifact.' }
$WorkingRoot = Assert-Path $WorkingRoot
if (-not (Test-Path -LiteralPath $WorkingRoot -PathType Container)) { throw 'WorkingRoot must already exist.' }
$local = [Environment]::GetFolderPath('LocalApplicationData')
foreach ($protected in @('Programs\PRIM-1', 'io.prim1.runtime', 'io.prim1.v2')) {
  if (Test-Overlap $Root (Join-Path $local $protected)) { throw "Live path overlap: $protected" }
}
# Scheduler uses its user environment, not the calling PRIM pane's environment.
# dotenv preserves existing values: refuse persistent overrides rather than edit them.
foreach ($scope in @('User', 'Machine')) {
  Assert-PersistentEnvironment ([Environment]::GetEnvironmentVariables($scope)) $scope
}
$bin = Assert-Path (Join-Path $Root 'bin')
$runtime = Assert-Path (Join-Path $Root 'runtime')
$profile = Assert-Path (Join-Path $Root 'profile')
foreach ($isolated in @($runtime, $bin, $profile)) {
  if (Test-Overlap $isolated $WorkingRoot) { throw 'Agent working root must be disjoint from runtime, bin and profile.' }
}
$staged = Assert-Path (Join-Path $bin 'prim1-experimental.exe')
$envFile = Assert-Path (Join-Path $bin '.env')
foreach ($directory in @($Root, $bin, $runtime, $profile)) {
  if ((Test-Path -LiteralPath $directory) -and -not (Test-Path -LiteralPath $directory -PathType Container)) { throw "Not a directory: $directory" }
}
Assert-Hash $SourceExe
if (Test-Path -LiteralPath $staged) { Assert-Hash $staged }
$envLines = @("PRIM1_RUNTIME_DIR='$($runtime.Replace('\', '/'))'",
  "PRIM1_AGENT_WORKING_ROOT='$($WorkingRoot.Replace('\', '/'))'", "PRIM1_START_MINIMIZED='0'",
  "WEBVIEW2_USER_DATA_FOLDER='$($profile.Replace('\', '/'))'")
if ($DebugCdpPort) { $envLines += "PRIM1_DEBUG_CDP_PORT='$DebugCdpPort'" }
$envText = ($envLines -join "`n") + "`n"
if ((Test-Path -LiteralPath $envFile) -and [IO.File]::ReadAllText($envFile) -cne $envText) { throw 'Existing .env differs; use another dedicated root. Nothing is overwritten.' }
$taskName = 'PRIM1-' + (Split-Path -Leaf $Root)
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$description = "PRIM1 isolated launcher v1; root=$Root; sha256=$($Sha256.ToUpperInvariant())"
$tasks = @(Get-ScheduledTask -TaskPath '\' | Where-Object { $_.TaskName -eq $taskName })
if ($tasks.Count -gt 1) { throw 'Ambiguous scheduled task.' }
$task = if ($tasks.Count) { $tasks[0] } else { $null }
if ($task) {
  $actions = @($task.Actions)
  # CIM may abbreviate UserId to a local name; exported XML retains the SID.
  [xml]$taskXml = Export-ScheduledTask -TaskName $taskName -TaskPath '\'
  $principals = @($taskXml.Task.Principals.Principal)
  if ($task.Description -cne $description -or $actions.Count -ne 1 -or
      $actions[0].Execute -ine $staged -or $actions[0].WorkingDirectory -ine $bin -or $actions[0].Arguments -or
      $principals.Count -ne 1 -or [string]$principals[0].UserId -cne $identity.User.Value -or
      [string]$task.Principal.LogonType -ne 'Interactive' -or [string]$task.Principal.RunLevel -ne 'Limited' -or
      @($task.Triggers | Where-Object { $null -ne $_ }).Count -ne 0 -or [string]$task.Settings.MultipleInstances -ne 'IgnoreNew' -or
      $task.Settings.ExecutionTimeLimit -ne 'PT0S' -or [string]$task.State -eq 'Disabled') { throw 'Foreign or changed scheduled task refused.' }
  if (-not (Test-Path -LiteralPath $staged) -or -not (Test-Path -LiteralPath $envFile)) { throw 'Existing task has incomplete staging; inspect it manually.' }
}
$processes = @(Get-CimInstance Win32_Process -Filter "Name='prim1-experimental.exe'")
if (@($processes | Where-Object { -not $_.ExecutablePath }).Count) { throw 'Cannot identify an existing Experimental process.' }
$running = @($processes | Where-Object { $_.ExecutablePath -ieq $staged })
if ($running.Count -and -not $task) { throw 'Staged executable is running without the matching owned task.' }
if ($DebugCdpPort -and -not $running.Count -and @(Get-NetTCPConnection -State Listen -LocalPort $DebugCdpPort -ErrorAction SilentlyContinue).Count) { throw 'QA CDP port is already listening.' }
$shortcutPath = $null
if ($CreateShortcut) {
  $label = if ($isQa) { "PRIM-1 QA ($((Split-Path -Leaf $Root)))" } elseif ($isMain) { 'PRIM-1' } else { 'PRIM-1 Experimental' }
  $shortcutPath = Assert-Path (Join-Path ([Environment]::GetFolderPath('DesktopDirectory')) ($label + '.lnk'))
  if (Test-Path -LiteralPath $shortcutPath) { throw 'Shortcut already exists; omit -CreateShortcut. No existing shortcut is changed.' }
  $vbsFile = Assert-Path (Join-Path $bin 'launch.vbs')
  $runCommand = '"' + (Join-Path ([Environment]::GetFolderPath('System')) 'schtasks.exe') + '" /Run /TN "' + $taskName + '"'
  $vbsText = 'CreateObject("WScript.Shell").Run "' + $runCommand.Replace('"', '""') + '", 0, False' + "`r`n"
  if ((Test-Path -LiteralPath $vbsFile) -and [IO.File]::ReadAllText($vbsFile) -cne $vbsText) { throw 'Existing shortcut helper differs; nothing is overwritten.' }
}
if ($Check) {
  [pscustomobject]@{ Mode = 'Read-only check'; Root = $Root; Task = $taskName; TaskExists = [bool]$task; RunningPids = @($running | ForEach-Object { $_.ProcessId }); Sha256 = $Sha256; CdpPort = $DebugCdpPort }
  return
}
foreach ($directory in @($Root, $bin, $runtime, $profile)) {
  $null = Assert-Path $directory
  $null = [IO.Directory]::CreateDirectory($directory)
}
if (-not (Test-Path -LiteralPath $staged)) { [IO.File]::Copy($SourceExe, $staged, $false) }
Assert-Hash $staged
if (-not (Test-Path -LiteralPath $envFile)) {
  $bytes = [Text.UTF8Encoding]::new($false).GetBytes($envText)
  $stream = [IO.File]::Open($envFile, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
  try { $stream.Write($bytes, 0, $bytes.Length); $stream.Flush($true) } finally { $stream.Dispose() }
}
if ([IO.File]::ReadAllText($envFile) -cne $envText) { throw 'Staged environment changed; refusing launch.' }
if (-not $task) {
  $action = New-ScheduledTaskAction -Execute $staged -WorkingDirectory $bin
  $principal = New-ScheduledTaskPrincipal -UserId $identity.Name -LogonType Interactive -RunLevel Limited
  $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew -Priority 4
  $task = Register-ScheduledTask -TaskName $taskName -TaskPath '\' -Action $action -Principal $principal -Settings $settings -Description $description
}
if ($CreateShortcut) {
  if (-not (Test-Path -LiteralPath $vbsFile)) {
    $bytes = [Text.UTF8Encoding]::new($false).GetBytes($vbsText)
    $stream = [IO.File]::Open($vbsFile, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try { $stream.Write($bytes, 0, $bytes.Length); $stream.Flush($true) } finally { $stream.Dispose() }
  }
  if ([IO.File]::ReadAllText($vbsFile) -cne $vbsText) { throw 'Shortcut helper changed; refusing shortcut creation.' }
  $shortcut = (New-Object -ComObject WScript.Shell).CreateShortcut($shortcutPath)
  $shortcut.TargetPath = Join-Path ([Environment]::GetFolderPath('System')) 'wscript.exe'
  $shortcut.Arguments = '//B //NoLogo "' + $vbsFile + '"'
  $shortcut.IconLocation = $staged + ',0'
  $shortcut.Description = if ($isMain) { 'Start PRIM-1 through its launcher task.' } else {
    'Start the separately staged PRIM-1 Experimental task; leaves the installed PRIM-1 alone.' }
  $shortcut.Save()
}
if ($PrepareOnly) {
  Write-Output "Prepared: $taskName. No start requested; use the shortcut or repeat without -PrepareOnly to start."
  return
}
if ($running.Count -or [string]$task.State -eq 'Running') { Write-Output "Already running: $taskName (no second start requested)." }
else { Start-ScheduledTask -TaskName $taskName; Write-Output "Start requested: $taskName. Verify the visible app and its runtime before using copied sessions." }
