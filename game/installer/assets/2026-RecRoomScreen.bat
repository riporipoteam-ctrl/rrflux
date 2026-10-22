@echo off
setlocal
cd /d "%~dp0"
if not exist "Injector.exe" (
    echo Injector.exe is missing. Extract all files from the patch ZIP.
    pause
    exit /b 1
)
"%~dp0Injector.exe" --check-install
if errorlevel 1 (
    pause
    exit /b 1
)
start "Injector" "%~dp0Injector.exe"
"%~dp0Injector.exe" --wait-ready
if errorlevel 1 (
    pause
    exit /b 1
)
start "" "%~dp0RecRoom.exe" +forcemode:screen
