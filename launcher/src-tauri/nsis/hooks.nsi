; Flux Rec installer hooks — included by Tauri's NSIS template.
; Creates a desktop shortcut on install, removes it on uninstall.

!macro customInstall
  CreateShortCut "$DESKTOP\Flux Rec.lnk" "$INSTDIR\Flux Rec.exe" "" "$INSTDIR\Flux Rec.exe" 0
!macroend

!macro customUnInstall
  Delete "$DESKTOP\Flux Rec.lnk"
!macroend
