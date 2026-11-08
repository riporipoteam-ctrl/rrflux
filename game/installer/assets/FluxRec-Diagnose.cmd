@echo off
setlocal DisableDelayedExpansion
set "FLUX_GAME_DIR="
if exist "%~dp0RecRoom.exe" set "FLUX_GAME_DIR=%~dp0"
if not defined FLUX_GAME_DIR if exist "%LOCALAPPDATA%\FluxRec\install_dir.txt" set /p FLUX_GAME_DIR=<"%LOCALAPPDATA%\FluxRec\install_dir.txt"
if not defined FLUX_GAME_DIR goto missing
set "FLUX_DIAGNOSTICS=%FLUX_GAME_DIR%\Diagnostics"
if not exist "%FLUX_DIAGNOSTICS%" mkdir "%FLUX_DIAGNOSTICS%" 2>nul
type nul >>"%FLUX_DIAGNOSTICS%\Player.log" 2>nul
if errorlevel 1 set "FLUX_DIAGNOSTICS=%LOCALAPPDATA%\FluxRec\Diagnostics"
if not exist "%FLUX_DIAGNOSTICS%" mkdir "%FLUX_DIAGNOSTICS%"
if not exist "%FLUX_GAME_DIR%\RecRoom.exe" goto missing
if not exist "%FLUX_GAME_DIR%\Injector.exe" goto missing
if not exist "%FLUX_GAME_DIR%\2026patch.dll" goto missing
pushd "%FLUX_GAME_DIR%"
echo Checking the installed Flux Rec patch...
"%FLUX_GAME_DIR%\Injector.exe" --check-install >"%FLUX_DIAGNOSTICS%\Injector-check.log" 2>&1
if errorlevel 1 goto failed
start "" /b "%FLUX_GAME_DIR%\Injector.exe" >"%FLUX_DIAGNOSTICS%\Injector.log" 2>&1
"%FLUX_GAME_DIR%\Injector.exe" --wait-ready >>"%FLUX_DIAGNOSTICS%\Injector-check.log" 2>&1
if errorlevel 1 goto failed
echo Logs are being written to "%FLUX_DIAGNOSTICS%".
start "" "%FLUX_DIAGNOSTICS%"
"%FLUX_GAME_DIR%\RecRoom.exe" +forcemode:screen -logFile "%FLUX_DIAGNOSTICS%\Player.log"
if exist "%FLUX_GAME_DIR%\2026patch.log" copy /y "%FLUX_GAME_DIR%\2026patch.log" "%FLUX_DIAGNOSTICS%\2026patch.log" >nul
popd
exit /b 0
:failed
echo The patch preflight or injector startup failed. See Injector-check.log.
start "" "%FLUX_DIAGNOSTICS%"
popd
pause
exit /b 1
:missing
echo Could not locate a complete Flux Rec installation.
echo Put this file beside RecRoom.exe, Injector.exe and 2026patch.dll, then run it again.
pause
exit /b 1
