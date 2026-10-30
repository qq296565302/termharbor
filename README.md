# TermHarbor / 终端港

跨平台 SSH 终端与文件管理桌面客户端。Tauri 2 + Rust 内核 + React/TypeScript 前端。

当前进度：**M0（工程骨架）已完成**。见 [docs/开发计划.md](docs/开发计划.md)。

---

## 技术栈

| 层级 | 选型 |
|---|---|
| 桌面框架 | Tauri 2.x（不用 Electron） |
| 内核 | Rust 2021，Tokio 异步运行时 |
| 前端 | React 18 + TypeScript + Vite + Zustand |
| 终端 | xterm.js（M2 接入） |
| SSH/SFTP | russh + russh-sftp（M2/M3 接入） |
| 存储 | SQLite（rusqlite，bundled） |
| 加密 | Argon2id + AES-256-GCM + 系统钥匙串 |

## 目录结构

```text
.
├── src/                      # 前端
│   ├── lib/                  # ipc 封装、错误码映射、DTO 类型
│   ├── hooks/                # usePing 等
│   ├── stores/               # uiStore（UI 态）/ sessionStore（会话态）
│   └── pages/                # 自检页 + 各模块占位页
├── src-tauri/
│   ├── migrations/           # SQL 迁移，编译期内嵌
│   └── src/
│       ├── commands/         # Tauri command，只做校验与转发
│       ├── crypto/           # cipher / kdf / provider
│       ├── storage/          # db / migrations
│       ├── events/           # 事件与 Channel 载荷
│       ├── monitor/          # 服务器状态采集（M4）
│       ├── sftp/             # SFTP（M3）
│       ├── snippets/         # 命令片段（M6/M7）
│       ├── ssh/              # SSH 会话与终端（M2）
│       ├── error.rs          # 统一错误码
│       ├── logging.rs        # 日志初始化
│       ├── redact.rs         # 脱敏
│       └── state.rs          # 全局状态
├── scripts/app-icon.py       # 生成图标源图（纯 Python，无需 Pillow）
├── spikes/                   # 技术 Spike（独立 crate，不进产品构建）
│   ├── src/bin/s1_compat.rs      # 算法兼容性矩阵
│   ├── src/bin/s2_throughput.rs  # 终端吞吐与背压
│   ├── src/utf8_stream.rs        # UTF-8 增量解码器（M2 直接搬进产品）
│   └── fixtures/                 # 一次性测试主机密钥
└── .github/workflows/ci.yml  # 三平台 CI
```

## 开发环境

前置：Rust stable（MSVC 工具链）、Node 22、pnpm、WebView2（Win10/11 一般已自带）。

> **Windows 构建注意**：后续引入 `russh` 时，它默认的 `aws-lc-rs` 后端需要 NASM 汇编器。
> 解决办法是在 `Cargo.toml` 里加 `aws-lc-rs = { version = "1", features = ["prebuilt-nasm"] }`，
> 用官方预编译目标，不必安装 NASM。详见 [spike-notes.md §1.5](docs/spike-notes.md)。

```bash
pnpm install
pnpm tauri:dev      # 启动桌面应用（带热更新）
```

> `tauri:*` 脚本走的是 `scripts/with-cargo.mjs`：如果 PATH 里找不到 cargo，
> 它会把 `~/.cargo/bin` 临时补进**子进程**（不改系统设置），并在控制台打印一行提示。
> 起因是本机 rustup 装完没把该目录写进 PATH，而 `tauri dev` 报的
> `cargo metadata ... program not found` 完全看不出缺什么。PATH 正常时它等于直通。

其他命令：

```bash
pnpm check          # 前端类型检查
pnpm test           # 前端单测（vitest）
cargo test --manifest-path src-tauri/Cargo.toml   # Rust 单测
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
pnpm tauri:build    # 三平台安装包
```

## 数据与密钥

- 配置库：`app_data_dir/termharbor.db`（SQLite，WAL）
- 日志：`app_log_dir/termharbor.log`，按天滚动
- 数据密钥：默认存**系统钥匙串**；开发期可强制走明文文件：

```bash
# Windows PowerShell
$env:TERMHARBOR_KEY_PROVIDER="devfile"
# macOS / Linux
export TERMHARBOR_KEY_PROVIDER=devfile
```

`devfile` 模式会把 32 字节密钥明文写在 `app_data_dir/dev_key.hex`，**只用于开发与 CI**，启动时会打 warn。

日志级别用 `TERMHARBOR_LOG` 覆盖，例如 `TERMHARBOR_LOG=termharbor_lib=debug,info`。

## M0 已落地的东西

- **错误码体系**：`ErrCode` 16 个码，序列化为 `{ code, message, detail }`；detail 构造时即脱敏；前端 `src/lib/errors.ts` 一一对应，禁止直显 Rust 原文。
- **加密层**：`FieldCipher`（AES-256-GCM，密文自带 1 字节版本号且参与 AAD，nonce 每次随机）；`KeyProvider` 抽象出钥匙串 / 开发期文件两种来源；Argon2id 派生已实现，留给主密码模式。
- **存储层**：`PRAGMA user_version` 迁移框架，迁移 SQL 编译期内嵌，逐条独立事务；`Db::call` 强制在 `spawn_blocking` 里跑同步 SQLite。
- **通信层**：Command 请求响应 + Channel 高频推送两条链路，自检页可实时验证。
- **脱敏**：`redact()` 文本打码 + `Redacted<T>` 包装器，`Debug` 恒输出 `***`。
- **CI**：前端类型检查/单测/构建 + Rust fmt/clippy/test 三平台矩阵 + 手动触发的打包冒烟。

## M1 已落地的东西

- **连接配置 CRUD**：`connection_list / get / create / update / delete`，按名称/主机/用户名模糊搜索；迁移 002 增加 `legacy_algorithms` 列。
- **凭据加密**：密码与私钥口令经 AES-256-GCM 加密后存入 `secret_blob` / `key_pass_blob`。前端只能拿到 `hasPassword` 这类布尔量，永远拿不到明文。
- **连接测试**：15s 超时，返回握手耗时；失败按 `SSH_*` 错误码区分（超时/认证失败/算法协商失败）。
- **主机指纹 TOFU**：首次连接弹确认框（指纹格式与 `ssh-keygen -lf` 一致），确认后写入 `known_hosts`；**指纹变更直接阻断**，只提供「清除本地记录」这一个显式出口。
- **兼容老服务器算法**：S1 Spike 结论落成连接级开关，打开后追加 SHA-1 系列算法兜底，现代算法仍排最前。
- **私钥文件选择**：接 `tauri-plugin-dialog`，capabilities 只开 `dialog:allow-open`。
- **测试**：69 个 Rust 测试，其中 8 个是**起真实 SSH 服务端**的集成测试（TOFU 全流程、指纹变更阻断、错密码、私钥认证、老算法开关），不是 mock。

## M2 已落地的东西

- **交互式终端**：xterm.js 渲染 + russh PTY，支持 `ls`/`vim`/`top` 这类全屏程序（窗口 resize 会转成 `window_change`）。
- **多标签**：标签常驻挂载，切页/切标签只切显隐，不会销毁终端、不丢回滚缓冲；关闭标签即释放 Rust 侧会话。
- **UTF-8 增量解码**：`ssh/decoder.rs` 缓存不完整尾字节，SSH 分包切断多字节字符也不会乱码。
- **批量推送**：16ms / 32KB 聚合，配合前端 `term.write` 回调 ack，未确认量超 2MB 就停止读取 —— 端到端背压。
- **复制粘贴**：`Ctrl/Cmd+Shift+C/V`；普通 `Ctrl+C` 仍然是发 SIGINT，不抢终端的肌肉记忆。
- **断线重连**：断开时在终端顶部弹一条状态带（保留现场和回滚内容），点「重连」会在**同一个标签**里开一个新会话，并明确写一行提示，不会让人误以为还是原来那条连接。
- **快捷键**：`Ctrl/Cmd+Shift+T` 新建、`Ctrl/Cmd+Shift+W` 关闭、`Ctrl+Tab` / `Ctrl+Shift+Tab` 切换、`Ctrl/Cmd+Shift+C/V` 复制粘贴。
  刻意不用裸 `Ctrl+T` / `Ctrl+W` —— 它们在 shell 里分别是转置字符和删除前一个词。
- **测试**：77 个 Rust 测试，其中 9 个是起真实 SSH 服务端的集成测试；会话那条覆盖 PTY 尺寸、中文回显、resize、清理。

## M3 已落地的东西

- **独立建连**：SFTP 不复用终端那条 TCP，而是用同一份凭据另开一条 —— SSH 通道共享窗口，传大文件会把终端饿死。
- **浏览**：目录优先、再按名称排序；带 `size / mtime / mode / uid / gid`；面包屑与「上一级」。
- **上传下载**：128KB 分块流式，先写 `<目标>.part` 再改名，中途失败不会留半截文件。
- **进度**：`sftp:progress` 事件 150ms 节流；前端显示百分比、速率，可中途取消（取消检查在块边界）。
- **文件操作**：新建目录、重命名、删除（目录需显式 `recursive`，前端二次确认）；递归删除带 64 层深度上限。
- **本地路径校验**：下载目标必须是绝对路径、父目录存在、且不在系统目录下 —— 不信任前端传来的任意路径。
- **测试**：96 个 Rust 测试，其中 9 个是起**真实 SFTP 服务端**（`russh-sftp` 的 server 实现 + 真实文件系统）的集成测试，
  覆盖上传跨块字节一致、下载回放、取消后无残留、失败必发终态事件、递归删除、非空目录拒删。

## M6 已落地的东西（命令片段 · 只读库）

- **22 条内置命令，分 6 组**：
  - **只读诊断 15 条** —— 系统（`uname -a` / `/etc/os-release` / `uptime` / 运行时版本检查）、
    资源（`free -h` / `df -h` / `df -i` / `du -sh ./*`）、进程（CPU/内存 Top、进程树）、
    网络（`ss -tulnp` / `ss -s` / `lsof -i`）、日志（`journalctl`）
  - **环境准备 14 条**（有副作用）—— 按组件排列，同一组件的安装/卸载相邻：
    包索引、Git、Python3+pip+venv、Node.js 20、Docker、Nginx、常用工具、编译工具链
  - **环境探测**：打开侧栏时自动跑一次 `command -v` + `--version`，
    在「环境准备」组顶部显示 chips —— 绿点=已装（带版本），虚线圈=未装。
    这样才知道**该点哪条安装命令**，而不是点开一堆命令自己找
- **终端右侧侧栏**：`Ctrl+Shift+P` 开关，搜索覆盖名称/命令/描述/分组，分组可折叠，点一下插入光标处。
- **插入不执行**（A-12）：只 `session_write` 文本，绝不追加 `\r`，是否执行由用户按回车决定。
- **风险由 Rust 现算**（SEC-SN-10）：`classify_risk` 与展示用同一个函数，标注与判定不可能不一致。
  测试按分组建模，两个方向都守：**诊断类必须只读**（误判危险会造成警惕疲劳），
  **环境准备类必须非只读**（漏判会让人以为只是看看，按回车就改了系统）。
- **装环境的三档**：`apt-get install` 之类 → 危险；`curl … | sh`、`apt-get upgrade` → 高危
  （前者等于把执行权交给网络对面，后者可能换内核）。高危命令插入后会明确提示「还没运行」。
- **卸载一律 `--purge`**，description 里写清会带走什么（Nginx 的站点配置、Docker 的镜像数据、
  npm 的全局包）。**唯独不提供 Python 3 的卸载** —— 它是系统组件，purge 会连带拆掉依赖它的系统包。
- **内置库不进数据库**：内置命令是程序的一部分（同快捷键表），升级自然生效，不需要 seed/合并。
  `snippets` 表留给 M7 的用户自定义片段。

## 自定义片段（部署命令管理）

侧栏「+ 新建」可以把自己项目的更新流程存成一条片段，**分组名当项目名用**：

| 名称 | 分组 | 命令 |
|---|---|---|
| 部署 web | web 项目 | `cd /opt/web && git pull && npm ci && npm run build && systemctl reload nginx` |
| 部署 api | api 服务 | `cd /opt/api && git pull && mvn package -q && systemctl restart api` |

自定义片段排在列表最前面，带「改 / 删」；内置的不可编辑。

**一条硬约束：命令不能换行。** 终端把换行当回车，多行粘贴会**逐行执行**——
点一下就把部署跑了一半，而且中途某行失败后面的还会继续跑。所以多步必须用 `&&` 连成一行。
UI 输入时会实时显示这条命令的风险等级（只读 / 会改动系统 / 高危），判定在 Rust 侧。

## 结构化诊断面板（M4 先做的一部分）

命令片段的输出是给人读的，不是给机器读的。**6 条**（磁盘 / inode / 内存 / 进程 / 端口 / 进程树）除了「插入终端」还能点「表格」：

- **一句话结论**：「使用率最高的是 /data，85%」
- **告警**：磁盘 ≥80%、可用内存 <15%、单进程 CPU ≥80%、零监听端口时才出现
- **表格**：列是中文表头，按关注点排序（磁盘按使用率降序、端口把 LISTEN 排前面）
- **原始输出**：随时可展开对照，解析失败时只降级不过滤（不编造数据）

技术实现：在同一会话的 SSH 连接上**新开一个 exec channel** 执行命令，不碰终端的 PTY。
有一条测试专门验证「诊断不干扰终端」——诊断输出不会混进终端画面，诊断之后终端仍可正常输入。

进程树**不解析 `ps --forest` 的缩进**：那段缩进和 ps 的列宽填充混在同一列，切不干净。
改成拿全量 `pid ppid comm` 在 Rust 里按 ppid 重建：

```text
systemd
├─ systemd-journald
├─ sshd
│  ├─ bash
│  └─ sh
└─ crond
```

用 `├─` / `└─` / `│` **连线而不是空格缩进** —— 空白字符是否被折叠取决于渲染细节
（`white-space` 取值、是否 nbsp），制表符号是可见字符，一定画得出来。前端那一列再加 `white-space: pre` 保住间距。

## 设置页（M4）

- **字号** 10–24px，**主题** 深色/浅色，**回滚行数** 1000–50000
- 存在 `settings` 表（KV），与连接配置分离
- 改完**立刻生效**：拖动滑块时终端实时跟着变，停手 400ms 后才落库
- 越界值静默 clamp（拖到边界外、或手改库），不弹错误
- 终端**就地更新**而不重建，切换字号/主题不会丢回滚缓冲

## 界面：浅色 + 赛博朋克点缀

配色全在 `:root` 的 CSS 变量里，改一处即全局生效：

| 用途 | 颜色 |
|---|---|
| 底 / 面板 / 凹陷 | `#f5f8fb` / `#e9eff6` / `#edf2f8`（冷白，不是纯白——纯白对比过强） |
| 主色（青绿） | `#0d9488` |
| 文本 三级 | `#0d1b2a` / `#47607a` / `#7b8ca1` |
| 危险（洋红） | `#d81b60` |

**浅底上的"发光"不靠亮色叠加**（那样只会糊成一片），改用三样东西做霓虹感：
侧栏右缘 1px 青色分隔线、导航激活项左侧 2px 霓虹条、顶栏底边一条中间亮两端渐隐的光线。
滚动条与输入框聚焦描边也统一走青色。

**刻意没做**背景网格和扫描线 —— 长时间盯终端会累，而且多层不透明面板会把它整个盖住。

终端本身的深/浅是**独立设置**（设置 → 主题），默认深色（终端惯例）。

## 界面结构：终端与文件合并为一个会话页

以前「终端」和「文件」是两个页面，同一台服务器要**选两次、连两次**（前端意义上的），
用户反馈「不直观」。现在合并成一个会话页：

```
┌───────────────────────────────────────────────────────┐
│ 标签1  标签2  + 新连接   [片段] [文件]                 │
├──────────────────────────────┬────────────────────────┤
│                              │ 文件  root@1.2.3.4:22 ×│
│  终端（当前标签）             │ ├─ 上一级 刷新 新建 …  │
│                              │ /var/www               │
│                              │ 📁 html   📄 nginx.conf │
│                              │                        │
└──────────────────────────────┴────────────────────────┘
```

- **文件面板跟着当前标签走**：切标签就切服务器，不用再选一次
- 右边可拖动分隔条调宽度（280–760px，双击复位，宽度记在 localStorage）
- 每个标签第一次打开面板时才建 SFTP 连接，切走保留（不重连、目录和传输进度都在）
- 关标签 / 关面板都会关掉对应的 SFTP 会话，不留孤儿连接
- 文件面板与命令片段互斥（两个都开，终端只剩一条缝）
- 左侧导航少了「文件」一项；老版本存档里的 `activePage=files` 会自动落到终端页

**底层仍然是两条 SSH 连接**（终端一条、SFTP 一条）——这是 M3 定的：
共用一条连接时，传大文件会和终端抢同一条 TCP，输命令会卡。
合并的只是界面，不是连接。用户感受到的"一次连接"，是只选一次服务器。

`files` 指令与 `sftp:*` 命令没变，改动集中在 `src/pages/FilePanel.tsx`（原 FilesPage）
与 `TerminalPage` 的右侧面板。

> `sftp:progress` 事件现在带 `sessionId`：多个面板同时监听同一个广播事件，
> 没有它 A 标签的传输会显示在 B 标签的面板里。有测试盯着这个字段名。

## 数据备份（防"升级后配置全丢"）

起因是一次真实事故：0.1.1 的安装包升级时，NSIS 卸载器执行了「删除应用数据」，
`%APPDATA%\com.termharbor.app` 和 `%LOCALAPPDATA%\com.termharbor.app` 整个被删，
连接配置、known_hosts、日志一起没了。

那条删除动作只针对**以 bundle id 命名**的目录，所以快照放在同级的**产品名**目录：

```
%APPDATA%\com.termharbor.app\termharbor.db     ← 主库（会被卸载器删）
%APPDATA%\TermHarbor\backups\termharbor-*.db   ← 快照（不在删除路径上）
```

- 每次启动自动留一份（跑迁移**之前**，先 `wal_checkpoint(TRUNCATE)` 再拷，否则会漏掉 WAL 里最新的写入）
- 保留最近 5 份，按文件名里的时间戳排序（不用 mtime，同步工具会改它）
- 设置页「数据备份」：看列表（时间 / 多少台服务器 / 大小）、立即备份、恢复
- **恢复不立刻覆盖**：落 `restore.pending`，下次启动 `Db::open` 之前换上去（库连接还开着时换文件会写坏）；
  换之前先自动快照当前库，点错了还能退回来
- 启动自检：当前库 0 条连接但快照里有数据时，日志打一条 warn —— 下次再有人问"数据没了"，日志里直接有答案

## 安装包：升级默认「直接安装」

**`src-tauri/nsis/installer.nsi` 是自定义的 NSIS 模板**（派生自 tauri-cli v2.12.4 官方模板），
只改了一处：检测到旧版本时，默认选中「**不卸载，直接安装**」。

原因：官方模板在「已安装旧版本」那一页**默认选中第一个单选项，也就是「安装前卸载旧版本」**。
用户一路点「下一步」，就会静默拉起旧卸载器；而卸载器的确认页带一个「删除应用数据」复选框，
勾上就会 `RmDir /r` 掉 `%APPDATA%\com.termharbor.app` —— 0.1.1 升级时真实发生过一次，
连接配置全丢。

改后：
- **升级/降级** → 默认「不卸载，直接安装」（覆盖安装不碰用户数据）
- **同版本** → 默认「添加/重新安装」（与官方一致）
- 焦点跟着默认项走（否则回车仍然落在「卸载」上）
- 降级且不允许降级时，回落到第一个选项（第二个是禁用的，不能把焦点放上去）

那一页仍然保留 —— 装坏了需要先卸载重装时还有退路，只是不再默认帮用户卸载。

维护：Tauri 升级后把官方模板重新拷过来，再套用文件里标了「本地改动」的那一处。
官方源 `crates/tauri-bundler/src/bundle/windows/nsis/installer.nsi`。

## 打包

```bash
pnpm tauri build --bundles nsis   # Windows 安装包，约 3 分钟（增量）
```

产物：`src-tauri/target/release/bundle/nsis/TermHarbor_0.1.0_x64-setup.exe`，**2.89 MiB**。

体积是靠 release profile 压下来的：`lto = "fat"` + `codegen-units = 1` + `opt-level = "s"` + `strip = true`。
首次打包会自动下载 NSIS 到 `%LOCALAPPDATA%\tauri\NSIS`。

没装 WebView2 的机器不用管：`webviewInstallMode` 设的是 `downloadBootstrapper`，安装器会自己拉。

> 注意：Windows 未签名，SmartScreen 会提示"未知发布者"，需要点「更多信息 → 仍要运行」。
> 安装界面为简体中文（`bundle.windows.nsis.languages = ["SimpChinese"]`）。

## 下一步

M3、M6、诊断面板、设置页、关于页已完成，Windows 安装包能出。剩下：

- **M4 监控、设置与打磨**：`/proc` 只读采集、设置页（字体/字号/主题/回滚行数）、关于页、错误文案全覆盖
- **M7 剩余**：变量占位符 `{port}`、执行前预览与二次确认
- **M5 剩余**：macOS dmg / Linux AppImage+deb、代码签名、三平台回归清单

### Spike 已完成（见 [docs/spike-notes.md](docs/spike-notes.md)）

- **S1 算法兼容**：russh 默认连不上只有 SHA-1 的老服务器，放宽 `Preferred` 后通过 —— 不需要换 `ssh2`。
- **S2 吞吐与背压**：82.4 MB/s，慢消费者下压到 1.0 MB/s 且内存平稳；`String` 比 `Vec<u8>` 省 3.5× IPC 体积。
- **S3 中文输入法**：取证工具已就绪（应用内「输入法测试」页，仅 `pnpm tauri:dev` 可见），待三平台人工执行。

```bash
cd spikes
cargo test                      # 13 个单测
cargo run --bin s1_compat       # 算法兼容性矩阵
cargo run --bin s2_throughput   # 吞吐与背压实测
```
