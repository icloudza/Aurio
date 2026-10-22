; Aurio Windows 安装程序（NSIS 3，Unicode）
; 由 scripts/build-win.sh 调用：makensis -DVERSION=x.y.z -DSRC=<程序目录> -DOUTFILE=<输出> installer/aurio.nsi
;
; 安装：欢迎 → 说明 → 组件 → 安装位置 → 安装 → 完成（可勾选运行）
; 卸载：自定义确认面板（检测虚拟声卡驱动）→ 先卸载驱动再卸载软件 → 完成页提示重启

Unicode true
ManifestDPIAware true
SetCompressor /SOLID lzma

!ifndef VERSION
  !define VERSION "0.2.6"
!endif
!ifndef SRC
  !define SRC "../dist/Aurio-Windows"
!endif
!ifndef OUTFILE
  !define OUTFILE "../dist/Aurio-Setup-${VERSION}.exe"
!endif

!define APP        "Aurio"
!define EXE        "Aurio.exe"
!define PUBLISHER  "Aurio"
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Aurio"
!define APP_KEY    "Software\Aurio"
!define RUN_KEY    "Software\Microsoft\Windows\CurrentVersion\Run"
!define WEBVIEW2_GUID "{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"
; 驱动检测：Aurio.exe --driver-helper status 的返回值
!define DRV_INSTALLED 10

Name "${APP}"
Caption "${APP} ${VERSION} 安装"
UninstallCaption "卸载 ${APP}"
OutFile "${OUTFILE}"
InstallDir "$PROGRAMFILES64\${APP}"
InstallDirRegKey HKLM "${APP_KEY}" "InstallDir"
RequestExecutionLevel admin
BrandingText "${APP} ${VERSION}"

VIProductVersion "${VERSION}.0"
VIAddVersionKey /LANG=2052 "ProductName" "${APP}"
VIAddVersionKey /LANG=2052 "FileDescription" "${APP} 安装程序"
VIAddVersionKey /LANG=2052 "FileVersion" "${VERSION}"
VIAddVersionKey /LANG=2052 "ProductVersion" "${VERSION}"
VIAddVersionKey /LANG=2052 "LegalCopyright" "${PUBLISHER}"

!include "MUI2.nsh"
!include "nsDialogs.nsh"
!include "LogicLib.nsh"
!include "x64.nsh"
!include "FileFunc.nsh"

; ———————————— 界面 ————————————
!define MUI_ICON   "../apps/win-tauri/icons/icon.ico"
!define MUI_UNICON "../apps/win-tauri/icons/icon.ico"
!define MUI_ABORTWARNING
!define MUI_UNABORTWARNING
!define MUI_COMPONENTSPAGE_SMALLDESC

!define MUI_WELCOMEPAGE_TITLE "安装 ${APP}"
!define MUI_WELCOMEPAGE_TEXT "Aurio 把 Mac 上的 Studio Display 变成这台电脑的扬声器和麦克风。$\r$\n$\r$\n安装程序会把 Aurio ${VERSION} 装到这台电脑上。安装前会关闭正在运行的 Aurio。$\r$\n$\r$\n点「下一步」继续。"
!insertmacro MUI_PAGE_WELCOME

!define MUI_LICENSEPAGE_TEXT_TOP "安装前请阅读以下说明。"
!define MUI_LICENSEPAGE_BUTTON "下一步(&N) >"
!define MUI_LICENSEPAGE_TEXT_BOTTOM " "
!insertmacro MUI_PAGE_LICENSE "license.txt"

!insertmacro MUI_PAGE_COMPONENTS

!define MUI_DIRECTORYPAGE_TEXT_TOP "选择 Aurio 的安装位置。要装到其他文件夹，点「浏览」选择。"
!insertmacro MUI_PAGE_DIRECTORY

!insertmacro MUI_PAGE_INSTFILES

!define MUI_FINISHPAGE_TITLE "安装完成"
!define MUI_FINISHPAGE_TEXT "Aurio 已经装好了。$\r$\n$\r$\n第一次使用时，在 Aurio 的「设置」里点「安装驱动」，就能用上 Studio Display 的麦克风。"
!define MUI_FINISHPAGE_RUN
!define MUI_FINISHPAGE_RUN_TEXT "运行 Aurio"
!define MUI_FINISHPAGE_RUN_FUNCTION LaunchAsUser
!insertmacro MUI_PAGE_FINISH

; 卸载：自定义确认面板 → 卸载进度 → 完成（提示重启）
UninstPage custom un.OverviewPage un.OverviewLeave
!insertmacro MUI_UNPAGE_INSTFILES
!define MUI_UNFINISHPAGE_NOAUTOCLOSE
!define MUI_FINISHPAGE_TITLE "卸载完成"
!define MUI_FINISHPAGE_TEXT "Aurio 已经从这台电脑上删除。"
!define MUI_FINISHPAGE_TEXT_REBOOT "Aurio 已经删除。需要重启电脑，驱动和音频设备的改动才能完全生效。"
!define MUI_FINISHPAGE_TEXT_REBOOTNOW "立即重启"
!define MUI_FINISHPAGE_TEXT_REBOOTLATER "稍后我自己重启"
!insertmacro MUI_UNPAGE_FINISH

!insertmacro MUI_LANGUAGE "SimpChinese"

; ———————————— 安装 ————————————
Function .onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_OK|MB_ICONSTOP "Aurio 只支持 64 位 Windows 10 / 11。"
    Abort
  ${EndIf}
  SetRegView 64
  ; 升级时沿用上次的安装位置
  ReadRegStr $0 HKLM "${APP_KEY}" "InstallDir"
  ${If} $0 != ""
    StrCpy $INSTDIR $0
  ${EndIf}
FunctionEnd

; 关掉正在运行的 Aurio（界面和命令行版）
!macro CloseAurio
  nsExec::Exec 'taskkill /F /IM Aurio.exe /T'
  Pop $0
  nsExec::Exec 'taskkill /F /IM aurio-win.exe /T'
  Pop $0
  Sleep 500
!macroend

; 安装程序是管理员权限，直接运行会让 Aurio 也以管理员身份运行；借资源管理器以普通权限启动
Function LaunchAsUser
  Exec '"$WINDIR\explorer.exe" "$INSTDIR\${EXE}"'
FunctionEnd

Section "Aurio 程序（必需）" SecCore
  SectionIn RO
  SetShellVarContext all
  !insertmacro CloseAurio

  SetOutPath "$INSTDIR"
  File "${SRC}/${EXE}"
  File "${SRC}/WebView2Loader.dll"
  File "${SRC}/aurio-win.exe"
  File "/oname=Aurio.ico" "../apps/win-tauri/icons/icon.ico"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  ; 开始菜单：Aurio、卸载 Aurio
  CreateDirectory "$SMPROGRAMS\${APP}"
  CreateShortcut "$SMPROGRAMS\${APP}\${APP}.lnk" "$INSTDIR\${EXE}" "" "$INSTDIR\${EXE}" 0
  CreateShortcut "$SMPROGRAMS\${APP}\卸载 ${APP}.lnk" "$INSTDIR\uninstall.exe" "" "$INSTDIR\Aurio.ico" 0

  ; 「设置 → 应用」里的卸载入口
  WriteRegStr HKLM "${APP_KEY}" "InstallDir" "$INSTDIR"
  WriteRegStr HKLM "${UNINST_KEY}" "DisplayName" "${APP}"
  WriteRegStr HKLM "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKLM "${UNINST_KEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr HKLM "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\${EXE}"
  WriteRegStr HKLM "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKLM "${UNINST_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKLM "${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKLM "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKLM "${UNINST_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKLM "${UNINST_KEY}" "EstimatedSize" "$0"
SectionEnd

Section "桌面快捷方式" SecDesktop
  SetShellVarContext all
  CreateShortcut "$DESKTOP\${APP}.lnk" "$INSTDIR\${EXE}" "" "$INSTDIR\${EXE}" 0
SectionEnd

Section /o "开机时自动启动" SecAutostart
  SetShellVarContext current
  WriteRegStr HKCU "${RUN_KEY}" "${APP}" '"$INSTDIR\${EXE}" --tray'
SectionEnd

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SecCore} "Aurio 程序本身，以及开始菜单里的「Aurio」和「卸载 Aurio」快捷方式。"
  !insertmacro MUI_DESCRIPTION_TEXT ${SecDesktop} "在桌面上放一个 Aurio 快捷方式。"
  !insertmacro MUI_DESCRIPTION_TEXT ${SecAutostart} "登录 Windows 后自动打开 Aurio。配合 Aurio 设置里的「打开时自动连接」，开机就能直接用。"
!insertmacro MUI_FUNCTION_DESCRIPTION_END

Function .onInstSuccess
  ; WebView2 运行时：Windows 10/11 一般自带，缺了界面打不开
  ReadRegStr $0 HKLM "SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\${WEBVIEW2_GUID}" "pv"
  ${If} $0 == ""
  ${OrIf} $0 == "0.0.0.0"
    ReadRegStr $0 HKCU "Software\Microsoft\EdgeUpdate\Clients\${WEBVIEW2_GUID}" "pv"
  ${EndIf}
  ${If} $0 == ""
  ${OrIf} $0 == "0.0.0.0"
    MessageBox MB_YESNO|MB_ICONEXCLAMATION "这台电脑缺少 Microsoft WebView2 运行时，Aurio 的界面需要它。$\r$\n$\r$\n现在打开微软的下载页面吗？（下载「常青版引导程序」安装即可）" IDNO +2
    ExecShell "open" "https://developer.microsoft.com/microsoft-edge/webview2/"
  ${EndIf}
FunctionEnd

; ———————————— 卸载 ————————————
Var DriverInstalled
Var ChkSettings
Var RemoveSettings

Function un.onInit
  SetRegView 64
  ; 检测虚拟声卡驱动（卸载程序已是管理员权限，不会再弹 UAC）
  StrCpy $DriverInstalled 0
  ${If} ${FileExists} "$INSTDIR\${EXE}"
    ExecWait '"$INSTDIR\${EXE}" --driver-helper status' $0
    ${If} $0 == ${DRV_INSTALLED}
      StrCpy $DriverInstalled 1
    ${EndIf}
  ${EndIf}
  StrCpy $RemoveSettings 0
FunctionEnd

Function un.OverviewPage
  !insertmacro MUI_HEADER_TEXT "卸载 Aurio" "确认要从这台电脑上删除的内容"
  nsDialogs::Create 1018
  Pop $0
  ${If} $0 == error
    Abort
  ${EndIf}

  ${NSD_CreateLabel} 0 0 100% 12u "将要删除："
  Pop $0

  ${NSD_CreateLabel} 8u 18u 100% 12u "•  Aurio 程序和快捷方式（$INSTDIR）"
  Pop $0

  ${If} $DriverInstalled == 1
    ${NSD_CreateLabel} 8u 34u 95% 22u "•  虚拟声卡驱动 VB-Cable：检测到已安装，会先把它卸载干净（设备、音频端点和驱动文件），再删除 Aurio。"
  ${Else}
    ${NSD_CreateLabel} 8u 34u 95% 12u "•  虚拟声卡驱动 VB-Cable：没有安装，不用处理。"
  ${EndIf}
  Pop $0

  ${NSD_CreateCheckbox} 0 68u 100% 12u "同时删除我的设置（选过的 Mac、播放来源等）"
  Pop $ChkSettings

  ${NSD_CreateLabel} 0 92u 100% 24u "卸载完成后需要重启电脑才能完全生效。点「卸载」开始。"
  Pop $0

  GetDlgItem $0 $HWNDPARENT 1
  SendMessage $0 ${WM_SETTEXT} 0 "STR:卸载(&U)"
  nsDialogs::Show
FunctionEnd

Function un.OverviewLeave
  ${NSD_GetState} $ChkSettings $RemoveSettings
FunctionEnd

Section "Uninstall"
  SetShellVarContext all
  DetailPrint "正在关闭 Aurio…"
  !insertmacro CloseAurio

  ; 1. 先卸载驱动
  ${If} $DriverInstalled == 1
    DetailPrint "正在卸载虚拟声卡驱动 VB-Cable（删除设备、音频端点和驱动文件），可能需要半分钟…"
    ExecWait '"$INSTDIR\${EXE}" --driver-helper uninstall "" "$TEMP\aurio-uninstall.log"' $0
    ${If} $0 != 0
      MessageBox MB_YESNO|MB_ICONEXCLAMATION "虚拟声卡驱动没能完全卸载。$\r$\n$\r$\n可以重启电脑后重新安装 Aurio，再卸载一次。现在仍然继续卸载 Aurio 吗？" IDYES +2
      Abort "已取消卸载。"
      DetailPrint "驱动没有完全卸载，继续删除 Aurio"
    ${Else}
      DetailPrint "虚拟声卡驱动已卸载"
    ${EndIf}
    Delete "$TEMP\aurio-uninstall.log"
  ${EndIf}

  ; 2. 删除程序
  DetailPrint "正在删除 Aurio…"
  Delete "$INSTDIR\${EXE}"
  Delete "$INSTDIR\WebView2Loader.dll"
  Delete "$INSTDIR\aurio-win.exe"
  Delete "$INSTDIR\Aurio.ico"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"

  Delete "$SMPROGRAMS\${APP}\${APP}.lnk"
  Delete "$SMPROGRAMS\${APP}\卸载 ${APP}.lnk"
  RMDir "$SMPROGRAMS\${APP}"
  Delete "$DESKTOP\${APP}.lnk"

  DeleteRegKey HKLM "${UNINST_KEY}"
  DeleteRegKey HKLM "${APP_KEY}"

  ; 3. 当前用户的数据：开机启动项、界面缓存、驱动下载目录总是删；设置按用户的选择
  SetShellVarContext current
  DeleteRegValue HKCU "${RUN_KEY}" "${APP}"
  RMDir /r "$LOCALAPPDATA\com.aurio.win"
  RMDir /r "$LOCALAPPDATA\Aurio"
  ${If} $RemoveSettings == ${BST_CHECKED}
    RMDir /r "$APPDATA\com.aurio.win"
    DetailPrint "已删除设置"
  ${EndIf}

  DetailPrint "卸载完成，重启电脑后完全生效"
  SetRebootFlag true
SectionEnd
