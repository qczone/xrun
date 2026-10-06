param([string]$ProfileDir, [string]$ArtifactDir, [switch]$Release)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$root = Split-Path $PSScriptRoot -Parent
$buildProfileDir = [IO.Path]::GetFullPath($ProfileDir)
$testDir = Join-Path ([IO.Path]::GetTempPath()) "xrun-installer-test-$([Guid]::NewGuid())"
[void](New-Item -ItemType Directory -Path $testDir)
$sandboxHome = Join-Path $testDir 'home'
[void](New-Item -ItemType Directory -Path $sandboxHome)

function Assert-That([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}
function Invoke-Process([string]$File, [string]$Arguments, [int]$Timeout = 120000) {
    $info = New-Object Diagnostics.ProcessStartInfo
    $info.FileName = $File
    $info.Arguments = $Arguments
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.EnvironmentVariables['HOME'] = $sandboxHome
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
function Run-Installer([string]$Component, [string]$Directory, [string]$Source = $ArtifactDir) {
    $arguments = "-NoProfile -NonInteractive -ExecutionPolicy Bypass -File `"$root\scripts\install.ps1`" -Version $version -Component $Component -InstallDir `"$Directory`" -SourceDir `"$Source`""
    $result = Invoke-Process 'powershell.exe' $arguments
    $data = $result.Stdout | ConvertFrom-Json
    return [pscustomobject]@{ ExitCode = $result.ExitCode; Data = $data }
}

try {
    if (-not $ArtifactDir) { $ArtifactDir = Join-Path $testDir 'artifacts' }
    $ArtifactDir = [IO.Path]::GetFullPath($ArtifactDir)
    [void](New-Item -ItemType Directory -Path $ArtifactDir -Force)
    $cliArchive = Join-Path $ArtifactDir 'xrun-windows-x86_64.zip'
    if (-not (Test-Path -LiteralPath $cliArchive)) {
        Compress-Archive -Path (Join-Path $buildProfileDir 'xrun.exe') -DestinationPath $cliArchive
    }
    $installer = Join-Path $ArtifactDir 'xrun-app-windows-x86_64.exe'
    if (-not (Test-Path -LiteralPath $installer)) {
        $bundles = @(Get-ChildItem (Join-Path $buildProfileDir 'bundle/nsis/*.exe'))
        Assert-That ($bundles.Count -eq 1) 'Expected one NSIS installer.'
        Copy-Item -LiteralPath $bundles[0].FullName -Destination $installer
    }
    & bun "$root/desktop/scripts/package-manifest.ts" --platform windows-x86_64 --directory $ArtifactDir
    Assert-That ($LASTEXITCODE -eq 0) 'Artifact manifest generation failed.'
    $manifest = Get-Content -LiteralPath (Join-Path $ArtifactDir 'xrun-windows-x86_64.json') -Raw | ConvertFrom-Json
    $version = $manifest.version
    $appDir = Join-Path $testDir '安装 App with spaces'
    $cliDir = Join-Path $testDir 'CLI with spaces'
    $result = Run-Installer 'app' $appDir
    Assert-That ($result.ExitCode -eq 0 -and $result.Data.ok -and $result.Data.changed) "App installation failed: $($result.Data | ConvertTo-Json -Compress)"
    Assert-That ($result.Data.path -ceq $appDir) 'JSON did not preserve the Unicode installation path.'
    $appBinary = $result.Data.executable
    $result = Run-Installer 'app' $appDir
    Assert-That ($result.ExitCode -eq 0 -and -not $result.Data.changed) 'Repeated App installation should reuse the verified installation.'
    $result = Run-Installer 'cli' $cliDir
    Assert-That ($result.ExitCode -eq 0 -and $result.Data.ok -and $result.Data.changed) 'CLI installation failed.'
    $cliBinary = $result.Data.executable
    $result = Run-Installer 'cli' $cliDir
    Assert-That ($result.ExitCode -eq 0 -and -not $result.Data.changed) 'Repeated CLI installation should reuse identical bytes.'

    $bad = Join-Path $testDir 'corrupt'
    [void](New-Item -ItemType Directory -Path $bad)
    Copy-Item -LiteralPath $installer -Destination $bad
    Copy-Item -LiteralPath (Join-Path $ArtifactDir 'xrun-windows-x86_64.json') -Destination $bad
    [IO.File]::AppendAllText((Join-Path $bad 'xrun-app-windows-x86_64.exe'), 'corrupt')
    $before = (Get-FileHash -LiteralPath $appBinary).Hash
    $result = Run-Installer 'app' $appDir $bad
    Assert-That ($result.ExitCode -ne 0 -and $result.Data.error.code -eq 'CHECKSUM_MISMATCH') 'Corrupt installer was not rejected.'
    Assert-That ((Get-FileHash -LiteralPath $appBinary).Hash -eq $before) 'Checksum failure changed the installed helper.'
    $manifest.version = '9.9.9'
    $manifest | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $bad 'xrun-windows-x86_64.json') -Encoding UTF8
    $result = Run-Installer 'app' $appDir $bad
    Assert-That ($result.ExitCode -ne 0 -and $result.Data.error.code -eq 'INVALID_MANIFEST') 'Wrong-version manifest was not rejected.'

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
    $result = Run-Installer 'app' $blocked
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
    Copy-Item -LiteralPath $savedDesktop -Destination $desktop -Force

    Set-Location $root
    $cargoArgs = @('test', '--locked')
    if ($Release) { $cargoArgs += '--release' }
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
    Write-Host 'Automatic Windows App and CLI installation and silent failure tests passed.'
} finally {
    Remove-Item -LiteralPath $testDir -Recurse -Force
}
