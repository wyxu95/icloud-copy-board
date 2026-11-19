# Windows 与 iPhone 共享文字剪贴板

在 iPhone 上选中文字并复制，在 Windows 上按 Ctrl+V。也可以在电脑复制，在手机上粘贴。Rust 后台程序通过已配对设备的 Apple CoreDevice 开发服务传输文字，支持 USB 和局域网 Wi-Fi。手机使用系统剪贴板，无需安装 App。Mac 可以关机。

## 准备设备

1. Windows 需要正在运行的 Apple Mobile Device Service。安装 Apple 的“Apple 设备”或适用于系统版本的 iTunes，使用 USB 数据线连接并解锁手机，按提示信任电脑。
2. 在 iPhone 的“设置 → 隐私与安全性”中启用开发者模式，并完成 iOS 要求的重启确认。设备需要提供 `com.apple.coredevice.pasteboardservice`。已在 Windows 10 x64 与 iOS 27.0.1 上验证无线连接。
3. 使用 Windows Rust MSVC 工具链和 Visual Studio C++ Build Tools，在项目目录运行 `cargo build --release --locked`。
4. 对于初次准备的设备，指定与手机兼容的 Apple Cryptex1 开发者映像解包后的 Restore 目录，里面应包含 `BuildManifest.plist` 和清单引用的映像文件。下列命令通过 USB 启用 Wi-Fi 连接，在需要时安装签名开发者映像。请将示例路径换成实际目录；已完成这一步的设备可直接安装后台任务。

```powershell
$p = Start-Process .\target\release\icloud-copy-board.exe -ArgumentList '--usb --prepare-ddi "C:\AppleDDI\Restore"' -Wait -PassThru
$p.ExitCode
```

## 安装后台任务

在项目目录打开 PowerShell，运行下列其中一条命令。无线模式请使用 iPhone 当前的局域网地址，并让电脑和手机保持网络互通。电脑上保存有多台设备的配对记录时，可加上 `-Device <UDID>` 选择目标。

```powershell
.\install.ps1 -PhoneAddress 192.0.2.10
# 或使用 USB
.\install.ps1 -Usb
```

安装脚本会将程序复制到 `%LOCALAPPDATA%\ClipboardNative`，立即启动，并注册当前 Windows 用户的登录任务。它在交互式用户会话中访问剪贴板，使用期间可以关闭终端。无线 IP 变动时，用新地址重新运行安装命令即可。

```powershell
Get-Content "$env:LOCALAPPDATA\ClipboardNative\status.txt"
Get-ScheduledTask -TaskName 'ClipboardNative-*' | Select-Object TaskName, State
.\install.ps1 -Uninstall
```

状态显示 `Connected` 时可开始复制。每次同步新复制的 UTF-8 文字，上限 1 MiB。程序轮询手机剪贴板版本；若 iOS 先报告文字格式、稍后才提供正文，程序会按格式请求正文，并在写入 Windows 前确认该条目仍是当前版本。锁屏或网络中断可能暂时影响 iOS 开发服务，恢复后程序会自动重连。

## 诊断

使用 `--check` 验证从本机配对记录到手机剪贴板服务的连接。退出码 0 表示已成功读取剪贴板版本。GUI 子系统程序在 PowerShell 中可通过管道显示诊断输出。

```powershell
.\target\release\icloud-copy-board.exe --host 192.0.2.10 --check 2>&1 | Out-Host
```

[Apple Windows 设备支持](https://support.apple.com/zh-cn/118290) · [Rust idevice 库](https://github.com/jkcoxson/idevice)
