param(
    [int]$Jobs = 1,
    [int]$DebounceMs = 900,
    [string]$Plugins,
    [switch]$Takeover,
    [switch]$Preview,
    [switch]$PreferDevelopmentPlugins,
    [switch]$NoWatch,
    [int]$SmokeTestSeconds = 0,
    [string]$Task = 'Run'
)
$ErrorActionPreference = 'Stop'
if ($Takeover -and $Preview) { throw 'Choose either -Takeover or -Preview.' }
$root = Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $root
$env:CARGO_BUILD_JOBS = "$Jobs"
$env:CARGO_TARGET_DIR = Join-Path $root 'target/dev-build'
$env:ULTRA_BAR_LOG_DIR = Join-Path $root 'target/logs'
$stateDir = Join-Path $root 'target/dev-watch'
$runDir = Join-Path $root 'target/dev-run'
New-Item -ItemType Directory -Force $stateDir, $runDir | Out-Null
$mutexName = if ($Task -eq 'Run') { 'Local\UltraBar.DevRunner' } else { 'Local\UltraBar.DevTask' }
$mutex = [Threading.Mutex]::new($false, $mutexName)
$ownsMutex = $false
$app = $null
$watchers = @()
$subscriptions = @()
$pending = $false
$lastChange = [DateTime]::MinValue
$exitCode = 0

function Stop-App {
    if ($script:app -and -not $script:app.HasExited) {
        & $script:app.StartInfo.FileName --request-exit | Out-Host
        if ($LASTEXITCODE -ne 0 -or -not $script:app.WaitForExit(2000)) {
            Write-Warning '[dev] Keeping the current app and unsaved windows; close the editor before retrying reload.'
            return $false
        }
    }
    if ($script:app) { $script:app.Dispose(); $script:app = $null }
    return $true
}

function Build-App {
    $operation = switch ($Task) { 'Check' { 'check' } 'Test' { 'test' } 'Clippy' { 'clippy' } default { 'build' } }
    if ($Task -eq 'PluginBuild') { return (Build-Plugins 'build') }
    Write-Host "[dev] cargo $operation --jobs $Jobs"
    & cargo $operation --workspace --jobs $Jobs | Out-Host
    if ($LASTEXITCODE -ne 0) { Write-Warning '[dev] build failed; existing application remains running'; return $false }
    if ($Task -ne 'TrayDiagnose' -and -not (Build-Plugins $operation)) { return $false }
    if ($Task -ne 'Run') { return $true }
    if (-not (Stop-App)) { return $false }
    Close-Logs
    # Recovery helpers can retain the previous image after the UI has exited.
    # Keep immutable development copies rather than overwriting their executable.
    $built = Join-Path $env:CARGO_TARGET_DIR 'debug/ultra-bar.exe'
    $hash = (Get-FileHash -LiteralPath $built -Algorithm SHA256).Hash.Substring(0,16).ToLowerInvariant()
    $release = Join-Path $runDir "releases/$hash"
    New-Item -ItemType Directory -Force $release | Out-Null
    $binary = Join-Path $release 'ultra-bar.exe'
    if (-not (Test-Path -LiteralPath $binary)) { Copy-Item -LiteralPath $built -Destination $binary }
    $start = [Diagnostics.ProcessStartInfo]::new($binary)
    $start.WorkingDirectory = $root
    $start.UseShellExecute = $false
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.ArgumentList.Add('--plugins'); $start.ArgumentList.Add($Plugins)
    if ($Takeover) { $start.ArgumentList.Add('--takeover') }
    if ($Preview) { $start.ArgumentList.Add('--preview') }
    if ($PreferDevelopmentPlugins) { $start.ArgumentList.Add('--prefer-dev-plugins') }
    if ($SmokeTestSeconds -gt 0) { $start.ArgumentList.Add('--smoke-seconds'); $start.ArgumentList.Add("$SmokeTestSeconds") }
    $script:app = [Diagnostics.Process]::Start($start)
    # Async stream copies avoid both pipe deadlocks and callbacks into PowerShell runspaces.
    $script:outFile = [IO.FileStream]::new((Join-Path $stateDir 'app.out.log'), 'Create', 'Write', 'ReadWrite', 1, $true)
    $script:errFile = [IO.FileStream]::new((Join-Path $stateDir 'app.err.log'), 'Create', 'Write', 'ReadWrite', 1, $true)
    $script:outCopy = $script:app.StandardOutput.BaseStream.CopyToAsync($script:outFile)
    $script:errCopy = $script:app.StandardError.BaseStream.CopyToAsync($script:errFile)
    Write-Host "[dev] started pid=$($script:app.Id); plugin saves reload without cargo"
    return $true
}

function Build-Plugins([string]$Operation) {
    if (-not (Test-Path -LiteralPath $Plugins -PathType Container)) { return $true }
    foreach ($directory in Get-ChildItem -LiteralPath $Plugins -Directory) {
        $manifestPath = Join-Path $directory.FullName 'plugin.json'
        $cargoPath = Join-Path $directory.FullName 'Cargo.toml'
        if (-not (Test-Path -LiteralPath $manifestPath) -or -not (Test-Path -LiteralPath $cargoPath)) { continue }
        $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
        if (-not $manifest.panel) { continue }
        $binaryName = if ($manifest.panel.binary) { [string]$manifest.panel.binary } else { [string]$manifest.id }
        if ($binaryName -notmatch '^[a-z0-9_-]+$') { throw "Invalid native plugin binary: $binaryName" }
        Write-Host "[dev] plugin=$($manifest.id) cargo $Operation --jobs $Jobs"
        & cargo $Operation --manifest-path $cargoPath --jobs $Jobs | Out-Host
        if ($LASTEXITCODE -ne 0) { return $false }
        if ($Operation -ne 'build') { continue }
        $binary = Join-Path $env:CARGO_TARGET_DIR "debug/$binaryName.exe"
        if (-not (Test-Path -LiteralPath $binary)) { throw "Missing plugin binary: $binary" }
        $hash = (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash.Substring(0,16).ToLowerInvariant()
        $runtime = Join-Path $directory.FullName '.runtime'
        $release = Join-Path $runtime "releases/$hash"
        New-Item -ItemType Directory -Force $release | Out-Null
        $destination = Join-Path $release "$binaryName.exe"
        if (-not (Test-Path -LiteralPath $destination)) { Copy-Item -LiteralPath $binary -Destination $destination }
        $pointerPath = Join-Path $runtime 'current.json'
        $pointer = @{ executable = "releases/$hash/$binaryName.exe" } | ConvertTo-Json -Compress
        if (-not (Test-Path -LiteralPath $pointerPath) -or (Get-Content -LiteralPath $pointerPath -Raw).Trim() -ne $pointer) { [IO.File]::WriteAllText($pointerPath, $pointer) }
    }
    return $true
}

function Close-Logs {
    if ($script:outCopy) { $script:outCopy.GetAwaiter().GetResult(); $script:outCopy = $null }
    if ($script:errCopy) { $script:errCopy.GetAwaiter().GetResult(); $script:errCopy = $null }
    if ($script:outFile) { $script:outFile.Dispose(); $script:outFile = $null }
    if ($script:errFile) { $script:errFile.Dispose(); $script:errFile = $null }
}

try {
    try { $ownsMutex = $mutex.WaitOne(0) } catch [Threading.AbandonedMutexException] { $ownsMutex = $true }
    if (-not $ownsMutex) { throw 'Another runner for this task is already active.' }
    if ($Task -eq 'Run' -and -not (Test-Path -LiteralPath $Plugins -PathType Container)) { throw "Plugin directory does not exist: $Plugins" }
    if ($Task -eq 'Run' -and -not $NoWatch -and $SmokeTestSeconds -eq 0) {
        # Register before the first build so changes made during compilation are retained.
        foreach ($directory in @((Join-Path $root 'src'), (Join-Path $root 'crates'), (Join-Path $root '.cargo'), $root, $Plugins)) {
            $watcher = [IO.FileSystemWatcher]::new($directory)
            $watcher.IncludeSubdirectories = $directory -ne $root
            $watcher.NotifyFilter = [IO.NotifyFilters]'FileName, LastWrite, Size, DirectoryName'
            foreach ($kind in @('Changed', 'Created', 'Deleted', 'Renamed', 'Error')) {
                $subscriptions += Register-ObjectEvent -InputObject $watcher -EventName $kind -SourceIdentifier "UltraBar.Dev.$([guid]::NewGuid())"
            }
            $watcher.EnableRaisingEvents = $true
            $watchers += $watcher
        }
    }
    $ok = Build-App
    if ($Task -eq 'TrayDiagnose' -and $ok) {
        & (Join-Path $env:CARGO_TARGET_DIR 'debug/ultra-bar.exe') --tray-diagnose | Out-Host
        if ($LASTEXITCODE -ne 0) { $exitCode = $LASTEXITCODE }
    }
    if ($Task -ne 'Run') { if (-not $ok) { $exitCode = 1 } }
    elseif ($SmokeTestSeconds -gt 0 -or $NoWatch) {
        if ($ok) {
            $app.WaitForExit()
            $exitCode = $app.ExitCode
        } else { $exitCode = 1 }
    }
    else {
        Write-Host '[dev] watching Rust/config files; Ctrl+C stops the app. Logs: target/dev-watch/app.err.log'
        while ($true) {
            $null = Wait-Event -Timeout 1
            foreach ($event in @(Get-Event)) {
                if ($event.SourceIdentifier -notlike 'UltraBar.Dev.*') { continue }
                $path = $event.SourceEventArgs.FullPath
                if ($event.SourceEventArgs -is [IO.ErrorEventArgs] -or $path -match '\.rs$|[\\/](Cargo\.toml|Cargo\.lock|config\.toml)$') {
                    $pending = $true; $lastChange = [DateTime]::UtcNow
                    Write-Host "[dev] changed: $path"
                }
                Remove-Event -EventIdentifier $event.EventIdentifier
            }
            if ($pending -and ([DateTime]::UtcNow - $lastChange).TotalMilliseconds -ge $DebounceMs) {
                $pending = $false
                $ok = Build-App
            }
            if ($app -and $app.HasExited) {
                Write-Warning "[dev] app exited ($($app.ExitCode)); save a Rust file to restart"
                $null = Stop-App; Close-Logs
            }
        }
    }
}
finally {
    foreach ($subscription in $subscriptions) { Unregister-Event -SubscriptionId $subscription.SubscriptionId }
    foreach ($watcher in $watchers) { $watcher.Dispose() }
    while (-not (Stop-App)) { Start-Sleep -Seconds 2 }
    Close-Logs
    if ($ownsMutex) { $mutex.ReleaseMutex() }
    $mutex.Dispose()
}
exit $exitCode
