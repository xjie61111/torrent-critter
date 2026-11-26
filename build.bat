@echo off
chcp 65001 >nul
cd /d "%~dp0"
set "CARGO_BIN=%USERPROFILE%\.cargo\bin"
if exist "%CARGO_BIN%\cargo.exe" set "PATH=%CARGO_BIN%;%PATH%"
cargo build --release
if errorlevel 1 (
  echo.
  echo Сборка не удалась. Проверьте, что установлены Rust и Microsoft C++ Build Tools.
  pause
  exit /b 1
)
copy /Y "target\release\torrent-client.exe" "torrent-client.exe" >nul
echo.
echo Готово: %~dp0torrent-client.exe
pause
