; A running daemon holds xrun.exe open. Cleanly stop it before replacing files.
!macro NSIS_HOOK_PREINSTALL
  IfFileExists "$INSTDIR\xrun-desktop.exe" 0 xrun_install_ready
    nsExec::ExecToStack '"$INSTDIR\xrun-desktop.exe" --prepare-update'
    Pop $0
    Pop $1
    ${If} $0 != 0
      MessageBox MB_OK|MB_ICONSTOP "Could not stop xrun before updating: $1"
      Abort
    ${EndIf}
  xrun_install_ready:
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::ExecToStack '"$INSTDIR\xrun-desktop.exe" --prepare-uninstall'
  Pop $0
  Pop $1
  ${If} $0 != 0
    MessageBox MB_OK|MB_ICONSTOP "Could not remove the xrun background service: $1"
    Abort
  ${EndIf}
!macroend
