<#
Focused isolated helper check. Run with Windows PowerShell 5.1:
  powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File scripts/test-v2-stop-handle.ps1
Compiles two disposable sleep stubs; never runs the preview script's dispatcher,
launches a desktop/harness, or enumerates/stops any existing process. Scratch is retained.
#>
[CmdletBinding()]
param([string]$PreviewScript)
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
if ([string]::IsNullOrWhiteSpace($PreviewScript)) {
  $PreviewScript = Join-Path (Split-Path -Parent $MyInvocation.MyCommand.Path) "v2-preview.ps1"
}

function Assert-Fixture([bool]$Condition, [string]$Message) {
  if (-not $Condition) { throw $Message }
}

$tokens = $null
$parseErrors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile(
  $PreviewScript, [ref]$tokens, [ref]$parseErrors)
Assert-Fixture ($parseErrors.Count -eq 0) "preview script has parser errors: $parseErrors"
$definitions = @($ast.FindAll({ param($node)
  $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
    $node.Name -eq "Initialize-V2StopHandle"
}, $true))
Assert-Fixture ($definitions.Count -eq 1) "expected exactly one isolated native-helper initializer"
# Only the native helper definition executes. No launcher dispatcher or CIM query.
$null = $definitions[0].Body.GetScriptBlock().Invoke()

$scratch = Join-Path ([System.IO.Path]::GetTempPath()) ("prim1-stop-handle-" + [guid]::NewGuid())
$null = New-Item -ItemType Directory -Path $scratch
$ownedExe = Join-Path $scratch "owned-stub.exe"
$otherExe = Join-Path $scratch "other-stub.exe"
Add-Type -OutputAssembly $ownedExe -OutputType ConsoleApplication -TypeDefinition @'
using System.Threading;
public static class PreviewStopSleepStub {
  public static void Main() { Thread.Sleep(60000); }
}
'@
Copy-Item -LiteralPath $ownedExe -Destination $otherExe
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class PreviewStopFixtureCleanup {
  [DllImport("kernel32.dll", SetLastError = true)]
  public static extern bool TerminateProcess(IntPtr retainedHandle, uint exitCode);
  [DllImport("kernel32.dll", SetLastError = true)]
  public static extern uint WaitForSingleObject(IntPtr retainedHandle, uint milliseconds);
}
'@

$ownedChild = $null
$otherChild = $null
$binding = $null
$ownedKernel = [IntPtr]::Zero
$otherKernel = [IntPtr]::Zero
try {
  foreach ($image in @($ownedExe, $otherExe)) {
    $start = [System.Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $image
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $child = [System.Diagnostics.Process]::Start($start)
    # Retain each test-owned process object for independent emergency cleanup.
    if ($image -eq $ownedExe) { $ownedChild = $child; $ownedKernel = $child.Handle }
    else { $otherChild = $child; $otherKernel = $child.Handle }
  }

  $refused = $false
  $unexpected = $null
  try { $unexpected = [Prim1V2.OwnedPreviewProcess]::Open([uint32]$otherChild.Id, $ownedExe) }
  catch { $refused = $_.Exception.ToString().Contains("exact staged executable") }
  finally { if ($null -ne $unexpected) { $unexpected.Dispose() } }
  Assert-Fixture $refused "mismatched image was not refused"
  Assert-Fixture ([PreviewStopFixtureCleanup]::WaitForSingleObject($otherKernel, 0) -eq 258) "mismatch altered the decoy"
  Write-Output "PASS: wrong-image candidate refused; decoy still alive"

  $binding = [Prim1V2.OwnedPreviewProcess]::Open([uint32]$ownedChild.Id, $ownedExe)
  Assert-Fixture ($null -ne $binding) "owned stub exited before binding"
  Assert-Fixture ([string]::Equals($binding.ImagePath, $ownedExe, [StringComparison]::OrdinalIgnoreCase)) "bound image differs"
  Assert-Fixture ($binding.CreationFileTime -gt 0) "native creation identity absent"
  Assert-Fixture ($binding.CreationFileTime -eq [uint64]$ownedChild.StartTime.ToUniversalTime().ToFileTimeUtc()) "native creation differs from retained test process"
  Write-Output "PASS: owned image and exact native creation identity captured"

  Assert-Fixture ($binding.TerminateAndWait()) "owned live stub was not terminated"
  Assert-Fixture ([PreviewStopFixtureCleanup]::WaitForSingleObject($ownedKernel, 0) -eq 0) "independent retained handle did not signal"
  Assert-Fixture (-not $binding.TerminateAndWait()) "already-exited process reported a new termination"
  Assert-Fixture ([PreviewStopFixtureCleanup]::WaitForSingleObject($otherKernel, 0) -eq 258) "owned stop altered decoy"
  Write-Output "PASS: exact handle exit confirmed; repeat is inert; decoy still alive"
} finally {
  if ($null -ne $binding) { $binding.Dispose() }
  # Handles belong only to the two stubs created above; no PID reopen/kill fallback.
  $cleanupFailures = [System.Collections.Generic.List[string]]::new()
  foreach ($kernel in @($ownedKernel, $otherKernel)) {
    try {
      if ($kernel -ne [IntPtr]::Zero -and
          [PreviewStopFixtureCleanup]::WaitForSingleObject($kernel, 0) -eq 258) {
        if (-not [PreviewStopFixtureCleanup]::TerminateProcess($kernel, 1)) {
          throw "fixture-owned stub cleanup failed"
        }
        if ([PreviewStopFixtureCleanup]::WaitForSingleObject($kernel, 5000) -ne 0) {
          throw "fixture-owned stub did not exit"
        }
      }
    } catch {
      $cleanupFailures.Add($_.Exception.Message)
    }
  }
  if ($null -ne $ownedChild) { $ownedChild.Dispose() }
  if ($null -ne $otherChild) { $otherChild.Dispose() }
  Write-Output "fixture scratch retained: $scratch"
  if ($cleanupFailures.Count -gt 0) { throw ($cleanupFailures -join "; ") }
}
