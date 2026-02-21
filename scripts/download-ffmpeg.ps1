$ProgressPreference = 'SilentlyContinue'
$zip  = "$env:TEMP\ffmpeg-dl.zip"
$tmp  = "$env:TEMP\ffmpeg-tmp"
$dest = "D:\Projects\NeuroScreenCaster\src-tauri\binaries\ffmpeg-x86_64-pc-windows-msvc.exe"

Write-Host "Downloading FFmpeg release essentials..."
Invoke-WebRequest -Uri "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip" `
    -OutFile $zip -UseBasicParsing

Write-Host "Extracting..."
Expand-Archive -Path $zip -DestinationPath $tmp -Force

$exe = Get-ChildItem -Path $tmp -Recurse -Filter "ffmpeg.exe" | Select-Object -First 1
if (-not $exe) { Write-Error "ffmpeg.exe not found in archive"; exit 1 }

Copy-Item $exe.FullName -Destination $dest -Force

Remove-Item $zip  -Force
Remove-Item $tmp  -Recurse -Force

Write-Host "Done!"
$info = Get-Item $dest
Write-Host "  $($info.Name)  $([math]::Round($info.Length/1MB, 1)) MB"
