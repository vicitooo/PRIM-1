# Disposable console stubs only. Scheduler cmdlets are mocked; no actual tasks, PRIM-1 or harness.
[CmdletBinding()]
param([string]$Helper)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if (-not $Helper) { $Helper = Join-Path $PSScriptRoot 'continue-upgrade.ps1' }
[Diagnostics.Process]::GetCurrentProcess().PriorityClass = 'BelowNormal'
$root = Join-Path ([IO.Path]::GetTempPath()) ('prim1-upgrade-fixtures-' + [Guid]::NewGuid().ToString('N'))
$null = [IO.Directory]::CreateDirectory($root)
$powershell = Join-Path ([Environment]::GetFolderPath('System')) 'WindowsPowerShell\v1.0\powershell.exe'
$stub = Join-Path $root 'upgrade-fixture-stub.exe'
Add-Type -OutputAssembly $stub -OutputType ConsoleApplication -TypeDefinition @'
using System;
using System.IO;
using System.Threading;
class UpgradeFixtureStub {
 static void Main(string[] args) {
  if(args.Length==1) {
   DateTime deadline=DateTime.UtcNow.AddSeconds(25);
   while(!File.Exists(args[0]) && DateTime.UtcNow<deadline) Thread.Sleep(50);
   return;
  }
  string request=Environment.GetEnvironmentVariable("PRIM1_UPGRADE_REQUEST_FILE");
  string directory=Path.GetDirectoryName(request);
  string[] values={Environment.GetEnvironmentVariable("PRIM1_RUNTIME_DIR"),
   Environment.GetEnvironmentVariable("WEBVIEW2_USER_DATA_FOLDER"),
   Environment.GetEnvironmentVariable("PRIM1_AGENT_WORKING_ROOT"),
   Environment.GetEnvironmentVariable("PRIM1_UPGRADE_ATTEMPT_ID"),
   Environment.GetEnvironmentVariable("PRIM1_PANE_SECRET")==null?"secret_absent":"secret_leaked",
   Environment.GetEnvironmentVariable("PRIM1_TRIAL_ATTEMPT")==null?"trial_absent":"trial_leaked",
   Environment.GetEnvironmentVariable("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS")==null?"debug_absent":"debug_leaked",
   Environment.GetEnvironmentVariable("PRIM1_PERSONAL_REPO")};
  File.AppendAllText(Path.Combine(directory,"candidate-observed.txt"),String.Join("\n",values)+"\n");
  Thread.Sleep(2500);
 }
}
'@
$stubHash = (Get-FileHash -LiteralPath $stub -Algorithm SHA256).Hash.ToLowerInvariant()
$wrapper = Join-Path $root 'mock-scheduler-worker.ps1'
[IO.File]::WriteAllText($wrapper, @'
param([string]$Helper,[string]$RequestPath)
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
$global:fixtureTask=$null; $global:fixtureCandidate=$null
function Get-ScheduledTask {
 param($TaskPath)
 if($null -ne $global:fixtureTask) {
  if([IO.File]::Exists((Join-Path ([IO.Path]::GetDirectoryName($RequestPath)) 'change-task'))) { $global:fixtureTask.Settings.RestartCount=1 }
  return [pscustomobject]$global:fixtureTask
 }
 return @()
}
function Export-ScheduledTask { param($TaskName,$TaskPath) return ('<Task><Principals><Principal><UserId>'+[Security.Principal.WindowsIdentity]::GetCurrent().User.Value+'</UserId></Principal></Principals></Task>') }
function New-ScheduledTaskAction { param($Execute,$WorkingDirectory,$Argument) return @{Execute=$Execute;WorkingDirectory=$WorkingDirectory;Arguments=$Argument} }
function New-ScheduledTaskPrincipal { param($UserId,$LogonType,$RunLevel) return @{UserId=$UserId;LogonType=$LogonType;RunLevel=$RunLevel} }
function New-ScheduledTaskSettingsSet {
 param($MultipleInstances,$ExecutionTimeLimit,$Priority,[switch]$AllowStartIfOnBatteries,[switch]$DontStopIfGoingOnBatteries)
 return @{MultipleInstances=$MultipleInstances;ExecutionTimeLimit=[Xml.XmlConvert]::ToString($ExecutionTimeLimit);RestartCount=0;
  Priority=$Priority;DisallowStartIfOnBatteries=(-not $AllowStartIfOnBatteries);StopIfGoingOnBatteries=(-not $DontStopIfGoingOnBatteries)}
}
function Register-ScheduledTask {
 param($TaskName,$TaskPath,$Action,$Principal,$Settings,$Description)
 if(-not $TaskName.EndsWith('-Candidate') -or [Xml.XmlConvert]::ToTimeSpan($Settings.ExecutionTimeLimit) -ne [TimeSpan]::Zero -or $Settings.Priority -ne 4 -or $Action.Arguments) { throw 'Fixture refuses unexpected Scheduler registration.' }
 $global:fixtureTask=@{TaskName=$TaskName;Action=$Action;Actions=@($Action);Principal=$Principal;Settings=$Settings;Description=$Description;Triggers=@();State='Ready'}
 return $global:fixtureTask
}
function Start-ScheduledTask {
 param($TaskName,$TaskPath)
 if($null -eq $global:fixtureTask -or $TaskName -cne $global:fixtureTask.TaskName) { throw 'Fixture refuses non-candidate task execution.' }
 $action=$global:fixtureTask.Action
 $info=[Diagnostics.ProcessStartInfo]::new()
 $info.FileName=$action.Execute; $info.WorkingDirectory=$action.WorkingDirectory
 $info.UseShellExecute=$false; $info.CreateNoWindow=$true; $info.WindowStyle='Hidden'
 # Emulate Scheduler's independent environment and the app's dotenv loader.
 foreach($key in @($info.EnvironmentVariables.Keys)) { if([string]$key -match '^(PRIM1_|WEBVIEW2_)') { $info.EnvironmentVariables.Remove([string]$key) } }
 foreach($line in Get-Content -LiteralPath (Join-Path $action.WorkingDirectory '.env')) {
  if($line -notmatch "^([A-Z0-9_]+)='([^']*)'$" ) { throw 'Malformed generated environment.' }
  $info.EnvironmentVariables[$Matches[1]]=$Matches[2]
 }
 $global:fixtureCandidate=[Diagnostics.Process]::Start($info)
}
function Get-CimInstance {
 param($ClassName,$Filter)
 if($null -ne $global:fixtureCandidate -and -not $global:fixtureCandidate.HasExited) {
  return [pscustomobject]@{ProcessId=$global:fixtureCandidate.Id;ExecutablePath=$global:fixtureTask.Action.Execute}
 }
 return @()
}
& $Helper -RequestPath $RequestPath
'@, [Text.UTF8Encoding]::new($false))
function Assert([bool]$Condition, [string]$Why) { if (-not $Condition) { throw $Why } }
function Json([string]$Path, $Value) {
  [IO.File]::WriteAllText($Path, ($Value | ConvertTo-Json -Depth 12 -Compress), [Text.UTF8Encoding]::new($false))
}
function Read-FixtureJson([string]$Path) {
  # The helper atomically replaces files; the assertion must tolerate the same
  # short Windows file-name/share transition as the product's state reader.
  $watch=[Diagnostics.Stopwatch]::StartNew()
  while($true) {
    try {
      $file=[IO.File]::Open($Path,'Open','Read',[IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete)
      try {
        $reader=[IO.StreamReader]::new($file)
        try { return ($reader.ReadToEnd() | ConvertFrom-Json) } finally { $reader.Dispose() }
      } finally { $file.Dispose() }
    } catch {
      $code=$_.Exception.GetBaseException().HResult -band 0xffff
      if($code -notin @(2,5,32) -or $watch.Elapsed.TotalSeconds -ge 2) { throw }
      Start-Sleep -Milliseconds 25
    }
  }
}
function Await([scriptblock]$Condition, [string]$Why, [int]$Seconds = 12) {
  $deadline = [DateTime]::UtcNow.AddSeconds($Seconds)
  while (-not (& $Condition)) {
    if ([DateTime]::UtcNow -ge $deadline) { throw $Why }
    Start-Sleep -Milliseconds 50
  }
}
function Spawn([string]$Exe, [string]$Arguments) {
  $info = [Diagnostics.ProcessStartInfo]::new()
  $info.FileName = $Exe; $info.Arguments = $Arguments
  $info.UseShellExecute = $false; $info.CreateNoWindow = $true; $info.WindowStyle = 'Hidden'
  $info.RedirectStandardOutput = $true; $info.RedirectStandardError = $true
  $info.EnvironmentVariables['PRIM1_PANE_SECRET'] = 'fixture-canary'
  $info.EnvironmentVariables['PRIM1_TRIAL_ATTEMPT'] = 'fixture-canary'
  $info.EnvironmentVariables['WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS'] = 'fixture-canary'
  return [Diagnostics.Process]::Start($info)
}
function Run-Case([string]$Case) {
  $base = Join-Path $root $Case
  $runtime = Join-Path $base 'runtime'; $profile = Join-Path $base 'profile'
  $workspace = Join-Path $base 'workspace'; $tasks = Join-Path $workspace 'tasks'
  $attempt = [Guid]::NewGuid().ToString()
  $dir = Join-Path $runtime ('maintenance\upgrades\' + $attempt)
  $candidateDir = Join-Path $dir 'candidate'; $candidate = Join-Path $candidateDir 'prim1.exe'
  foreach ($path in @($candidateDir, $profile, $tasks)) { $null = [IO.Directory]::CreateDirectory($path) }
  [IO.File]::Copy($stub, $candidate, $false)
  [IO.File]::WriteAllText((Join-Path $runtime 'desktop-instance.lock'), '')
  Json (Join-Path $runtime 'session-catalog-v1.json') @{ schema_version=1; workspace_preference=@{canonical_path=$workspace}; sessions=@() }
  $requestPath = Join-Path $dir 'request.json'; $helperPath = Join-Path $dir 'continue-upgrade.ps1'
  [IO.File]::Copy($Helper, $helperPath, $false)
  $finish = Join-Path $base 'source-finish'
  $source = Spawn $stub ('"' + $finish + '"')
  $worker = $null; $runtimeLock = $null; $taskLock = $null; $profileLock = $null
  try {
    $created = $source.StartTime.ToUniversalTime().ToFileTimeUtc().ToString()
    $now = [DateTimeOffset]::UtcNow
    $expires = $now.AddSeconds(20)
    if ($Case -in @('expiry', 'no_shutdown_marker')) { $expires = $now.AddSeconds(6) }
    $request = [ordered]@{
      schema_version=1; attempt_id=$attempt; prepared_at=$now.AddSeconds(-61).ToString('o')
      notice_until=$now.AddSeconds(-1).ToString('o'); expires_at=$expires.ToString('o')
      source=@{pid=$source.Id; creation_filetime=$created; exe_path=$stub; exe_sha256=$stubHash}
      candidate=@{exe_path=$candidate; exe_sha256=$stubHash}; runtime_dir=$runtime; profile_dir=$profile; task_folder=$tasks; personal_repo_override='off'
      requester_session_id=[Guid]::NewGuid().ToString(); members=@()
    }
    if ($Case -eq 'wrong_creation') { $request.source.creation_filetime = ([uint64]$created + 1).ToString() }
    if ($Case -eq 'wrong_hash') { $request.candidate.exe_sha256 = '0' * 64 }
    if ($Case -eq 'task_changed') {
      $other = Join-Path $base 'other-tasks'; $null = [IO.Directory]::CreateDirectory($other)
      Json (Join-Path $runtime 'task-folder.json') @{ schema_version=1; folder=$other }
    }
    if ($Case -eq 'task_setting_removed') { Json (Join-Path $runtime 'task-folder.json') @{schema_version=1;folder=$tasks} }
    if ($Case -eq 'extended_paths') {
      foreach ($key in @('runtime_dir','profile_dir','task_folder')) { $request[$key] = '\\?\' + $request[$key] }
      $request.source.exe_path = '\\?\' + $request.source.exe_path
      $request.candidate.exe_path = '\\?\' + $request.candidate.exe_path
      Json (Join-Path $runtime 'session-catalog-v1.json') @{ schema_version=1; workspace_preference=@{canonical_path=('\\?\'+$workspace)}; sessions=@() }
    }
    Json $requestPath $request
    $requestHash = (Get-FileHash -LiteralPath $requestPath -Algorithm SHA256).Hash.ToLowerInvariant()
    $state = @{schema_version=1; attempt_id=$attempt; request_sha256=$requestHash; phase='prepared'; changed_at=$now.ToString('o')}
    Json (Join-Path $dir 'state.json') $state
    if ($Case -in @('happy','extended_paths')) {
      $runtimeLock = [IO.File]::Open((Join-Path $runtime 'desktop-instance.lock'), 'Open', 'ReadWrite', 'ReadWrite')
      $taskLock = [IO.FileStream]::new((Join-Path $tasks '.prim1-tasks.lock'), [IO.FileMode]::CreateNew,
        [IO.FileAccess]::ReadWrite, [IO.FileShare]::None, 4096, [IO.FileOptions]::DeleteOnClose)
      $webview = Join-Path $profile 'EBWebView'
      $null = [IO.Directory]::CreateDirectory($webview)
      $profileLock = [IO.FileStream]::new((Join-Path $webview 'lockfile'), [IO.FileMode]::CreateNew,
        [IO.FileAccess]::ReadWrite, [IO.FileShare]::None, 4096, [IO.FileOptions]::DeleteOnClose)
    }
    $worker = Spawn $powershell ('-NoProfile -NonInteractive -ExecutionPolicy Bypass -WindowStyle Hidden -File "' + $wrapper + '" -Helper "' + $helperPath + '" -RequestPath "' + $requestPath + '"')
    $ready = Join-Path $dir 'helper-ready.json'; $resultPath = Join-Path $dir 'helper-result.json'
    if ($Case -in @('wrong_creation','wrong_hash','task_changed')) {
      Await { $worker.HasExited } 'Refusal helper did not exit.'
      Assert (-not [IO.File]::Exists($ready)) 'Refused helper falsely became ready.'
    } else {
      Await { [IO.File]::Exists($ready) -or $worker.HasExited } 'Helper readiness timed out.'
      if (-not [IO.File]::Exists($ready)) { throw ('Helper failed before ready: ' + $worker.StandardError.ReadToEnd()) }
      $boundReady = Read-FixtureJson $ready
      Assert ($boundReady.request_sha256 -ceq $requestHash -and $boundReady.helper_pid -eq $worker.Id) 'Readiness binding mismatch.'
      if ($Case -eq 'cancelled') { $state.phase='cancelled'; Json (Join-Path $dir 'state.json') $state }
      elseif ($Case -eq 'bad_state') { $state.request_sha256='1'*64; Json (Join-Path $dir 'state.json') $state }
      elseif ($Case -in @('task_setting_removed','task_reconfigured')) {
        if ($Case -eq 'task_setting_removed') { [IO.File]::Delete((Join-Path $runtime 'task-folder.json')) }
        else { [IO.File]::WriteAllText((Join-Path $dir 'change-task'),'fixture') }
        $state.phase='shutdown_complete'; Json (Join-Path $dir 'state.json') $state
        [IO.File]::WriteAllText($finish,'exit')
      }
      elseif ($Case -in @('happy','extended_paths')) {
        $state.phase='shutdown_complete'; Json (Join-Path $dir 'state.json') $state
        [IO.File]::WriteAllText($finish,'exit'); Await { $source.HasExited } 'Stub source did not exit.'
        Start-Sleep -Milliseconds 600
        Assert (-not [IO.File]::Exists((Join-Path $dir 'candidate-observed.txt'))) 'Candidate started while runtime/task owned.'
        $runtimeLock.Dispose(); $runtimeLock=$null
        Start-Sleep -Milliseconds 400
        Assert (-not [IO.File]::Exists((Join-Path $dir 'candidate-observed.txt'))) 'Candidate started while task folder owned.'
        $taskLock.Dispose(); $taskLock=$null
        Start-Sleep -Milliseconds 400
        Assert (-not [IO.File]::Exists((Join-Path $dir 'candidate-observed.txt'))) 'Candidate started while WebView2 profile owned.'
        $profileLock.Dispose(); $profileLock=$null
      } elseif ($Case -eq 'no_shutdown_marker') { [IO.File]::WriteAllText($finish,'exit') }
      Await { $worker.HasExited } 'Helper did not finish its bounded attempt.'
    }
    $out = $worker.StandardOutput.ReadToEnd(); $err = $worker.StandardError.ReadToEnd()
    [IO.File]::WriteAllText((Join-Path $dir 'fixture-worker-output.txt'), $out + "`n" + $err)
    $result = Read-FixtureJson $resultPath
    $expected = if ($Case -in @('happy','extended_paths')) { 'launched' } elseif ($Case -eq 'cancelled') { 'cancelled' } elseif ($Case -in @('expiry','no_shutdown_marker')) { 'expired' } else { 'failed' }
    Assert ($result.outcome -ceq $expected) ("${Case}: expected $expected, got " + $result.outcome + ': ' + $result.message)
    Assert ($result.attempt_id -ceq $attempt -and $result.request_sha256 -ceq $requestHash) 'Result binding mismatch.'
    if ($Case -in @('happy','extended_paths')) {
      $observation = Join-Path $dir 'candidate-observed.txt'
      Await { [IO.File]::Exists($observation) } 'Candidate stub did not record environment.'
      $lines = @(Get-Content $observation)
      Assert ($lines.Count -eq 8 -and $lines[0] -ceq $runtime.Replace('\','/') -and $lines[1] -ceq $profile.Replace('\','/') -and
        $lines[2] -ceq $workspace.Replace('\','/') -and $lines[3] -ceq $attempt -and $lines[4] -ceq 'secret_absent' -and
        $lines[5] -ceq 'trial_absent' -and $lines[6] -ceq 'debug_absent' -and $lines[7] -ceq 'off') 'Candidate environment was wrong or leaked caller values.'
      $again = Spawn $powershell ('-NoProfile -NonInteractive -ExecutionPolicy Bypass -WindowStyle Hidden -File "' + $wrapper + '" -Helper "' + $helperPath + '" -RequestPath "' + $requestPath + '"')
      try { Await { $again.HasExited } 'Duplicate helper did not refuse.'; Assert ($again.ExitCode -ne 0) 'Duplicate helper returned success.' }
      finally { $again.Dispose() }
      Assert ((Read-FixtureJson $resultPath).outcome -ceq 'launched') 'Duplicate helper overwrote the first result.'
      Assert (@(Get-Content $observation).Count -eq 8) 'Duplicate candidate launched.'
    } else { Assert (-not [IO.File]::Exists((Join-Path $dir 'candidate-observed.txt'))) 'Refused attempt launched a candidate.' }
    Write-Output "PASS $Case"
  } finally {
    if ($runtimeLock) { $runtimeLock.Dispose() }; if ($taskLock) { $taskLock.Dispose() }; if ($profileLock) { $profileLock.Dispose() }
    [IO.File]::WriteAllText($finish,'exit')
    if ($source) { $null = $source.WaitForExit(5000); $source.Dispose() }
    if ($worker) { $worker.Dispose() }
  }
}
Write-Output "Fixture root: $root"
foreach ($case in @('happy','extended_paths','wrong_creation','wrong_hash','task_changed','cancelled','bad_state','expiry','no_shutdown_marker','task_setting_removed','task_reconfigured')) { Run-Case $case }
# Exercise the exact production reader with bounded transient failures. No
# duplicate implementation: only Read-BoundedJson is the fixture seam.
& {
  $tokens=$null; $errors=$null
  $ast=[Management.Automation.Language.Parser]::ParseFile($Helper,[ref]$tokens,[ref]$errors)
  Assert ($errors.Count -eq 0) 'Helper has PowerShell parse errors.'
  $reader=$ast.Find({param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Read-State'},$true)
  Invoke-Expression $reader.Extent.Text
  $script:attemptDir=$root; $script:request=@{attempt_id='fixture'}; $script:requestHash='fixture-hash'
  function Read-BoundedJson {
    param($Path,$Maximum)
    $script:readCalls++
    if($script:readCalls -eq 1 -or $script:alwaysFail) { throw [IO.IOException]::new('Transient fixture read',(-2147024896 + $script:errorCode)) }
    return @{Value=@{schema_version=1;attempt_id='fixture';request_sha256='fixture-hash';phase='closing'}}
  }
  $script:alwaysFail=$false
  foreach($code in @(2,5,32)) {
    $script:errorCode=$code; $script:readCalls=0
    Assert ((Read-State) -ceq 'closing' -and $script:readCalls -eq 2) "Transient Win32 error $code was not retried."
  }
  $script:errorCode=13; $script:readCalls=0; $failed=$false
  try { $null=Read-State } catch { $failed=$true }
  Assert ($failed -and $script:readCalls -eq 1) 'Non-transient state error was retried.'
  $script:errorCode=32; $script:readCalls=0; $script:alwaysFail=$true
  $watch=[Diagnostics.Stopwatch]::StartNew(); $failed=$false
  try { $null=Read-State } catch { $failed=$true }
  Assert ($failed -and $watch.Elapsed.TotalSeconds -ge 2 -and $watch.Elapsed.TotalSeconds -lt 4) 'Transient retries were not bounded to two seconds.'
  Write-Output 'PASS state_read_retry'
}
Write-Output 'PASS: 11 disposable helper cases plus bounded state-read retry; Scheduler never invoked; no PRIM-1/harness/UI launched.'
