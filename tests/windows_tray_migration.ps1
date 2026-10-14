param(
    [Parameter(Mandatory = $true)][string]$ReleaseArchive
)
$ErrorActionPreference = 'Stop'

# Test the real installer and post-upgrade helper with a local, checksum-verified
# package. All user-scoped configuration/registry changes are restored.
$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$Archive = (Resolve-Path -LiteralPath $ReleaseArchive).Path
$ExpectedName = 'zodex-x86_64-pc-windows-msvc.tar.gz'
$WorkDir = Join-Path ([System.IO.Path]::GetTempPath()) ('zodex-tray-migrate-' + [Guid]::NewGuid().ToString('N'))
$RunKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$OldEntry = (Get-ItemProperty -Path $RunKey -Name ZodexTray -ErrorAction SilentlyContinue).ZodexTray
$OldUserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$OldProcessPath = $env:Path
$OldAssetUrl = $env:ZODEX_ASSET_URL
$OldInstallDir = $env:ZODEX_INSTALL_DIR
$OldNoLaunch = $env:ZODEX_SKIP_TRAY_LAUNCH

function Assert-True([bool]$Condition, [string]$Description) {
    if (-not $Condition) { throw $Description }
}

function Invoke-Installer([string]$InstallDir) {
    $env:ZODEX_INSTALL_DIR = $InstallDir
    & "$RepoRoot/scripts/install.ps1"
    Assert-True ($? -and $LASTEXITCODE -eq 0) 'The Zodex Windows installer failed'
}

function Set-TrayLogin([string]$Tray, [bool]$Enabled) {
    $Flag = if ($Enabled) { '--enable-autostart' } else { '--disable-autostart' }
    $Result = Start-Process -FilePath $Tray -ArgumentList $Flag -Wait -PassThru -WindowStyle Hidden
    Assert-True ($Result.ExitCode -eq 0) "Tray $Flag failed with exit code $($Result.ExitCode)"
}

function Assert-RegisteredTray([string]$ExpectedPath) {
    $Value = (Get-ItemProperty -Path $RunKey -Name ZodexTray -ErrorAction SilentlyContinue).ZodexTray
    Assert-True ([string]::Equals($Value, ('"' + $ExpectedPath + '"'), [StringComparison]::OrdinalIgnoreCase)) `
        "Wrong Launch at Login value. Expected $ExpectedPath, got '$Value'"
}

try {
    New-Item -ItemType Directory -Path $WorkDir -Force | Out-Null
    $LocalArchive = Join-Path $WorkDir $ExpectedName
    Copy-Item -LiteralPath $Archive -Destination $LocalArchive
    $Hash = (Get-FileHash -LiteralPath $LocalArchive -Algorithm SHA256).Hash.ToLowerInvariant()
    "$Hash  $ExpectedName" | Set-Content -Encoding ascii "$LocalArchive.sha256"
    $env:ZODEX_ASSET_URL = [Uri]::new($LocalArchive).AbsoluteUri
    $env:ZODEX_SKIP_TRAY_LAUNCH = '1' # No desktop session on CI runners.
    Remove-ItemProperty -Path $RunKey -Name ZodexTray -ErrorAction SilentlyContinue

    $Extracted = Join-Path $WorkDir 'extracted'
    New-Item -ItemType Directory -Path $Extracted | Out-Null
    & tar -C $Extracted -xzf $LocalArchive
    Assert-True ($LASTEXITCODE -eq 0) 'Failed to unpack release test fixture'
    $ReleaseDir = Join-Path $Extracted 'zodex-x86_64-pc-windows-msvc'
    $ReleaseCli = Join-Path $ReleaseDir 'zodex.exe'
    $ReleaseTray = Join-Path $ReleaseDir 'zodex-tray.exe'
    Assert-True ((Test-Path -LiteralPath $ReleaseCli -PathType Leaf) -and
        (Test-Path -LiteralPath $ReleaseTray -PathType Leaf)) 'Release must contain both Zodex executables'

    # A configured 0.5.6 installation has the CLI but no tray.
    $Installed = Join-Path $WorkDir 'existing-install'
    New-Item -ItemType Directory -Path $Installed | Out-Null
    Set-Content -Path (Join-Path $Installed 'zodex.exe') -Value 'v0.5.6 fixture'
    Invoke-Installer $Installed
    $InstalledCli = Join-Path $Installed 'zodex.exe'
    $InstalledTray = Join-Path $Installed 'zodex-tray.exe'
    & $InstalledCli --version
    Assert-True ($LASTEXITCODE -eq 0) 'Windows installer did not replace the old CLI'
    Assert-True (Test-Path -LiteralPath $InstalledTray -PathType Leaf) 'Windows tray was not installed'
    Assert-RegisteredTray $InstalledTray

    # No setup, and deliberate login opt-out must survive repeated upgrades.
    Set-TrayLogin $InstalledTray $false
    Invoke-Installer $Installed
    $Registered = (Get-ItemProperty -Path $RunKey -Name ZodexTray -ErrorAction SilentlyContinue).ZodexTray
    Assert-True (-not $Registered) 'Repeated install reversed Launch at Login opt-out'

    # Fresh installs must still defer tray registration to `zodex local setup`
    # so setup --no-menu-bar is honored.
    $Fresh = Join-Path $WorkDir 'fresh-install'
    Invoke-Installer $Fresh
    Assert-True (Test-Path -LiteralPath (Join-Path $Fresh 'zodex-tray.exe')) 'Fresh install lacks tray binary'
    $Registered = (Get-ItemProperty -Path $RunKey -Name ZodexTray -ErrorAction SilentlyContinue).ZodexTray
    Assert-True (-not $Registered) 'Fresh install registered tray before the setup opt-out decision'

    # A 0.5.7 self-upgrader can install the CLI first but leave the tray
    # missing; the new helper must repair that on the next upgrade.
    $SelfUpgrade = Join-Path $WorkDir 'self-upgrade'
    New-Item -ItemType Directory -Path $SelfUpgrade | Out-Null
    $PendingCli = Join-Path $SelfUpgrade 'pending-zodex.exe'
    $PendingTray = Join-Path $SelfUpgrade 'pending-zodex-tray.exe'
    $UpgradedCli = Join-Path $SelfUpgrade 'zodex.exe'
    $UpgradedTray = Join-Path $SelfUpgrade 'zodex-tray.exe'
    Copy-Item $ReleaseCli $PendingCli
    Copy-Item $ReleaseTray $PendingTray
    Set-Content -Path $UpgradedCli -Value 'v0.5.7 fixture'
    $Helper = Join-Path $SelfUpgrade 'finish-upgrade.ps1'
    Copy-Item "$RepoRoot/scripts/finish-windows-upgrade.ps1" $Helper
    & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $Helper `
        -Source $PendingCli -Destination $UpgradedCli `
        -TraySource $PendingTray -TrayDestination $UpgradedTray `
        -ParentPid 99999999 -ScriptPath $Helper
    Assert-True ($LASTEXITCODE -eq 0) 'Self-upgrade migration helper failed'
    Assert-True (Test-Path -LiteralPath $UpgradedTray -PathType Leaf) 'Self-upgrade did not install the tray'
    Assert-RegisteredTray $UpgradedTray
    Set-TrayLogin $UpgradedTray $false

    Write-Host 'PASS: Windows tray migration, registration, opt-out preservation, fresh install, and self-upgrade.'
} finally {
    if ($null -ne $OldEntry) {
        Set-ItemProperty -Path $RunKey -Name ZodexTray -Value $OldEntry
    } else {
        Remove-ItemProperty -Path $RunKey -Name ZodexTray -ErrorAction SilentlyContinue
    }
    [Environment]::SetEnvironmentVariable('Path', $OldUserPath, 'User')
    $env:Path = $OldProcessPath
    $env:ZODEX_ASSET_URL = $OldAssetUrl
    $env:ZODEX_INSTALL_DIR = $OldInstallDir
    $env:ZODEX_SKIP_TRAY_LAUNCH = $OldNoLaunch
    Remove-Item -LiteralPath $WorkDir -Recurse -Force -ErrorAction SilentlyContinue
}
