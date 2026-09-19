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

; Branded icon for the installer, uninstaller, and shortcuts.
!define MUI_ICON "${STAGEDIR}\fluxrec.ico"
!define MUI_UNICON "${STAGEDIR}\fluxrec.ico"

!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Section "Flux Rec"
  SetOutPath "$INSTDIR"

  ; Headless bootstrapper (this is what the shortcuts point at),
  ; the download helper used below, and the self-updater the
  ; bootstrapper uses to replace itself on auto-update.
  File "${STAGEDIR}\Flux Rec.exe"
  File "${STAGEDIR}\fluxrec-download.exe"
  File "${STAGEDIR}\fluxrec-selfupdate.exe"
  File "${STAGEDIR}\fluxrec.ico"

  ; Fetch the full game (one-time) straight into $INSTDIR\game.
  ; The downloader shows its own progress window, downloads files in
  ; parallel, resumes interrupted files where they broke off, and keeps
  ; the PC awake. Flaky connections get a few automatic retries; every
  ; retry resumes partial files instead of starting over.
  ; After this, the bootstrapper auto-updates on every launch, so
  ; setup never needs to run again.
  DetailPrint "Downloading game files (one-time)..."
  StrCpy $R9 0
  download_retry:
  nsExec::ExecToLog '"$INSTDIR\fluxrec-download.exe" --manifest "${MANIFEST_URL}" --dir "$INSTDIR\game" --state-dir "$INSTDIR"'
  Pop $0
  ${If} $0 != "0"
    IntOp $R9 $R9 + 1
    ${If} $R9 < 3
      DetailPrint "Download interrupted -- retrying ($R9/3), resuming partial files..."
      Goto download_retry
    ${EndIf}
    MessageBox MB_ICONSTOP "The game download failed, so setup cannot continue.$\nCheck your internet connection and run the installer again -- it resumes where it left off."
    Abort
  ${EndIf}

  ; Shortcuts: the game, directly.
  CreateDirectory "$SMPROGRAMS\Flux Rec"
  CreateShortcut "$SMPROGRAMS\Flux Rec\Flux Rec.lnk" "$INSTDIR\Flux Rec.exe" "" "$INSTDIR\fluxrec.ico"
  CreateShortcut "$DESKTOP\Flux Rec.lnk" "$INSTDIR\Flux Rec.exe" "" "$INSTDIR\fluxrec.ico"

  WriteUninstaller "$INSTDIR\Uninstall.exe"
SectionEnd

Section "Uninstall"
  Delete "$SMPROGRAMS\Flux Rec\Flux Rec.lnk"
  Delete "$DESKTOP\Flux Rec.lnk"
  RMDir /r "$INSTDIR\game"
  Delete "$INSTDIR\Flux Rec.exe"
  Delete "$INSTDIR\Flux Rec.new.exe"
  Delete "$INSTDIR\fluxrec-download.exe"
  Delete "$INSTDIR\fluxrec-selfupdate.exe"
  Delete "$INSTDIR\fluxrec.ico"
  Delete "$INSTDIR\manifest.json"
  Delete "$INSTDIR\manifest.etag"
  Delete "$INSTDIR\translator.log"
  Delete "$INSTDIR\crash.log"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$SMPROGRAMS\Flux Rec"
  RMDir "$INSTDIR"
SectionEnd
