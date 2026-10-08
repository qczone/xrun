param(
    [string]$ProfileDir,
    [string]$ArtifactDir,
    [switch]$Release,
    [ValidateSet('x86_64', 'arm64')][string]$Arch,
    [string]$Target
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$root = Split-Path $PSScriptRoot -Parent
$buildProfileDir = [IO.Path]::GetFullPath($ProfileDir)
$testDir = Join-Path ([IO.Path]::GetTempPath()) "xrun-installer-test-$([Guid]::NewGuid())"
[void](New-Item -ItemType Directory -Path $testDir)
$sandboxHome = Join-Path $testDir 'home'
[void](New-Item -ItemType Directory -Path $sandboxHome)
$userEnvironment = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Environment')
$rawValue = [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames
$savedPath = $userEnvironment.GetValue('Path', $null, $rawValue)
$savedPathKind = if ($null -ne $savedPath) { $userEnvironment.GetValueKind('Path') } else { $null }
$machinePath = [Environment]::GetEnvironmentVariable('Path', 'Machine')

Add-Type -Namespace XrunInstallerTests -Name EnvironmentNotification -MemberDefinition @'
    [System.Runtime.InteropServices.DllImport("user32.dll", CharSet = System.Runtime.InteropServices.CharSet.Unicode)]
    public static extern System.IntPtr SendMessageTimeout(
        System.IntPtr window, uint message, System.UIntPtr parameter, string value,
        uint flags, uint timeout, System.IntPtr result);
'@

function Assert-That([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}
function Assert-BinaryArchitecture([string]$File, [switch]$Console) {
    # The NSIS bootstrap is x86 even for ARM64 apps; check its installed payload instead.
    $bytes = [IO.File]::ReadAllBytes($File)
    $peOffset = [BitConverter]::ToInt32($bytes, 0x3c)
    Assert-That ($peOffset -ge 0 -and $peOffset + 6 -le $bytes.Length) "Invalid PE executable: $File"
    Assert-That ([BitConverter]::ToUInt32($bytes, $peOffset) -eq 0x00004550) "Missing PE signature: $File"
    $machine = [BitConverter]::ToUInt16($bytes, $peOffset + 4)
    $expected = if ($Arch -eq 'arm64') { 0xaa64 } else { 0x8664 }
    Assert-That ($machine -eq $expected) "Installed executable has the wrong architecture: $File"
    if ($Console) {
        Assert-That ([BitConverter]::ToUInt16($bytes, $peOffset + 0x5c) -eq 3) 'Terminal CLI must use the console subsystem.'
    }
}
function Invoke-Process([string]$File, [string]$Arguments, [int]$Timeout = 120000, [string]$SearchPath) {
    $info = New-Object Diagnostics.ProcessStartInfo
    $info.FileName = $File
    $info.Arguments = $Arguments
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.EnvironmentVariables['HOME'] = $sandboxHome
    if ($SearchPath) { $info.EnvironmentVariables['PATH'] = $SearchPath }
    # Process.Start bypasses PowerShell 7's Windows PowerShell module-path handling.
    $info.EnvironmentVariables.Remove('PSModulePath')
    $process = New-Object Diagnostics.Process
    $process.StartInfo = $info
    try {
        [void]$process.Start()
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit($Timeout)) {
            $process.Kill()
            throw "Process hung, possibly on an unexpected dialog: $File"
        }
        return [pscustomobject]@{
            ExitCode = $process.ExitCode
            Stdout = $stdout.GetAwaiter().GetResult().Trim()
            Stderr = $stderr.GetAwaiter().GetResult().Trim()
        }
    } finally { $process.Dispose() }
}
function Run-Installer([string]$Directory, [string]$Source = $ArtifactDir) {
    $arguments = "-NoProfile -NonInteractive -ExecutionPolicy Bypass -File `"$root\scripts\install.ps1`" -Version $version -InstallDir `"$Directory`" -SourceDir `"$Source`""
    $result = Invoke-Process 'powershell.exe' $arguments
    $data = $result.Stdout | ConvertFrom-Json
    return [pscustomobject]@{ ExitCode = $result.ExitCode; Data = $data }
}

function Assert-TerminalCommand([string]$Directory) {
    # A child otherwise inherits this already-running runner's stale PATH. Rebuild
    # its environment from the persisted values, as a newly opened terminal does.
    $userPath = $userEnvironment.GetValue('Path', '', $rawValue)
    $searchPath = [Environment]::ExpandEnvironmentVariables("$machinePath;$userPath")
    $command = '-NoProfile -NonInteractive -Command "$ErrorActionPreference=''Stop''; (Get-Command xrun -CommandType Application).Source; xrun --version; exit $LASTEXITCODE"'
    $result = Invoke-Process 'powershell.exe' $command 30000 $searchPath
    $lines = $result.Stdout -split '\r?\n'
    Assert-That ($result.ExitCode -eq 0) "PowerShell could not run xrun: $($result.Stderr)"
    Assert-That ($lines[0] -ieq (Join-Path $Directory 'xrun.exe')) 'PowerShell resolved the wrong CLI.'
    Assert-That ($lines[-1] -ceq "xrun $version") 'PowerShell CLI version output is missing or incorrect.'
    $result = Invoke-Process 'cmd.exe' '/d /c "xrun --version"' 30000 $searchPath
    Assert-That ($result.ExitCode -eq 0 -and $result.Stdout -ceq "xrun $version") 'CMD could not run xrun and receive its version output.'
}

try {
    $nativeArch = switch ([Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()) {
        'X64' { 'x86_64' }
        'Arm64' { 'arm64' }
        default { throw 'Installer tests require Windows x86_64 or arm64.' }
    }
    if (-not $Arch) { $Arch = $nativeArch }
    Assert-That ($Arch -eq $nativeArch) 'Windows installer tests must run on the target architecture.'
    $platform = "windows-$Arch"
    if ($Target) {
        $expectedTarget = if ($Arch -eq 'arm64') { 'aarch64-pc-windows-msvc' } else { 'x86_64-pc-windows-msvc' }
        Assert-That ($Target -ceq $expectedTarget) 'Rust target does not match the requested architecture.'
    }
    if (-not $ArtifactDir) { $ArtifactDir = Join-Path $testDir 'artifacts' }
    $ArtifactDir = [IO.Path]::GetFullPath($ArtifactDir)
    [void](New-Item -ItemType Directory -Path $ArtifactDir -Force)
    $installer = Join-Path $ArtifactDir "xrun-app-$platform.exe"
    if (-not (Test-Path -LiteralPath $installer)) {
        $bundles = @(Get-ChildItem (Join-Path $buildProfileDir 'bundle/nsis/*.exe'))
        Assert-That ($bundles.Count -eq 1) 'Expected one NSIS installer.'
        Copy-Item -LiteralPath $bundles[0].FullName -Destination $installer
    }
    & bun "$root/desktop/scripts/package-manifest.ts" --platform $platform --directory $ArtifactDir
    Assert-That ($LASTEXITCODE -eq 0) 'Artifact manifest generation failed.'
    $manifest = Get-Content -LiteralPath (Join-Path $ArtifactDir "xrun-$platform.json") -Raw | ConvertFrom-Json
    $version = $manifest.version
    $appDir = Join-Path $testDir '安装 App with spaces'
    # Exercise preservation of unrelated entries and an unexpanded variable.
    # The runner's original value and registry type are restored even on failure.
    $baselinePath = "$sandboxHome\工具;%USERPROFILE%\xrun-unrelated"
    $userEnvironment.SetValue('Path', $baselinePath, [Microsoft.Win32.RegistryValueKind]::ExpandString)
    $result = Run-Installer $appDir
    Assert-That ($result.ExitCode -eq 0 -and $result.Data.ok -and $result.Data.changed) "App installation failed: $($result.Data | ConvertTo-Json -Compress)"
    Assert-That ($result.Data.path -ceq $appDir) 'JSON did not preserve the Unicode installation path.'
    Assert-That ($result.Data.platform -ceq $platform) 'Installer selected the wrong architecture.'
    $appBinary = $result.Data.helper_executable
    $cliBinary = $result.Data.executable
    Assert-BinaryArchitecture $appBinary
    Assert-BinaryArchitecture $result.Data.desktop_executable
    $terminalDir = Join-Path $appDir 'cli'
    Assert-That ($cliBinary -ceq (Join-Path $terminalDir 'xrun.exe')) 'JSON must expose the console CLI executable.'
    Assert-BinaryArchitecture (Join-Path $terminalDir 'xrun.exe') -Console
    $installedPath = "$baselinePath;$terminalDir"
    Assert-That ($userEnvironment.GetValue('Path', '', $rawValue) -ceq $installedPath) 'Installer did not preserve and extend user PATH.'
    Assert-That ($userEnvironment.GetValueKind('Path') -eq [Microsoft.Win32.RegistryValueKind]::ExpandString) 'Installer changed the registry value type.'
    Assert-TerminalCommand $terminalDir
    $result = Invoke-Process $installer "/S /D=$appDir"
    Assert-That ($result.ExitCode -eq 0) 'Direct EXE reinstallation failed.'
    Assert-That ($userEnvironment.GetValue('Path', '', $rawValue) -ceq $installedPath) 'Reinstallation duplicated or changed PATH entries.'
    $result = Run-Installer $appDir
    Assert-That ($result.ExitCode -eq 0 -and -not $result.Data.changed) 'Repeated App installation should reuse the verified installation.'

    $bad = Join-Path $testDir 'corrupt'
    [void](New-Item -ItemType Directory -Path $bad)
    Copy-Item -LiteralPath $installer -Destination $bad
    Copy-Item -LiteralPath (Join-Path $ArtifactDir "xrun-$platform.json") -Destination $bad
    [IO.File]::AppendAllText((Join-Path $bad "xrun-app-$platform.exe"), 'corrupt')
    $before = (Get-FileHash -LiteralPath $appBinary).Hash
    $result = Run-Installer $appDir $bad
    Assert-That ($result.ExitCode -ne 0 -and $result.Data.error.code -eq 'CHECKSUM_MISMATCH') 'Corrupt installer was not rejected.'
    Assert-That ((Get-FileHash -LiteralPath $appBinary).Hash -eq $before) 'Checksum failure changed the installed helper.'
    $manifest.version = '9.9.9'
    $manifest | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $bad "xrun-$platform.json") -Encoding UTF8
    $result = Run-Installer $appDir $bad
    Assert-That ($result.ExitCode -ne 0 -and $result.Data.error.code -eq 'INVALID_MANIFEST') 'Wrong-version manifest was not rejected.'
    $manifest.version = $version
    $manifest.platform = 'wrong-platform'
    $manifest | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $bad "xrun-$platform.json") -Encoding UTF8
    $result = Run-Installer $appDir $bad
    Assert-That ($result.ExitCode -ne 0 -and $result.Data.error.code -eq 'INVALID_MANIFEST') 'Wrong-platform manifest was not rejected.'
    $manifest.platform = $platform
    $manifest.artifacts.app.file = 'other-architecture.exe'
    $manifest | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $bad "xrun-$platform.json") -Encoding UTF8
    $result = Run-Installer $appDir $bad
    Assert-That ($result.ExitCode -ne 0 -and $result.Data.error.code -eq 'INVALID_MANIFEST') 'Wrong artifact name was not rejected.'
    Assert-That ((Get-FileHash -LiteralPath $appBinary).Hash -eq $before) 'Manifest failure changed the installed helper.'

    # The CLI rejects App-only flags, providing a real failing preparation process.
    $blocked = Join-Path $testDir 'blocked update'
    [void](New-Item -ItemType Directory -Path $blocked)
    $fakeDesktop = Join-Path $blocked 'xrun-desktop.exe'
    Copy-Item -LiteralPath $cliBinary -Destination $fakeDesktop
    $before = (Get-FileHash -LiteralPath $fakeDesktop).Hash
    $result = Invoke-Process $installer "/S /D=$blocked" 30000
    Assert-That ($result.ExitCode -eq 32) "Silent update should return 32, got $($result.ExitCode)."
    Assert-That ((Get-Content -LiteralPath (Join-Path $blocked 'xrun-install-error.log') -Raw) -match '^UPDATE_PREPARE_FAILED:') 'Silent update failure did not write diagnostics.'
    Assert-That ((Get-FileHash -LiteralPath $fakeDesktop).Hash -eq $before) 'Failed preparation replaced an existing executable.'
    $result = Run-Installer $blocked
    Assert-That ($result.ExitCode -ne 0 -and $result.Data.error.code -eq 'UPDATE_PREPARE_FAILED' -and $result.Data.error.installer_exit_code -eq 32) 'Automatic installer did not preserve the structured preparation failure.'

    $desktop = Join-Path $appDir 'xrun-desktop.exe'
    $savedDesktop = Join-Path $testDir 'saved-desktop.exe'
    Copy-Item -LiteralPath $desktop -Destination $savedDesktop
    Copy-Item -LiteralPath $cliBinary -Destination $desktop -Force
    $uninstaller = Join-Path $appDir 'uninstall.exe'
    # _?= prevents NSIS from relaunching a temporary child and losing its exit code.
    $result = Invoke-Process $uninstaller "/S _?=$appDir" 30000
    Assert-That ($result.ExitCode -eq 33) "Silent uninstall should return 33, got $($result.ExitCode)."
    Assert-That ((Get-Content -LiteralPath (Join-Path $appDir 'xrun-install-error.log') -Raw) -match '^UNINSTALL_PREPARE_FAILED:') 'Silent uninstall failure did not write diagnostics.'
    Assert-That (Test-Path -LiteralPath $appBinary) 'Failed preparation removed the installed helper.'
    Assert-That ($userEnvironment.GetValue('Path', '', $rawValue) -ceq $installedPath) 'Failed uninstall changed user PATH.'
    Copy-Item -LiteralPath $savedDesktop -Destination $desktop -Force

    Set-Location $root
    $cargoArgs = @('test', '--locked')
    if ($Release) { $cargoArgs += '--release' }
    if ($Target) { $cargoArgs += @('--target', $Target) }
    $cargoArgs += @('--test', 'smoke', '--', '--nocapture')
    $env:XRUN_TEST_BINARY = $appBinary
    & cargo @cargoArgs
    Assert-That ($LASTEXITCODE -eq 0) 'Installed App helper smoke test failed.'
    $env:XRUN_TEST_BINARY = $cliBinary
    & cargo @cargoArgs
    Assert-That ($LASTEXITCODE -eq 0) 'Installed CLI smoke test failed.'
    $result = Invoke-Process $uninstaller "/S _?=$appDir" 30000
    $logPath = Join-Path $appDir 'xrun-install-error.log'
    $details = if (Test-Path -LiteralPath $logPath) { Get-Content -LiteralPath $logPath -Raw } else { $result.Stderr }
    Assert-That ($result.ExitCode -eq 0) "Restored App did not uninstall successfully (exit $($result.ExitCode)): $details"
    Assert-That ($userEnvironment.GetValue('Path', '', $rawValue) -ceq $baselinePath) 'Uninstall removed unrelated PATH entries or left its own entry behind.'
    Assert-That (-not (Test-Path -LiteralPath (Join-Path $terminalDir 'xrun.exe'))) 'Uninstall left the bundled terminal CLI behind.'

    # A directory already registered by the user must survive App removal.
    $userEnvironment.SetValue('Path', $installedPath, [Microsoft.Win32.RegistryValueKind]::ExpandString)
    $result = Run-Installer $appDir
    Assert-That ($result.ExitCode -eq 0 -and $result.Data.ok) 'Installation with a preexisting PATH entry failed.'
    Assert-That (-not (Test-Path -LiteralPath (Join-Path $appDir '.xrun-cli-path.json'))) 'Installer claimed ownership of a preexisting PATH entry.'
    $result = Invoke-Process $uninstaller "/S _?=$appDir" 30000
    Assert-That ($result.ExitCode -eq 0) 'Uninstall with a preexisting PATH entry failed.'
    Assert-That ($userEnvironment.GetValue('Path', '', $rawValue) -ceq $installedPath) 'Uninstall removed a user-owned PATH entry.'

    # An invalid registry value must produce a visible structured failure, without
    # silently overwriting it or hanging the unattended installer on a dialog.
    $userEnvironment.SetValue('Path', 7, [Microsoft.Win32.RegistryValueKind]::DWord)
    $pathFailure = Join-Path $testDir 'PATH failure'
    $result = Run-Installer $pathFailure
    Assert-That ($result.ExitCode -ne 0 -and $result.Data.error.code -eq 'CLI_INSTALL_FAILED' -and $result.Data.error.installer_exit_code -eq 34) 'PATH registration failure was not reported.'
    Assert-That ($userEnvironment.GetValue('Path') -eq 7) 'Failed PATH registration overwrote the existing value.'
    $result = Invoke-Process (Join-Path $pathFailure 'uninstall.exe') "/S _?=$pathFailure" 30000
    Assert-That ($result.ExitCode -eq 0) 'Partially installed App did not uninstall successfully.'
    Assert-That ([Environment]::GetEnvironmentVariable('Path', 'Machine') -ceq $machinePath) 'Installer changed machine PATH.'
    Write-Host 'Automatic Windows App and CLI installation and silent failure tests passed.'
} finally {
    if ($null -eq $savedPath) { $userEnvironment.DeleteValue('Path', $false) }
    else { $userEnvironment.SetValue('Path', $savedPath, $savedPathKind) }
    $userEnvironment.Dispose()
    [void][XrunInstallerTests.EnvironmentNotification]::SendMessageTimeout(
        [IntPtr]0xffff, 0x1a, [UIntPtr]::Zero, 'Environment', 2, 5000, [IntPtr]::Zero)
    Remove-Item -LiteralPath $testDir -Recurse -Force
}
