; Navi's installer hooks (tauri.conf.json > bundle > windows > nsis > installerHooks).

; An update starts this installer and then closes Navi (tauri-plugin-updater runs the installer,
; then exits). Navi can take a moment to close, and if it is still closing when the installer
; checks whether it is running, Windows can't close it for the installer, which stopped with
; "Failed to kill Navi" about one update in three. So in an update, wait for Navi to finish closing
; (up to 15 seconds) before that check. A copy of Navi still open after that is closed by the
; check as before.
!macro NSIS_HOOK_PREINSTALL
  ${If} $UpdateMode = 1
    Push $R8
    Push $R9
    StrCpy $R9 0
    navi_wait_for_close:
      ; 0 while a navi.exe of this user is running.
      nsis_tauri_utils::FindProcessCurrentUser "${MAINBINARYNAME}.exe"
      Pop $R8
      ${If} $R8 = 0
      ${AndIf} $R9 < 75
        Sleep 200
        IntOp $R9 $R9 + 1
        Goto navi_wait_for_close
      ${EndIf}
    Pop $R9
    Pop $R8
  ${EndIf}
!macroend
