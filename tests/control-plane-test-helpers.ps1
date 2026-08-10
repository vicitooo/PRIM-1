Set-StrictMode -Version Latest

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
  $infoPath = Join-Path $runtimeDir "control-plane.json"
  $capturePath = Join-Path $runtimeDir "captured-request.json"
  $status = @{
    transport = "named_pipe"
    endpoint = "\\.\pipe\$pipeName"
    token = "test-token"
    info_path = $infoPath
  }
  $utf8NoBom = [System.Text.UTF8Encoding]::new($false)
  [System.IO.File]::WriteAllText($infoPath, ($status | ConvertTo-Json -Compress), $utf8NoBom)

  return [pscustomobject]@{
    Root = $root
    RuntimeDir = $runtimeDir
    InfoPath = $infoPath
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

    [int]$DelayMs = 0
  )

  $readyPath = "$CapturePath.ready"
  Remove-Item -LiteralPath $readyPath -Force -ErrorAction SilentlyContinue
  $job = Start-Job -ScriptBlock {
    param($PipeName, $CapturePath, $ResponseJson, $DelayMs, $ReadyPath)

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
      [System.IO.File]::WriteAllText($ReadyPath, "ready", [System.Text.UTF8Encoding]::new($false))
      $pipe.WaitForConnection()
      $reader = [System.IO.StreamReader]::new($pipe, [System.Text.Encoding]::UTF8, $true, 4096, $true)
      $writer = [System.IO.StreamWriter]::new($pipe, [System.Text.UTF8Encoding]::new($false), 4096, $true)
      $writer.AutoFlush = $true

      $raw = $reader.ReadLine()
      if ($null -eq $raw) {
        throw "Named-pipe client disconnected before sending a request."
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
  } -ArgumentList $PipeName, $CapturePath, $ResponseJson, $DelayMs, $readyPath

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

  return [pscustomobject]@{
    Job = $job
    ReadyPath = $readyPath
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

  if ($Responder -and $Responder.Job) {
    Remove-Job -Job $Responder.Job -Force -ErrorAction SilentlyContinue
  }
  if ($Responder -and $Responder.ReadyPath) {
    Remove-Item -LiteralPath $Responder.ReadyPath -Force -ErrorAction SilentlyContinue
  }
}
