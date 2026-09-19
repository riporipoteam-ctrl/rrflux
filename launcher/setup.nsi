; RRFlux setup.exe — NSIS installer scaffold
; Builds on Linux with: makensis launcher/setup.nsi
; Produces: RRFlux-Setup.exe (Windows installer)

!include "MUI2.nsh"

Name "RRFlux"
OutFile "..\dist\RRFlux-Setup.exe"
InstallDir "$LOCALAPPDATA\RRFlux"
RequestExecutionLevel user

; --- Distribution endpoint (patched build payload lives here) ---
!define GAME_URL "https://dist.rrflux.example/game.zip"
!define GAME_SHA256 ""  ; fill at release time; installer verifies before extract

!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Section "Install"
  SetOutPath "$INSTDIR"

  ; Download the game payload (patched client)
  DetailPrint "Downloading RRFlux game files..."
  ; NOTE: replace with real download at build time.
  ; inetc::get is the classic NSIS download plugin; wire GAME_URL here.
  ; inetc::get "${GAME_URL}" "$INSTDIR\game.zip" /END

  ; Verify checksum, then extract
  ; (checksum check placeholder — mandatory before extract)

  ; Create shortcuts
  CreateDirectory "$SMPROGRAMS\RRFlux"
  CreateShortcut "$SMPROGRAMS\RRFlux\RRFlux.lnk" "$INSTDIR\RRFlux-Launcher.exe"
  CreateShortcut "$DESKTOP\RRFlux.lnk" "$INSTDIR\RRFlux-Launcher.exe"

  WriteUninstaller "$INSTDIR\Uninstall.exe"
SectionEnd

Section "Uninstall"
  Delete "$SMPROGRAMS\RRFlux\RRFlux.lnk"
  Delete "$DESKTOP\RRFlux.lnk"
  RMDir /r "$INSTDIR"
SectionEnd
