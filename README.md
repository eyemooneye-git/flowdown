# FlowDown

一个轻量的 Windows 原生下载管理器：监听剪贴板里的 HTTP / HTTPS / FTP / 磁力链接，
用多连接分段加速下载，并内置 BitTorrent 支持。界面全部使用 Win32 原生控件
（`EDIT` / `BUTTON` / `SysListView32` / 进度条 / 状态栏），没有 Electron、WebView
或第三方 GUI 框架。单个 exe，静态链接 C 运行库，不需要安装。

## 功能

- **剪贴板监听**：复制链接后自动加入任务，可配置是否立即开始。
- **多线程 HTTP / HTTPS**：通过 `Range` 请求把文件切成多个分段并行下载，支持断点续传。
- **FTP**：支持 `SIZE` / `REST` 续传，服务器允许时同样使用多分段。
- **BitTorrent / 磁力链接**：内置 [librqbit](https://github.com/ikatson/rqbit) 会话，支持
  DHT、TCP 监听端口、公共 tracker 兜底、暂停 / 继续 / 删除。列表显示种子数 / 连接数；
  磁力链接超过 90 秒拿不到元数据会明确报错，而不是一直停留在“连接中”。
- **原生界面**：虚拟列表（十万级任务也不卡）、状态栏、总进度、托盘气泡通知。
- **中英文界面**：跟随 Windows 显示语言；中文系统显示中文，其他语言显示英文。
- **托盘图标**：显示 / 隐藏窗口、全部暂停 / 继续、打开下载目录、剪贴板开关、开机启动、退出。
- **开机启动**：写入 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`，可在托盘或设置中开关。
- **单实例**：重复启动时把新链接通过 `WM_COPYDATA` 转发给已运行的窗口，然后自行退出。
- **可配置下载目录**：默认 `%USERPROFILE%\Downloads\FlowDown`，可在设置中修改。
- 窗口位置、大小、最大化状态和所有选项保存在
  `%LOCALAPPDATA%\FlowDown\settings.ini`。

## 构建

需要 Rust stable 和 MSVC 工具链（`x86_64-pc-windows-msvc`）。

```powershell
cd C:\Projects\flowdown
cargo test
cargo build --release
```

产物：`target\release\flowdown.exe`（约 5.9 MB）。`.cargo/config.toml` 已启用
`+crt-static`，因此不依赖 VC++ Redistributable。

图标、版本信息和 manifest（Common Controls v6、PerMonitorV2 DPI、长路径）由
`build.rs` / `app.rc` 在构建时嵌入。

## 使用

1. 运行 `flowdown.exe`。
2. 复制一个链接，例如：
   - `https://example.com/file.zip`
   - `ftp://ftp.example.com/pub/file.iso`
   - `magnet:?xt=urn:btih:...`
3. FlowDown 会自动把链接加入列表并开始下载；也可以粘贴到顶部输入框，按
   **Enter** 或点“Add”。
4. 列表操作：
   - **双击 / Enter**：打开已完成的文件
   - **Delete**：移除任务
   - **右键菜单**：打开、在文件夹中显示、复制链接、暂停、继续、移除、清除已完成
5. 关闭窗口默认最小化到托盘（可在设置中关闭）。从托盘菜单选择 **Exit** 才会真正退出。

### 命令行

```text
flowdown.exe [--minimized] [--add <link>] [<link> ...]
```

- `--minimized`：启动后只显示托盘图标。
- `--add <link>`：把链接加入已有实例（或新实例）。
- 直接传链接也可以。

## 设置

`%LOCALAPPDATA%\FlowDown\settings.ini`：

| 键 | 说明 |
| --- | --- |
| `download_dir` | 下载保存目录 |
| `connections` | 每个 HTTP/FTP 任务的连接数（1–32） |
| `monitor_clipboard` | 是否监听剪贴板 |
| `auto_start_downloads` | 新链接是否自动开始 |
| `autostart` | 是否开机启动（启动时以注册表为准） |
| `close_to_tray` | 关闭窗口时是否保留在托盘 |
| `start_minimized` | 启动时是否最小化到托盘 |
| `window_x/y/w/h` | 窗口位置与大小 |

## 代码结构

| 文件 | 职责 |
| --- | --- |
| `src/main.rs` | 参数解析、单实例互斥体、启动入口 |
| `src/ui.rs` | 主窗口、列表、工具栏、设置对话框、剪贴板与托盘事件 |
| `src/download.rs` | HTTP/FTP 分段下载、暂停 / 取消 / 续传、`.part` 合并与清理 |
| `src/torrent.rs` | librqbit 后台 Tokio 运行时、命令通道、状态轮询 |
| `src/tray.rs` | `Shell_NotifyIconW` 图标、气泡通知和右键菜单 |
| `src/settings.rs` | INI 读写、默认目录、窗口状态 |
| `src/lang.rs` | 中英文文案选择（跟随 Windows 显示语言） |
| `src/autostart.rs` | HKCU Run 键读写 |
| `src/util.rs` | 剪贴板、链接提取、文件名处理、文件夹选择、格式化 |

## 已验证

- `cargo test`：11 个单元测试通过（链接解析、文件名、Range、INI、启动命令引号、语言选择等）。
- 本地带 `Range` 支持的 HTTP 服务：5 MiB 文件多分段下载，SHA-256 与源文件一致。
- 剪贴板复制链接后自动加入并完成下载。
- 第二次启动通过 `WM_COPYDATA` 转发链接，1 秒内退出，首个实例完成第二个任务。
- 设置对话框打开 / 保存、开机启动注册表写入 / 删除。
- 关闭窗口后进程正常退出，托盘图标移除。

## 说明

- FTP 分段加速取决于服务器是否支持 `REST`；不支持时自动退回单连接。
- BitTorrent 速度取决于种子健康度和网络环境。只有 info hash 的磁力链接会使用内置的公共
  tracker 列表辅助找 peer；如果 90 秒内仍拿不到元数据，任务会显示超时错误。
- 目前不包含浏览器扩展、下载限速、计划任务和任务持久化（重启后任务列表为空，
  但已下载的文件和 `.part` 文件仍在磁盘上）。
