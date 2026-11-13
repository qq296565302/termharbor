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
│   └── pages/                # 各页面（会话页、连接、设置、关于；自检页仅 dev）
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
- **通信层**：Command 请求响应 + Channel 高频推送两条链路。
  心跳验证页只在开发构建里保留（`import.meta.env.DEV`），生产包不含这段代码。
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

插入只写文本、不回车（高危命令会提示「它还没运行」）。那条提示的生命周期是：
**按回车就收**（此时命令真的发出去了）、换标签就收、20 秒兜底自动收，也可以点右边的 × 手动收起。

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

- **界面外观** 跟随系统 / 浅色 / 深色（管整个 UI）
- **终端配色** 深色 / 浅色（只管终端区域，和上面互不影响）
- **字号** 10–24px，**回滚行数** 1000–50000
- 存在 `settings` 表（KV），与连接配置分离
- 改完**立刻生效**：拖动滑块时终端实时跟着变，停手 400ms 后才落库
- 越界值静默 clamp（拖到边界外、或手改库），不弹错误
- 终端**就地更新**而不重建，切换字号/配色不会丢回滚缓冲

## 界面：浅色 + 一点点青绿

配色全在 `:root` 的 CSS 变量里，改一处即全局生效（0.2.0 重做过一版）：

| 用途 | 浅色 | 暗色 |
|---|---|---|
| 页面底 / 面板 / 次级 / 凹陷 | `#f6f7f9` / `#ffffff` / `#f9fafb` / `#f2f4f7` | `#0f1115` / `#16191f` / `#1c2027` / `#0b0d11` |
| 主色（青绿，只给主操作与选中态） | `#0d9488` | `#2dd4bf` |
| 文本 三级 | `#171a20` / `#5a6472` / `#8d96a5` | `#e8ebf0` / `#9aa4b2` / `#6b7480` |
| 语义色 | 成功 `#12805c` · 警告 `#b54708` · 危险 `#c4321b` | `#3ddc97` / `#f5a524` / `#f87171` |

层次靠**底色差 + 中性边框 + 极轻阴影**三件套，不用彩色边框和渐变划分层级
（0.1.x 用 `rgba(13,148,136,.16)` 当边框，整片发灰绿）。

**"点缀"只在两处**：顶栏底边一条中间亮两端渐隐的光线、滚动条与输入框聚焦的青色。
侧栏原来是「左霓虹条 + 向右渐隐渐变 + 投影」三层叠加，2026-10-31 去掉了
（用户原话：一股子 AI 的味道）—— 现在导航项只是平铺一行，选中给一条 2px 竖线 + 极淡底。
终端标签同理：从「圆角药丸 + 每格边框 + 底部发光线」改成编辑器式平铺，
标签之间只留 1px 细分隔线，激活只差底色 + 一条 2px 实线，关闭按钮平时隐藏。

**刻意没做**背景网格和扫描线 —— 长时间盯终端会累，而且多层不透明面板会把它整个盖住。

终端本身的配色是**独立设置**（设置 → 终端配色），默认深色（终端惯例）；
界面明暗另有一项「界面外观」，三态：跟随系统 / 浅色 / 深色。

实现上**只有一份暗色令牌**：`styles.css` 里 `:root[data-theme="dark"]`，
不用媒体查询。选哪种由 JS 决定（`src/lib/theme.ts`）—— 永远给 `<html>` 挂
`data-theme="light|dark"`，「跟随系统」时用 `matchMedia` 解析并监听系统变化。
写成「媒体查询 + 属性选择器」两份迟早会漂移，这个项目已经在这种事上吃过亏。
首帧闪白用 `index.html` 里一段内联脚本读缓存值兜住，设置从库里读回来后校正。

> 改完这两处想先看效果又不想装包：开 `docs/ui-preview.html`（静态预览，
> CSS 是从 `styles.css` 抄的片段，不参与构建）。

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
- 每个标签第一次打开面板时才建 SFTP 连接，**收起面板（×）只是隐藏，不关连接** ——
  再点回来是秒开的，不会重连一遍；真要断开点面板里的「断开」
- 关标签才会关掉对应的 SFTP 会话，不留孤儿连接
- 文件面板与命令片段互斥（两个都开，终端只剩一条缝）
- 左侧导航少了「文件」一项；老版本存档里的 `activePage=files` 会自动落到终端页

**底层仍然是两条 SSH 连接**（终端一条、SFTP 一条）——这是 M3 定的：
共用一条连接时，传大文件会和终端抢同一条 TCP，输命令会卡。
合并的只是界面，不是连接。用户感受到的"一次连接"，是只选一次服务器。

`files` 指令与 `sftp:*` 命令没变，改动集中在 `src/pages/FilePanel.tsx`（原 FilesPage）
与 `TerminalPage` 的右侧面板。

> `sftp:progress` 事件现在带 `sessionId`：多个面板同时监听同一个广播事件，
> 没有它 A 标签的传输会显示在 B 标签的面板里。有测试盯着这个字段名。

### 文件面板打开在哪儿

- **默认落在 `/`**（根目录），不是 `$HOME`。要在 `/etc`、`/var` 之间来回翻时，
  从家目录往上爬很烦；工具栏的 `~` 一键回家目录。
- 根目录列不出来的服务器（Windows OpenSSH 之类的实现）自动退回家目录，不留空面板。
- **在终端里敲 `cd`，面板跟着切过去**（见下）。

### 终端 `cd` 时面板跟随

SSH 协议里拿不到对端 shell 的 pid，也就读不到它的 cwd；OSC 7 又要用户自己往
`.bashrc`/`.zshrc` 里加东西。所以这里退一步，**只看用户敲下来的那一行**：

```
用户输入 → feedLine 攒行 → parseCd 认出 cd → resolveCdPath 算出绝对路径
        → sftp_list 验证能列出来 → 才切换面板
```

- 支持：`cd /tmp`、`cd ..`、`cd ../x`、`cd ~/logs`、`cd`、`cd -`、带引号的路径、
  `cd /tmp && ls` 这种只看第一段
- **认不出就不动**（宁可不同步也不乱跳）：`cd` 别名、`pushd`、子 shell、
  `cd $(pwd)/x` 这类带变量或命令替换的、`~user`，以及**用方向键翻过历史之后**
  —— 那时本地记录的行和真实命令行已经不一致，直接放弃
- 相对路径按 **shell 的 cwd** 算，不是按面板正在看的目录 —— 你用面板逛到别处再敲
  `cd sub`，走的是 shell 那一边
- 面板还没打开过的标签不跟踪（没建 SFTP 会话，也就没法验证），打开时按默认 `/` 落

解析与归一化在 `src/lib/shell.ts`，纯函数、22 个单测（`shell.test.ts`）。

### 拖拽上传

把本地文件从资源管理器拖到文件面板上，出现虚框就松手 —— 传到**当前目录**。
用 Tauri 的 webview 级拖拽事件（`dragDropEnabled` 默认开），按落点是否在面板矩形内判断，
所以拖到终端区域不会误传。隐藏的标签页矩形宽高为 0，天然不接。

**目前只支持文件，不支持整个目录**（`ensure_upload_source` 会直接拒绝并给出提示）——
目录递归上传在 Backlog F-4 里。

**传完自动刷新**：上传完成（`state: done`）且目标目录就是当前目录时，列表自己重列一次，
不用手动点「刷新」。一次传十几个文件会合并成一次列表请求（250ms 内只刷最后一次）；
下载只动本地磁盘，远端没变，不刷。


## 负载看板

会话页工具栏的「负载」按钮，右侧滑出看板，**跟着当前标签的服务器**，每 3 秒自动刷新。

一次 exec 采全部指标（不是每个指标开一次通道）：

| 指标 | 来源 | 说明 |
|---|---|---|
| CPU 使用率 + 每核 | `/proc/stat` 两帧求差 | 60% 不是猜的，是两次采样之间「忙的 jiffies / 总 jiffies」 |
| CPU 曲线 | 最近 40 个采样点 | 自己画的 SVG polyline，不引图表库 |
| 内存 / Swap | `/proc/meminfo` | 用 `MemAvailable` 而非 `MemFree`（后者会把缓存算成"已用"，虚高）|
| 负载均值 1/5/15 | `/proc/loadavg` | 附一句**按核心数**解读：4 核上 load 4 才算满载 |
| 磁盘 | `df -hP -x tmpfs -x devtmpfs -x overlay -x squashfs` | 按使用率降序，70% 黄 / 90% 红 |
| 网络速率 | `/proc/net/dev` 两帧求差 | 每接口 ↓/↑，有流量的排前面 |
| 进程 Top5 | `ps -eo pid,comm,pcpu,pmem --sort=-pcpu` | PID / 名称 / CPU / 内存 |
| 运行时长 / 登录数 / TCP 已建立 / 内核 | `/proc/uptime`、`who`、`ss -tan`、`uname` | 底部一行 |

几个刻意的设计：

- **两次采样用 `/proc/uptime` 算间隔**，不假设 `sleep 0.5` 精确睡满 0.5 秒，
  也不依赖 `date +%s%N`（busybox 不保证支持）。
- **只读**：全部来自伪文件系统。有测试遍历命令字符串，出现 `rm`/`kill`/`systemctl`/写文件就红。
- **页面切走或窗口最小化就停止轮询**：会话页是常驻挂载的，不停会每 3 秒往生产服务器打一次 exec。
- **刷新失败保留旧数据**，只在顶部挂一条错误——网络抖一下就抹白比看到旧数据更糟。
- 解析不完整只在 `warning` 里说明（例如不是 Linux），不整块报错。

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

## 能连 Windows 服务器吗

能，但要分清「哪个功能能用」——我们依赖的是服务器上的 OpenSSH，不是 Linux：

| 功能 | Windows 服务器 | 说明 |
|---|---|---|
| 终端 | 可以 | 需启用「OpenSSH 服务器」可选功能（Win10 1809+ / Server 2019+）。默认进 `cmd.exe` |
| SFTP 文件面板 | 可以 | 路径是 `/C:/Users/...` 形式，拼接与跳转按 POSIX 斜杠处理，正好对得上 |
| 主机指纹 / 密码 / 私钥 | 可以 | 与 Linux 完全一致 |
| 命令片段（环境准备那组） | 不行 | 全是 apt-get，Windows 用不上 |
| 诊断面板 / 负载看板 | 不行 | 读的是 `/proc`、`df`、`ps`、`ss`，Windows 没有这些 |

两个已处理的 Windows 细节：

- **盘符根没有「上一级」**：`/C:` 再往上会去列 `/`，Windows 的 sftp-server 列不了 `/` 会报错。
  现在 `parent_path("/C:")` 直接返回 None，前端把按钮置灰。
- **看板会说明原因**：读不到 `/proc/stat` 时提示「这台机器可能不是 Linux，
  Windows 的 OpenSSH 只有终端和 SFTP」，而不是留一块空表格。

终端有个体验差异要先说清：`cmd.exe` 不认 `TERM`，颜色与光标控制基本没有，
xterm.js 只能显示纯文本流。要在 Windows 上舒服用，把 sshd 的默认 shell 改成 PowerShell 7。

## 连接配置导入导出（不含密码）

连接页搜索框下面两个按钮：**导出 / 导入**，文件是 JSON。

文件里只有「怎么连到哪台机器」：

```json
{
  "format": "termharbor.connections",
  "version": 1,
  "exportedAt": 1793409748229,
  "appVersion": "0.1.9",
  "connections": [
    {
      "name": "阿里云",
      "host": "1.2.3.4",
      "port": 22,
      "username": "root",
      "authType": "password",
      "keyPath": null,
      "legacyAlgorithms": false
    }
  ]
}
```

**刻意不写进去的四样东西**，每样都有理由：

| 不导出 | 为什么 |
|---|---|
| 密码 / 私钥口令 | 本地密码用机器绑定密钥加密，换机器解不开；要带密码必须用口令派生新密钥，风险面大一个量级，留给下一步 |
| 主机指纹 | 导入的是外来文件。带指纹的话，攻击者给的「配置包」可以 pin 他自己代理的指纹，你首次连接就"指纹匹配"，中间人永远发现不了 |
| 内部 id | 导入时重新生成，避免撞掉你已有的记录 |
| 使用时间等元数据 | 对搬家没用，只会误导 |

行为约定：

- **导入只新增，不覆盖**。判重按「主机 + 端口 + 用户名」，主机不区分大小写、用户名区分（Linux 上 `Root` 和 `root` 是两个账户）。已存在的跳过并在结果里报数量。
- 校验严格：格式标识、版本、条数上限（500）、字段长度、端口范围、认证方式取值。任何一条不合法整份拒绝，并指出「第 3 条：端口必须在 1-65535 之间」——半导入的状态比拒绝更难解释。
- 导出先写 `.json.tmp` 再改名，中途失败不留半截文件。
- 导入后**第一次连接仍要确认主机指纹**、密码要自己补填。这不是缺陷，是上面那条「不导出指纹」的直接结果。

## 安全清单（2026-10-30 审计后的状态）

已经守住、且有测试跟着的点：

| 面 | 状态 |
|---|---|
| 凭据进日志 | `ProbeAuth` / `ConnectionInput` **手写 Debug**，`{:?}` 只输出 `***`。派生 Debug 就等于埋雷——`tracing::error!(?request)` 是最自然的排查写法 |
| 远端误删 | `sftp_remove` 拒绝 `/`、`/C:`、`.`、`..`；递归删除只删链接本身（`lstat`），不跟进目标目录，深度上限 64 |
| 本地路径 | 下载目标先 `canonicalize` 父目录解开 `..` 与符号链接，再比对系统目录黑名单；上传源必须是绝对路径，且**不许上传应用自己的数据目录**（库里有加密凭据） |
| 密钥落盘 | Unix 上创建即 `0600`（不是先写后 chmod，那有几微秒的 0644 窗口） |
| 主机指纹 | 有记录时即便传了 `trust` 也阻断；改指纹只能走显式的「忘记」，不给"忽略并继续" |
| Web 侧 | CSP 已收紧（`default-src 'self'`，无 `unsafe-eval`）；权限集只有 `core:default` + 文件对话框；全仓无 `innerHTML` / `dangerouslySetInnerHTML` / `eval` |
| SQL | 全参数化，`format!` 只拼常量列名与固定 WHERE |
| 终端 | 只装 FitAddon：没有链接插件（不可点开外部 URL）、没有剪贴板插件（不响应 OSC 52） |
| 调试入口 | `ime_echo` 在正式构建里运行时拒绝 |

已知未做（都不在当前威胁模型里，但记在这里免得被当成"已经安全"）：

- **主密码（S-05）只有 KDF 没有入口**：`crypto/kdf.rs` 的 Argon2id 参数化实现是完整的
  （OWASP 2024 下限 19MiB/t=2/p=1，参数随记录落库以便逐条升级），
  `KeySource::MasterPassword` 这个枚举值也在，但**没有任何 provider 实现它**，
  也没有设置主密码的界面。当前默认走系统钥匙串（Windows 凭据管理器 / macOS Keychain /
  Linux Secret Service），密钥由操作系统用用户登录凭据保护。
- `devfile` 密钥是明文文件：只在 `TERMHARBOR_KEY_PROVIDER=devfile` 或构建时
  `--no-default-features` 关掉钥匙串时才会走到。日志有 warn，设置页/关于页会显示密钥来源。
- 没有自动更新（`tauri-plugin-updater` 未引入），所以也不存在更新包签名问题；
  代价是用户升级要手动装包（M5 待做）。引入时必须同时做签名密钥管理与清单校验。
- `trust_fingerprint` 仍由前端传入——这是 TOFU 首次信任的实现方式：
  后端只校验"你传的指纹 == 服务器实际给的指纹"，无法证明**人**看过这个指纹。
  配合 CSP、零注入点、指纹变更阻断，风险可接受；要更强需要主密码那套（见下）。

## 页面结构

左侧导航：**连接 · 终端 · 设置 · 关于**（开发构建额外有「自检」「输入法测试」）。

- 默认落点是**连接页**——服务器的家目录，也是新用户唯一该先看的地方。
- 原「自检」页降级为**开发构建专用排障工具**：它的运行信息（版本/schema/密钥来源/
  数据与日志目录）在「关于」页已经有了，Channel 心跳测试也被真正的终端输出取代，
  M0 的"已完成清单"对使用者没有意义。它还是有个用处——排查"终端没输出/连不上"时，
  点一下心跳推送能立刻分清是后端不推、还是前端不渲染。
  `import.meta.env.DEV` 守卫 + 动态 import，生产包里连这段代码都不存在
  （已验证：`dist/assets/*.js` 里搜不到页面专属文案）。

### 底部状态栏三个格子

| 显示 | 含义 |
|---|---|
| `会话 N` | 已打开的**终端标签数**，每个标签一条独立的 SSH 连接（不是"连了几台服务器"） |
| `密钥 …` | 数据库里密码/私钥口令用 AES-256-GCM 加密，这里说的是**那把数据密钥**存在哪。正常是「系统凭据管理器」（Windows 凭据管理器，不落盘）；开发构建才可能是「开发密钥文件」 |
| `IPC 就绪` | 启动时 `ping` 成功 —— 前端能调到 Rust 侧的 command。启动失败显示**错误码**（如 `DB_ERROR`）；运行期出现「根本没调通」的调用则变成 `IPC 异常：命令名`，悬停看原因、点一下清除，同一条命令后来又成功会自动恢复 |

三格都有 `title`，鼠标悬停能看到上面这些解释。注意 `IPC 就绪` **不轮询**。
它记的是「上一次没调通」而不是「现在通不通」：Rust 返回的业务错误（认证失败、
目录不存在）不算，只有 `invoke` 自己抛的错（命令不存在、参数对不上、IPC 通道出问题）
才算 —— 判断在 `lib/errors.ts::isTransportFailure`，有单测。

> 为什么需要这条：后台调用大量写成 `.catch(() => undefined)`，命令名拼错或参数漂移
> 会被吞得干干净净，症状只是"某个功能没反应"。留一条痕迹比事后翻日志强。


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

### 发版：打 tag 出包

`release.yml` 监听 `v*` 标签：三处版本号一致 + tag 等于版本号 → 构建 NSIS → 发布到 GitHub Releases
（自动生成 release notes）。手动 `workflow_dispatch` 只构建、传 artifact，不发 Release。

```bash
# 改完三处 version（package.json / tauri.conf.json / Cargo.toml）后
git commit -am "chore(release): 0.1.16"
git tag v0.1.16
git push && git push --tags
```

**刻意没做自动更新**：更新包没有代码签名时，静默安装会被 SmartScreen 拦，
"全自动更新"实际会变成"自动失败"。现在就一个「关于 → 检查更新」：查
`api.github.com/.../releases/latest` 比对版本号（按数字比，不是按字符串），
有新版就提示并打开下载页，不下载、不安装。等签名落地再上 `tauri-plugin-updater`
和 minisign（见开发计划 R-2 / R-3）。

### 图标：ICO 里小尺寸不能是 PNG

任务栏图标走的是「exe 内嵌图标资源」这条路（`tao` 只显式设了窗口小图标，
任务栏那个大图标靠 Windows 从 exe 里取），所以 exe 里的 ICO 必须**每个尺寸都真能解出来**：

- Windows 只保证 **256x256 用 PNG 压缩**，16/24/32/48/64/128 必须是 32bpp DIB；
- `cargo tauri icon app-icon.png` 生成的是**全 PNG** 的 ICO —— 资源管理器按大图标渲染还行，
  任务栏按 32x32 取图失败，就退回系统那张默认的「空白窗口」图（0.1.9 就是这个症状）。

改图标时按这个顺序来，别直接跑 `cargo tauri icon`：

```bash
python scripts/app-icon.py     # 重画 1024x1024 源图（可选）
cargo tauri icon app-icon.png  # 生成 png/icns/ico 全套
python scripts/make-ico.py     # 用 icons/icon.png 重打包 icon.ico（DIB + 256 PNG）
python scripts/make-ico.py --check   # 打印每个条目是 PNG 还是 DIB
```

顺带把 `bundle.icon` 里的 `icons/icon.ico` 保持在列表内，`tauri-build` 就是拿它写进 exe 资源的。


## MCP（让智能体直接操作服务器）

目标：让 AI 客户端（Claude Desktop / WorkBuddy 等）**通过本客户端**干活，
而不是把 SSH 密码或私钥交给它。凭据始终留在 TermHarbor 里。

**不用额外装东西**：装好的主程序自带 `--mcp` 子命令，走 stdio + JSON-RPC 2.0（MCP 协议）。
这个模式**完全不初始化 Tauri**（不开窗口、不加载 WebView2），但复用 `storage` / `crypto` /
`ssh` / `sftp` 四层 —— 它们本来就不依赖 Tauri。计划与红线见
[docs/开发计划.md](docs/开发计划.md) §17。

宿主配置（路径就是安装目录里那个 exe）：

```json
{
  "mcpServers": {
    "termharbor": {
      "command": "%LOCALAPPDATA%\\TermHarbor\\termharbor.exe",
      "args": ["--mcp"]
    }
  }
}
```

> 有些宿主不展开 `%LOCALAPPDATA%`，写全路径即可。排障时也可以用独立的
> `termharbor-mcp.exe`（`cargo build --bin termharbor-mcp`），它和 `--mcp` 跑的是同一份循环。

**工具清单**（每次调用开一条临时连接、做完即断，复用 GUI 的 `prepare_probe`，
所以凭据解密、主机指纹校验、负载算法都是同一套）：

| 工具 | 作用 |
|---|---|
| `list_connections` | 列出对 MCP 开放的连接（不解密凭据，带 `usable` 表示此刻能否使用） |
| `sftp_list` | 列目录 |
| `sftp_read` | 读远端文本文件（默认 256KB 上限） |
| `sftp_write` | 覆盖写远端文本文件（上限 1MB） |
| `sftp_upload` / `sftp_download` | 本机与服务器互传文件，等传输完成才返回 |
| `sftp_mkdir` / `sftp_rename` | 建目录、改名或移动 |
| `load_snapshot` | 负载快照（CPU / 内存 / 磁盘 / 网络 / 进程数） |
| `exec` | 执行单条命令 |

密钥**用到才取**：只列连接不会去碰系统凭据管理器。

> **升级后先做一步**：连接默认全关。打开应用 → 设置 → MCP 接入，勾上要交给智能体的机器
> （或点「全部开放」）。不勾的话智能体看不到任何服务器 —— 这一条不放松，
> 因为升级不能把用户已有的服务器静默交出去。

> **0.1.28 及更早的文件位置**：那两版把 `mcp.json` / `audit.jsonl` 直接写在了
> `%APPDATA%` 根目录（少拼了一层）。0.1.29 起收进 `%APPDATA%\TermHarbor\`，
> 第一次运行时自动搬过去；搬不动就继续用旧位置，不会让你的白名单凭空消失。

### 只在客户端连着的时候能用

**关掉连接就等于收回使用权。** 智能体连服务器走的是它自己的一条 SSH 连接，
所以「关掉终端标签」在 0.2.1 之前拦不住它。现在多了一道**在线会话校验**：

- 客户端在跑的时候，每 5 秒往 `%APPDATA%\TermHarbor\session-lease.json` 写一次
  「当前打开着的连接」（终端会话 + 文件面板都算）。连接建立/断开会**立刻**再写一次，
  所以关掉是当场生效，不用等下一次心跳。
- 智能体每次调工具都读一次这份文件。**这台机器不在名单里**、或者**心跳超过 15 秒没更新**，
  一律拒绝，原因码 `no_live_session` —— 文案会明说「不是权限没开，是没连着」。
- 正常退出应用会删掉这个文件（立即失效）；进程被强杀则由 15 秒 TTL 兜底。

`list_connections` 照旧能列出来，只是带 `usable: false`。这样智能体知道
「有这台机器，但你现在用不了」，而不是把它当成不存在去瞎猜。

> 真要无人值守（客户端不运行也让智能体干活），把「MCP 接入」里的
> **只允许使用当前已连接的服务器** 取消勾选，对应 `mcp.json` 的 `requireLiveSession: false`。
> 默认是勾上的。

### 权限模型：勾选即授权，细节可收窄

心智模型很简单：**你给出去的是一个 SSH 连接，那本来就是整台机器的使用权**。
所以勾选一台机器 = 读文件、传文件、执行命令全部放开；默认不弹一堆复选框让人猜。

权限全部由**本地配置**决定，不经过智能体的自述。设置页「MCP 接入」是主界面，
改动自动写回 `%APPDATA%\TermHarbor\mcp.json` 并**热加载**（不用重启宿主）；
高级用户也可以直接编辑这个 JSON。

需要收紧时，展开每台机器的「细化权限」，三处都是**留空 = 不限**、填了才变成限制：

- **限制可用工具** —— 勾选后只给勾上的
- **限制可访问路径** —— 只允许这些目录及其子目录（读写都算）
- **限制可执行命令** —— 只允许这些 token 前缀

每次调用依次过：连接是否启用 → 工具是否允许 → 参数（路径前缀 / 命令前缀）→ 执行限额。
任一不通过就拒绝，并往 `%APPDATA%\TermHarbor\audit.jsonl` 追加一条（成功和拒绝都记）。

设置页的「最近调用」把放行和拒绝一起列出来，每条都带**放行依据**：
这次用的是哪份租约（谁写的进程号、写完多久）。因为租约文件本身防不了伪造
（同一用户下任何进程都能改它），这一栏的意义是给你核对 —— 某条记录的租约不是当前
客户端进程写的，会被标黄。

> 这条也说明了它的极限：它能告诉你「那次是谁放的行」，不能阻止同用户下的进程伪造。
> 真要隔离只有换账号或换机器跑。

### 默认挡住的凭据类文件

「任意路径」不等于什么都能读。下面这些**默认挡**，命中会明确告诉你怎么办，
不是伪装成「文件不存在」：

| 类别 | 具体 |
|---|---|
| SSH 密钥 | `id_rsa` / `id_ecdsa` / `id_ed25519` / `id_dsa`、`authorized_keys` |
| 系统提权 | `/etc/shadow`、`/etc/gshadow`、`/etc/sudoers`、`/etc/sudoers.d/*`、`/etc/ssh/ssh_host_*_key` |
| 数据库 / 仓库 | `.pgpass`、`.my.cnf`、`.netrc`、`.git-credentials`、`.htpasswd` |
| 云 / 容器 / GPG | `.aws/credentials`、`.docker/config.json`、`.kube/config`、`.gnupg` |

判据只有一条：**读到它等于拿到更大的访问权**（私钥能登别的机器，`authorized_keys`
一写就是永久后门，`shadow` 是离线爆破的原料）。

刻意**不挡**的：`*.pem`、`*.key`、`.env`、`.bash_history`、`~/.ssh/config`、`~/.ssh/known_hosts`。
前几个歧义太大（`/usr/share/keyrings/*.key` 根本不是秘密），后两个是排查问题的必需品。
误伤一次，用户就会把这个功能当成麻烦 —— 那比漏挡更伤。

确实需要时，在「细化权限」里勾「允许智能体读写凭据类文件」。这是一台机器一个开关，
勾了就影响这台机器的全部路径。默认不勾，普通使用根本碰不到这个开关。

`exec` 的规则：默认允许任意**单条**命令；引号**外**的 `; &&` 管道 重定向 `$()` 反引号
一律拒 —— 这不是权限门槛，而是「一次调用只做一件事」的契约，需要多步就多调几次。

**参数里的特殊字符带引号就行**。判定按 shell 的引号语义走，不是无脑字符扫描：
单引号内全部是字面量，双引号内 `; & | > <` 也是字面量（但 `$()` 和反引号照样会执行，仍然拦）。
所以下面这些都是合法单条命令：

```bash
curl -sS -o /dev/null -w '%{http_code}' http://127.0.0.1:80/
curl 'http://127.0.0.1:80/?a=1&b=2'
grep -E 'error|fatal' /var/log/app.log
awk '{print $1}' /var/log/app.log
find /var/log -name '*.log' -exec ls -l {} \;
```

被拒时错误信息会指出**具体是哪个符号**，并说明这是写法问题、设置里没有对应开关。
超时默认 60 秒（硬上限 300），输出默认 64KB（硬上限 512KB），可在细化权限里调。

> `sh -c '多条命令'` 这种写法能通过 —— 引号内是字面量。它没有扩大能力：
> 默认档本来就允许任意单条命令，agent 想干多步也能分多次调。真正会让它失效的是
> **收窄白名单**（`sh` 不在清单里就进不来）。

审计里记的是**命令原文**（「允许这条」要用），但形如 `mysql -pXXX`、`--password XXX`
的密码会被替换成 `***`。脱敏后的命令不会出现在「允许这条」按钮上 —— 脱敏值写进白名单
永远匹配不上真实命令，那会变成「看着已允许、实际全被拒」。

可以这样自测：

```bash
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}' \
              '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
              '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_connections","arguments":{}}}' \
  | ./src-tauri/target/debug/termharbor.exe --mcp
```

调试提醒：**stdout 只走协议帧**，日志一律 stderr。混进去一行，宿主那边就是
"连接成功但一个工具都看不到"。

### 后台会话里的密钥：不需要额外操作

MCP 宿主常把子进程起在**非交互登录会话**（Windows 服务、计划任务）里，那种会话打不开
系统凭据管理器。为了不让用户为此做任何配置，应用在**每次正常启动时会写一份密钥镜像**：

- Windows 上用 DPAPI（`CryptProtectData`）把同一把数据密钥加密到
  `%APPDATA%\com.termharbor.app\key.bin`，绑到当前 Windows 用户主密钥，
  不依赖交互登录，别人拿到文件也解不开
- MCP 启动时优先读这份镜像，读不到才回退系统凭据管理器
- 镜像写在应用数据目录里，卸载时随目录一起删掉

所以用户的操作只有一步：**装完后正常打开一次 TermHarbor**（本来就要配连接）。
之后 MCP 宿主在桌面、后台、开机自启都能用，不需要环境变量、不需要导出密钥、
也不需要手动确认指纹以外的任何东西。

> 边界：宿主必须**以同一个 Windows 账号**运行。如果它跑在另一个账号下（比如
> `LOCAL SYSTEM` 服务），连用户目录都不是同一个，任何本地方案都无解 ——
> 那种情况需要让宿主改用同一账号，或者走 HTTP/SSE 传输（尚未实现）。

MCP 模式的日志只打 stderr（不落盘、绝不碰 stdout），宿主一般会把它收进自己的日志。
连服务器慢的时候看这几行就能定位阶段：

```
INFO  SSH 建连完成 host=… port=22 handshake_ms=133 total_ms=220 legacy=false
WARN  握手超时：TCP 连不上，或连上了但密钥交换没有回应  stage=tcp+kex secs=15
WARN  认证阶段失败或超时  stage=auth handshake_ms=… code=…
```

要更细的日志，在宿主配置里加 `"env": { "TERMHARBOR_LOG": "termharbor_lib=debug,info" }`。

## 下一步

M3、M6、诊断面板、设置页、关于页已完成，Windows 安装包能出。剩下：

- **M8 MCP**：只读三工具的实现（见 §17.7 第 2 步）
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
