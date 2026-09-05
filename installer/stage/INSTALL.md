# 灵犀拼音输入法 for Windows — 安装说明

本安装器(LingxiIME-...-Setup.exe)已自动完成:解压文件到
`C:\Program Files\PrisirIME\`、注册输入法(prisir_tsfsvc --register)、
重启 explorer 让输入法生效。装完用 `Win+Space` 切到「Prisir 输入法」即可。

## 手动验证 / 排查

```cmd
cd "C:\Program Files\PrisirIME"
prisir_tsfsvc.exe --status        :: 查注册状态
prisir_tsfsvc.exe --version       :: 版本号
prisir_tsfsvc.exe --about privacy :: 隐私说明
```

## 卸载

「设置 → 应用 → 已安装的应用」找 Prisir灵犀拼音,卸载;
或跑 `C:\Program Files\PrisirIME\Uninstall.exe`。
卸载会自动 --unregister 并清理文件。

## 反馈问题

状态栏右键菜单 →「反馈问题(打包诊断)」→ 自动打诊断 zip 到桌面并打开反馈页。
