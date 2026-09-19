; Flux Rec setup — plain NSIS installer (built with makensis on CI).
; Installs the headless game bootstrapper, downloads the full game files
; during setup, and creates a desktop shortcut. No launcher, no login:
; double-clicking "Flux Rec" starts the game directly.
;
; Build: makensis /DVERSION=0.2.0 /DSTAGEDIR=<abs path to stage dir> `
;                /DOUTFILE=<abs output path> game/installer/fluxrec.nsi

!include "MUI2.nsh"
!include "LogicLib.nsh"

!ifndef VERSION
  !define VERSION "0.2.0"
!endif
!ifndef STAGEDIR
  !define STAGEDIR "stage"
!endif
!ifndef OUTFILE
  !define OUTFILE "FluxRec-Setup.exe"
!endif
!ifndef MANIFEST_URL
  !define MANIFEST_URL "https://huggingface.co/datasets/Echoxr/rrflux-game/resolve/main/manifest.json"
!endif

Name "Flux Rec ${VERSION}"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\FluxRec"
RequestExecutionLevel user

!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Section "Flux Rec"
  SetOutPath "$INSTDIR"

  ; Headless bootstrapper (this is what the shortcuts point at) +
  ; the download helper used below.
  File "${STAGEDIR}\Flux Rec.exe"
  File "${STAGEDIR}\fluxrec-download.exe"

  ; Fetch the full game (~7 GB, one-time) straight into $INSTDIR\game.
  DetailPrint "Downloading game files (one-time, about 7 GB)..."
  nsExec::ExecToLog '"$INSTDIR\fluxrec-download.exe" --manifest "${MANIFEST_URL}" --dir "$INSTDIR\game"'
  Pop $0
  ${If} $0 != "0"
    MessageBox MB_ICONSTOP "The game download failed, so setup cannot continue.$\nCheck your internet connection and run the installer again."
    Abort
  ${EndIf}

  ; Shortcuts: the game, directly.
  CreateDirectory "$SMPROGRAMS\Flux Rec"
  CreateShortcut "$SMPROGRAMS\Flux Rec\Flux Rec.lnk" "$INSTDIR\Flux Rec.exe"
  CreateShortcut "$DESKTOP\Flux Rec.lnk" "$INSTDIR\Flux Rec.exe"

  WriteUninstaller "$INSTDIR\Uninstall.exe"
SectionEnd

Section "Uninstall"
  Delete "$SMPROGRAMS\Flux Rec\Flux Rec.lnk"
  Delete "$DESKTOP\Flux Rec.lnk"
  RMDir /r "$INSTDIR\game"
  Delete "$INSTDIR\Flux Rec.exe"
  Delete "$INSTDIR\fluxrec-download.exe"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$SMPROGRAMS\Flux Rec"
  RMDir "$INSTDIR"
SectionEnd
