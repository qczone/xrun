# Install a published xrun App or CLI without opening the desktop UI.
[CmdletBinding()]
param(
    [string]$Version,
    [string]$Component = 'app',
    [string]$InstallDir,
    [string]$SourceDir,
    [string]$BaseUrl
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
Set-StrictMode -Version Latest
$workDir = $null
$backup = $null
$destination = $null
$replaced = $false
$success = $false
$failureCode = 'INSTALL_FAILED'
$installerExitCode = $null

function Fail-Install([string]$Code, [string]$Message) {
    $script:failureCode = $Code
    throw $Message
}

function Invoke-Native([string]$File, [string]$Arguments, [int]$Timeout = 30000) {
    $info = New-Object System.Diagnostics.ProcessStartInfo
    $info.FileName = $File
    $info.Arguments = $Arguments
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    # Native children can start Windows PowerShell 5.1, which needs its own modules.
    $info.EnvironmentVariables.Remove('PSModulePath')
    $process = New-Object System.Diagnostics.Process
    $process.StartInfo = $info
    try {
        [void]$process.Start()
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit($Timeout)) {
            $process.Kill()
            Fail-Install 'PROCESS_TIMEOUT' "Timed out running $File."
        }
        return [pscustomobject]@{
            ExitCode = $process.ExitCode
            Stdout = $stdout.GetAwaiter().GetResult().Trim()
            Stderr = $stderr.GetAwaiter().GetResult().Trim()
        }
    } finally {
        $process.Dispose()
    }
}

function Fetch-Artifact([string]$Name) {
    $path = Join-Path $workDir $Name
    if ($SourceDir) {
        $source = Join-Path $SourceDir $Name
        if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
            Fail-Install 'ARTIFACT_NOT_FOUND' "Missing artifact: $Name."
        }
        Copy-Item -LiteralPath $source -Destination $path
    } else {
        try {
            # Windows PowerShell 5.1 otherwise inherits older TLS defaults.
            [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
            Invoke-WebRequest -UseBasicParsing -Uri "$($BaseUrl.TrimEnd('/'))/$Name" -OutFile $path -TimeoutSec 600 | Out-Null
        } catch {
            Fail-Install 'DOWNLOAD_FAILED' "Could not download $Name. $($_.Exception.Message)"
        }
    }
    return $path
}

function Check-App([string]$File) {
    $result = Invoke-Native $File '--self-check'
    if ($result.ExitCode -ne 0) {
        Fail-Install 'SELF_CHECK_FAILED' "App self-check failed: $($result.Stderr)"
    }
    try { $status = $result.Stdout | ConvertFrom-Json } catch {
        Fail-Install 'SELF_CHECK_FAILED' 'App self-check did not return JSON.'
    }
    if ($status.version -cne $Version) {
        Fail-Install 'VERSION_MISMATCH' 'App version does not match the manifest.'
    }
    return $status
}

try {
    $Version = $Version -replace '^v', ''
    if ($Version -notmatch '^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$') {
        Fail-Install 'INVALID_ARGUMENT' 'Specify the full release version with -Version.'
    }
    if ($Component -cnotin @('app', 'cli')) { Fail-Install 'INVALID_ARGUMENT' 'Component must be app or cli.' }
    if ($SourceDir -and $BaseUrl) { Fail-Install 'INVALID_ARGUMENT' 'Choose SourceDir or BaseUrl.' }
    if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
        Fail-Install 'UNSUPPORTED_PLATFORM' 'install.ps1 supports Windows.'
    }
    $architecture = [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    $artifactArchitecture = switch ($architecture) {
        'X64' { 'x86_64' }
        'Arm64' { 'arm64' }
        default { Fail-Install 'UNSUPPORTED_PLATFORM' 'This release supports Windows x86_64 and arm64.' }
    }
    $platform = "windows-$artifactArchitecture"
    if (-not $InstallDir) {
        $InstallDir = if ($Component -eq 'app') { Join-Path $env:LOCALAPPDATA 'Programs\xrun' } else { Join-Path $env:LOCALAPPDATA 'xrun\bin' }
    }
    $InstallDir = [IO.Path]::GetFullPath($InstallDir)
    if ($InstallDir.Contains('"') -or $InstallDir -match '[\r\n]') { Fail-Install 'INVALID_ARGUMENT' 'Invalid installation directory.' }
    if (-not $BaseUrl) { $BaseUrl = "https://github.com/qczone/xrun/releases/download/v$Version" }
    if ($BaseUrl -notmatch '^https://') { Fail-Install 'INVALID_ARGUMENT' 'BaseUrl must use HTTPS.' }
    $workDir = Join-Path ([IO.Path]::GetTempPath()) "xrun-download-$([Guid]::NewGuid())"
    [void](New-Item -ItemType Directory -Path $workDir)
    $manifest = Get-Content -LiteralPath (Fetch-Artifact "xrun-$platform.json") -Raw | ConvertFrom-Json
    if ($manifest.schema -ne 1 -or $manifest.version -cne $Version -or $manifest.platform -cne $platform) {
        Fail-Install 'INVALID_MANIFEST' 'Manifest version or platform does not match the requested release.'
    }
    $entry = $manifest.artifacts.$Component
    $expected = if ($Component -eq 'app') { "xrun-app-$platform.exe" } else { "xrun-$platform.zip" }
    if ($entry.file -cne $expected -or $entry.sha256 -cnotmatch '^[0-9a-f]{64}$') {
        Fail-Install 'INVALID_MANIFEST' 'Manifest artifact name or SHA-256 is invalid.'
    }
    $artifact = Fetch-Artifact $entry.file
    $digest = (Get-FileHash -LiteralPath $artifact -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($digest -cne $entry.sha256) { Fail-Install 'CHECKSUM_MISMATCH' 'Artifact SHA-256 does not match the manifest.' }
    [void](New-Item -ItemType Directory -Path $InstallDir -Force)
    $destination = Join-Path $InstallDir 'xrun.exe'
    $desktopExecutable = $null
    $changed = $true
    if ($Component -eq 'app') {
        $desktopExecutable = Join-Path $InstallDir 'xrun-desktop.exe'
        $receiptPath = Join-Path $InstallDir '.xrun-install.json'
        $sameArtifact = $false
        if (Test-Path -LiteralPath $receiptPath -PathType Leaf) {
            try {
                $receipt = Get-Content -LiteralPath $receiptPath -Raw | ConvertFrom-Json
                $sameArtifact = $receipt.sha256 -ceq $digest -and $receipt.version -ceq $Version -and
                    $receipt.cli_sha256 -ceq (Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash -and
                    $receipt.desktop_sha256 -ceq (Get-FileHash -LiteralPath $desktopExecutable -Algorithm SHA256).Hash
            } catch { $sameArtifact = $false }
        }
        if ($sameArtifact) {
            $changed = $false
        } else {
            # NSIS parses /D as the unquoted remainder of the command line.
            $installed = Invoke-Native $artifact "/S /D=$InstallDir" 600000
            if ($installed.ExitCode -ne 0) {
                $installerExitCode = $installed.ExitCode
                $logPath = Join-Path $InstallDir 'xrun-install-error.log'
                $details = if (Test-Path -LiteralPath $logPath) { (Get-Content -LiteralPath $logPath -Raw).Trim() } else { $installed.Stderr }
                $code = switch ($installed.ExitCode) {
                    32 { 'UPDATE_PREPARE_FAILED' }
                    34 { 'CLI_INSTALL_FAILED' }
                    default { 'INSTALLER_FAILED' }
                }
                Fail-Install $code "NSIS exited with code $($installed.ExitCode). $details"
            }
        }
        $selfCheck = Check-App $desktopExecutable
        $receipt = @{
            version = $Version; sha256 = $digest
            cli_sha256 = (Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash
            desktop_sha256 = (Get-FileHash -LiteralPath $desktopExecutable -Algorithm SHA256).Hash
        }
        $receipt | ConvertTo-Json | Set-Content -LiteralPath $receiptPath -Encoding UTF8
        $path = $InstallDir
    } else {
        $extracted = Join-Path $workDir 'cli'
        Expand-Archive -LiteralPath $artifact -DestinationPath $extracted
        $staged = Join-Path $extracted 'xrun.exe'
        if (-not (Test-Path -LiteralPath $staged -PathType Leaf)) { Fail-Install 'INVALID_ARTIFACT' 'Archive does not contain xrun.exe.' }
        $stagedVersion = Invoke-Native $staged '--version'
        if ($stagedVersion.ExitCode -ne 0 -or $stagedVersion.Stdout -cne "xrun $Version") { Fail-Install 'VERSION_MISMATCH' 'Artifact executable version does not match the manifest.' }
        if ((Test-Path -LiteralPath $destination) -and (Get-FileHash -LiteralPath $destination).Hash -eq (Get-FileHash -LiteralPath $staged).Hash) {
            $changed = $false
        } else {
            if (Test-Path -LiteralPath $destination) {
                $oldVersion = Invoke-Native $destination '--version'
                if ($oldVersion.ExitCode -ne 0 -or $oldVersion.Stdout -notlike 'xrun *') { Fail-Install 'INVALID_DESTINATION' 'Existing executable is not xrun.' }
                $stopped = Invoke-Native $destination 'daemon stop'
                if ($stopped.ExitCode -ne 0) { Fail-Install 'UPDATE_PREPARE_FAILED' "Could not stop the daemon: $($stopped.Stderr)" }
                $backup = Join-Path $InstallDir ".xrun-backup-$([Guid]::NewGuid()).exe"
                Move-Item -LiteralPath $destination -Destination $backup
            }
            $replaced = $true
            Copy-Item -LiteralPath $staged -Destination $destination
        }
        $installedVersion = Invoke-Native $destination '--version'
        if ($installedVersion.ExitCode -ne 0 -or $installedVersion.Stdout -cne "xrun $Version") { Fail-Install 'SELF_CHECK_FAILED' 'Installed CLI version check failed.' }
        $selfCheck = @{ version = $Version }
        $path = $destination
    }
    [pscustomobject]@{
        ok = $true; component = $Component; version = $Version; platform = $platform
        changed = $changed; path = $path; executable = $destination
        desktop_executable = $desktopExecutable; sha256 = $digest; self_check = $selfCheck
    } | ConvertTo-Json -Depth 8 -Compress
    $success = $true
} catch {
    $errorResult = @{ code = $failureCode; message = $_.Exception.Message }
    if ($null -ne $installerExitCode) { $errorResult.installer_exit_code = $installerExitCode }
    [pscustomobject]@{ ok = $false; error = $errorResult } | ConvertTo-Json -Depth 4 -Compress
} finally {
    if (-not $success -and ($replaced -or ($backup -and (Test-Path -LiteralPath $backup)))) {
        Remove-Item -LiteralPath $destination -Force -ErrorAction SilentlyContinue
        if ($backup -and (Test-Path -LiteralPath $backup)) { Move-Item -LiteralPath $backup -Destination $destination }
    }
    if ($backup -and (Test-Path -LiteralPath $backup)) { Remove-Item -LiteralPath $backup -Force }
    if ($workDir -and (Test-Path -LiteralPath $workDir)) { Remove-Item -LiteralPath $workDir -Recurse -Force }
}
if (-not $success) { exit 1 }
