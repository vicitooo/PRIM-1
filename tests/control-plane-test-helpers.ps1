Set-StrictMode -Version Latest

function Initialize-ControlPlaneTestNativeMethods {
  if ($null -ne ("Prim1.ControlPlane.Tests.NativeMethods" -as [type])) {
    return
  }

  Add-Type -Language CSharp -ErrorAction Stop -TypeDefinition @"
using System;
using System.Runtime.InteropServices;

namespace Prim1.ControlPlane.Tests
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

function Get-ControlPlaneTestProcessCreationFiletime {
  param(
    [Parameter(Mandatory = $true)]
    [uint32]$ProcessId
  )

  Initialize-ControlPlaneTestNativeMethods
  $processHandle = [Prim1.ControlPlane.Tests.NativeMethods]::OpenProcess(
    [uint32]0x1000,
    $false,
    $ProcessId
  )
  if ($processHandle -eq [IntPtr]::Zero) {
    $errorCode = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
    throw "Test OpenProcess failed for named-pipe server PID $ProcessId (Win32 $errorCode)."
  }

  try {
    $creationTime = New-Object Prim1.ControlPlane.Tests.NativeFileTime
    $exitTime = New-Object Prim1.ControlPlane.Tests.NativeFileTime
    $kernelTime = New-Object Prim1.ControlPlane.Tests.NativeFileTime
    $userTime = New-Object Prim1.ControlPlane.Tests.NativeFileTime
    $succeeded = [Prim1.ControlPlane.Tests.NativeMethods]::GetProcessTimes(
      $processHandle,
      [ref]$creationTime,
      [ref]$exitTime,
      [ref]$kernelTime,
      [ref]$userTime
    )
    if (-not $succeeded) {
      $errorCode = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
      throw "Test GetProcessTimes failed for named-pipe server PID $ProcessId (Win32 $errorCode)."
    }

    return ([uint64]$creationTime.HighDateTime * [uint64]4294967296) + [uint64]$creationTime.LowDateTime
  } finally {
    $null = [Prim1.ControlPlane.Tests.NativeMethods]::CloseHandle($processHandle)
  }
}

function New-ControlPlaneTestRuntime {
  param(
    [Parameter(Mandatory = $true)]
    [string]$Prefix
  )

  $id = [guid]::NewGuid().ToString("N")
  $root = Join-Path ([System.IO.Path]::GetTempPath()) "$Prefix-$id"
  $runtimeDir = Join-Path $root "runtime"
  $null = New-Item -ItemType Directory -Force -Path $runtimeDir
  $pipeName = "$Prefix-$id"
  $capturePath = Join-Path $runtimeDir "captured-request.json"

  return [pscustomobject]@{
    Root = $root
    RuntimeDir = $runtimeDir
    Endpoint = "\\.\pipe\$pipeName"
    CapturePath = $capturePath
    PipeName = $pipeName
  }
}

function Start-ControlPlanePipeResponder {
  param(
    [Parameter(Mandatory = $true)]
    [string]$PipeName,

    [Parameter(Mandatory = $true)]
    [string]$CapturePath,

    [Parameter(Mandatory = $true)]
    [string]$ResponseJson,

    [int]$DelayMs = 0,

    [switch]$AllowClientDisconnectWithoutRequest
  )

  $readyPath = "$CapturePath.ready"
  $readyTempPath = "$readyPath.tmp"
  Remove-Item -LiteralPath $CapturePath -Force -ErrorAction SilentlyContinue
  Remove-Item -LiteralPath "$CapturePath.preamble" -Force -ErrorAction SilentlyContinue
  Remove-Item -LiteralPath $readyPath -Force -ErrorAction SilentlyContinue
  Remove-Item -LiteralPath $readyTempPath -Force -ErrorAction SilentlyContinue
  $originalServerPid = [Environment]::GetEnvironmentVariable("PRIM1_CONTROL_PLANE_SERVER_PID", "Process")
  $originalServerStartedFiletime = [Environment]::GetEnvironmentVariable("PRIM1_CONTROL_PLANE_SERVER_STARTED_FILETIME", "Process")
  $job = Start-Job -ScriptBlock {
    param($PipeName, $CapturePath, $ResponseJson, $DelayMs, $ReadyPath, $ReadyTempPath, $AllowClientDisconnectWithoutRequest)

    $pipe = $null
    $reader = $null
    $writer = $null
    try {
      $pipe = [System.IO.Pipes.NamedPipeServerStream]::new(
        $PipeName,
        [System.IO.Pipes.PipeDirection]::InOut,
        1,
        [System.IO.Pipes.PipeTransmissionMode]::Byte,
        [System.IO.Pipes.PipeOptions]::Asynchronous
      )
      $readyJson = @{ process_id = $PID } | ConvertTo-Json -Compress
      [System.IO.File]::WriteAllText($ReadyTempPath, $readyJson, [System.Text.UTF8Encoding]::new($false))
      [System.IO.File]::Move($ReadyTempPath, $ReadyPath)
      $pipe.WaitForConnection()
      if ($AllowClientDisconnectWithoutRequest) {
        $probeBuffer = New-Object byte[] 4096
        $received = $pipe.Read($probeBuffer, 0, $probeBuffer.Length)
        if ($received -eq 0) {
          [System.IO.File]::WriteAllBytes($CapturePath, (New-Object byte[] 0))
        } else {
          $capturedBytes = New-Object byte[] $received
          [Array]::Copy($probeBuffer, $capturedBytes, $received)
          [System.IO.File]::WriteAllBytes($CapturePath, $capturedBytes)
        }
        return
      }

      $reader = [System.IO.StreamReader]::new($pipe, [System.Text.Encoding]::UTF8, $true, 4096, $true)
      $writer = [System.IO.StreamWriter]::new($pipe, [System.Text.UTF8Encoding]::new($false), 4096, $true)
      $writer.AutoFlush = $true

      $raw = $reader.ReadLine()
      if ($null -eq $raw) {
        throw "Named-pipe client disconnected before sending a request."
      }
      # A pane-secret preamble is its own first line; the request follows it.
      # Mirror the supervisor: read both before answering.
      if ($raw -match '^\s*\{\s*"secret"\s*:' -and $raw -notmatch '"kind"') {
        [System.IO.File]::WriteAllText("$CapturePath.preamble", $raw, [System.Text.UTF8Encoding]::new($false))
        $raw = $reader.ReadLine()
        if ($null -eq $raw) {
          throw "Named-pipe client disconnected after its preamble without a request."
        }
      }
      [System.IO.File]::WriteAllText($CapturePath, $raw, [System.Text.UTF8Encoding]::new($false))
      if ($DelayMs -gt 0) {
        Start-Sleep -Milliseconds $DelayMs
      }
      $writer.WriteLine($ResponseJson)
    } finally {
      if ($writer) { $writer.Dispose() }
      if ($reader) { $reader.Dispose() }
      if ($pipe) { $pipe.Dispose() }
    }
  } -ArgumentList $PipeName, $CapturePath, $ResponseJson, $DelayMs, $readyPath, $readyTempPath, $AllowClientDisconnectWithoutRequest.IsPresent

  $deadline = [DateTime]::UtcNow.AddSeconds(10)
  while (-not (Test-Path -LiteralPath $readyPath)) {
    if ($job.State -in @("Failed", "Stopped", "Completed")) {
      $detail = ($job | Receive-Job -ErrorAction SilentlyContinue | Out-String).Trim()
      throw "Named-pipe responder failed before readiness. $detail"
    }
    if ([DateTime]::UtcNow -ge $deadline) {
      throw "Timed out waiting for named-pipe responder readiness."
    }
    Start-Sleep -Milliseconds 50
  }

  $ready = Get-Content -LiteralPath $readyPath -Raw -ErrorAction Stop | ConvertFrom-Json -ErrorAction Stop
  [uint32]$serverPid = $ready.process_id
  [uint64]$serverStartedFiletime = Get-ControlPlaneTestProcessCreationFiletime -ProcessId $serverPid
  [Environment]::SetEnvironmentVariable("PRIM1_CONTROL_PLANE_SERVER_PID", $serverPid.ToString([Globalization.CultureInfo]::InvariantCulture), "Process")
  [Environment]::SetEnvironmentVariable("PRIM1_CONTROL_PLANE_SERVER_STARTED_FILETIME", $serverStartedFiletime.ToString([Globalization.CultureInfo]::InvariantCulture), "Process")

  return [pscustomobject]@{
    Job = $job
    ReadyPath = $readyPath
    ReadyTempPath = $readyTempPath
    ServerPid = $serverPid
    ServerStartedFiletime = $serverStartedFiletime
    OriginalServerPid = $originalServerPid
    OriginalServerStartedFiletime = $originalServerStartedFiletime
  }
}

function Wait-ControlPlanePipeResponder {
  param(
    [Parameter(Mandatory = $true)]
    $Responder,

    [int]$TimeoutSec = 25
  )

  Wait-Job -Job $Responder.Job -Timeout $TimeoutSec | Out-Null
  $detail = ($Responder.Job | Receive-Job -ErrorAction Stop | Out-String).Trim()
  if ($Responder.Job.State -ne "Completed") {
    throw "Named-pipe responder did not complete cleanly (state=$($Responder.Job.State)). $detail"
  }
}

function Remove-ControlPlanePipeResponder {
  param($Responder)

  if ($Responder) {
    [Environment]::SetEnvironmentVariable("PRIM1_CONTROL_PLANE_SERVER_PID", $Responder.OriginalServerPid, "Process")
    [Environment]::SetEnvironmentVariable("PRIM1_CONTROL_PLANE_SERVER_STARTED_FILETIME", $Responder.OriginalServerStartedFiletime, "Process")
  }
  if ($Responder -and $Responder.Job) {
    Remove-Job -Job $Responder.Job -Force -ErrorAction SilentlyContinue
  }
  if ($Responder -and $Responder.ReadyPath) {
    Remove-Item -LiteralPath $Responder.ReadyPath -Force -ErrorAction SilentlyContinue
  }
  if ($Responder -and $Responder.ReadyTempPath) {
    Remove-Item -LiteralPath $Responder.ReadyTempPath -Force -ErrorAction SilentlyContinue
  }
}
