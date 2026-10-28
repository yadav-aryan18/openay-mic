# OpenAY Mic - Windows test-session logger.
#
# Wraps openay-server.exe so every run is captured to a timestamped log
# directory for debugging, while still streaming the output to the console:
#
#   .\openay-log.ps1 --transport udp --output
#   .\openay-log.ps1 --transport tcp --output --output-device "CABLE Input"
#   .\openay-log.ps1 --list-devices
#
# Logs land in "$HOME\OpenAY-logs\<yyyyMMdd-HHmmss>\openay-server.log".
# Ctrl-C stops the server; the log file is kept.
#
# Optional phone-side capture (USB debugging enabled, device attached):
#   adb logcat -s MicCaptureService:* openaymic:* > phone.logcat
# run in a second terminal while the session is live.

$ErrorActionPreference = "Stop"

$exe = Join-Path $PSScriptRoot "openay-server.exe"
if (-not (Test-Path $exe)) {
    Write-Error "openay-server.exe not found next to this script ($PSScriptRoot)"
    exit 1
}

$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$logDir = Join-Path $HOME "OpenAY-logs\$stamp"
New-Item -ItemType Directory -Force $logDir | Out-Null

# Environment snapshot: versions and selected output devices, so a bug
# report can be reproduced against the exact build and device.
& $exe --list-devices 2>&1 | Out-File (Join-Path $logDir "devices.txt")
"OpenAY log session $stamp" | Out-File (Join-Path $logDir "session.txt")

Write-Host "Logging to $logDir\openay-server.log" -ForegroundColor Cyan

try {
    & $exe @args 2>&1 | ForEach-Object {
        $_ | Tee-Object -FilePath (Join-Path $logDir "openay-server.log") -Append
    }
} finally {
    Write-Host "`nLog saved: $logDir\openay-server.log" -ForegroundColor Cyan
}
