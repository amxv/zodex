param(
    [Parameter(Mandatory=$true)][string]$Source,
    [Parameter(Mandatory=$true)][string]$Destination,
    [Parameter(Mandatory=$true)][string]$TraySource,
    [Parameter(Mandatory=$true)][string]$TrayDestination,
    [Parameter(Mandatory=$true)][int]$ParentPid,
    [Parameter(Mandatory=$true)][string]$ScriptPath,
    [string]$RestartDirectory = '',
    [string]$RestartExpiresAt = ''
)
$ErrorActionPreference = 'Stop'
try {
    # The parent CLI has to exit before Windows allows replacing zodex.exe.
    Wait-Process -Id $ParentPid -ErrorAction SilentlyContinue
    Copy-Item -LiteralPath $Source -Destination $Destination -Force
    $trayWasInstalled = Test-Path -LiteralPath $TrayDestination -PathType Leaf
    $trayWasRunning = $false
    # Only stop a tray helper belonging to this exact installation.
    $trays = @(Get-CimInstance Win32_Process -Filter "Name='zodex-tray.exe'" -ErrorAction SilentlyContinue |
        Where-Object { $_.ExecutablePath -and $_.ExecutablePath -ieq $TrayDestination })
    foreach ($tray in $trays) {
        $trayWasRunning = $true
        Stop-Process -Id $tray.ProcessId -Force -ErrorAction SilentlyContinue
        Wait-Process -Id $tray.ProcessId -Timeout 5 -ErrorAction SilentlyContinue
    }
    $replaced = $false
    for ($attempt = 0; $attempt -lt 40; $attempt++) {
        try {
            Copy-Item -LiteralPath $TraySource -Destination $TrayDestination -Force -ErrorAction Stop
            $replaced = $true
            break
        } catch {
            Start-Sleep -Milliseconds 250
        }
    }
    if (-not $replaced) { throw 'Could not replace the Zodex tray executable' }
    Remove-Item -LiteralPath $Source -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $TraySource -Force -ErrorAction SilentlyContinue
    if (-not $trayWasInstalled) {
        # A 0.5.7 CLI may have arrived through the 0.5.6 self-updater, which
        # copied only zodex.exe. This is the first time we install the tray.
        $registration = Start-Process -FilePath $TrayDestination -ArgumentList '--enable-autostart' -Wait -PassThru -WindowStyle Hidden
        if ($registration.ExitCode -ne 0) {
            throw "Failed to enable the new Zodex tray at login (exit code $($registration.ExitCode))."
        }
        if ($env:ZODEX_SKIP_TRAY_LAUNCH -ne '1') {
            Start-Process -FilePath $TrayDestination -WindowStyle Hidden
        }
    } elseif ($trayWasRunning) {
        # Preserve deliberate Quit and Launch at Login opt-out choices.
        Start-Process -FilePath $TrayDestination -WindowStyle Hidden
    }
} finally {
    if ($RestartDirectory -and (Test-Path -LiteralPath $Destination -PathType Leaf)) {
        # The parent CLI exits before replacement on Windows. Restart from
        # this detached helper *after* the new executable has been installed.
        # A failed replacement still restarts with the previous usable binary.
        try {
            $argsForStart = @('local', 'start', $RestartDirectory)
            if ($RestartExpiresAt) {
                $deadline = [DateTimeOffset]::Parse($RestartExpiresAt)
                $ttlSeconds = [math]::Floor(($deadline - [DateTimeOffset]::UtcNow).TotalSeconds)
                if ($ttlSeconds -le 0) {
                    throw 'The original Zodex Local TTL expired during upgrade; not extending it'
                }
                $argsForStart += @('--ttl', "$ttlSeconds" + 's')
            }
            & $Destination @argsForStart
            if ($LASTEXITCODE -ne 0) {
                throw "Zodex Local restart exited with code $LASTEXITCODE"
            }
        } catch {
            # A failed restart is significant, but should not hide the
            # installer result or prevent temporary helper cleanup.
            Write-Warning "Zodex automatic update could not restart Local: $_"
        }
    }
    Remove-Item -LiteralPath $ScriptPath -Force -ErrorAction SilentlyContinue
}
