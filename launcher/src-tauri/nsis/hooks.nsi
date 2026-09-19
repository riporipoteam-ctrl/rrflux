; RRFlux installer hooks — included by Tauri's NSIS template.
; Creates a desktop shortcut on install, removes it on uninstall.

!macro customInstall
  CreateShortCut "$DESKTOP\RRFlux Launcher.lnk" "$INSTDIR\RRFlux Launcher.exe" "" "$INSTDIR\RRFlux Launcher.exe" 0
!macroend

!macro customUnInstall
  Delete "$DESKTOP\RRFlux Launcher.lnk"
!macroend
