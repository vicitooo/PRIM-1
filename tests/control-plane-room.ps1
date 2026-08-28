Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
. "$PSScriptRoot\control-plane-test-helpers.ps1"

function Assert-True {
  param([bool]$Condition, [string]$Message)
  if (-not $Condition) { throw $Message }
}

function Invoke-ControlPlane {
  param([string[]]$Arguments)
  $previousPreference = $ErrorActionPreference
  $ErrorActionPreference = "Continue"
  $output = & powershell.exe -NoProfile -ExecutionPolicy Bypass @Arguments 2>&1
  $exitCode = $LASTEXITCODE
  $ErrorActionPreference = $previousPreference
  [pscustomobject]@{
    ExitCode = $exitCode
    Output = ($output | Out-String).Trim()
  }
}

function Invoke-CapturedRoomRequest {
  param(
    [string]$Prefix,
    [string[]]$Arguments,
    [string]$ResponseMessage
  )
  $runtime = New-ControlPlaneTestRuntime -Prefix $Prefix
  $response = @{ ok = $true; message = $ResponseMessage } | ConvertTo-Json -Compress
  $responder = Start-ControlPlanePipeResponder -PipeName $runtime.PipeName -CapturePath $runtime.CapturePath -ResponseJson $response
  try {
    $result = Invoke-ControlPlane -Arguments (@(
      "-File", $controlPlaneScript,
      "-Endpoint", $runtime.Endpoint,
      "-Quiet"
    ) + $Arguments)
    Assert-True ($result.ExitCode -eq 0) "Room request failed: $($result.Output)"
    Wait-ControlPlanePipeResponder -Responder $responder
    $strictUtf8 = [System.Text.UTF8Encoding]::new($false, $true)
    return [System.IO.File]::ReadAllText($runtime.CapturePath, $strictUtf8) | ConvertFrom-Json
  } finally {
    Remove-ControlPlanePipeResponder -Responder $responder
    Remove-Item -LiteralPath $runtime.Root -Recurse -Force -ErrorAction SilentlyContinue
  }
}

function Invoke-CapturedRoomRequestWithPreamble {
  param(
    [string]$Prefix,
    [string[]]$Arguments,
    [string]$ResponseMessage
  )
  $runtime = New-ControlPlaneTestRuntime -Prefix $Prefix
  $response = @{ ok = $true; message = $ResponseMessage } | ConvertTo-Json -Compress
  $responder = Start-ControlPlanePipeResponder -PipeName $runtime.PipeName -CapturePath $runtime.CapturePath -ResponseJson $response
  try {
    $result = Invoke-ControlPlane -Arguments (@(
      "-File", $controlPlaneScript,
      "-Endpoint", $runtime.Endpoint,
      "-Quiet"
    ) + $Arguments)
    Assert-True ($result.ExitCode -eq 0) "Room request failed: $($result.Output)"
    Wait-ControlPlanePipeResponder -Responder $responder
    $strictUtf8 = [System.Text.UTF8Encoding]::new($false, $true)
    $preamblePath = "$($runtime.CapturePath).preamble"
    $preamble = if (Test-Path -LiteralPath $preamblePath) { [System.IO.File]::ReadAllText($preamblePath, $strictUtf8) | ConvertFrom-Json } else { $null }
    return [pscustomobject]@{
      Preamble = $preamble
      Request = ([System.IO.File]::ReadAllText($runtime.CapturePath, $strictUtf8) | ConvertFrom-Json)
    }
  } finally {
    Remove-ControlPlanePipeResponder -Responder $responder
    Remove-Item -LiteralPath $runtime.Root -Recurse -Force -ErrorAction SilentlyContinue
  }
}

$repoRoot = Split-Path -Parent $PSScriptRoot
$controlPlaneScript = Join-Path $repoRoot "scripts\control-plane.ps1"

$read = Invoke-CapturedRoomRequest -Prefix "prim1-room-read" -Arguments @(
  "-Action", "room_read"
) -ResponseMessage "room feed read"
Assert-True ($read.kind -ceq "room_read") "Expected room_read request."
Assert-True (@($read.PSObject.Properties.Name).Count -eq 1) "room_read must carry no room, sender, session, or token authority."

$epoch = [guid]::NewGuid().ToString()
$readCursor = Invoke-CapturedRoomRequest -Prefix "prim1-room-cursor" -Arguments @(
  "-Action", "room_read",
  "-CursorEpoch", $epoch,
  "-CursorSequence", "42"
) -ResponseMessage "room feed read"
Assert-True ($readCursor.kind -ceq "room_read") "Expected cursor room_read request."
Assert-True ($readCursor.cursor.epoch -ceq $epoch) "Cursor epoch changed in transit."
Assert-True ([uint64]$readCursor.cursor.sequence -eq 42) "Cursor sequence changed in transit."
Assert-True (-not ($readCursor.PSObject.Properties.Name -contains "room_id")) "room_read must not carry RoomId authority."

$lambda = [char]0x03BB
$emoji = [char]::ConvertFromUtf32(0x1F680)
$content = "  pane room $lambda $emoji`r`nsecond line  `n"
$post = Invoke-CapturedRoomRequest -Prefix "prim1-room-post" -Arguments @(
  "-Action", "room_post",
  "-Content", $content
) -ResponseMessage "room message posted"
Assert-True ($post.kind -ceq "room_post") "Expected room_post request."
Assert-True ($post.content -ceq $content) "Room content must remain exact through the PowerShell JSON boundary."
Assert-True (-not ($post.PSObject.Properties.Name -contains "room_id")) "room_post must not carry RoomId authority."
Assert-True (-not ($post.PSObject.Properties.Name -contains "sender")) "room_post must not carry sender authority."
Assert-True (-not ($post.PSObject.Properties.Name -contains "token")) "room_post must never carry a bearer."

$invalid = Invoke-ControlPlane -Arguments @(
  "-File", $controlPlaneScript,
  "-Action", "room_post",
  "-Session", "other-pane",
  "-Content", "must fail",
  "-Endpoint", "\\.\pipe\unused",
  "-Quiet"
)
Assert-True ($invalid.ExitCode -ne 0) "Pane room post with renderer-selected session must fail."
Assert-True ($invalid.Output -like "*derives room membership from the calling pane*") "Expected derived-membership guard."

$deliver = Invoke-CapturedRoomRequest -Prefix "prim1-room-deliver" -Arguments @(
  "-Action", "room_deliver",
  "-Recipient", "Codex",
  "-Content", "wake up"
) -ResponseMessage "room delivery written for 1 of 1 recipients"
Assert-True ($deliver.kind -ceq "room_deliver") "Expected room_deliver request."
Assert-True ($deliver.recipient -ceq "Codex") "Recipient must pass through unchanged."
Assert-True ($deliver.content -ceq "wake up") "Delivery content must pass through unchanged."
Assert-True (-not ($deliver.PSObject.Properties.Name -contains "room_id")) "room_deliver must not carry RoomId authority."
Assert-True (-not ($deliver.PSObject.Properties.Name -contains "sender")) "room_deliver must not carry sender authority."
Assert-True (@($deliver.PSObject.Properties.Name).Count -eq 3) "room_deliver carries exactly kind, recipient, content."

$noRecipient = Invoke-ControlPlane -Arguments @(
  "-File", $controlPlaneScript,
  "-Action", "room_deliver",
  "-Content", "must fail",
  "-Endpoint", "\.\pipe\unused",
  "-Quiet"
)
Assert-True ($noRecipient.ExitCode -ne 0) "room_deliver without -Recipient must fail."
Assert-True ($noRecipient.Output -like "*requires -Recipient*") "Expected the recipient guard. Output: $($noRecipient.Output)"

$strayRecipient = Invoke-ControlPlane -Arguments @(
  "-File", $controlPlaneScript,
  "-Action", "room_post",
  "-Recipient", "Codex",
  "-Content", "must fail",
  "-Endpoint", "\.\pipe\unused",
  "-Quiet"
)
Assert-True ($strayRecipient.ExitCode -ne 0) "room_post with -Recipient must fail."
Assert-True ($strayRecipient.Output -like "*-Recipient is supported only for -Action room_deliver*") "Expected the stray-recipient guard. Output: $($strayRecipient.Output)"

# With PRIM1_PANE_SECRET in the environment the connection opens with the
# secret preamble as its first line; the request follows on the second.
$secret = "secret-" + [guid]::NewGuid().ToString("N")
[Environment]::SetEnvironmentVariable("PRIM1_PANE_SECRET", $secret, "Process")
try {
  $withSecret = Invoke-CapturedRoomRequestWithPreamble -Prefix "prim1-room-preamble" -Arguments @(
    "-Action", "room_post",
    "-Content", "with secret"
  ) -ResponseMessage "room message posted"
} finally {
  [Environment]::SetEnvironmentVariable("PRIM1_PANE_SECRET", $null, "Process")
}
Assert-True ($null -ne $withSecret.Preamble) "With PRIM1_PANE_SECRET set, the first line must be the preamble."
Assert-True ($withSecret.Preamble.secret -ceq $secret) "The preamble carries the exact pane secret."
Assert-True (@($withSecret.Preamble.PSObject.Properties.Name).Count -eq 1) "The preamble carries the secret and nothing else."
Assert-True ($withSecret.Request.kind -ceq "room_post") "The request follows the preamble unchanged."
Assert-True (-not ($withSecret.Request.PSObject.Properties.Name -contains "secret")) "The secret never rides inside the request."

Write-Host "control-plane room tests passed"
exit 0
