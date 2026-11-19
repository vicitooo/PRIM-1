<#
Pure held-trial parameter checks for Windows PowerShell 5.1.
Only two helper definitions are extracted; the preview dispatcher never runs.
All existence checks are injected fixtures: no runtime, task or process access.
#>
[CmdletBinding()]
param([string]$PreviewScript)
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
if ([string]::IsNullOrWhiteSpace($PreviewScript)) {
  $PreviewScript = Join-Path $PSScriptRoot "v2-preview.ps1"
}

function Assert-Fixture([bool]$Condition, [string]$Message) {
  if (-not $Condition) { throw $Message }
}

$tokens = $null
$parseErrors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile(
  $PreviewScript, [ref]$tokens, [ref]$parseErrors)
Assert-Fixture ($parseErrors.Count -eq 0) "preview script has parser errors: $parseErrors"
foreach ($name in @("Test-PathInside", "New-TrialEnvironmentLines")) {
  $definitions = @($ast.FindAll({ param($node)
    $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name
  }, $true))
  Assert-Fixture ($definitions.Count -eq 1) "expected exactly one $name helper"
  . ([scriptblock]::Create($definitions[0].Extent.Text))
}

$attempt = "a1234567-89ab-4cde-8fab-0123456789ab"
$attemptRoot = "C:\pure-fixture\$attempt"
$base = @{
  HasAttempt = $true; HasWindow = $true; AttemptId = $attempt
  HealthWindowSeconds = "30"; Root = $attemptRoot; AgentWorkingRoot = "C:\pure-fixture-work"
}
$script:cases = 0
function Assert-Refused([string]$Name, [hashtable]$Overrides, [string]$Expected) {
  $arguments = $base.Clone()
  foreach ($key in $Overrides.Keys) { $arguments[$key] = $Overrides[$key] }
  $failure = $null
  try { $null = @(New-TrialEnvironmentLines @arguments -PathExists { $true }) }
  catch { $failure = $_.Exception.Message }
  Assert-Fixture ($null -ne $failure -and $failure.Contains($Expected)) "$Name did not refuse as expected: $failure"
  $script:cases++
}

$normal = @(New-TrialEnvironmentLines -HasAttempt $false -HasWindow $false `
  -PathExists { throw "normal startup must not inspect trial paths" })
Assert-Fixture ($normal.Count -eq 0) "normal startup emitted trial variables"
$script:cases++
Assert-Refused "attempt only" @{ HasWindow = $false } "requires both"
Assert-Refused "window only" @{ HasAttempt = $false } "requires both"
foreach ($invalid in @("", " ", "00000000-0000-0000-0000-000000000000", $attempt.ToUpperInvariant(),
    "{$attempt}", "not-a-uuid")) {
  Assert-Refused "invalid UUID '$invalid'" @{ AttemptId = $invalid } "canonical non-nil UUID"
}
foreach ($invalid in @("", " ", "0", "301", "-1", "1.5", "30s", "2147483648")) {
  Assert-Refused "invalid window '$invalid'" @{ HealthWindowSeconds = $invalid } "integer in 1..300"
}
Assert-Refused "mismatched basename" @{ Root = "C:\pure-fixture\another-attempt" } "exact TrialAttemptId"
Assert-Refused "uppercase basename" @{ Root = "C:\pure-fixture\$($attempt.ToUpperInvariant())" } "exact TrialAttemptId"
foreach ($overlap in @($attemptRoot, "$attemptRoot\work", "C:\pure-fixture")) {
  Assert-Refused "working-root overlap '$overlap'" @{ AgentWorkingRoot = $overlap } "outside the attempt"
}

$required = @(
  "$attemptRoot\runtime|Container",
  "$attemptRoot\profile|Container",
  "$attemptRoot\runtime\session-catalog-v1.json|Leaf",
  "$attemptRoot\runtime\room-catalog-v1.json|Leaf"
)
foreach ($missing in $required) {
  $pathExists = { param($Path, $Kind) "$Path|$Kind" -ne $missing }.GetNewClosure()
  $failure = $null
  try { $null = @(New-TrialEnvironmentLines @base -PathExists $pathExists) }
  catch { $failure = $_.Exception.Message }
  Assert-Fixture ($null -ne $failure -and $failure.Contains("requires existing staged")) "missing $missing was not refused"
  $script:cases++
}
foreach ($window in @("1", "300")) {
  $arguments = $base.Clone()
  $arguments.HealthWindowSeconds = $window
  $checks = [System.Collections.Generic.List[string]]::new()
  $pathExists = { param($Path, $Kind) $checks.Add("$Path|$Kind"); $true }.GetNewClosure()
  $lines = @(New-TrialEnvironmentLines @arguments -PathExists $pathExists)
  $expected = @(
    "PRIM1_TRIAL_ATTEMPT_ID='$attempt'",
    "PRIM1_TRIAL_ROOT='C:/pure-fixture/$attempt'",
    "PRIM1_TRIAL_HEALTH_WINDOW_SECONDS='$window'"
  )
  Assert-Fixture (($lines -join "`n") -ceq ($expected -join "`n")) "window $window produced incorrect trial environment"
  Assert-Fixture (($checks -join "`n") -ceq ($required -join "`n")) "window $window did not check the required staged paths and types"
  $script:cases++
}
Write-Output "PASS: $script:cases pure held-trial parameter cases; launcher parsed; no runtime, process or task access"
