# Run the actual release payload through a separate Inno product identity and
# private association. Never installs over the user's real application/data.
[CmdletBinding()]
param([string]$TestRoot = 'D:/tmp/ultra-bar-installer-20261110', [string]$InstallerPath, [switch]$DevelopmentPayload)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$TestRoot = [IO.Path]::GetFullPath($TestRoot)
if (-not $TestRoot.StartsWith('D:\tmp\',[StringComparison]::OrdinalIgnoreCase)) { throw 'TestRoot must be a task directory below D:/tmp.' }
New-Item -ItemType Directory -Force $TestRoot | Out-Null
if (-not ('UltraBarInstallerIcons' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class UltraBarInstallerIcons {
    [DllImport("shell32.dll", CharSet=CharSet.Unicode)]
    public static extern uint ExtractIconEx(string file, int index, IntPtr large, IntPtr small, uint count);
}
'@
}
if ($InstallerPath) {
    $resolvedInstaller=[IO.Path]::GetFullPath($InstallerPath)
    if (-not $DevelopmentPayload -or -not $resolvedInstaller.StartsWith($TestRoot+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)) {throw 'A development installer must be explicitly marked and inside TestRoot.'}
    $installer=Get-Item -LiteralPath $resolvedInstaller
    if ($installer.VersionInfo.ProductName.Trim() -ne 'Ultra Bar Installer Test') {throw 'Incorrect isolated product identity'}
} else {
    & "$PSScriptRoot/package.ps1" -SkipBuild -IsolationTest
    $installer = Get-ChildItem -LiteralPath "$root/target/installer-test" -Filter '*-ISOLATED-TEST.exe' | Sort-Object LastWriteTime -Descending | Select-Object -First 1
}
if (-not $installer) { throw 'Missing isolated installer' }
$installDir = Join-Path $TestRoot 'Install With Spaces'
$profile = Join-Path $TestRoot 'Profile'
New-Item -ItemType Directory -Force $profile | Out-Null
$installerData=Join-Path $TestRoot 'Installer Local App Data'
$startupProfile=Join-Path $installerData 'UltraBar'
New-Item -ItemType Directory -Force $startupProfile | Out-Null
$startupKey='Registry::HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run'
function Startup-Value([string]$Name) {if(Test-Path $startupKey){(Get-Item $startupKey).GetValue($Name)}}
$startupBefore=Startup-Value 'Ultra Bar'
$exe = Join-Path $installDir 'ultra-bar.exe'
$association = 'Registry::HKEY_CURRENT_USER\Software\Classes\UltraBar.PluginPackage.InstallerTest.1'
$realAssociation = 'Registry::HKEY_CURRENT_USER\Software\Classes\UltraBar.PluginPackage.1\shell\open\command'
$realBefore = if (Test-Path $realAssociation) { (Get-Item $realAssociation).GetValue('') } else { $null }
$devBefore = @(Get-CimInstance Win32_Process -Filter "Name = 'ultra-bar.exe'" | Where-Object ExecutablePath -Like "$root\target\dev-run\*").ProcessId
function Assert([bool]$Condition, [string]$Message) { if (-not $Condition) { throw $Message } }
function Install([string]$Name, [switch]$ReusePrevious) {
    $info=[Diagnostics.ProcessStartInfo]::new($installer.FullName)
    $info.UseShellExecute=$false
    $info.WindowStyle=[Diagnostics.ProcessWindowStyle]::Hidden
    foreach($arg in @('/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART',"/LOG=$TestRoot/$Name.log")){$info.ArgumentList.Add($arg)}
    if(-not $ReusePrevious){$info.ArgumentList.Add("/DIR=$installDir")}
    $info.Environment['LOCALAPPDATA']=$installerData
    $p=[Diagnostics.Process]::Start($info);$p.WaitForExit()
    return $p.ExitCode
}
function Start-TestApp([switch]$Busy) {
    $args = @('--installer-test','--plugins',"`"$installDir/plugins`"",'--profile-root',"`"$profile`"")
    if ($Busy) { $args += '--test-modal' }
    $p = Start-Process -FilePath $exe -ArgumentList $args -WindowStyle Hidden -PassThru
    Start-Sleep -Seconds 2
    Assert (-not $p.HasExited) 'Isolated app did not start'
    return $p
}
$app = $null
try {
    Assert ((Install 'fresh') -eq 0) 'Fresh install failed'
    Assert (Test-Path -LiteralPath $exe) 'Missing installed executable'
    $payloadSha256=(Get-FileHash -LiteralPath $exe).Hash.ToLowerInvariant()
    Assert ((Startup-Value 'Ultra Bar Installer Test') -eq "`"$exe`" --from-startup") 'Logon startup must be enabled by default'
    Assert ((Get-Item -LiteralPath $exe).VersionInfo.ProductName -eq 'Ultra Bar') 'Missing EXE resource'
    Assert ([UltraBarInstallerIcons]::ExtractIconEx($exe,-1,[IntPtr]::Zero,[IntPtr]::Zero,0) -gt 0) 'EXE has no icon resource'
    Assert ([UltraBarInstallerIcons]::ExtractIconEx($installer.FullName,-1,[IntPtr]::Zero,[IntPtr]::Zero,0) -gt 0) 'Installer has no icon resource'
    $handler = (Get-Item "$association\shell\open\command").GetValue('')
    Assert ($handler -eq "`"$exe`" --install `"%1`" --force") 'File association quoting or path is wrong'
    $uninstallKey = 'Registry::HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Uninstall\{B90D83EE-2C79-459E-8D36-96033E55D8DC}_is1'
    $entry = Get-ItemProperty $uninstallKey
    Assert ($entry.DisplayIcon -eq $exe) 'Uninstall display icon path is wrong'
    $shortcutPath = Join-Path ([Environment]::GetFolderPath('Programs')) 'Ultra Bar Installer Test/Ultra Bar Installer Test.lnk'
    Assert (Test-Path $shortcutPath) 'Missing Start menu shortcut'
    $shell = New-Object -ComObject WScript.Shell
    $shortcut = $shell.CreateShortcut($shortcutPath)
    Assert ($shortcut.TargetPath -eq $exe) 'Shortcut target does not match installation path'
    [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($shortcut)
    [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($shell)
    $desktopShortcut = Join-Path ([Environment]::GetFolderPath('DesktopDirectory')) 'Ultra Bar Installer Test.lnk'
    Assert (-not (Test-Path $desktopShortcut)) 'Desktop shortcut must be optional and unchecked'
    $nativeDirectory=Join-Path $TestRoot 'Native Task With Spaces'
    $native=Start-Process -FilePath $exe -ArgumentList @('--jump-list-smoke',"`"$nativeDirectory`"") -WindowStyle Hidden -PassThru
    if (-not $native.WaitForExit(20000)) {
        $identity=Get-CimInstance Win32_Process -Filter "ProcessId = $($native.Id)"
        if ($identity.ExecutablePath -eq $exe) { Stop-Process -Id $native.Id }
        throw 'Installed native task self-test timed out'
    }
    Assert ($native.ExitCode -eq 0) 'Installed Jump List helper or saved task failed with spaces in the EXE path'
    $nativeResult=Get-Content -LiteralPath "$nativeDirectory/result.json" -Raw | ConvertFrom-Json
    Assert ($nativeResult.async_reader_cache_and_cleanup -eq 'passed') 'Installed Jump List cache cleanup failed'
    '{"icons_only":true,"clock_format":"HH:mm"}' | Set-Content -LiteralPath "$profile/settings.json"
    'keep-calendar-and-plugins' | Set-Content -LiteralPath "$profile/data-marker.txt"
    $settingsBefore = (Get-FileHash -LiteralPath "$profile/settings.json").Hash
    '{"startup_enabled":false}' | Set-Content -LiteralPath "$startupProfile/settings.json"
    $app = Start-TestApp
    $loadedRuntime = $app.Modules | Where-Object ModuleName -IEQ 'vcruntime140.dll' | Select-Object -First 1
    Assert ($loadedRuntime.FileName -eq (Join-Path $installDir 'vcruntime140.dll')) 'App did not load its bundled runtime'
    Assert ((Install 'upgrade-running' -ReusePrevious) -eq 0) 'Running upgrade or remembered installation directory failed'
    Assert ($app.WaitForExit(3000)) 'Upgrade did not exit the installed process'
    Assert (-not (Startup-Value 'Ultra Bar Installer Test')) 'Upgrade re-enabled disabled startup'
    Assert ((Get-FileHash -LiteralPath "$profile/settings.json").Hash -eq $settingsBefore) 'Upgrade changed settings'
    $app = Start-TestApp -Busy
    $binaryBefore = (Get-FileHash -LiteralPath $exe).Hash
    Assert ((Install 'upgrade-modal') -ne 0) 'Upgrade must refuse when a modal dialog is open'
    Assert (-not $app.HasExited) 'Busy app was forcibly closed'
    Assert ((Get-FileHash -LiteralPath $exe).Hash -eq $binaryBefore) 'Busy upgrade replaced the executable'
    # Only this recorded, isolated test process may be terminated to release the
    # deliberately disabled test window. Verify PID identity immediately first.
    $identity = Get-CimInstance Win32_Process -Filter "ProcessId = $($app.Id)"
    Assert ($identity.ExecutablePath -eq $exe) 'Test process identity changed'
    Stop-Process -Id $app.Id
    Assert ($app.WaitForExit(3000)) 'Test process did not exit'
    '{"startup_enabled":true}' | Set-Content -LiteralPath "$startupProfile/settings.json"
    $startupSync=Start-Process -FilePath $exe -ArgumentList @('--sync-startup','--installer-test','--profile-root',"`"$startupProfile`"") -WindowStyle Hidden -PassThru -Wait
    Assert ($startupSync.ExitCode -eq 0 -and [bool](Startup-Value 'Ultra Bar Installer Test')) 'Explicit startup enable failed'
    $app = Start-TestApp
    $uninstall = Start-Process -FilePath "$installDir/unins000.exe" -ArgumentList @('/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART',"/LOG=`"$TestRoot/uninstall.log`"") -WindowStyle Hidden -PassThru -Wait
    Assert ($uninstall.ExitCode -eq 0) 'Uninstall failed'
    Assert ($app.WaitForExit(3000)) 'Uninstall did not exit the installed app'
    Assert (-not (Test-Path -LiteralPath $exe)) 'Executable remains after uninstall'
    Assert (-not (Startup-Value 'Ultra Bar Installer Test')) 'Owned startup entry remains after uninstall'
    Assert (-not (Test-Path -LiteralPath $association)) 'Owned ProgID remains after uninstall'
    $privateExtension = 'Registry::HKEY_CURRENT_USER\Software\Classes\.ubplugin-ultrabar-test'
    if (Test-Path -LiteralPath $privateExtension) {
        Assert (-not (Get-Item $privateExtension).GetValue('')) 'Private extension still has an association'
        # These exact keys were created solely by this isolated test product.
        Remove-Item -LiteralPath $privateExtension -Recurse
    }
    Assert (-not (Test-Path $uninstallKey)) 'Uninstall entry remains'
    Assert (-not (Test-Path $shortcutPath)) 'Start menu shortcut remains'
    Assert ((Get-FileHash -LiteralPath "$profile/settings.json").Hash -eq $settingsBefore) 'Uninstall changed settings'
    Assert ((Get-Content -LiteralPath "$profile/data-marker.txt" -Raw).Trim() -eq 'keep-calendar-and-plugins') 'User data was removed'
    $realAfter = if (Test-Path $realAssociation) { (Get-Item $realAssociation).GetValue('') } else { $null }
    Assert ($realAfter -eq $realBefore) 'Production association was changed by tests'
    Assert ((Startup-Value 'Ultra Bar') -eq $startupBefore) 'Production startup entry was changed by tests'
    foreach ($id in $devBefore) { Assert ([bool](Get-Process -Id $id -ErrorAction SilentlyContinue)) 'Existing development instance was closed' }
    [ordered]@{ result='passed'; configuration=$(if($DevelopmentPayload){'development'}else{'release'}); installer=$installer.FullName; payloadSha256=$payloadSha256; scenarios=@('fresh install with spaces','private file association','EXE and installer icon resources','uninstall icon and Start menu shortcut','bundled runtime loaded','running upgrade with remembered custom directory','modal upgrade refusal','running uninstall','settings and data preservation','production association unchanged','development instance unchanged','default logon startup','disabled startup preserved on upgrade','owned startup removed on uninstall','production startup unchanged') } | ConvertTo-Json | Set-Content -LiteralPath "$TestRoot/result.json"
    Write-Host "[installer-test] all scenarios passed: $TestRoot/result.json"
} finally {
    if ($app -and -not $app.HasExited) {
        $identity = Get-CimInstance Win32_Process -Filter "ProcessId = $($app.Id)"
        if ($identity.ExecutablePath -eq $exe) { Stop-Process -Id $app.Id }
    }
}
