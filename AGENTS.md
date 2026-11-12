# 项目开发约定

- 这是 Rust / Win32 自建任务栏，Rune 用作插件脚本层；不引入持续渲染循环。
- 构建、检查、测试使用 `pwsh.exe -NoLogo -NoProfile -File ./dev.ps1 -Task Check|Test|Clippy`；运行省略 `-Task`。默认 jobs=1、CPU 硬上限 20%、2 个逻辑处理器、BelowNormal 优先级，不绕过限制启动并行 Cargo。
- `dev.ps1` 和可执行程序默认接管原生任务栏并占用底部工作区；`-Preview` / `--preview` 保留原生任务栏。修改接管逻辑时必须保留正常退出和崩溃恢复能力，不得强制结束独立恢复进程。
- 主程序仓库：`D:/workspace/ultra-bar`；插件源码仓库：`D:/workspace/ultra-bar-plugins`。插件变更不得触发 Rust 全量构建。
- 插件身份使用反向域名 ID，版本使用 SemVer precedence；同 ID 的更新默认保持安装来源，独立 .ubplugin 安装允许显式强制替换。包代码放 plugin-system/store，插件数据独立保存；不得为了覆盖版本结束用户正在编辑的插件窗口。
- GitHub 官方源固定为 repository id 1452116417（adoin/ultra-bar-plugins）。仓库 URL 和目录名不作为插件身份；GitHub 请求按需获取并缓存，不加持续网络轮询。
- 临时验证产物使用 `D:/tmp` 下独立任务目录；正常构建、开发运行副本和日志放在本仓库 `target/`。
- 正式主程序交付使用 `scripts/package.ps1` 构建 Inno Setup 安装包；该入口同样通过 DevProcess 以 jobs=1、CPU 20%、2 核及 BelowNormal 构建真正 release，禁止直接使用无限制 cargo release。安装器固定当前用户 AppId；注册和卸载只处理自身文件关联，升级使用按完整 EXE 路径限定的正常退出协议，保留用户数据。安装验证使用 `scripts/test-installer.ps1` 的独立产品身份，不覆盖开发实例。
- 产品界面只保留有操作用途的文案，不把开发要求、实现过程或交付汇报写进 UI。

- 编辑能力统一由主程序的编辑器服务提供。插件只声明类型、数据、选项、约束和业务校验，接收编辑结果；禁止在插件中创建 EDIT、COMBOBOX、日期/时间、颜色、文件、快捷键等编辑控件或调用旧的本地 form::edit。新增编辑类型必须扩展宿主注册表，不允许插件自绘编辑器作为降级。日历、图表、进度等展示渲染仍可由插件自定义。
