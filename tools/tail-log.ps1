# Tail the newest Life Zone log. A .ps1 rather than an inline command because
# `$_` does not survive being passed through a bash-hosted `cmd.exe /c`.
$dir = "$env:APPDATA\com.lifezone.app\logs"
if (-not (Test-Path $dir)) { Write-Output "no log directory at $dir"; exit 1 }
$latest = Get-ChildItem $dir | Sort-Object LastWriteTime | Select-Object -Last 1
Write-Output "== $($latest.Name) =="
Get-Content $latest.FullName -Tail 20
