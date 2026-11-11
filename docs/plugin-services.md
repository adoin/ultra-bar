# API v3 后台插件

API v1 Rune 插件、API v2 日历面板继续沿用原协议。v3 在 Rune 入口之外增加宿主管理的原生服务，新增 `leading`（开始按钮之后）和 `tray`（原生托盘之前）槽位；宽度范围 16–320。时钟槽仍使用既有机制。

```json
{
  "id":"com.example.search",
  "name":"Search",
  "version":"0.1.0",
  "api_version":3,
  "entry":"main.rn",
  "slot":"leading",
  "width":20,
  "icon":"",
  "capabilities":["panel","service","shell.open"],
  "service":true,
  "panel":{"runtime":".runtime/current.json","binary":"search","background":true}
}
```

`service` 要求 API 3、显式 `service` capability 和后台原生 panel。面板仍按哈希运行目录打包，ID／SemVer、来源锁定及 `.ubplugin` 安装不变。原生代码属于受信任代码；资源隔离不是权限安全沙箱。

## 生命周期与预算

宿主以 `--service --owner HWND` 启动一份服务，在完成 Windows Job 关联后发送 Initialize。服务等待 Initialize 后才执行后台工作。每个服务及其子进程共用 5% CPU 硬上限、1 个逻辑处理器、BelowNormal 和 384 MiB 提交内存上限；所有 v3 服务再共用 10% CPU、1 GiB 内存上限，避免插件数增加后上限相加。开发构建另受现有 20%／2 核／jobs=1 限制。

同一 ID 热重载切换二进制、卸载或退出会终止其自有任务组；异常退出最多尝试重启 3 次，延时递增。当前 v3 适用于可随时退出的只读查询和监测服务；有未保存编辑状态的插件继续用 v2 日历协议，不能直接改为 v3。宿主退出时发送 Shutdown，所有服务共享短暂退出宽限，之后关闭各自 Job；宿主崩溃由 KILL_ON_JOB_CLOSE 清理。不会接管或关闭已运行的 Everything、Codex 桌面客户端等外部进程。

服务 stdout 是 UTF-8 JSON Lines，单行最多 64 KiB；SDK `crates/plugin-sdk` 提供有界读取和消息类型，`crates/plugin-ui` 提供阻塞消息循环、暗色圆角毛玻璃面板、搜索输入和列表。宿主读写都有界队列，后台查询不能阻塞任务栏 UI。不在 stdout 写日志、令牌或对话数据。

## 协议

宿主发送：`initialize {protocol:1,id,data_dir,accent}`、`show`、`theme {accent}`、`shutdown`。声明 `foreground` capability 后才接收 `foreground {executable,title}`；标题最多 512 UTF-16 单元。宿主自己的窗口和其拥有的插件窗口不改变当前外部应用判断。

服务发送：

- `ready`：完成初始化。
- `widget {pages:[{id,text,tooltip,progress}]}`：最多 8 页，文字 80 字符、提示 1024 字符。`progress` 为 0–1 的剩余比例，未知用 null；空页数组隐藏 tray 槽位。多页每 5 秒切换，只有 180ms 过渡期间启动短时绘制计时器。
- `open {path,select_parent}`：仅声明 `shell.open` 的服务可请求。路径必须为绝对本地文件路径，宿主执行 Shell 打开或 Explorer 定位。操作通过脱离插件和开发 Job 的宿主 helper 执行，插件退出不会关闭用户刚打开的程序。

此阶段后台服务不获得系统服务安装、任意 Shell 命令或索引扫描接口。Everything 插件使用既有客户端的官方 IPC；如以后需要额外数据源，其常驻职责也放在独立服务进程内，并明确其轮询／事件订阅和资源预算。

## 验证

`dev.ps1 -Task Check|Test|Clippy|PluginBuild` 同时处理两个新增原生插件。`ultra-bar.exe --plugin-services-smoke D:/tmp/<任务目录>` 在独立 profile 下验证服务启动、真实 Codex 账户查询、空闲 CPU 和退出清理，不接管任务栏。Everything CLI 探针为 `everything-search.exe --probe QUERY ABSOLUTE_OUTPUT_JSON`；Codex 探针为 `agent-usage.exe --probe-codex ABSOLUTE_OUTPUT_JSON`。探针结果只包含搜索路径或额度，不输出认证数据。

## 原生时钟摘要（宿主 0.2.0）

API v2 的后台 clock 面板可向启动环境 `ULTRA_BAR_CLOCK_STATE` 指定的独立插件数据目录，原子写入 `clock.json`。此路径按插件反向域名 ID 分配，与包目录和日程私有数据分离；没有该环境时，面板不得假定宿主支持天气。

摘要字段：`version: 1`、`display: 0|1|2`（时间／并排／六秒轮播）、`lunar`、`weather`、`tooltip`、`utc_offset_seconds`、`updated`（Unix 秒）。天气字符串不超过80字符，农历40字符，提示1024字符，JSON不超过16 KiB；严禁放 API Key、账户或日程正文。`updated` 超过三分钟后宿主回退系统时间。偏移用于用户已有时间格式，包含秒的格式仍按当前系统时刻计算，不冻结在发布时刻。

面板负责天气网络缓存和凭据；宿主只订阅目录事件并在既有计时中处理摘要，不增加网络轮询或持续绘制。时钟后台进程保存草稿的生命周期仍遵循 API v2，不能作为无状态 API v3 服务直接强制替换。
