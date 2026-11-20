Unicode true
Name "TinyPDF"
OutFile "${OUTPUT}"
InstallDir "$LOCALAPPDATA\Programs\TinyPDF"
RequestExecutionLevel user
SetCompressor /SOLID lzma
ShowInstDetails nevershow

!include "MUI2.nsh"
!define MUI_ICON "${__FILEDIR__}\..\assets\tinypdf.ico"
!define MUI_UNICON "${__FILEDIR__}\..\assets\tinypdf.ico"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\tinypdf.exe"
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

!define UNINSTALL "Software\Microsoft\Windows\CurrentVersion\Uninstall\TinyPDF"
!define CLASSES "Software\Classes"
!define PROGID "TinyPDF.PDF"

Section
    SetShellVarContext current
    SetOutPath "$INSTDIR"
    File "${PAYLOAD}\tinypdf.exe"
    File "${PAYLOAD}\pdfium.dll"
    File "${__FILEDIR__}\..\assets\tinypdf.ico"
    File /r "${PAYLOAD}\PDFium"
    WriteUninstaller "$INSTDIR\uninstall.exe"
    CreateShortCut "$SMPROGRAMS\TinyPDF.lnk" "$INSTDIR\tinypdf.exe" "" "$INSTDIR\tinypdf.ico" 0

    WriteRegStr HKCU "${UNINSTALL}" "DisplayName" "TinyPDF"
    WriteRegStr HKCU "${UNINSTALL}" "DisplayVersion" "0.1.0"
    WriteRegStr HKCU "${UNINSTALL}" "DisplayIcon" "$INSTDIR\tinypdf.exe"
    WriteRegStr HKCU "${UNINSTALL}" "InstallLocation" "$INSTDIR"
    WriteRegStr HKCU "${UNINSTALL}" "UninstallString" '"$INSTDIR\uninstall.exe"'
    WriteRegDWORD HKCU "${UNINSTALL}" "NoModify" 1
    WriteRegDWORD HKCU "${UNINSTALL}" "NoRepair" 1

    WriteRegStr HKCU "${CLASSES}${PROGID}" "" "PDF document"
    WriteRegStr HKCU "${CLASSES}${PROGID}\DefaultIcon" "" "$INSTDIR\tinypdf.exe,0"
    WriteRegStr HKCU "${CLASSES}${PROGID}\shell\open\command" "" '"$INSTDIR\tinypdf.exe" "%1"'
    WriteRegStr HKCU "${CLASSES}\.pdf\OpenWithProgids" "${PROGID}" ""
    System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
SectionEnd

Section "Uninstall"
    SetShellVarContext current
    DeleteRegValue HKCU "${CLASSES}\.pdf\OpenWithProgids" "${PROGID}"
    DeleteRegKey HKCU "${CLASSES}${PROGID}"
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
