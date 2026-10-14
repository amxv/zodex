param(
    [Parameter(Mandatory=$true)][string]$Source,
    [Parameter(Mandatory=$true)][string]$Destination,
    [Parameter(Mandatory=$true)][string]$TraySource,
    [Parameter(Mandatory=$true)][string]$TrayDestination,
    [Parameter(Mandatory=$true)][int]$ParentPid,
    [Parameter(Mandatory=$true)][string]$ScriptPath
)
$ErrorActionPreference = 'Stop'
try {
    # The parent CLI has to exit before Windows allows replacing zodex.exe.
    Wait-Process -Id $ParentPid -ErrorAction SilentlyContinue
    Copy-Item -LiteralPath $Source -Destination $Destination -Force
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
    if ($trayWasRunning) { Start-Process -FilePath $TrayDestination -WindowStyle Hidden }
} finally {
    Remove-Item -LiteralPath $ScriptPath -Force -ErrorAction SilentlyContinue
}
