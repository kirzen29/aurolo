# Builds Aurolo and installs it for the current user:
#   %LOCALAPPDATA%\Programs\Aurolo\aurolo.exe  + a Start menu shortcut ("Aurolo"),
# so it can be found with Windows search. Re-run after pulling changes to update.
#
#   powershell -ExecutionPolicy Bypass -File scripts\install.ps1

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

Write-Host 'Building Aurolo (release)...'
cargo build --release
if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

$dir = Join-Path $env:LOCALAPPDATA 'Programs\Aurolo'
$exe = Join-Path $dir 'aurolo.exe'
New-Item -ItemType Directory -Force $dir | Out-Null

# a running copy locks the file; Aurolo restores the Windows taskbar when it quits
$running = Get-Process aurolo -ErrorAction SilentlyContinue
if ($running) {
    $running | Stop-Process -Force
    Start-Sleep -Milliseconds 800
}
Copy-Item (Join-Path $repo 'target\release\aurolo.exe') $exe -Force

$lnk = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Aurolo.lnk'
$shell = New-Object -ComObject WScript.Shell
$s = $shell.CreateShortcut($lnk)
$s.TargetPath = $exe
$s.WorkingDirectory = $dir
$s.IconLocation = "$exe,0"
$s.Description = 'Dynamic Island, synced lyrics, edge glow and taskbar dock'
$s.Save()

Start-Process $exe
Write-Host "Installed to $exe"
Write-Host 'Search "Aurolo" in Windows to open it.'
