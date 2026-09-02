# Install IO Gateway from a GitHub Release into the current user's profile.
#
# Examples:
#   irm https://github.com/giofahreza/io-gateway/releases/latest/download/install.ps1 | iex
#   .\install.ps1 -Version v0.1.18
#   .\install.ps1 --version v0.1.18

[CmdletBinding()]
param(
    [string]$Version = $env:IO_GATEWAY_VERSION,
    [string]$Repository = $env:IO_GATEWAY_REPOSITORY,
    [string]$InstallDir = $env:IO_GATEWAY_INSTALL_DIR,
    [string]$ConfigDir = $env:IO_GATEWAY_CONFIG_DIR,
    [string]$ConfigPath = $env:IO_GATEWAY_CONFIG,
    [switch]$NoStart,
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$RemainingArguments
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$GatewayTaskName = 'IO Gateway'

function Write-Note {
    param([string]$Message)
    Write-Host "io-gateway installer: $Message"
}

function Write-WarningNote {
    param([string]$Message)
    Write-Warning "io-gateway installer: $Message"
}

function Show-Usage {
    $usageText = @'
Usage: install.ps1 [-Version <tag>] [-NoStart]

Installs the matching IO Gateway GitHub Release for this Windows computer.

Options:
  -Version <tag>    Install a release tag such as v0.1.18 (or 0.1.18).
  --version <tag>   POSIX-style spelling of -Version.
  -NoStart          Install only; do not start the local gateway process.
  --no-start        POSIX-style spelling of -NoStart.
  -Help             Show this help.

Environment overrides:
  IO_GATEWAY_VERSION       Same as -Version.
  IO_GATEWAY_INSTALL_DIR   Directory for io-gateway.exe and iogw.exe.
  IO_GATEWAY_CONFIG        Exact path for config.json.
  IO_GATEWAY_CONFIG_DIR    Config directory when IO_GATEWAY_CONFIG is unset.
  IO_GATEWAY_REPOSITORY    GitHub owner/repository (advanced use).
'@
    Write-Host $usageText
}

function Get-RemainingOptionValue {
    param(
        [string[]]$Arguments,
        [ref]$Index,
        [string]$OptionName
    )

    if (($Index.Value + 1) -ge $Arguments.Count) {
        throw "$OptionName needs a release tag."
    }
    $Index.Value++
    return $Arguments[$Index.Value]
}

if ($RemainingArguments) {
    for ($argumentIndex = 0; $argumentIndex -lt $RemainingArguments.Count; $argumentIndex++) {
        $argument = $RemainingArguments[$argumentIndex]
        switch -Regex ($argument) {
            '^(--version|--Version)$' {
                $Version = Get-RemainingOptionValue -Arguments $RemainingArguments -Index ([ref]$argumentIndex) -OptionName '--version'
                continue
            }
            '^--version=(.+)$' {
                $Version = $Matches[1]
                continue
            }
            '^(--no-start|--NoStart)$' {
                $NoStart = $true
                continue
            }
            '^(--help|--Help|-h)$' {
                Show-Usage
                exit 0
            }
            default {
                throw "Unknown option: $argument. Run with -Help for usage."
            }
        }
    }
}

if ([Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) {
    throw 'This installer is for Windows. Use scripts/install.sh on Linux or macOS.'
}

if ([string]::IsNullOrWhiteSpace($Version)) {
    $Version = 'latest'
}
if ([string]::IsNullOrWhiteSpace($Repository)) {
    $Repository = 'giofahreza/io-gateway'
}

if ($Repository -notmatch '^[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$') {
    throw 'Repository must be in owner/repository form.'
}

try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
}
catch {
    # Modern PowerShell defaults to a secure TLS version. Keep going when the
    # legacy ServicePointManager API is unavailable.
}

function Get-Release {
    param([string]$OwnerRepository, [string]$RequestedVersion)

    if ($RequestedVersion -eq 'latest') {
        $uri = "https://api.github.com/repos/$OwnerRepository/releases/latest"
    }
    else {
        if ($RequestedVersion -notmatch '^v') {
            $RequestedVersion = "v$RequestedVersion"
        }
        if ($RequestedVersion -notmatch '^v[0-9A-Za-z._-]+$') {
            throw "Invalid release version: $RequestedVersion"
        }
        $uri = "https://api.github.com/repos/$OwnerRepository/releases/tags/$RequestedVersion"
    }

    try {
        return Invoke-RestMethod -Uri $uri -Headers @{
            Accept = 'application/vnd.github+json'
            'User-Agent' = 'io-gateway-installer'
        }
    }
    catch {
        throw "Could not retrieve the GitHub release: $($_.Exception.Message)"
    }
}

function Get-TargetName {
    try {
        $architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    }
    catch {
        $architecture = $env:PROCESSOR_ARCHITEW6432
        if ([string]::IsNullOrWhiteSpace($architecture)) {
            $architecture = $env:PROCESSOR_ARCHITECTURE
        }
    }

    switch ($architecture.ToUpperInvariant()) {
        'X64' { return 'windows-x86_64' }
        'AMD64' { return 'windows-x86_64' }
        'ARM64' { return 'windows-aarch64' }
        default { throw "Unsupported Windows CPU architecture: $architecture. Releases support x86_64 and ARM64." }
    }
}

function Get-RandomHex {
    $bytes = New-Object byte[] 32
    $generator = [System.Security.Cryptography.RandomNumberGenerator]::Create()
    try {
        $generator.GetBytes($bytes)
    }
    finally {
        $generator.Dispose()
    }
    return ([BitConverter]::ToString($bytes)).Replace('-', '').ToLowerInvariant()
}

function Download-ReleaseAsset {
    param([string]$Uri, [string]$Destination)

    try {
        Invoke-WebRequest -Uri $Uri -OutFile $Destination -Headers @{
            Accept = 'application/octet-stream'
            'User-Agent' = 'io-gateway-installer'
        } -UseBasicParsing
    }
    catch {
        throw "Could not download ${Uri}: $($_.Exception.Message)"
    }
}

function Get-ExpectedSha256 {
    param([string]$ChecksumsPath, [string]$AssetName)

    $assetPattern = [regex]::Escape($AssetName)
    $match = [regex]::Match(
        [System.IO.File]::ReadAllText($ChecksumsPath),
        "(?m)^\s*([A-Fa-f0-9]{64})\s+\*?$assetPattern\s*$"
    )
    if (-not $match.Success) {
        throw "SHA256SUMS does not contain $AssetName."
    }
    return $match.Groups[1].Value.ToLowerInvariant()
}

function Install-Binary {
    param([string]$SourcePath, [string]$DestinationPath)

    $temporaryPath = "$DestinationPath.install-$([Guid]::NewGuid().ToString('N'))"
    Copy-Item -LiteralPath $SourcePath -Destination $temporaryPath -Force
    try {
        if (Test-Path -LiteralPath $DestinationPath) {
            $backupPath = "$DestinationPath.backup-$([Guid]::NewGuid().ToString('N'))"
            try {
                [System.IO.File]::Replace($temporaryPath, $DestinationPath, $backupPath, $true)
                Remove-Item -LiteralPath $backupPath -Force -ErrorAction SilentlyContinue
            }
            catch {
                Remove-Item -LiteralPath $temporaryPath -Force -ErrorAction SilentlyContinue
                throw
            }
        }
        else {
            Move-Item -LiteralPath $temporaryPath -Destination $DestinationPath
        }
    }
    catch {
        throw "Could not install $([System.IO.Path]::GetFileName($DestinationPath)). Close any running copy and retry. $($_.Exception.Message)"
    }
}

function Stop-ManagedGateway {
    param([string]$PidPath, [string]$GatewayPath)

    if (-not (Test-Path -LiteralPath $PidPath)) {
        return
    }
    $content = (Get-Content -LiteralPath $PidPath -Raw).Trim()
    $managedPid = 0
    if (-not [int]::TryParse($content, [ref]$managedPid)) {
        Remove-Item -LiteralPath $PidPath -Force -ErrorAction SilentlyContinue
        return
    }

    try {
        $process = Get-Process -Id $managedPid -ErrorAction Stop
        $processPath = $process.Path
        if ($processPath -and ([System.IO.Path]::GetFullPath($processPath) -eq [System.IO.Path]::GetFullPath($GatewayPath))) {
            Stop-Process -Id $managedPid -ErrorAction Stop
            $process.WaitForExit(10000)
        }
    }
    catch {
        # A stale PID, exited process, or inaccessible unrelated process must
        # never make the installer terminate a process it does not own.
    }
    finally {
        Remove-Item -LiteralPath $PidPath -Force -ErrorAction SilentlyContinue
    }
}

function Get-InstallerTask {
    param([string]$GatewayPath)

    if (-not (Get-Command Get-ScheduledTask -ErrorAction SilentlyContinue)) {
        return $null
    }
    $task = Get-ScheduledTask -TaskName $GatewayTaskName -ErrorAction SilentlyContinue
    if ($null -eq $task) {
        return $null
    }
    try {
        $expectedGatewayPath = [System.IO.Path]::GetFullPath($GatewayPath)
        foreach ($action in @($task.Actions)) {
            if ($action.Execute -and ([System.IO.Path]::GetFullPath([string]$action.Execute) -eq $expectedGatewayPath)) {
                return $task
            }
        }
    }
    catch {
        return $null
    }
    return $null
}

function Stop-InstallerTask {
    param([string]$GatewayPath)

    $task = Get-InstallerTask -GatewayPath $GatewayPath
    if ($null -eq $task) {
        return
    }
    try {
        Stop-ScheduledTask -InputObject $task -ErrorAction Stop
        Start-Sleep -Milliseconds 500
    }
    catch {
        Write-WarningNote "Could not stop the existing user startup task: $($_.Exception.Message)"
    }
}

function Register-AndStartInstallerTask {
    param([string]$GatewayPath, [string]$GatewayConfigPath)

    if (-not (Get-Command Register-ScheduledTask -ErrorAction SilentlyContinue) -or
        -not (Get-Command Get-ScheduledTask -ErrorAction SilentlyContinue) -or
        -not (Get-Command New-ScheduledTaskAction -ErrorAction SilentlyContinue) -or
        -not (Get-Command New-ScheduledTaskTrigger -ErrorAction SilentlyContinue) -or
        -not (Get-Command New-ScheduledTaskSettingsSet -ErrorAction SilentlyContinue) -or
        -not (Get-Command Start-ScheduledTask -ErrorAction SilentlyContinue)) {
        return $false
    }

    $existingTask = Get-ScheduledTask -TaskName $GatewayTaskName -ErrorAction SilentlyContinue
    if ($null -ne $existingTask -and $null -eq (Get-InstallerTask -GatewayPath $GatewayPath)) {
        throw "A Scheduled Task named '$GatewayTaskName' is not managed by this installer; it was left unchanged."
    }

    $action = New-ScheduledTaskAction -Execute $GatewayPath -Argument "--config `"$GatewayConfigPath`""
    $trigger = New-ScheduledTaskTrigger -AtLogOn
    $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
    Register-ScheduledTask -TaskName $GatewayTaskName `
        -Action $action `
        -Trigger $trigger `
        -Settings $settings `
        -Description 'Starts IO Gateway installed by the IO Gateway release installer.' `
        -Force | Out-Null
    Start-ScheduledTask -TaskName $GatewayTaskName
    return $true
}

function Add-InstallDirectoryToUserPath {
    param([string]$Directory)

    $normalizedDirectory = [System.IO.Path]::GetFullPath($Directory).TrimEnd('\\')
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $pathContainsDirectory = $false
    foreach ($pathEntry in @($userPath -split ';')) {
        if ([string]::IsNullOrWhiteSpace($pathEntry)) {
            continue
        }
        try {
            $normalizedEntry = [System.IO.Path]::GetFullPath([Environment]::ExpandEnvironmentVariables($pathEntry)).TrimEnd('\\')
        }
        catch {
            $normalizedEntry = $pathEntry.TrimEnd('\\')
        }
        if ([string]::Equals($normalizedEntry, $normalizedDirectory, [System.StringComparison]::OrdinalIgnoreCase)) {
            $pathContainsDirectory = $true
            break
        }
    }

    if (-not $pathContainsDirectory) {
        $updatedPath = if ([string]::IsNullOrWhiteSpace($userPath)) {
            $normalizedDirectory
        }
        else {
            "$userPath;$normalizedDirectory"
        }
        [Environment]::SetEnvironmentVariable('Path', $updatedPath, 'User')
        if ($env:Path -notlike "*$normalizedDirectory*") {
            $env:Path = "$normalizedDirectory;$env:Path"
        }
        return $true
    }
    return $false
}

$target = Get-TargetName
$release = Get-Release -OwnerRepository $Repository -RequestedVersion $Version
$tag = [string]$release.tag_name
if ($tag -notmatch '^v[0-9A-Za-z._-]+$') {
    throw "GitHub returned an invalid release tag: $tag"
}

$assetName = "io-gateway-$tag-$target.zip"
$asset = @($release.assets | Where-Object { $_.name -eq $assetName } | Select-Object -First 1)
$sumsAsset = @($release.assets | Where-Object { $_.name -eq 'SHA256SUMS' } | Select-Object -First 1)
if ($asset.Count -ne 1) {
    throw "Release $tag does not contain $assetName."
}
if ($sumsAsset.Count -ne 1) {
    throw "Release $tag does not contain SHA256SUMS."
}

if ([string]::IsNullOrWhiteSpace($InstallDir)) {
    $localAppData = $env:LOCALAPPDATA
    if ([string]::IsNullOrWhiteSpace($localAppData) -and -not [string]::IsNullOrWhiteSpace($env:USERPROFILE)) {
        $localAppData = Join-Path $env:USERPROFILE 'AppData\Local'
    }
    if ([string]::IsNullOrWhiteSpace($localAppData)) {
        throw 'Could not determine LOCALAPPDATA for the user-local installation.'
    }
    $InstallDir = Join-Path $localAppData 'Programs\io-gateway'
}
if ([string]::IsNullOrWhiteSpace($ConfigPath)) {
    if ([string]::IsNullOrWhiteSpace($ConfigDir)) {
        $appData = $env:APPDATA
        if ([string]::IsNullOrWhiteSpace($appData) -and -not [string]::IsNullOrWhiteSpace($env:USERPROFILE)) {
            $appData = Join-Path $env:USERPROFILE 'AppData\Roaming'
        }
        if ([string]::IsNullOrWhiteSpace($appData)) {
            throw 'Could not determine APPDATA for the user-local configuration.'
        }
        $ConfigDir = Join-Path $appData 'io-gateway'
    }
    $ConfigDir = [System.IO.Path]::GetFullPath($ConfigDir)
    $ConfigPath = Join-Path $ConfigDir 'config.json'
}
else {
    $ConfigPath = [System.IO.Path]::GetFullPath($ConfigPath)
    $ConfigDir = Split-Path -Parent $ConfigPath
}
$InstallDir = [System.IO.Path]::GetFullPath($InstallDir)

if ([string]::IsNullOrWhiteSpace($InstallDir) -or [string]::IsNullOrWhiteSpace($ConfigDir)) {
    throw 'Could not determine a user-local install or config directory.'
}

New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
New-Item -ItemType Directory -Path $ConfigDir -Force | Out-Null
if ((Test-Path -LiteralPath $ConfigPath) -and -not (Test-Path -LiteralPath $ConfigPath -PathType Leaf)) {
    throw "Config path exists but is not a regular file: $ConfigPath"
}

$pathAdded = $false

$temporaryDirectory = Join-Path ([System.IO.Path]::GetTempPath()) "io-gateway-install-$([Guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $temporaryDirectory -Force | Out-Null
try {
    $archivePath = Join-Path $temporaryDirectory $assetName
    $sumsPath = Join-Path $temporaryDirectory 'SHA256SUMS'
    $extractDirectory = Join-Path $temporaryDirectory 'package'

    Write-Note "Downloading $assetName."
    Download-ReleaseAsset -Uri $asset[0].browser_download_url -Destination $archivePath
    Download-ReleaseAsset -Uri $sumsAsset[0].browser_download_url -Destination $sumsPath

    $expectedSha256 = Get-ExpectedSha256 -ChecksumsPath $sumsPath -AssetName $assetName
    $actualSha256 = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualSha256 -ne $expectedSha256) {
        throw "Checksum verification failed for $assetName."
    }
    Write-Note 'Release checksum verified.'

    Expand-Archive -LiteralPath $archivePath -DestinationPath $extractDirectory -Force
    foreach ($requiredFile in @('io-gateway.exe', 'iogw.exe', 'config.example.json')) {
        if (-not (Test-Path -LiteralPath (Join-Path $extractDirectory $requiredFile) -PathType Leaf)) {
            throw "Release archive is missing required file: $requiredFile"
        }
    }

    $gatewayPath = Join-Path $InstallDir 'io-gateway.exe'
    $iogwPath = Join-Path $InstallDir 'iogw.exe'
    $pidPath = Join-Path $ConfigDir 'io-gateway.pid'
    Stop-ManagedGateway -PidPath $pidPath -GatewayPath $gatewayPath
    Stop-InstallerTask -GatewayPath $gatewayPath

    Install-Binary -SourcePath (Join-Path $extractDirectory 'io-gateway.exe') -DestinationPath $gatewayPath
    Install-Binary -SourcePath (Join-Path $extractDirectory 'iogw.exe') -DestinationPath $iogwPath
    $pathAdded = Add-InstallDirectoryToUserPath -Directory $InstallDir

    $createdConfig = $false
    if (-not (Test-Path -LiteralPath $ConfigPath)) {
        $exampleConfigPath = Join-Path $extractDirectory 'config.example.json'
        try {
            $config = Get-Content -LiteralPath $exampleConfigPath -Raw | ConvertFrom-Json
        }
        catch {
            throw "Release config.example.json is invalid: $($_.Exception.Message)"
        }

        $config.listen = '127.0.0.1:8319'
        $config.proxy_api_key = "iogw_$(Get-RandomHex)"
        if ($null -eq $config.admin_auth) {
            $config | Add-Member -NotePropertyName admin_auth -NotePropertyValue ([pscustomobject]@{})
        }
        $config.admin_auth.enabled = $false
        $config.admin_auth.api_key = ''
        $config.admin_auth.totp_secret = ''
        $configJson = $config | ConvertTo-Json -Depth 100
        $temporaryConfigPath = "$ConfigPath.install-$([Guid]::NewGuid().ToString('N'))"
        $utf8WithoutBom = New-Object System.Text.UTF8Encoding($false)
        [System.IO.File]::WriteAllText($temporaryConfigPath, $configJson, $utf8WithoutBom)
        Move-Item -LiteralPath $temporaryConfigPath -Destination $ConfigPath
        New-Item -ItemType Directory -Path (Join-Path $ConfigDir 'auths') -Force | Out-Null
        $createdConfig = $true
        Write-Note "Created a localhost-only config at $ConfigPath."
    }
    else {
        Write-Note "Keeping existing config and credentials at $ConfigDir."
    }

    $started = $false
    $startupTaskRegistered = $false
    if (-not $NoStart) {
        try {
            $startupTaskRegistered = Register-AndStartInstallerTask -GatewayPath $gatewayPath -GatewayConfigPath $ConfigPath
            if ($startupTaskRegistered) {
                $started = $true
                Write-Note 'Started the gateway and registered a per-user startup task.'
            }
        }
        catch {
            Write-WarningNote "Could not register the per-user startup task: $($_.Exception.Message)"
        }

        if (-not $started) {
            $standardOutputPath = Join-Path $ConfigDir 'io-gateway.log'
            $standardErrorPath = Join-Path $ConfigDir 'io-gateway-error.log'
            try {
                $gatewayProcess = Start-Process -FilePath $gatewayPath `
                    -ArgumentList @('--config', $ConfigPath) `
                    -WorkingDirectory $ConfigDir `
                    -RedirectStandardOutput $standardOutputPath `
                    -RedirectStandardError $standardErrorPath `
                    -WindowStyle Hidden `
                    -PassThru
                [System.IO.File]::WriteAllText($pidPath, [string]$gatewayProcess.Id, (New-Object System.Text.UTF8Encoding($false)))
                $started = $true
                Write-Note 'Started the gateway as a user-local background process.'
            }
            catch {
                Write-WarningNote "Could not start the gateway automatically: $($_.Exception.Message)"
            }
        }
    }

    Write-Host ''
    Write-Note "Installed $tag for $target."
    Write-Note "Gateway binary: $gatewayPath"
    Write-Note "Management client: $iogwPath"
    Write-Note "Config: $ConfigPath"
    if ($pathAdded) {
        Write-Host 'Added the install directory to your user PATH; open a new terminal to use io-gateway and iogw by name.'
    }
    if ($createdConfig) {
        Write-Host 'The first-run gateway is bound to 127.0.0.1 and admin authentication is disabled only for local setup.'
        Write-Host 'Before changing listen to a LAN/public address, configure a TOTP secret and enable admin_auth in config.json.'
        Write-Host 'The generated client API key is stored in proxy_api_key in config.json; keep that file private.'
    }
    if ($started) {
        Write-Host 'Open http://127.0.0.1:8319/ to finish provider setup.'
        if ($startupTaskRegistered) {
            Write-Host 'The gateway will also start automatically at your next sign-in.'
        }
        else {
            Write-Host 'Start the gateway with the command below after a restart.'
        }
    }
    else {
        Write-Host 'Start the gateway with:'
    }
    Write-Host "  & `"$gatewayPath`" --config `"$ConfigPath`""
}
finally {
    if (Test-Path -LiteralPath $temporaryDirectory) {
        Remove-Item -LiteralPath $temporaryDirectory -Recurse -Force
    }
}
