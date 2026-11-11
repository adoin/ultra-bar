[CmdletBinding()]
param([switch]$SkipBuild, [switch]$IsolationTest, [string]$OutputDirectory)
$ErrorActionPreference = 'Stop'
if ($PSVersionTable.PSVersion.Major -lt 7 -or -not $IsWindows) { throw 'Use PowerShell 7 on Windows.' }
$root = Split-Path -Parent $PSScriptRoot
$cargo = (Get-Command cargo.exe -ErrorAction Stop).Source
$iscc = (Get-Command ISCC.exe -ErrorAction SilentlyContinue).Source
if (-not $iscc) { $iscc = 'C:/Program Files (x86)/Inno Setup 6/ISCC.exe' }
if (-not (Test-Path -LiteralPath $iscc)) { throw 'Install Inno Setup 6 (ISCC.exe).' }
$metadata = (& $cargo metadata --no-deps --format-version 1 --manifest-path "$root/Cargo.toml" | ConvertFrom-Json)
if ($LASTEXITCODE) { throw 'cargo metadata failed' }
$version = ($metadata.packages | Where-Object name -eq 'ultra-bar').version
if ($version -notmatch '^\d+\.\d+\.\d+$') { throw 'Installer requires a numeric major.minor.patch version.' }
$target = Join-Path $root 'target/release-build'
$binary = Join-Path $target 'release/ultra-bar.exe'
if (-not $SkipBuild) {
    Add-Type -Path "$PSScriptRoot/DevProcess.cs"
    $previousTarget = $env:CARGO_TARGET_DIR
    try {
        $env:CARGO_TARGET_DIR = $target
        Write-Host '[package] release; jobs=1; CPU cap=20%; processors=2; priority=BelowNormal'
        $code = [UltraBar.DevProcess]::Run($cargo, @('build','--release','--locked','--jobs','1','--package','ultra-bar'), $root, 20, 2)
        if ($code) { throw "Release build failed: $code" }
    } finally { $env:CARGO_TARGET_DIR = $previousTarget }
}
if (-not (Test-Path -LiteralPath $binary)) { throw 'Release binary not found. Run without -SkipBuild.' }
if ((Get-Item -LiteralPath $binary).VersionInfo.ProductVersion -ne $version) { throw 'Binary version does not match Cargo.toml.' }
$stage = Join-Path $root 'target/installer-payload'
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item -LiteralPath $binary -Destination "$stage/ultra-bar.exe" -Force
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
if (-not (Test-Path -LiteralPath $vswhere)) { throw 'Visual Studio vswhere.exe is required to locate the redistributable runtime.' }
$vs = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $vs) { throw 'MSVC installation not found.' }
$runtime = Get-ChildItem -LiteralPath "$vs/VC/Redist/MSVC" -Directory | ForEach-Object {
    Get-Item -LiteralPath "$($_.FullName)/x64/Microsoft.VC143.CRT/vcruntime140.dll" -ErrorAction SilentlyContinue
} | Sort-Object { $_.VersionInfo.FileVersionRaw } -Descending | Select-Object -First 1
if (-not $runtime) { throw 'Install the Visual Studio 2022 x64 redistributable runtime component.' }
Copy-Item -LiteralPath $runtime.FullName -Destination "$stage/vcruntime140.dll" -Force
$commit = & git -C $root rev-parse HEAD 2>$null
if ($LASTEXITCODE) { $commit = 'uncommitted' }
$info = [ordered]@{ product='Ultra Bar'; version=$version; architecture='windows-x86_64'; configuration='release'; commit=$commit; sourceDirty=[bool](& git -C $root status --porcelain); builtAt=[DateTime]::UtcNow.ToString('o'); sha256=(Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash.ToLowerInvariant() }
$info.runtime = @{ name='vcruntime140.dll'; version=$runtime.VersionInfo.FileVersion; sha256=(Get-FileHash -LiteralPath $runtime.FullName -Algorithm SHA256).Hash.ToLowerInvariant(); source='Visual Studio VC/Redist/MSVC/x64/Microsoft.VC143.CRT' }
$info | ConvertTo-Json | Set-Content -LiteralPath "$stage/build-info.json" -Encoding utf8NoBOM
# Include the exact dependency licenses from this build's Cargo metadata, rather than
# assuming that a license expression alone satisfies redistribution obligations.
$full = (& $cargo metadata --locked --format-version 1 --manifest-path "$root/Cargo.toml" | ConvertFrom-Json)
if ($LASTEXITCODE) { throw 'Dependency metadata failed' }
$notices = [Text.StringBuilder]::new("Ultra Bar third-party dependency notices`n`n")
[void]$notices.AppendLine("Microsoft Visual C++ Runtime $($runtime.VersionInfo.FileVersion): vcruntime140.dll")
[void]$notices.AppendLine('Copyright Microsoft Corporation. Distributed under the Microsoft Visual Studio distributable code terms.')
[void]$notices.AppendLine('https://learn.microsoft.com/visualstudio/releases/2022/redistribution')
[void]$notices.AppendLine()
foreach ($package in ($full.packages | Sort-Object name,version)) {
    [void]$notices.AppendLine("$($package.name) $($package.version) | $($package.license) | $($package.repository)")
    $directory = Split-Path -Parent $package.manifest_path
    $licenses = @(Get-ChildItem -LiteralPath $directory -File | Where-Object Name -Match '^(LICENSE|LICENCE|COPYING|NOTICE)(\.|-|$)')
    if ($package.license_file) { $licenses += Get-Item -LiteralPath (Join-Path $directory $package.license_file) }
    foreach ($license in ($licenses | Sort-Object FullName -Unique)) {
        [void]$notices.AppendLine("--- $($license.Name) ---")
        [void]$notices.AppendLine([IO.File]::ReadAllText($license.FullName))
    }
    [void]$notices.AppendLine()
}
[IO.File]::WriteAllText("$stage/THIRD-PARTY-NOTICES.txt",$notices.ToString(),[Text.UTF8Encoding]::new($false))
if (-not $OutputDirectory) { $OutputDirectory = if ($IsolationTest) { "$root/target/installer-test" } else { "$root/dist" } }
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
$name = "UltraBar-$version-windows-x86_64-setup"
if ($IsolationTest) { $name += '-ISOLATED-TEST' }
$arguments = @("/DAppVersion=$version", "/DPayload=$stage", "/DOutputPath=$OutputDirectory", "/DOutputName=$name")
if ($IsolationTest) { $arguments += '/DIsolationTest=1' }
& $iscc @arguments "$root/installer/ultra-bar.iss"
if ($LASTEXITCODE) { throw "Installer compilation failed: $LASTEXITCODE" }
$installer = Join-Path $OutputDirectory "$name.exe"
"$((Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash.ToLowerInvariant())  $name.exe" | Set-Content -LiteralPath "$installer.sha256" -Encoding ascii
Copy-Item -LiteralPath "$stage/build-info.json" -Destination "$OutputDirectory/$name.build-info.json" -Force
Write-Host "[package] $installer"
