# Install IO Gateway from a GitHub Release into the current user's profile.
#
# Examples:
#   irm https://github.com/giofahreza/io-gateway/releases/latest/download/install.ps1 | iex
#   .\install.ps1 -Version v0.1.18
#   .\install.ps1 -Port 9000 -NoIogw -NoAutoStart
#   .\install.ps1 --version v0.1.18

[CmdletBinding()]
param(
    [string]$Version = $env:IO_GATEWAY_VERSION,
    [string]$Repository = $env:IO_GATEWAY_REPOSITORY,
    [string]$InstallDir = $env:IO_GATEWAY_INSTALL_DIR,
    [string]$ConfigDir = $env:IO_GATEWAY_CONFIG_DIR,
    [string]$ConfigPath = $env:IO_GATEWAY_CONFIG,
    [string]$Port = $env:IO_GATEWAY_PORT,
    [Alias('WithIogw')]
    [switch]$InstallIogw,
    [Alias('WithoutIogw')]
    [switch]$NoIogw,
    [switch]$AutoStart,
    [switch]$NoAutoStart,
    [switch]$Interactive,
    [switch]$NonInteractive,
    [switch]$StartNow,
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
Usage: install.ps1 [-Version <tag>] [-Port <port>] [-InstallIogw | -NoIogw]
                   [-AutoStart | -NoAutoStart] [-StartNow | -NoStart]
                   [-Interactive | -NonInteractive]

Installs the matching IO Gateway GitHub Release for this Windows computer.

Options:
  -Version <tag>    Install a release tag such as v0.1.18 (or 0.1.18).
  --version <tag>   POSIX-style spelling of -Version.
  -Port <port>      TCP port for a newly created localhost config (1-65535).
  --port <port>     POSIX-style spelling of -Port.
  -InstallIogw      Install the optional iogw management client.
  -NoIogw           Do not install the optional iogw management client.
  --install-iogw    POSIX-style spelling of -InstallIogw.
  --no-iogw         POSIX-style spelling of -NoIogw.
  --with-iogw       Cross-platform spelling of -InstallIogw.
  --without-iogw    Cross-platform spelling of -NoIogw.
  -AutoStart        Register a per-user Scheduled Task at Windows sign-in.
  -NoAutoStart      Do not use a per-user Scheduled Task at sign-in; removes
                    an installer-managed task when one exists.
  --auto-start      POSIX-style spelling of -AutoStart.
  --no-auto-start   POSIX-style spelling of -NoAutoStart.
  --autostart       Cross-platform spelling of -AutoStart.
  --no-autostart    Cross-platform spelling of -NoAutoStart.
  -StartNow         Launch the gateway immediately after installation.
  --start-now       POSIX-style spelling of -StartNow.
  -NoStart          Install or update without launching the gateway now.
                    Preserves an existing startup task; combine with
                    -AutoStart to register a new task without launching it.
  --no-start        POSIX-style spelling of -NoStart.
  -Interactive      Require first-run setup questions in a terminal.
  --interactive     POSIX-style spelling of -Interactive.
  -NonInteractive   Do not prompt; use defaults or the supplied flags.
  --non-interactive POSIX-style spelling of -NonInteractive.
  -Help             Show this help.

Environment overrides:
  IO_GATEWAY_VERSION       Same as -Version.
  IO_GATEWAY_INSTALL_DIR   Directory for io-gateway.exe and iogw.exe.
  IO_GATEWAY_CONFIG        Exact path for config.json.
  IO_GATEWAY_CONFIG_DIR    Config directory when IO_GATEWAY_CONFIG is unset.
  IO_GATEWAY_PORT          Same as -Port for a new config.
  IO_GATEWAY_INSTALL_IOGW  auto, yes, or no choice for the optional client.
  IO_GATEWAY_AUTOSTART     auto, yes, or no choice for the sign-in task.
  IO_GATEWAY_START_NOW     auto, yes, or no choice for the immediate launch.
  IO_GATEWAY_INTERACTIVE   auto, yes, or no prompt mode.
  IO_GATEWAY_AUTO_START    Legacy true/false sign-in task override.
  IO_GATEWAY_NONINTERACTIVE Legacy true/false prompt override.
  IO_GATEWAY_REPOSITORY    GitHub owner/repository (advanced use).
'@
    Write-Host $usageText
}

function Get-RemainingOptionValue {
    param(
        [string[]]$Arguments,
        [ref]$Index,
        [string]$OptionName,
        [string]$ValueDescription = 'a value'
    )

    if (($Index.Value + 1) -ge $Arguments.Count) {
        throw "$OptionName needs $ValueDescription."
    }
    $Index.Value++
    return $Arguments[$Index.Value]
}

function ConvertTo-InstallerBoolean {
    param(
        [string]$Value,
        [string]$Name
    )

    if ([string]::IsNullOrWhiteSpace($Value)) {
        throw "$Name must be true or false."
    }

    switch ($Value.Trim().ToLowerInvariant()) {
        '1' { return $true }
        'true' { return $true }
        'yes' { return $true }
        'y' { return $true }
        'on' { return $true }
        '0' { return $false }
        'false' { return $false }
        'no' { return $false }
        'n' { return $false }
        'off' { return $false }
        default { throw "$Name must be true or false (accepted values: true/false, yes/no, or 1/0)." }
    }
}

function ConvertTo-InstallerMode {
    param(
        [string]$Value,
        [string]$Name
    )

    if ([string]::IsNullOrWhiteSpace($Value)) {
        throw "$Name must be auto, yes, or no."
    }

    switch ($Value.Trim().ToLowerInvariant()) {
        'auto' { return 'auto' }
        '1' { return 'yes' }
        'true' { return 'yes' }
        'yes' { return 'yes' }
        'y' { return 'yes' }
        'on' { return 'yes' }
        '0' { return 'no' }
        'false' { return 'no' }
        'no' { return 'no' }
        'n' { return 'no' }
        'off' { return 'no' }
        default { throw "$Name must be auto, yes, or no." }
    }
}

function ConvertTo-InstallerPort {
    param(
        [string]$Value,
        [string]$Name = 'Port'
    )

    [int]$parsedPort = 0
    if ([string]::IsNullOrWhiteSpace($Value) -or
        -not [int]::TryParse($Value, [ref]$parsedPort) -or
        $parsedPort -lt 1 -or $parsedPort -gt 65535) {
        throw "$Name must be a TCP port number from 1 through 65535."
    }
    return $parsedPort
}

function Test-InstallerInteractive {
    param([bool]$Disabled)

    if ($Disabled -or -not [string]::IsNullOrWhiteSpace($env:CI)) {
        return $false
    }
    try {
        if (-not [Environment]::UserInteractive -or [Console]::IsInputRedirected) {
            return $false
        }
        return $null -ne $Host -and $null -ne $Host.UI -and $null -ne $Host.UI.RawUI
    }
    catch {
        return $false
    }
}

function Read-InstallerYesNo {
    param(
        [string]$Prompt,
        [bool]$Default
    )

    $defaultHint = if ($Default) { 'Y/n' } else { 'y/N' }
    while ($true) {
        try {
            $answer = Read-Host "$Prompt [$defaultHint]"
        }
        catch {
            Write-WarningNote "Could not read a response; using the default. $($_.Exception.Message)"
            return $Default
        }

        if ([string]::IsNullOrWhiteSpace($answer)) {
            return $Default
        }
        switch ($answer.Trim().ToLowerInvariant()) {
            'y' { return $true }
            'yes' { return $true }
            'n' { return $false }
            'no' { return $false }
            default { Write-WarningNote 'Please answer yes or no.' }
        }
    }
}

function Read-InstallerPort {
    param([int]$Default = 8319)

    while ($true) {
        try {
            $answer = Read-Host "Local gateway TCP port [$Default]"
        }
        catch {
            Write-WarningNote "Could not read a response; using port $Default. $($_.Exception.Message)"
            return $Default
        }

        if ([string]::IsNullOrWhiteSpace($answer)) {
            return $Default
        }
        try {
            $parsedPort = ConvertTo-InstallerPort -Value $answer -Name 'Port'
            return $parsedPort
        }
        catch {
            Write-WarningNote $_.Exception.Message
        }
    }
}

function Test-InstallerPortAvailable {
    param([int]$Port)

    # The first-run configuration always listens on IPv4 localhost. Probe the
    # exact bind address before writing config or replacing binaries, so a
    # busy port fails early instead of leaving an installed gateway unable to
    # start. The socket is released immediately; the eventual gateway bind is
    # still subject to the usual unavoidable race with other local processes.
    $listener = $null
    try {
        $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, $Port)
        $listener.Start()
        return $true
    }
    catch {
        return $false
    }
    finally {
        if ($null -ne $listener) {
            $listener.Stop()
        }
    }
}

function Read-AvailableInstallerPort {
    param([int]$Default = 8319)

    while ($true) {
        $candidatePort = Read-InstallerPort -Default $Default
        if (Test-InstallerPortAvailable -Port $candidatePort) {
            return $candidatePort
        }
        Write-WarningNote "Port $candidatePort is already in use or unavailable on 127.0.0.1. Choose another port."
    }
}

if ($RemainingArguments) {
    for ($argumentIndex = 0; $argumentIndex -lt $RemainingArguments.Count; $argumentIndex++) {
        $argument = $RemainingArguments[$argumentIndex]
        switch -Regex ($argument) {
            '^(--version|--Version)$' {
                $Version = Get-RemainingOptionValue -Arguments $RemainingArguments -Index ([ref]$argumentIndex) -OptionName '--version' -ValueDescription 'a release tag'
                continue
            }
            '^--version=(.+)$' {
                $Version = $Matches[1]
                continue
            }
            '^(--port|--Port)$' {
                $Port = Get-RemainingOptionValue -Arguments $RemainingArguments -Index ([ref]$argumentIndex) -OptionName '--port' -ValueDescription 'a TCP port number'
                continue
            }
            '^--port=(.+)$' {
                $Port = $Matches[1]
                continue
            }
            '^(--install-iogw|--InstallIogw|--with-iogw|--WithIogw)$' {
                $InstallIogw = $true
                continue
            }
            '^(--no-iogw|--NoIogw|--without-iogw|--WithoutIogw)$' {
                $NoIogw = $true
                continue
            }
            '^(--auto-start|--AutoStart|--autostart)$' {
                $AutoStart = $true
                continue
            }
            '^(--no-auto-start|--NoAutoStart|--no-autostart)$' {
                $NoAutoStart = $true
                continue
            }
            '^(--start-now|--StartNow)$' {
                $StartNow = $true
                continue
            }
            '^(--no-start|--NoStart)$' {
                $NoStart = $true
                continue
            }
            '^(--interactive|--Interactive)$' {
                $Interactive = $true
                continue
            }
            '^(--non-interactive|--NonInteractive)$' {
                $NonInteractive = $true
                continue
            }
            '^(--help|--Help|-h|-Help)$' {
                Show-Usage
                exit 0
            }
            default {
                throw "Unknown option: $argument. Run with -Help for usage."
            }
        }
    }
}

if ($InstallIogw -and $NoIogw) {
    throw 'Choose only one of -InstallIogw and -NoIogw.'
}
if ($AutoStart -and $NoAutoStart) {
    throw 'Choose only one of -AutoStart and -NoAutoStart.'
}
if ($Interactive -and $NonInteractive) {
    throw 'Choose only one of -Interactive and -NonInteractive.'
}
if ($StartNow -and $NoStart) {
    throw 'Choose only one of -StartNow and -NoStart.'
}

$installIogwChoice = $null
if (-not $InstallIogw -and -not $NoIogw -and -not [string]::IsNullOrWhiteSpace($env:IO_GATEWAY_INSTALL_IOGW)) {
    $installIogwMode = ConvertTo-InstallerMode -Value $env:IO_GATEWAY_INSTALL_IOGW -Name 'IO_GATEWAY_INSTALL_IOGW'
    if ($installIogwMode -eq 'yes') {
        $installIogwChoice = $true
    }
    elseif ($installIogwMode -eq 'no') {
        $installIogwChoice = $false
    }
}
if ($InstallIogw) {
    $installIogwChoice = $true
}
elseif ($NoIogw) {
    $installIogwChoice = $false
}

$autoStartChoice = $null
$autoStartExplicit = $false
if (-not $AutoStart -and -not $NoAutoStart -and -not [string]::IsNullOrWhiteSpace($env:IO_GATEWAY_AUTOSTART)) {
    $autoStartMode = ConvertTo-InstallerMode -Value $env:IO_GATEWAY_AUTOSTART -Name 'IO_GATEWAY_AUTOSTART'
    if ($autoStartMode -eq 'yes') {
        $autoStartChoice = $true
        $autoStartExplicit = $true
    }
    elseif ($autoStartMode -eq 'no') {
        $autoStartChoice = $false
        $autoStartExplicit = $true
    }
}
elseif (-not $AutoStart -and -not $NoAutoStart -and -not [string]::IsNullOrWhiteSpace($env:IO_GATEWAY_AUTO_START)) {
    $autoStartChoice = ConvertTo-InstallerBoolean -Value $env:IO_GATEWAY_AUTO_START -Name 'IO_GATEWAY_AUTO_START'
    $autoStartExplicit = $true
}
if ($AutoStart) {
    $autoStartChoice = $true
    $autoStartExplicit = $true
}
elseif ($NoAutoStart) {
    $autoStartChoice = $false
    $autoStartExplicit = $true
}

$startNowChoice = $null
if (-not $StartNow -and -not $NoStart -and -not [string]::IsNullOrWhiteSpace($env:IO_GATEWAY_START_NOW)) {
    $startNowMode = ConvertTo-InstallerMode -Value $env:IO_GATEWAY_START_NOW -Name 'IO_GATEWAY_START_NOW'
    if ($startNowMode -eq 'yes') {
        $startNowChoice = $true
    }
    elseif ($startNowMode -eq 'no') {
        $startNowChoice = $false
    }
}
if ($StartNow) {
    $startNowChoice = $true
}
elseif ($NoStart) {
    # Keep the long-standing -NoStart behavior: no immediate launch, and for
    # a fresh auto-configured install, no newly created startup task either.
    $startNowChoice = $false
}

$interactiveMode = 'auto'
if (-not $Interactive -and -not $NonInteractive -and -not [string]::IsNullOrWhiteSpace($env:IO_GATEWAY_INTERACTIVE)) {
    $interactiveMode = ConvertTo-InstallerMode -Value $env:IO_GATEWAY_INTERACTIVE -Name 'IO_GATEWAY_INTERACTIVE'
}
elseif (-not $Interactive -and -not $NonInteractive -and -not [string]::IsNullOrWhiteSpace($env:IO_GATEWAY_NONINTERACTIVE)) {
    $legacyNonInteractive = ConvertTo-InstallerBoolean -Value $env:IO_GATEWAY_NONINTERACTIVE -Name 'IO_GATEWAY_NONINTERACTIVE'
    if ($legacyNonInteractive) {
        $interactiveMode = 'no'
    }
}
if ($Interactive) {
    $interactiveMode = 'yes'
}
if ($NonInteractive) {
    $interactiveMode = 'no'
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

function Unregister-InstallerTask {
    param([string]$GatewayPath)

    $task = Get-InstallerTask -GatewayPath $GatewayPath
    if ($null -eq $task) {
        return $false
    }
    if (-not (Get-Command Unregister-ScheduledTask -ErrorAction SilentlyContinue)) {
        throw 'Windows Scheduled Tasks are available, but this PowerShell session cannot remove the existing IO Gateway task.'
    }

    Unregister-ScheduledTask -TaskName $task.TaskName -TaskPath $task.TaskPath -Confirm:$false -ErrorAction Stop
    return $true
}

function Register-InstallerTask {
    param(
        [string]$GatewayPath,
        [string]$GatewayConfigPath,
        [bool]$StartNow = $true
    )

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
    if ($StartNow) {
        Start-ScheduledTask -TaskName $GatewayTaskName
    }
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

$gatewayPath = Join-Path $InstallDir 'io-gateway.exe'
$iogwPath = Join-Path $InstallDir 'iogw.exe'
$configAlreadyExists = Test-Path -LiteralPath $ConfigPath
if ($configAlreadyExists -and -not (Test-Path -LiteralPath $ConfigPath -PathType Leaf)) {
    throw "Config path exists but is not a regular file: $ConfigPath"
}

$portWasSpecified = -not [string]::IsNullOrWhiteSpace($Port)
$selectedPort = $null
if ($configAlreadyExists) {
    if ($portWasSpecified) {
        Write-WarningNote "Keeping the existing config at $ConfigPath; the requested port was not applied. Edit config.json to change its listen address."
    }
}
elseif ($portWasSpecified) {
    $selectedPort = ConvertTo-InstallerPort -Value $Port -Name 'Port'
}

$interactiveSetup = $false
switch ($interactiveMode) {
    'yes' {
        $interactiveSetup = Test-InstallerInteractive -Disabled $false
        if (-not $interactiveSetup) {
            throw '-Interactive requires a controlling terminal. Use -NonInteractive for automation.'
        }
    }
    'no' {
        $interactiveSetup = $false
    }
    default {
        $interactiveSetup = Test-InstallerInteractive -Disabled $false
    }
}
if (-not $configAlreadyExists -and $interactiveSetup) {
    Write-Host ''
    Write-Note 'First-run setup. Press Enter to accept the default shown in brackets.'
    if ($null -eq $selectedPort) {
        $selectedPort = Read-AvailableInstallerPort -Default 8319
    }
    if ($null -eq $installIogwChoice) {
        $installIogwChoice = Read-InstallerYesNo -Prompt 'Install the optional iogw management client?' -Default $true
    }
    if ($null -eq $autoStartChoice -and -not $NoStart) {
        $autoStartChoice = Read-InstallerYesNo -Prompt 'Start IO Gateway automatically at Windows sign-in?' -Default $true
        $autoStartExplicit = $true
    }
    if ($null -eq $startNowChoice) {
        $startNowChoice = Read-InstallerYesNo -Prompt 'Start IO Gateway now?' -Default $true
    }
}

if (-not $configAlreadyExists -and $null -eq $selectedPort) {
    $selectedPort = 8319
}

if ($null -eq $startNowChoice) {
    # Retain the original unattended/upgrading behavior unless an explicit
    # option, environment value, or first-run prompt chose otherwise.
    $startNowChoice = $true
}

if (-not $configAlreadyExists -and -not (Test-InstallerPortAvailable -Port $selectedPort)) {
    throw "Port $selectedPort is already in use or unavailable on 127.0.0.1. Choose a free port with -Port, then run the installer again."
}

if ($null -eq $installIogwChoice) {
    # Retain a previous optional-client choice on upgrades. A fresh,
    # noninteractive install keeps the original behavior and installs iogw.
    $installIogwChoice = if ($configAlreadyExists) {
        Test-Path -LiteralPath $iogwPath -PathType Leaf
    }
    else {
        $true
    }
}

$hadStartupTask = $false
try {
    $hadStartupTask = $null -ne (Get-InstallerTask -GatewayPath $gatewayPath)
}
catch {
    # A missing or unavailable Scheduled Tasks API only changes the preferred
    # launch method; direct background startup below remains available.
    $hadStartupTask = $false
}
if ($null -eq $autoStartChoice) {
    if ($configAlreadyExists) {
        # Preserve an earlier first-run choice rather than creating a task on
        # every upgrade. Existing installer-managed tasks are updated below.
        $autoStartChoice = $hadStartupTask
    }
    elseif ($NoStart) {
        $autoStartChoice = $false
    }
    else {
        # Preserve the original unattended first-install behavior.
        $autoStartChoice = $true
    }
}

New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
New-Item -ItemType Directory -Path $ConfigDir -Force | Out-Null

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
    $requiredFiles = @('io-gateway.exe', 'config.example.json')
    if ($installIogwChoice) {
        $requiredFiles += 'iogw.exe'
    }
    foreach ($requiredFile in $requiredFiles) {
        if (-not (Test-Path -LiteralPath (Join-Path $extractDirectory $requiredFile) -PathType Leaf)) {
            throw "Release archive is missing required file: $requiredFile"
        }
    }

    $pidPath = Join-Path $ConfigDir 'io-gateway.pid'
    Stop-ManagedGateway -PidPath $pidPath -GatewayPath $gatewayPath
    Stop-InstallerTask -GatewayPath $gatewayPath

    Install-Binary -SourcePath (Join-Path $extractDirectory 'io-gateway.exe') -DestinationPath $gatewayPath
    $iogwInstalled = $false
    if ($installIogwChoice) {
        Install-Binary -SourcePath (Join-Path $extractDirectory 'iogw.exe') -DestinationPath $iogwPath
        $iogwInstalled = $true
    }
    else {
        Write-Note 'Skipped installation of the optional iogw management client.'
    }
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

        $config.listen = "127.0.0.1:$selectedPort"
        $oauthProperty = $config.PSObject.Properties['oauth']
        $providersProperty = if ($null -ne $oauthProperty -and $null -ne $oauthProperty.Value) {
            $oauthProperty.Value.PSObject.Properties['providers']
        }
        else {
            $null
        }
        $qwenProperty = if ($null -ne $providersProperty -and $null -ne $providersProperty.Value) {
            $providersProperty.Value.PSObject.Properties['qwen']
        }
        else {
            $null
        }
        if ($null -ne $qwenProperty -and $null -ne $qwenProperty.Value) {
            $qwenRedirectUri = "http://127.0.0.1:$selectedPort/login/qwen/callback"
            $redirectUriProperty = $qwenProperty.Value.PSObject.Properties['redirect_uri']
            if ($null -ne $redirectUriProperty) {
                $redirectUriProperty.Value = $qwenRedirectUri
            }
            else {
                $qwenProperty.Value | Add-Member -NotePropertyName redirect_uri -NotePropertyValue $qwenRedirectUri
            }
        }
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
    if ($autoStartExplicit -and -not $autoStartChoice) {
        try {
            if (Unregister-InstallerTask -GatewayPath $gatewayPath) {
                Write-Note 'Removed the per-user IO Gateway startup task.'
            }
        }
        catch {
            Write-WarningNote "Could not remove the per-user startup task: $($_.Exception.Message)"
        }
    }

    if ($autoStartChoice) {
        try {
            $startupTaskRegistered = Register-InstallerTask `
                -GatewayPath $gatewayPath `
                -GatewayConfigPath $ConfigPath `
                -StartNow:$startNowChoice
            if ($startupTaskRegistered) {
                if (-not $startNowChoice) {
                    if ($NoStart) {
                        Write-Note 'Registered a per-user startup task. The gateway was not launched because -NoStart was requested.'
                    }
                    else {
                        Write-Note 'Registered a per-user startup task. The gateway was not launched because starting now was skipped.'
                    }
                }
                else {
                    $started = $true
                    Write-Note 'Started the gateway and registered a per-user startup task.'
                }
            }
            elseif ($startNowChoice) {
                Write-WarningNote 'Windows Scheduled Tasks are unavailable; starting the gateway as a background process instead.'
            }
        }
        catch {
            Write-WarningNote "Could not register the per-user startup task: $($_.Exception.Message)"
        }
    }

    if ($startNowChoice) {
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
    if ($iogwInstalled) {
        Write-Note "Management client: $iogwPath"
    }
    else {
        Write-Note 'Management client: not installed by this run.'
    }
    Write-Note "Config: $ConfigPath"
    if ($pathAdded) {
        Write-Host 'Added the install directory to your user PATH; open a new terminal to use io-gateway and, when installed, iogw by name.'
    }
    if ($createdConfig) {
        Write-Host "The first-run gateway is bound to 127.0.0.1:$selectedPort and admin authentication is disabled only for local setup."
        Write-Host 'Before changing listen to a LAN/public address, configure a TOTP secret and enable admin_auth in config.json.'
        Write-Host 'The generated client API key is stored in proxy_api_key in config.json; keep that file private.'
    }
    if ($started) {
        if ($createdConfig) {
            Write-Host "Open http://127.0.0.1:$selectedPort/ to finish provider setup."
        }
        else {
            Write-Host "The gateway has started with its existing config. Open the configured address in $ConfigPath to finish provider setup."
        }
        if ($startupTaskRegistered) {
            Write-Host 'The gateway will also start automatically at your next sign-in.'
        }
        else {
            Write-Host 'The gateway will not start automatically at your next sign-in.'
        }
    }
    elseif ($startupTaskRegistered) {
        Write-Host 'The gateway is registered to start automatically at your next sign-in.'
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
