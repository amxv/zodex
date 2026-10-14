$ErrorActionPreference = 'Stop'

$Repo = if ($env:ZODEX_REPO) { $env:ZODEX_REPO } else { 'amxv/zodex' }
$Version = if ($env:ZODEX_VERSION) { $env:ZODEX_VERSION } else { 'latest' }
$AssetUrl = if ($env:ZODEX_ASSET_URL) { $env:ZODEX_ASSET_URL } else { $null }
$InstallDir = if ($env:ZODEX_INSTALL_DIR) {
    $env:ZODEX_INSTALL_DIR
} else {
    Join-Path $env:LOCALAPPDATA 'Programs\Zodex'
}

if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne [System.Runtime.InteropServices.Architecture]::X64) {
    throw 'The published Zodex Windows operator currently supports x86_64 Windows only.'
}

$ArchiveName = 'zodex-x86_64-pc-windows-msvc.tar.gz'
$Tag = if ($Version -eq 'latest') { $null } elseif ($Version.StartsWith('v')) { $Version } else { "v$Version" }
$Base = if ($AssetUrl) {
    $AssetUrl
} elseif ($Tag) {
    "https://github.com/$Repo/releases/download/$Tag/$ArchiveName"
} else {
    "https://github.com/$Repo/releases/latest/download/$ArchiveName"
}

function Copy-OrDownload([string]$Source, [string]$Destination) {
    if ($Source.StartsWith('file://', [StringComparison]::OrdinalIgnoreCase)) {
        Copy-Item -LiteralPath ([Uri]$Source).LocalPath -Destination $Destination -Force
        return
    }
    Invoke-WebRequest -UseBasicParsing -Uri $Source -OutFile $Destination
}

$Temp = Join-Path ([System.IO.Path]::GetTempPath()) ("zodex-install-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $Temp | Out-Null
try {
    $Archive = Join-Path $Temp $ArchiveName
    $Checksum = "$Archive.sha256"
    Copy-OrDownload $Base $Archive
    Copy-OrDownload "$Base.sha256" $Checksum

    $Expected = ((Get-Content -Raw $Checksum).Trim() -split '\s+')[0].ToLowerInvariant()
    $Actual = (Get-FileHash $Archive -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($Expected -ne $Actual) {
        throw "Zodex release checksum mismatch: expected $Expected, got $Actual"
    }

    tar -C $Temp -xzf $Archive
    if ($LASTEXITCODE -ne 0) { throw 'Failed to extract the Zodex release archive.' }
    $Source = Join-Path $Temp 'zodex-x86_64-pc-windows-msvc\zodex.exe'
    $TraySource = Join-Path $Temp 'zodex-x86_64-pc-windows-msvc\zodex-tray.exe'
    if (-not (Test-Path -LiteralPath $Source -PathType Leaf)) {
        throw 'The Zodex Windows release archive did not contain zodex.exe.'
    }
    if (-not (Test-Path -LiteralPath $TraySource -PathType Leaf)) {
        throw 'The Zodex Windows release archive did not contain zodex-tray.exe.'
    }

    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    $InstalledCli = Join-Path $InstallDir 'zodex.exe'
    $CliWasInstalled = Test-Path -LiteralPath $InstalledCli -PathType Leaf
    $InstalledTray = Join-Path $InstallDir 'zodex-tray.exe'
    $TrayWasInstalled = Test-Path -LiteralPath $InstalledTray -PathType Leaf
    # Do not interrupt a running Local runtime or overwrite its loaded binary.
    # This installer is also the upgrade path for v0.5.6, whose self-updater
    # does not know how to install additional executables.
    $RunningLocal = @(Get-CimInstance Win32_Process -Filter "Name='zodex.exe'" -ErrorAction SilentlyContinue |
        Where-Object { $_.ExecutablePath -and $_.ExecutablePath -ieq $InstalledCli })
    if ($RunningLocal.Count -gt 0) {
        throw 'Zodex is running from this installation. Run zodex local stop before upgrading; the installer will not stop Local for you.'
    }
    $WasRunning = $false
    # Windows locks the executable of a running tray. Stop only the matching
    # per-user helper at our destination, never other Zodex installations.
    Get-CimInstance Win32_Process -Filter "Name='zodex-tray.exe'" -ErrorAction SilentlyContinue |
        Where-Object { $_.ExecutablePath -and $_.ExecutablePath -ieq $InstalledTray } |
        ForEach-Object {
            $WasRunning = $true
            Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue
        }
    try {
        Copy-Item -LiteralPath $Source -Destination $InstalledCli -Force
        $Replaced = $false
        for ($Attempt = 0; $Attempt -lt 40; $Attempt++) {
            try {
                Copy-Item -LiteralPath $TraySource -Destination $InstalledTray -Force -ErrorAction Stop
                $Replaced = $true
                break
            } catch {
                Start-Sleep -Milliseconds 250
            }
        }
        if (-not $Replaced) { throw 'Could not replace the Zodex tray executable.' }
    } finally {
        if ($WasRunning -and (Test-Path -LiteralPath $InstalledTray -PathType Leaf)) {
            Start-Process -FilePath $InstalledTray -WindowStyle Hidden
        }
    }

    if ($CliWasInstalled -and -not $TrayWasInstalled) {
        # The operator was already installed but the tray was not (0.5.6
        # upgrades). Only register the convenience tray, never Local itself.
        # A fresh install waits for `zodex local setup` so --no-menu-bar works;
        # a later upgrade preserves users' explicit login/quit preferences.
        $Registration = Start-Process -FilePath $InstalledTray -ArgumentList '--enable-autostart' -Wait -PassThru -WindowStyle Hidden
        if ($Registration.ExitCode -ne 0) {
            throw "Zodex tray was installed, but Launch at Login registration failed (exit code $($Registration.ExitCode))."
        }
        if ($env:ZODEX_SKIP_TRAY_LAUNCH -ne '1') {
            Start-Process -FilePath $InstalledTray -WindowStyle Hidden
        }
        Write-Host 'Zodex tray enabled at login. Zodex Local remains stopped until manually started.'
    }

    $UserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $PathParts = @($UserPath -split ';' | Where-Object { $_ })
    if (-not ($PathParts | Where-Object { $_.TrimEnd('\') -ieq $InstallDir.TrimEnd('\') })) {
        $NewUserPath = (@($PathParts) + $InstallDir) -join ';'
        [Environment]::SetEnvironmentVariable('Path', $NewUserPath, 'User')
    }
    if (-not (($env:Path -split ';') | Where-Object { $_.TrimEnd('\') -ieq $InstallDir.TrimEnd('\') })) {
        $env:Path = "$InstallDir;$env:Path"
    }

    Write-Host "Installed Zodex to $InstalledCli"
    Write-Host 'If this is a first installation, run: zodex local setup'
} finally {
    Remove-Item -LiteralPath $Temp -Recurse -Force -ErrorAction SilentlyContinue
}
