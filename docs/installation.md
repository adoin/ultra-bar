# Windows 安装包

在 Windows 10/11 x64、PowerShell 7、Rust MSVC 和 Windows SDK 环境中构建，安装 Inno Setup 6.5+ 并使 `ISCC.exe` 可用：

```powershell
pwsh.exe -NoLogo -NoProfile -File ./scripts/package.ps1
```

产物位于 `dist/UltraBar-<Cargo版本>-windows-x86_64-setup.exe`，同时生成 SHA-256 和构建信息。源码、EXE 版本资源和安装器共用根 `Cargo.toml` 的版本。发行编译使用单任务、20% CPU 上限、两个逻辑核及 BelowNormal 优先级；不会停止开发实例。`-SkipBuild` 仅用于明确复用已构建的相同版本 release 二进制，源码变化后应重新构建。

安装范围是当前用户，默认 `%LOCALAPPDATA%\Programs\Ultra Bar`，可以修改；不需要管理员权限。安装器建立开始菜单入口、可选桌面快捷方式、卸载入口和 `.ubplugin` 文件关联；双击包由程序独立执行强制安装。Windows 已有用户选择的默认打开方式可能优先于扩展名注册，此时可在系统“默认应用”中选择 Ultra Bar。不会修改受保护的 UserChoice 哈希。

安装后运行默认不勾选；静默安装不自动运行、不重启系统、不设置开机自启。正式程序从安装目录的 `plugins` 查找附带插件，同时优先读取用户已安装插件；插件单独发布和安装。

安装包从 Visual Studio 的 `VC/Redist/MSVC` 提取 x64 `vcruntime140.dll`，随程序本地部署，避免依赖开发机预装的运行库；不更改系统共享运行库。构建信息记录运行库版本与 SHA-256。采用微软文档中的 [本地部署方式](https://learn.microsoft.com/en-us/cpp/windows/deployment-in-visual-cpp)，后续发行应使用更新的 MSVC Redistributable 重建安装包。

覆盖安装沿用稳定 AppId、既有目录及快捷方式选择。安装和卸载通过目标安装路径中的 EXE 请求同路径实例退出，等待其实际结束（包含任务栏恢复守护进程）。有禁用的模态窗口或插件中心正在工作时拒绝升级；超时不强杀应用，不关闭 Explorer，也不关闭其他路径的开发实例。退出时恢复原生任务栏。

用户数据保存在 `%LOCALAPPDATA%\UltraBar`，包含设置、托盘偏好、插件仓库/安装记录和日程。升级及卸载保留这些数据。卸载仅在文件关联仍属于当前安装路径时移除关联，不覆盖其他安装的关联。

```powershell
# 静默安装及卸载；含空格路径应正确引用参数
./dist/UltraBar-0.1.0-windows-x86_64-setup.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART
& "$env:LOCALAPPDATA/Programs/Ultra Bar/unins000.exe" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART
# 构建 release 后运行隔离验证
pwsh.exe -NoLogo -NoProfile -File ./scripts/test-installer.ps1
```

隔离验证使用相同发行 EXE、独立安装器 AppId 和私有文件后缀，在 `D:/tmp/ultra-bar-installer-20261110` 安装；隐藏的测试实例使用独立配置，保留原有开发实例与真实 `.ubplugin` 注册。测试产物不会进入 `dist`。

安装包目前不带代码签名；项目尚未配置真实签名证书。构建不会上传或发布 GitHub Release。依赖许可随包包含在 `THIRD-PARTY-NOTICES.txt`；项目自身发布许可须由仓库所有者确定。

`.github/workflows/release.yml` 支持手动构建并保存安装包 artifact；推送与 Cargo 版本一致的 `v<版本>` 标签时，完成检查、测试和 release 打包后建立 GitHub Release，附安装包、SHA-256 及构建信息。该工作流文件本身不会触发发布；需要将代码和标签推送到仓库。

## 程序内更新

“常规”中可开关自动检查并手动检查更新。本体按 GitHub repository ID `1452115414` 获取稳定版，不采用预发布或只改变构建元数据的版本。发现新版后提示下载安装；下载在后台进行，检查本仓库安装包文件名、大小、SHA-256、GitHub 摘要及 EXE 中的产品身份和版本。临时文件完整写入后才原子发布，不运行半包或校验失败的文件。

下载完成且没有未保存编辑、模态窗口或全屏场景时，打开正常安装向导，并通过现有退出协议关闭当前实例。退出再次核对编辑状态，不强制结束恢复进程。安装向导沿用原有当前用户 AppId、安装路径、关联和配置保留行为；后台检查本身不下载、不安装。独立验证入口 `--update-smoke <绝对目录> --download-current` 只下载校验本体，并在隔离插件目录验证发行包更新和数据／启用状态保留，不运行安装器或插件 EXE。
