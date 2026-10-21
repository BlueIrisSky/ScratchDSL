Unicode true
!include "MUI2.nsh"
!include "x64.nsh"
!ifndef APP_VERSION
  !error "Build with packaging/package.ps1 to supply APP_VERSION."
!endif

Name "ScratchDSL"
OutFile "..\..\dist\Install.exe"
InstallDir "$LOCALAPPDATA\Programs\ScratchDSL"
InstallDirRegKey HKCU "Software\ScratchDSL" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma
VIProductVersion "${APP_VERSION}.0"
VIAddVersionKey "ProductName" "ScratchDSL"
VIAddVersionKey "ProductVersion" "${APP_VERSION}"
VIAddVersionKey "FileVersion" "${APP_VERSION}"
VIAddVersionKey "FileDescription" "ScratchDSL installer"
VIAddVersionKey "LegalCopyright" "Copyright (c) 2026 BlueIrisSky"

Var PreviousInstallDir
Var PreviousExitCode
Var PathExitCode
Var EnvironmentWarning

!macro UpdateUserPath Action
  ClearErrors
  ; ExecWait launches PowerShell directly. PATH itself never enters NSIS strings.
  ; The trailing \. avoids a quoted argument ending in a backslash (drive roots).
  ExecWait '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -WindowStyle Hidden -File "$INSTDIR\Update-UserPath.ps1" -Action ${Action} -InstallDir "$INSTDIR\."' $PathExitCode
  IfErrors path_failed
  StrCmp $PathExitCode 0 path_done
  StrCmp $PathExitCode 2 0 path_failed
  StrCpy $EnvironmentWarning 2
  DetailPrint "PATH was saved, but Windows did not acknowledge the environment-change broadcast."
  MessageBox MB_OK|MB_ICONEXCLAMATION "PATH was saved, but some running applications may still use the old environment. Open a new terminal; sign out and back in if necessary." /SD IDOK
  Goto path_done
path_failed:
  DetailPrint "Failed to ${Action} the user PATH entry (exit code $PathExitCode)."
  MessageBox MB_OK|MB_ICONSTOP "Could not ${Action} the ScratchDSL user PATH entry. Operation stopped. See $INSTDIR\path-update.log for details, if available." /SD IDOK
  SetErrorLevel 1
  Abort
path_done:
!macroend

!define MUI_ABORTWARNING
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "SimpChinese"
!insertmacro MUI_LANGUAGE "English"

Function .onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_ICONSTOP "ScratchDSL requires 64-bit Windows."
    Abort
  ${EndIf}
FunctionEnd

Function UninstallPrevious
  ClearErrors
  ReadRegStr $PreviousInstallDir HKCU "Software\ScratchDSL" "InstallDir"
  IfErrors previous_done
  StrCmp $PreviousInstallDir "" previous_done
  IfFileExists "$PreviousInstallDir\Uninstall.exe" 0 previous_missing
  ; Run only when installation begins, after the user has confirmed its directory.
  ; _?= must be last and unquoted. It prevents the self-copying launcher, so
  ; ExecWait waits for the real uninstall rather than an intermediate process.
  ; https://nsis.sourceforge.io/Docs/Chapter3.html#uninstallerusage
  SetOutPath "$TEMP"
  DetailPrint "Uninstalling the previous ScratchDSL installation: $PreviousInstallDir"
  ClearErrors
  ExecWait '"$PreviousInstallDir\Uninstall.exe" /S _?=$PreviousInstallDir' $PreviousExitCode
  IfErrors previous_failed
  StrCmp $PreviousExitCode 0 previous_verify
  ; Exit 2 means the PATH edit succeeded but the notification timed out.
  StrCmp $PreviousExitCode 2 0 previous_failed
  StrCpy $EnvironmentWarning 2
previous_verify:
  IfFileExists "$PreviousInstallDir\sbdsl.exe" previous_failed
  ClearErrors
  ReadRegStr $0 HKCU "Software\ScratchDSL" "InstallDir"
  IfErrors previous_check_uninstall_key
  StrCmp $0 "" 0 previous_failed
previous_check_uninstall_key:
  ClearErrors
  ReadRegStr $0 HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\ScratchDSL" "UninstallString"
  IfErrors previous_cleanup
  StrCmp $0 "" 0 previous_failed
previous_cleanup:
  ; An in-place (_?=) uninstaller cannot delete its running executable itself.
  ClearErrors
  Delete "$PreviousInstallDir\Uninstall.exe"
  IfErrors previous_failed
  IfFileExists "$PreviousInstallDir\Uninstall.exe" previous_failed
  ; Preserve user-created files: remove the directory only if it is empty.
  RMDir "$PreviousInstallDir"
  Goto previous_done
previous_missing:
  DetailPrint "The registered previous installation has no Uninstall.exe."
  MessageBox MB_OK|MB_ICONSTOP "The previous installation at $PreviousInstallDir has no Uninstall.exe. Repair or remove that installation before reinstalling ScratchDSL." /SD IDOK
  SetErrorLevel 1
  Abort
previous_failed:
  DetailPrint "Previous uninstall failed or did not finish (exit code $PreviousExitCode)."
  MessageBox MB_OK|MB_ICONSTOP "The previous ScratchDSL installation could not be completely uninstalled. Installation stopped before copying new files." /SD IDOK
  SetErrorLevel 1
  Abort
previous_done:
FunctionEnd

Section "ScratchDSL" SEC_MAIN
  SetShellVarContext current
  StrCpy $EnvironmentWarning 0
  Call UninstallPrevious
  SetOutPath "$INSTDIR"
  File "..\..\dist\sbdsl.exe"
  File "..\..\README.md"
  File "..\..\dist\LICENSE"
  File "..\..\dist\THIRD_PARTY_NOTICES.txt"
  File "Update-UserPath.ps1"
  SetOutPath "$INSTDIR\docs"
  File "..\..\docs\cli.md"
  File "..\..\docs\syntax.md"
  File "..\..\docs\sprites.md"
  File "..\..\docs\project.md"
  File "..\..\docs\extensions.md"
  File "..\..\docs\blocks.md"
  File "..\..\docs\vscode.md"
  File "..\..\docs\development.md"
  File "..\..\docs\templates.md"
  File "..\..\docs\sb3.md"
  File "..\..\docs\lsp.md"
  SetOutPath "$INSTDIR\docs\releases"
  File "..\..\docs\releases\${APP_VERSION}.md"
  SetOutPath "$INSTDIR\examples\hello"
  File "..\..\examples\hello\main.sbdsl"
  File "..\..\examples\hello\sbdsl.buildsb"
  SetOutPath "$INSTDIR\examples\extensions"
  File "..\..\examples\extensions\main.sbdsl"
  File "..\..\examples\extensions\sbdsl.buildsb"
  File "..\..\examples\extensions\自定义拓展.js"
  SetOutPath "$DOCUMENTS"
  CreateDirectory "$SMPROGRAMS\ScratchDSL"
  CreateShortCut "$SMPROGRAMS\ScratchDSL\ScratchDSL terminal.lnk" "$SYSDIR\cmd.exe" '/K sbdsl.exe --help'
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  CreateShortCut "$SMPROGRAMS\ScratchDSL\Uninstall.lnk" "$INSTDIR\Uninstall.exe"
  WriteRegStr HKCU "Software\ScratchDSL" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\ScratchDSL" "DisplayName" "ScratchDSL"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\ScratchDSL" "DisplayVersion" "${APP_VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\ScratchDSL" "UninstallString" '$\"$INSTDIR\Uninstall.exe$\"'
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\ScratchDSL" "InstallLocation" "$INSTDIR"
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\ScratchDSL" "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\ScratchDSL" "NoRepair" 1
  !insertmacro UpdateUserPath Add
  SetErrorLevel $EnvironmentWarning
SectionEnd

Section "Uninstall"
  SetShellVarContext current
  StrCpy $EnvironmentWarning 0
  ; On failure keep files and ownership metadata so that removal can be retried.
  !insertmacro UpdateUserPath Remove
  Delete "$INSTDIR\sbdsl.exe"
  IfFileExists "$INSTDIR\sbdsl.exe" 0 uninstall_files
  MessageBox MB_OK|MB_ICONSTOP "sbdsl.exe could not be removed. Close running ScratchDSL processes and retry uninstalling." /SD IDOK
  SetErrorLevel 1
  Abort
uninstall_files:
  Delete "$INSTDIR\README.md"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\THIRD_PARTY_NOTICES.txt"
  Delete "$INSTDIR\Update-UserPath.ps1"
  Delete "$INSTDIR\path-update.log"
  Delete "$INSTDIR\docs\cli.md"
  Delete "$INSTDIR\docs\syntax.md"
  Delete "$INSTDIR\docs\sprites.md"
  Delete "$INSTDIR\docs\project.md"
  Delete "$INSTDIR\docs\extensions.md"
  Delete "$INSTDIR\docs\blocks.md"
  Delete "$INSTDIR\docs\vscode.md"
  Delete "$INSTDIR\docs\development.md"
  Delete "$INSTDIR\docs\templates.md"
  Delete "$INSTDIR\docs\sb3.md"
  Delete "$INSTDIR\docs\lsp.md"
  Delete "$INSTDIR\docs\releases\${APP_VERSION}.md"
  RMDir "$INSTDIR\docs\releases"
  RMDir "$INSTDIR\docs"
  Delete "$INSTDIR\examples\hello\main.sbdsl"
  Delete "$INSTDIR\examples\hello\sbdsl.buildsb"
  Delete "$INSTDIR\examples\extensions\main.sbdsl"
  Delete "$INSTDIR\examples\extensions\sbdsl.buildsb"
  Delete "$INSTDIR\examples\extensions\自定义拓展.js"
  RMDir "$INSTDIR\examples\hello"
  RMDir "$INSTDIR\examples\extensions"
  RMDir "$INSTDIR\examples"
  Delete "$SMPROGRAMS\ScratchDSL\ScratchDSL terminal.lnk"
  Delete "$SMPROGRAMS\ScratchDSL\Uninstall.lnk"
  RMDir "$SMPROGRAMS\ScratchDSL"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\ScratchDSL"
  DeleteRegKey HKCU "Software\ScratchDSL"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
  SetErrorLevel $EnvironmentWarning
SectionEnd
