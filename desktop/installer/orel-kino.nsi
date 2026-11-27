; Installer of «Орловский кинопоказ» for Windows (NSIS).
; Built by .github/workflows/desktop.yml:
;   makensis /DVERSION=0.1.5 /DSTAGE=<dir with orel-kino.exe and libmpv-2.dll> /DOUTFILE=<setup.exe> orel-kino.nsi
Unicode true
SetCompressor /SOLID lzma
RequestExecutionLevel user

!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "FileFunc.nsh"

!define APP_NAME "Орловский кинопоказ"
!define APP_EXE "orel-kino.exe"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\OrelKino"

Name "${APP_NAME}"
OutFile "${OUTFILE}"
; Per-user install: no administrator rights needed, updates replace the same folder.
InstallDir "$LOCALAPPDATA\Programs\Orel Kino"
InstallDirRegKey HKCU "Software\OrelKino" "InstallDir"
BrandingText "${APP_NAME} ${VERSION}"

VIProductVersion "${VERSION}.0"
VIAddVersionKey /LANG=1049 "ProductName" "${APP_NAME}"
VIAddVersionKey /LANG=1049 "FileDescription" "Установка: ${APP_NAME}"
VIAddVersionKey /LANG=1049 "FileVersion" "${VERSION}"

!define MUI_ICON "..\assets\icon.ico"
!define MUI_UNICON "..\assets\icon.ico"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\${APP_EXE}"
!define MUI_FINISHPAGE_RUN_TEXT "Запустить ${APP_NAME}"

!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "Russian"

; The app draws its interface with the Microsoft Edge WebView2 runtime (built into Windows 11).
Function EnsureWebView2
  ReadRegStr $0 HKLM "SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "pv"
  ${If} $0 == ""
    ReadRegStr $0 HKCU "Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "pv"
  ${EndIf}
  ${If} $0 == ""
    DetailPrint "Скачиваем компонент WebView2…"
    nsExec::ExecToLog 'powershell -NoProfile -Command "Invoke-WebRequest -UseBasicParsing https://go.microsoft.com/fwlink/p/?LinkId=2124703 -OutFile $TEMP\wv2setup.exe"'
    ExecWait '"$TEMP\wv2setup.exe" /silent /install'
    Delete "$TEMP\wv2setup.exe"
  ${EndIf}
FunctionEnd

Section "Install"
  ; Close a running copy so its files can be replaced.
  nsExec::Exec 'taskkill /F /IM ${APP_EXE}'
  Sleep 500

  SetOutPath "$INSTDIR"
  File "${STAGE}\${APP_EXE}"
  File "${STAGE}\libmpv-2.dll"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  CreateDirectory "$SMPROGRAMS\${APP_NAME}"
  CreateShortcut "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk" "$INSTDIR\${APP_EXE}"
  CreateShortcut "$DESKTOP\${APP_NAME}.lnk" "$INSTDIR\${APP_EXE}"

  WriteRegStr HKCU "Software\OrelKino" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "${APP_NAME}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\${APP_EXE}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1

  Call EnsureWebView2

  ${If} ${Silent}
    ${GetParameters} $R0
    ClearErrors
    ${GetOptions} $R0 "/RELAUNCH=" $R1
    ${If} $R1 == "1"
      Exec '"$INSTDIR\${APP_EXE}"'
    ${EndIf}
  ${EndIf}
SectionEnd

Section "Uninstall"
  nsExec::Exec 'taskkill /F /IM ${APP_EXE}'
  Sleep 500
  Delete "$INSTDIR\${APP_EXE}"
  Delete "$INSTDIR\libmpv-2.dll"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk"
  RMDir "$SMPROGRAMS\${APP_NAME}"
  Delete "$DESKTOP\${APP_NAME}.lnk"
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
  DeleteRegKey HKCU "Software\OrelKino"
  ; Settings and the saved sign-in in %APPDATA%\Orel Kino are kept.
SectionEnd
