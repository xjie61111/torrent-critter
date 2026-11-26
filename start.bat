@echo off
chcp 65001 >nul
cd /d "%~dp0"
if not exist torrent-client.exe (
  echo Сначала запустите build.bat
  pause
  exit /b 1
)
start "" torrent-client.exe
