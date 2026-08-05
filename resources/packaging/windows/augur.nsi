; AugurRS Windows installer.
;
; Per-user install by design. Two reasons:
;   1. Lab and shared machines rarely give the person running the experiment
;      local admin rights; a per-user install works without any of that.
;   2. The in-app updater re-runs this installer with /S. A machine-wide
;      install would raise a UAC prompt that a silent run cannot answer, so
;      auto-update would just fail with no visible cause.
;
; Admins who want a machine-wide deployment should use the portable zip.
;
; Built by build-installer.ps1, which supplies APP_VERSION and OUT_FILE.

Unicode true
ManifestDPIAware true

!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"

!define APP_NAME "AugurRS"
!define APP_PUBLISHER "Mika Uthmann"
!define APP_URL "https://github.com/muthmann/augur-rs"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}"

; build-installer.ps1 supplies all of these as absolute paths. makensis resolves
; File/icon paths against its working directory rather than the script's, so
; relative paths here would break the moment someone invoked it from elsewhere.
!ifndef APP_VERSION
  !error "APP_VERSION must be defined (-DAPP_VERSION=...)"
!endif
!ifndef APP_VERSION_QUAD
  !error "APP_VERSION_QUAD must be defined (-DAPP_VERSION_QUAD=...)"
!endif
!ifndef OUT_FILE
  !error "OUT_FILE must be defined (-DOUT_FILE=...)"
!endif
!ifndef STAGE_DIR
  !error "STAGE_DIR must be defined (-DSTAGE_DIR=...)"
!endif
!ifndef ICON_FILE
  !error "ICON_FILE must be defined (-DICON_FILE=...)"
!endif
!ifndef LICENSE_FILE
  !error "LICENSE_FILE must be defined (-DLICENSE_FILE=...)"
!endif

Name "${APP_NAME} ${APP_VERSION}"
BrandingText "${APP_NAME} ${APP_VERSION}"
OutFile "${OUT_FILE}"
RequestExecutionLevel user
InstallDir "$LOCALAPPDATA\Programs\${APP_NAME}"
InstallDirRegKey HKCU "Software\${APP_NAME}" "InstallDir"
SetCompressor /SOLID lzma

VIProductVersion "${APP_VERSION_QUAD}"
VIAddVersionKey "ProductName" "${APP_NAME}"
VIAddVersionKey "ProductVersion" "${APP_VERSION}"
VIAddVersionKey "FileVersion" "${APP_VERSION}"
VIAddVersionKey "CompanyName" "${APP_PUBLISHER}"
VIAddVersionKey "LegalCopyright" "${APP_PUBLISHER}"
VIAddVersionKey "FileDescription" "${APP_NAME} installer"

!define MUI_ABORTWARNING
!define MUI_ICON "${ICON_FILE}"
!define MUI_UNICON "${ICON_FILE}"

!define MUI_FINISHPAGE_RUN "$INSTDIR\AugurRS.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Launch ${APP_NAME}"
!define MUI_FINISHPAGE_LINK "Documentation and releases"
!define MUI_FINISHPAGE_LINK_LOCATION "${APP_URL}"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "${LICENSE_FILE}"
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

Section "${APP_NAME} (required)" SecCore
  SectionIn RO
  SetOutPath "$INSTDIR"

  ; An update re-runs this installer over a live install. Shutting the running
  ; copy down first avoids "file in use" failures that would otherwise leave a
  ; half-updated directory behind.
  nsExec::Exec 'taskkill /F /IM AugurRS.exe'
  Pop $0

  File /r "${STAGE_DIR}\*.*"

  WriteRegStr HKCU "Software\${APP_NAME}" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\${APP_NAME}" "Version" "${APP_VERSION}"

  WriteUninstaller "$INSTDIR\Uninstall.exe"

  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  WriteRegStr   HKCU "${UNINSTALL_KEY}" "DisplayName"     "${APP_NAME}"
  WriteRegStr   HKCU "${UNINSTALL_KEY}" "DisplayVersion"  "${APP_VERSION}"
  WriteRegStr   HKCU "${UNINSTALL_KEY}" "DisplayIcon"     "$INSTDIR\AugurRS.exe"
  WriteRegStr   HKCU "${UNINSTALL_KEY}" "Publisher"       "${APP_PUBLISHER}"
  WriteRegStr   HKCU "${UNINSTALL_KEY}" "URLInfoAbout"    "${APP_URL}"
  WriteRegStr   HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr   HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr   HKCU "${UNINSTALL_KEY}" "QuietUninstallString" '"$INSTDIR\Uninstall.exe" /S'
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "EstimatedSize"   "$0"
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify"        1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair"        1
SectionEnd

Section "Start Menu shortcut" SecStartMenu
  CreateDirectory "$SMPROGRAMS\${APP_NAME}"
  CreateShortcut "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk" "$INSTDIR\AugurRS.exe" "" "$INSTDIR\AugurRS.exe" 0
  CreateShortcut "$SMPROGRAMS\${APP_NAME}\Uninstall ${APP_NAME}.lnk" "$INSTDIR\Uninstall.exe"
SectionEnd

Section /o "Desktop shortcut" SecDesktop
  CreateShortcut "$DESKTOP\${APP_NAME}.lnk" "$INSTDIR\AugurRS.exe" "" "$INSTDIR\AugurRS.exe" 0
SectionEnd

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SecCore} "The AugurRS GUI, the augur command line tool, and the bundled documentation."
  !insertmacro MUI_DESCRIPTION_TEXT ${SecStartMenu} "Add ${APP_NAME} to the Start Menu."
  !insertmacro MUI_DESCRIPTION_TEXT ${SecDesktop} "Put a ${APP_NAME} shortcut on the desktop."
!insertmacro MUI_FUNCTION_DESCRIPTION_END

Section "Uninstall"
  nsExec::Exec 'taskkill /F /IM AugurRS.exe'
  Pop $0

  Delete "$DESKTOP\${APP_NAME}.lnk"
  Delete "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk"
  Delete "$SMPROGRAMS\${APP_NAME}\Uninstall ${APP_NAME}.lnk"
  RMDir  "$SMPROGRAMS\${APP_NAME}"

  Delete "$INSTDIR\Uninstall.exe"
  RMDir /r "$INSTDIR\examples"
  RMDir /r "$INSTDIR"

  DeleteRegKey HKCU "${UNINSTALL_KEY}"
  DeleteRegKey HKCU "Software\${APP_NAME}"
SectionEnd
