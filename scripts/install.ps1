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
    if (-not (Test-Path -LiteralPath $Source -PathType Leaf)) {
        throw 'The Zodex Windows release archive did not contain zodex.exe.'
    }

    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    Copy-Item -LiteralPath $Source -Destination (Join-Path $InstallDir 'zodex.exe') -Force

    $UserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $PathParts = @($UserPath -split ';' | Where-Object { $_ })
    if (-not ($PathParts | Where-Object { $_.TrimEnd('\') -ieq $InstallDir.TrimEnd('\') })) {
        $NewUserPath = (@($PathParts) + $InstallDir) -join ';'
        [Environment]::SetEnvironmentVariable('Path', $NewUserPath, 'User')
    }
    if (-not (($env:Path -split ';') | Where-Object { $_.TrimEnd('\') -ieq $InstallDir.TrimEnd('\') })) {
        $env:Path = "$InstallDir;$env:Path"
    }

    Write-Host "Installed Zodex to $(Join-Path $InstallDir 'zodex.exe')"
    Write-Host 'Run: zodex local setup'
} finally {
    Remove-Item -LiteralPath $Temp -Recurse -Force -ErrorAction SilentlyContinue
}
