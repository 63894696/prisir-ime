; =============================================================================
; lingxi_ime.nsi — 灵犀拼音输入法 for Windows 安装器
; (Prisir 湃睿思 AI, 2026-09-04)
;
; 设计:
;   - RequestExecutionLevel admin: 装到 C:\Program Files\PrisirIME 必须管理员;
;     顺便让 prisir_tsfsvc --register 能写 HKLM(不提升时 HKLM 静默跳过, 仅 HKCU)。
;   - 零依赖: 只调自家 prisir_tsfsvc.exe 做注册/卸载, 不引入 regsvr32/外部工具。
;   - 自包含: 引擎/词库/索引全打进包, 装完即可用全部功能(拼音+手写走 Windows Ink)。
;   - 注册/卸载逻辑全在 prisir_tsfsvc(register.rs / unregister), 已在 VM 反复验证。
;
; 构建: makensis /DVERSION=1.0.0-beta.1 /DCHANNEL=beta lingxi_ime.nsi
; =============================================================================

Unicode true
!include "MUI2.nsh"
!include "FileFunc.nsh"

; ---- 可由命令行覆盖 ----
!ifndef VERSION
  !define VERSION "1.0.0-beta.1"
!endif
!ifndef CHANNEL
  !define CHANNEL "beta"
!endif
; 产物源目录(build_win.sh 的 STAGE_DIR 内容平铺进来前, 我们用 STAGE 变量指源)
!ifndef SRC
  !define SRC "stage"
!endif

!define PRODUCT   "Prisir灵犀拼音"
!define PRODUCT_EN "LingxiIME"
!define COMPANY   "Prisir (湃睿思) AI"
!define INSTALL_REG "Software\${PRODUCT_EN}"

Name "${PRODUCT} ${VERSION}"
OutFile "LingxiIME-Windows-x64-${VERSION}-${CHANNEL}-Setup.exe"
InstallDir "$PROGRAMFILES64\PrisirIME"
InstallDirRegKey HKLM "${INSTALL_REG}" "InstallDir"
RequestExecutionLevel admin
SetCompressor /SOLID lzma

; ---- 界面 ----
!define MUI_ABORTWARNING
!define MUI_ICON   "${SRC}\pinyin.ico"
!define MUI_UNICON "${SRC}\pinyin.ico"
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "${SRC}\ABOUT.md"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
; 完成页不挂「查看注册状态」运行项 — 用户不需要看技术细节; 出问题走反馈, 我们查日志。
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "SimpChinese"

; ---- 版本资源(资源管理器右键属性看) ----
VIProductVersion "1.0.0.0"
VIAddVersionKey "ProductName"   "${PRODUCT}"
VIAddVersionKey "CompanyName"   "${COMPANY}"
VIAddVersionKey "FileVersion"   "${VERSION}"
VIAddVersionKey "FileDescription" "灵犀拼音输入法安装器 (纯本地, 隐私保护)"
VIAddVersionKey "LegalCopyright" "${COMPANY}"

; =============================================================================
Section "安装" SEC01
    SetShellVarContext all

    ; ---- 清旧部署残留(关键, 防旧 DLL 影子覆盖新装) ----
    ; ffi.rs 引擎/词库查找顺序把 C:\PrisirIME 排在 C:\Program Files\PrisirIME 之前,
    ; 老开发/测试版若在 C:\PrisirIME 留过 DLL, 会让系统加载旧版而非本次安装版。
    ; 先解除占用(ctfmon 持有 DLL 句柄), 再删目录; 删除失败(DLL 仍被占)不致命, 仅提示。
    DetailPrint "清理旧版部署目录 C:\PrisirIME (若存在) ..."
    nsExec::ExecToLog 'taskkill /F /IM ctfmon.exe'
    Pop $9
    Sleep 500
    RMDir /r "C:\PrisirIME"
    ${If} ${FileExists} "C:\PrisirIME\prisir_ime_tsf.dll"
        DetailPrint "警告: C:\PrisirIME 旧 DLL 仍被占用, 未删除(不影响本次安装到 Program Files)"
    ${EndIf}

    SetOutPath "$INSTDIR"

    ; 核心二进制(覆盖安装)
    SetOverwrite on
    File "${SRC}\prisir_tsfsvc.exe"
    File "${SRC}\prisir_ime_tsf.dll"
    File "${SRC}\prisir_ime.dll"
    File "${SRC}\prisir_hw.exe"
    File "${SRC}\VERSION.txt"
    File "${SRC}\ABOUT.md"
    File "${SRC}\LICENSE.txt"
    File "${SRC}\INSTALL.md"
    File "${SRC}\pinyin.ico"

    ; 词库(只带 ciku.db;引擎自 09-02 起固定走 SQLite, .idx/.midx 已旁路不读, 不带)
    SetOutPath "$INSTDIR\models"
    File "${SRC}\models\ciku.db"

    ; ---- 语音听写(独立 exe + SenseVoice 模型, 装进 Program Files\PrisirIME\voice) ----
    ; lingxi_voice.exe 是 PyInstaller 冻结版(自带 Python 运行时和全部 pip 依赖, 无需装 Python);
    ; 模型是同目录 sensevoice-small\(lingxi_app._find_model_dir 优先认 exe 同目录)。
    ; 状态栏「语」按钮发命名事件触发; 未运行则 ShellExecute 拉起本 exe(status_bar.rs resolve_voice_launcher)。
    SetOutPath "$INSTDIR\voice"
    File "${SRC}\voice\lingxi_voice.exe"
    SetOutPath "$INSTDIR\voice\sensevoice-small"
    File "${SRC}\voice\sensevoice-small\model.onnx"
    File "${SRC}\voice\sensevoice-small\tokens.json"
    File "${SRC}\voice\sensevoice-small\config.yaml"

    ; ---- 插件框架: 写默认 plugins.json 到 %LOCALAPPDATA%\Prisir\ ----
    ; 声明 AI 助手插件(PrisirAI.exe, 用户独立安装); exe 不在则按钮/菜单自动隐藏。
    ; 后续插件(皮肤/宠物等)照此模板加, 用户从网站下载解压到 plugins\ 即用。
    DetailPrint "写入插件配置 plugins.json ..."
    CreateDirectory "$LOCALAPPDATA\Prisir"
    CreateDirectory "$LOCALAPPDATA\Prisir\plugins"
    FileOpen $0 "$LOCALAPPDATA\Prisir\plugins.json" w
    FileWrite $0 '{$\r$\n'
    FileWrite $0 '  "plugins": [$\r$\n'
    FileWrite $0 '    {$\r$\n'
    FileWrite $0 '      "id": "ai",$\r$\n'
    FileWrite $0 '      "name": "AI 助手",$\r$\n'
    FileWrite $0 '      "exe": "plugins/ai/PrisirAI.exe",$\r$\n'
    FileWrite $0 '      "event": "PrisirLingXi_AiToggle_Event",$\r$\n'
    FileWrite $0 '      "button": "AI",$\r$\n'
    FileWrite $0 '      "enabled": true$\r$\n'
    FileWrite $0 '    }$\r$\n'
    FileWrite $0 '  ]$\r$\n'
    FileWrite $0 '}$\r$\n'
    FileClose $0

    ; 卸载器
    SetOutPath "$INSTDIR"
    WriteUninstaller "$INSTDIR\Uninstall.exe"

    ; ---- 注册表: 卸载入口(「应用与功能」能看到/能卸) ----
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_EN}" \
        "DisplayName" "${PRODUCT} ${VERSION}"
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_EN}" \
        "UninstallString" "$\"$INSTDIR\Uninstall.exe$\""
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_EN}" \
        "DisplayIcon" "$INSTDIR\pinyin.ico"
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_EN}" \
        "Publisher" "${COMPANY}"
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_EN}" \
        "DisplayVersion" "${VERSION}"
    WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_EN}" \
        "NoModify" 1
    WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_EN}" \
        "NoRepair" 1

    ; 记录安装目录(供下次安装定位/升级)
    WriteRegStr HKLM "${INSTALL_REG}" "InstallDir" "$INSTDIR"

    ; ---- 注册 IME(关键路径, 真结果, 不许假 PASS) ----
    ; prisir_tsfsvc --register: HKCU CTF TIP + COM InprocServer32 + HKLM CATID(此时 elevated)。
    ; 返回码非 0 即视为失败, 弹窗告知并中止(不留半截注册)。
    DetailPrint "注册输入法 (prisir_tsfsvc --register) ..."
    nsExec::ExecToLog '"$INSTDIR\prisir_tsfsvc.exe" --register "$INSTDIR\prisir_ime_tsf.dll"'
    Pop $0
    ${If} $0 != 0
        MessageBox MB_ICONEXCLAMATION|MB_OK \
            "输入法注册返回码 $0 (非 0)。$\n$\n请手动在管理员 cmd 跑:$\n  cd /d $\"$INSTDIR$\"$\n  prisir_tsfsvc.exe --register-elevated$\n$\n然后重启 explorer.exe。"
    ${EndIf}

    ; ---- explorer 处理(2026-09-05 beta.3 起: 完全不杀) ----
    ; 真因链: taskkill /F /IM explorer.exe 后 ExecShell 拉不回 GUI → 黑屏(3 次实测)。
    ; 故无论静默/交互都不杀 explorer。beta.6 起装完即用靠 HKCU CTF\Assemblies 运行时
    ; 映射(ctfmon 即时读, 不需重启 explorer); 静默(常经 ssh/无人值守)时 tsfsvc 跑在
    ; SYSTEM token, 抢占写不到登录用户 HKCU, 此时装完即用可能不生效, 注销重登后 TIPC
    ; 仍会枚举生效 — 可接受。
    DetailPrint "安装完成。"

    ; ---- 装完一次性弹窗(2026-09-05, 对齐搜狗体验) ----
    ; 静默(/S)模式不弹(无人值守, ssh 下也弹不出交互框)。
    ${IfNot} ${Silent}
        MessageBox MB_ICONINFORMATION|MB_OK \
            "Prisir 灵犀拼音已安装完成, 并设为默认输入法。"
    ${EndIf}
SectionEnd

; =============================================================================
Section "Uninstall"
    SetShellVarContext all

    ; 先卸载注册(解 TIP/COM), 再删文件。
    DetailPrint "注销输入法 (prisir_tsfsvc --unregister) ..."
    nsExec::ExecToLog '"$INSTDIR\prisir_tsfsvc.exe" --unregister'
    Pop $0

    ; 释放 DLL 占用说明(2026-09-05 beta.3: 完全不杀 explorer)。
    ; 同安装段 — taskkill explorer 黑屏风险大于收益, 交互自启不可靠。
    ; 被 ctfmon/应用占用的 DLL 本次删不掉属正常, 残留待注销重登后由系统/用户清;
    ; 注销重登后重新安装即可覆盖。不杀 explorer 避免卸载即黑屏。
    DetailPrint "已注销注册。被占用的 DLL 若删不掉, 注销重登后手动删 $INSTDIR 即可。"

    ; 旧版部署目录残留(与本安装并存过才删, 防误删用户数据 — 只删已知文件)
    Delete "C:\PrisirIME\prisir_ime.dll"
    Delete "C:\PrisirIME\prisir_ime_tsf.dll"
    RMDir  "C:\PrisirIME\models"
    RMDir  "C:\PrisirIME"

    ; 删文件(词库大, 一并删)。
    ; 2026-09-05 beta.5: /REBOOTOK — ctfmon 永久持有 prisir_ime_tsf.dll, 当前删不掉
    ; 时静默返 0 会「假卸载」(文件留、注册已解, 状态不一致)。/REBOOTOK 把删不掉的
    ; 标记为重启后删, 保证最终清干净; 能删的仍立即删。
    Delete /REBOOTOK "$INSTDIR\prisir_tsfsvc.exe"
    Delete /REBOOTOK "$INSTDIR\prisir_ime_tsf.dll"
    Delete /REBOOTOK "$INSTDIR\prisir_ime.dll"
    Delete /REBOOTOK "$INSTDIR\prisir_hw.exe"
    Delete /REBOOTOK "$INSTDIR\VERSION.txt"
    Delete /REBOOTOK "$INSTDIR\ABOUT.md"
    Delete /REBOOTOK "$INSTDIR\LICENSE.txt"
    Delete /REBOOTOK "$INSTDIR\INSTALL.md"
    Delete /REBOOTOK "$INSTDIR\pinyin.ico"
    Delete /REBOOTOK "$INSTDIR\models\ciku.db"
    RMDir /REBOOTOK "$INSTDIR\models"

    ; 语音模块(exe + 模型)
    Delete /REBOOTOK "$INSTDIR\voice\lingxi_voice.exe"
    Delete /REBOOTOK "$INSTDIR\voice\sensevoice-small\model.onnx"
    Delete /REBOOTOK "$INSTDIR\voice\sensevoice-small\tokens.json"
    Delete /REBOOTOK "$INSTDIR\voice\sensevoice-small\config.yaml"
    RMDir /REBOOTOK "$INSTDIR\voice\sensevoice-small"
    RMDir /REBOOTOK "$INSTDIR\voice"

    Delete /REBOOTOK "$INSTDIR\Uninstall.exe"
    RMDir /REBOOTOK "$INSTDIR"

    ; 清注册表
    DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_EN}"
    DeleteRegKey HKLM "${INSTALL_REG}"

    ; 2026-09-05 beta.3: 不再补拉 explorer(没杀它, 无需拉; 且 ssh 下拉不动 GUI)。
    ; beta.5: 若有文件被占用(ctfmon 持 dll)标记了重启删除, 提示用户重启完成卸载。
    ${If} ${RebootFlag}
        DetailPrint "部分文件被占用(ctfmon 持有 DLL), 已标记重启后删除。重启后卸载彻底完成。"
        ; 静默模式不弹窗(无人值守), 只留 DetailPrint; 交互模式提示是否重启。
        ${IfNot} ${Silent}
            MessageBox MB_YESNO|MB_ICONQUESTION "部分文件正被系统占用, 需重启才能彻底删除。$\n现在重启吗?" IDNO +2
            Reboot
        ${EndIf}
    ${Else}
        DetailPrint "卸载完成。"
    ${EndIf}
SectionEnd
