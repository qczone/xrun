; A running daemon holds xrun.exe open. Cleanly stop it before replacing files.
!macro NSIS_HOOK_PREINSTALL
  Delete "$INSTDIR\xrun-install-error.log"
  IfFileExists "$INSTDIR\xrun-desktop.exe" 0 xrun_install_ready
    nsExec::ExecToStack '"$INSTDIR\xrun-desktop.exe" --prepare-update'
    Pop $0
    Pop $1
    ${If} $0 != 0
      DetailPrint "Could not stop xrun before updating: $1"
      ClearErrors
      FileOpen $2 "$INSTDIR\xrun-install-error.log" w
      ${IfNot} ${Errors}
        FileWriteUTF16LE /BOM $2 "UPDATE_PREPARE_FAILED: $1$\r$\n"
        FileClose $2
      ${EndIf}
      IfSilent +2
        MessageBox MB_OK|MB_ICONSTOP "Could not stop xrun before updating: $1"
      SetErrorLevel 32
      Abort
    ${EndIf}
  xrun_install_ready:
!macroend

!macro NSIS_HOOK_POSTINSTALL
  nsExec::ExecToStack '"$INSTDIR\xrun-desktop.exe" --install-cli'
  Pop $0
  Pop $1
  ${If} $0 != 0
    DetailPrint "Could not configure the xrun terminal command: $1"
    ClearErrors
    FileOpen $2 "$INSTDIR\xrun-install-error.log" w
    ${IfNot} ${Errors}
      FileWriteUTF16LE /BOM $2 "CLI_INSTALL_FAILED: $1$\r$\n"
      FileClose $2
    ${EndIf}
    IfSilent +2
      MessageBox MB_OK|MB_ICONSTOP "Could not configure the xrun terminal command: $1"
    SetErrorLevel 34
    Abort
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  Delete "$INSTDIR\xrun-install-error.log"
  nsExec::ExecToStack '"$INSTDIR\xrun-desktop.exe" --prepare-uninstall'
  Pop $0
  Pop $1
  ${If} $0 != 0
    DetailPrint "Could not remove the xrun background service: $1"
    ClearErrors
    FileOpen $2 "$INSTDIR\xrun-install-error.log" w
    ${IfNot} ${Errors}
      FileWriteUTF16LE /BOM $2 "UNINSTALL_PREPARE_FAILED: $1$\r$\n"
      FileClose $2
    ${EndIf}
    IfSilent +2
      MessageBox MB_OK|MB_ICONSTOP "Could not remove the xrun background service: $1"
    SetErrorLevel 33
    Abort
  ${EndIf}
  Delete "$INSTDIR\.xrun-install.json"
!macroend
