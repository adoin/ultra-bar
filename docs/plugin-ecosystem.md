# 插件仓库、包与更新协议 v1

## 三种身份

| 对象 | 示例 | 规则 |
| --- | --- | --- |
| 仓库 | `github:1452116417` | 使用 GitHub repository id，仓库改名后仍能定位；URL 和名称仅用于展示 |
| 插件 | `com.emssion.schedule-calendar` | 稳定、全局的反向域名 ID，跨目录和来源比较同一插件 |
| 版本 | `0.1.0` | SemVer precedence，数字比较，构建元数据不当作更新，默认排除预发布 |

默认官方源为 `adoin/ultra-bar-plugins`。插件中心以 tabs 区分已安装和各仓库；自定义仓库按 URL 或 owner/repository 添加，经 API 获取稳定 ID，重复添加会被拒绝。

## 线上目录

打开仓库 tab 时按需读取 GitHub Releases，缓存 15 分钟；刷新按钮强制检查当前仓库，不做后台持续轮询。优先读取最近已发布的 `plugin-market.json`（schema 2）完整市场快照，其中每项包含下述单包元数据以及 `asset_id`、`release_tag`、`download_url`、`size`；顶层包含 `repository_id` 和源码 `revision`。包可来自同一仓库的历史 Release，无需复制到快照中。仓库身份、标签与版本、下载地址和 SHA-256 均校验。

没有市场快照时，兼容扫描最近 1000 个 Release 的 `plugin-catalog.json`（schema 1）：

```json
{
  "schema_version": 1,
  "plugins": [{
    "id": "com.emssion.schedule-calendar",
    "name": "Schedule Calendar",
    "version": "0.1.0",
    "platform": "windows-x86_64",
    "host": ">=0.1.0,<1.0.0",
    "asset": "com.emssion.schedule-calendar--0.1.0--windows-x86_64.ubplugin",
    "sha256": "完整包文件的SHA-256"
  }]
}
```

单插件 tag 为 `<ID>/v<版本>`，目录项必须与 tag 一致。同仓库各插件独立发布，客户端对每个 ID 选最新兼容稳定版。没有目录资产时，仅对符合 tag 约定、带 GitHub SHA-256 digest 的 .ubplugin 资产提供基本发现。

本机装有 gh 时优先使用其认证，失败会报告错误，不改用匿名重试。最终用户未安装 gh 时，公开仓库使用系统 WinHTTP HTTPS；私有仓库依赖 gh 登录。Token 不写入本程序配置。

## .ubplugin

ZIP 容器，根为 `package.json` 和 `plugin/` 载荷。包清单声明格式 `ultra-bar-plugin`、schema_version=1、插件 ID／版本、平台、host 版本要求及每个载荷的路径与 SHA-256。运行清单 `plugin/plugin.json` 必须与包 ID／版本一致。

原生面板包含 `plugin/bin/<binary>.exe` 与指向该文件的 `.runtime/current.json`；兼容性检查也验证 Windows x86-64 PE 头。拒绝绝对路径、越界、Windows 保留名称、重复路径、链接、摘要错误、缺失入口和不受支持的 API。上限：压缩包 256 MiB、解压总量 512 MiB、单文件 128 MiB、2048 个 ZIP 条目。激活前编译并校验 Rune 渲染入口。

SHA-256 提供完整性校验，不是作者签名；原生面板是受信任本机代码，进程隔离不等于安全沙箱。添加仓库不自动安装其代码，安装通过用户操作触发。

## 安装和来源

代码和版本状态位于 `%LOCALAPPDATA%/UltraBar/plugin-system`：

- `repositories.json`：仓库 ID 与展示信息。
- `installed.json`：每个插件 ID 的当前版本、包摘要、安装实例、来源与启用状态。
- `store/<ID>/<版本>/<摘要>-<安装实例>/`：不可变代码快照。
- `cache/` 与 `downloads/`：发布目录和已下载包。

先在受限暂存目录完成校验，再放入新快照，最后原子更新安装记录。运行中的 EXE 不原地覆盖，失败保留旧记录。用户日程等数据留在插件的数据目录，安装过程不清空；插件自身的数据迁移由插件负责。历史代码快照暂时保留。

默认发布包按插件 ID 优先于开发目录，同 ID 不同时加载两份。开发源码时，可用 `dev.ps1 -PreferDevelopmentPlugins` 显式优先开发目录（可执行程序参数为 `--prefer-dev-plugins`）；普通运行仍保持发布包优先。本地 counter API v1 示例保留兼容。原日历短 ID `schedule-calendar` 的时钟选择映射到新 ID，日程数据路径不变。

在“已安装”tab 中刷新可检查更新，默认保持已安装的仓库来源；本地包尚未关联来源时，会从已配置仓库中寻找同 ID 的更高兼容版本，并显示来源后由用户选择。其他仓库的同 ID 项显示来源差异，用户在对应 tab 选择安装后才切换。普通安装要求更高版本；右键产物图标可明确强制安装当前版本。

## 独立强制安装与文件关联

`.ubplugin` 注册到当前用户 `Software/Classes`，不修改受保护的 UserChoice，不要求管理员权限。命令：

```text
"ultra-bar.exe" --install "%1" --force
```

此模式在创建任务栏窗口前完成并退出，不启动第二个任务栏。强制安装允许同版本覆盖与降级，仍执行结构、摘要、平台和 host 兼容检查。新快照避开文件占用并保留数据；本地包来源记录为“本地安装”，不会假称官方来源。

```powershell
ultra-bar.exe --install path/to/package.ubplugin --force
ultra-bar.exe --register-plugin-association
ultra-bar.exe --unregister-plugin-association
```

也可在插件中心齿轮菜单管理关联，在导入按钮选择本地包。移动主程序后需在新路径重新关联；卸载关联只移除本程序管理的 ProgID。正式安装器应在安装／卸载阶段调用这些入口。

## 发布

插件仓库的 `scripts/package-plugin.ps1` 生成包、摘要与目录；`.github/workflows/release-plugin.yml` 响应插件命名空间 tag。版本必须同时匹配清单、原生 Cargo 包和 tag。代码／许可／必要资源进入包，开发缓存、测试数据和调试版本不进入正式发行目录。

当前两个远程仓库仍需首次推送本次源码。工作流代码已配置；只有真正推送 tag 并成功生成 GitHub Release，客户端目录才会出现发布项。
