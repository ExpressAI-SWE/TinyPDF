Unicode true
Name "TinyPDF"
OutFile "${OUTPUT}"
InstallDir "$LOCALAPPDATA\Programs\TinyPDF"
InstallDirRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\TinyPDF" "InstallLocation"
RequestExecutionLevel user
SetCompressor /SOLID lzma
ShowInstDetails nevershow
VIProductVersion "${PRODUCT_VERSION}.0"
VIAddVersionKey "ProductName" "TinyPDF"
VIAddVersionKey "CompanyName" "ExpressAI-SWE"
VIAddVersionKey "FileDescription" "TinyPDF PDF Reader Setup"
VIAddVersionKey "FileVersion" "${RELEASE_LABEL}"
VIAddVersionKey "ProductVersion" "${RELEASE_LABEL}"
VIAddVersionKey "LegalCopyright" "TinyPDF contributors"

!include "MUI2.nsh"
!define MUI_ICON "${__FILEDIR__}\..\assets\tinypdf.ico"
!define MUI_UNICON "${__FILEDIR__}\..\assets\tinypdf.ico"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\tinypdf.exe"
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

!define UNINSTALL "Software\Microsoft\Windows\CurrentVersion\Uninstall\TinyPDF"
!define CLASSES "Software\Classes"
!define PROGID "TinyPDF.PDF"
!define CAPABILITIES "Software\ExpressAI-SWE\TinyPDF\Capabilities"

Section "TinyPDF (required)"
    SectionIn RO
    SetShellVarContext current
    SetOutPath "$INSTDIR"
    File "${PAYLOAD}\tinypdf.exe"
    File "${PAYLOAD}\pdfium.dll"
    File "${__FILEDIR__}\..\assets\tinypdf.ico"
    File /r "${PAYLOAD}\PDFium"
    WriteUninstaller "$INSTDIR\uninstall.exe"
    CreateShortCut "$SMPROGRAMS\TinyPDF.lnk" "$INSTDIR\tinypdf.exe" "" "$INSTDIR\tinypdf.ico" 0

    WriteRegStr HKCU "${UNINSTALL}" "DisplayName" "TinyPDF"
    WriteRegStr HKCU "${UNINSTALL}" "DisplayVersion" "${RELEASE_LABEL}"
    WriteRegStr HKCU "${UNINSTALL}" "Publisher" "ExpressAI-SWE"
    WriteRegStr HKCU "${UNINSTALL}" "DisplayIcon" "$INSTDIR\tinypdf.exe"
    WriteRegStr HKCU "${UNINSTALL}" "InstallLocation" "$INSTDIR"
    WriteRegStr HKCU "${UNINSTALL}" "UninstallString" '"$INSTDIR\uninstall.exe"'
    WriteRegStr HKCU "${UNINSTALL}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
    WriteRegDWORD HKCU "${UNINSTALL}" "NoModify" 1
    WriteRegDWORD HKCU "${UNINSTALL}" "NoRepair" 1

    WriteRegStr HKCU "${CLASSES}${PROGID}" "" "PDF document"
    WriteRegStr HKCU "${CLASSES}${PROGID}\DefaultIcon" "" "$INSTDIR\tinypdf.exe,0"
    WriteRegStr HKCU "${CLASSES}${PROGID}\shell\open\command" "" '"$INSTDIR\tinypdf.exe" "%1"'
    WriteRegStr HKCU "${CLASSES}\.pdf\OpenWithProgids" "${PROGID}" ""
    WriteRegStr HKCU "${CAPABILITIES}" "ApplicationName" "TinyPDF"
    WriteRegStr HKCU "${CAPABILITIES}" "ApplicationDescription" "Read and annotate PDF documents with TinyPDF."
    WriteRegStr HKCU "${CAPABILITIES}" "ApplicationIcon" "$INSTDIR\tinypdf.exe,0"
    WriteRegStr HKCU "${CAPABILITIES}\FileAssociations" ".pdf" "${PROGID}"
    WriteRegStr HKCU "Software\RegisteredApplications" "TinyPDF" "${CAPABILITIES}"
    System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
SectionEnd

Section /o "Desktop shortcut"
    SetShellVarContext current
    CreateShortCut "$DESKTOP\TinyPDF.lnk" "$INSTDIR\tinypdf.exe" "" "$INSTDIR\tinypdf.ico" 0
    WriteRegDWORD HKCU "${UNINSTALL}" "DesktopShortcut" 1
SectionEnd

Section "Uninstall"
    SetShellVarContext current
    ReadRegDWORD $0 HKCU "${UNINSTALL}" "DesktopShortcut"
    StrCmp $0 "1" 0 +2
    Delete "$DESKTOP\TinyPDF.lnk"
    DeleteRegValue HKCU "${CLASSES}\.pdf\OpenWithProgids" "${PROGID}"
    DeleteRegKey HKCU "${CLASSES}${PROGID}"
    DeleteRegValue HKCU "Software\RegisteredApplications" "TinyPDF"
    DeleteRegKey HKCU "${CAPABILITIES}"
    DeleteRegKey /ifempty HKCU "Software\ExpressAI-SWE\TinyPDF"
    DeleteRegKey /ifempty HKCU "Software\ExpressAI-SWE"
    DeleteRegKey HKCU "${UNINSTALL}"
    System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'

    Delete "$SMPROGRAMS\TinyPDF.lnk"
    Delete "$INSTDIR\tinypdf.exe"
    Delete "$INSTDIR\pdfium.dll"
    Delete "$INSTDIR\tinypdf.ico"
    Delete "$INSTDIR\PDFium\VERSION"
    Delete "$INSTDIR\PDFium\LICENSE"
    Delete "$INSTDIR\PDFium\licenses\*.txt"
    Delete "$INSTDIR\PDFium\licenses\libjpeg_turbo.ijg"
    Delete "$INSTDIR\PDFium\licenses\libjpeg_turbo.md"
    RMDir "$INSTDIR\PDFium\licenses"
    RMDir "$INSTDIR\PDFium"
    Delete "$INSTDIR\uninstall.exe"
    RMDir "$INSTDIR"
SectionEnd
