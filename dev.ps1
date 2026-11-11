# Entry point for a project environment: pwsh.exe -NoLogo -NoProfile -File ./dev.ps1
[CmdletBinding()]
param(
    [ValidateRange(1, 4)][int]$Jobs = 1,
    [ValidateRange(1, 100)][int]$CpuPercent = 20,
    [ValidateRange(1, 64)][int]$BuildCores = 2,
    [ValidateRange(200, 10000)][int]$DebounceMs = 900,
    [string]$Plugins = (Join-Path $PSScriptRoot '../ultra-bar-plugins'),
    [switch]$Takeover,
    [switch]$Preview,
    [switch]$PreferDevelopmentPlugins,
    [switch]$NoWatch,
    [ValidateRange(0, 3600)][int]$SmokeTestSeconds = 0,
    [ValidateSet('Run', 'Check', 'Test', 'Clippy', 'TrayDiagnose', 'PluginBuild')][string]$Task = 'Run'
)
$ErrorActionPreference = 'Stop'
if ($PSVersionTable.PSVersion.Major -lt 7) { throw 'Use PowerShell 7 (pwsh.exe).' }
if (-not $IsWindows) { throw 'Ultra Bar requires Windows.' }
if ($Takeover -and $Preview) { throw 'Choose either -Takeover or -Preview.' }
Add-Type -Path (Join-Path $PSScriptRoot 'scripts/DevProcess.cs')
$runner = Join-Path $PSScriptRoot 'scripts/dev-runner.ps1'
$resolvedPlugins = [IO.Path]::GetFullPath($Plugins, $PSScriptRoot)
$arguments = @('-NoLogo', '-NoProfile', '-File', $runner, '-Jobs', "$Jobs", '-DebounceMs', "$DebounceMs", '-Plugins', $resolvedPlugins, '-SmokeTestSeconds', "$SmokeTestSeconds", '-Task', $Task)
if ($Takeover) { $arguments += '-Takeover' }
if ($Preview) { $arguments += '-Preview' }
if ($PreferDevelopmentPlugins) { $arguments += '-PreferDevelopmentPlugins' }
if ($NoWatch) { $arguments += '-NoWatch' }
Write-Host "[dev] jobs=$Jobs; CPU hard cap=$CpuPercent%; logical processors=$BuildCores; priority=BelowNormal"
$code = [UltraBar.DevProcess]::Run((Get-Process -Id $PID).Path, $arguments, $PSScriptRoot, $CpuPercent, $BuildCores)
exit $code
