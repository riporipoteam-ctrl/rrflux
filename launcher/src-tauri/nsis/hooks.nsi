; Flux Rec installer hooks — included by Tauri's NSIS template.
; Adds an "Installation options" page with a desktop-shortcut checkbox.
; The shortcut is created only when checked; removed on uninstall.

!include "MUI2.nsh"
!include "nsDialogs.nsh"

Var FluxRec_CreateDesktopShortcut
Var FluxRec_ShortcutCheckbox

Function FluxRecOptionsPage
  !insertmacro MUI_HEADER_TEXT "Installation options" "Choose which shortcuts to create."
  nsDialogs::Create 1018
  Pop $0
  StrCmp $0 "error" 0 +2
    Abort
  ${NSD_CreateCheckbox} 10u 20u 90% 12u "Create a desktop shortcut"
  Pop $FluxRec_ShortcutCheckbox
  ${NSD_Check} $FluxRec_ShortcutCheckbox
  nsDialogs::Show
FunctionEnd

Function FluxRecOptionsPageLeave
  ${NSD_GetState} $FluxRec_ShortcutCheckbox $FluxRec_CreateDesktopShortcut
FunctionEnd

Page custom FluxRecOptionsPage FluxRecOptionsPageLeave

!macro customInstall
  StrCmp $FluxRec_CreateDesktopShortcut "1" 0 +2
    CreateShortCut "$DESKTOP\Flux Rec.lnk" "$INSTDIR\Flux Rec.exe" "" "$INSTDIR\Flux Rec.exe" 0
!macroend

!macro customUnInstall
  Delete "$DESKTOP\Flux Rec.lnk"
!macroend
