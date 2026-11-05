# Bead — 工单 01 实施现状快照

- **记录时间**：2026-11-05 23:01 (GMT+8)；深夜更新：冒烟与快照验证完成
- **状态**：**冒烟全部通过**，等待用户确认后分条提交；未提交、未推送
- **分支**：`dev`（本地新建，源自 `main` @ `b1fbc14`；`ls-remote` 确认远端无 dev，故新建本地分支）
- **Stage 3 门**：用户已确认工单 01 的切片与阻塞边、级别 L3、CI 只做测试 job、需要 demo 探针、依赖清单
- **本文件用途**：记录实施进度与验证输出。`STATE.md` 只记决策/假设/基线，不记进度；进度细节以本文件为准，提交后仍以 `git log` 为准

## 一、已落盘的文件（全部未跟踪，`git status` 显示 10 项）

| 路径 | 作用 |
|---|---|
| `Cargo.toml` / `Cargo.lock` | workspace（`bead-core` + `src-tauri`，`exclude = ["demo"]`） |
| `.cargo/config.toml`、`demo/.cargo/config.toml` | crates.io 替换为 rsproxy sparse 镜像、关闭 HTTP multiplexing |
| `bead-core/{Cargo.toml,src/lib.rs,document.rs,sync.rs,hover.rs}` | 纯 Rust 业务核心，**零第三方依赖**；24 个行为断言测试 |
| `src-tauri/{Cargo.toml,build.rs,tauri.conf.json,capabilities/default.json,src/main.rs,icons/icon.ico}` | Tauri v2 壳层：单向数据流、9 个 intent command、fake translator、剪贴板只在 `ReadClipboardOnce` 读一次 |
| `web/{index.html,styles.css,render.js,app.js,render.test.js}` | WebView 静态前端；`renderBoard` 为纯字符串渲染函数，可被 vitest 与视觉快照共用 |
| `package.json` / `package-lock.json` | 唯一 devDependency `vitest ^2.1.9`，`npm test` = `vitest run` |
| `.github/workflows/test.yml` | **仅** `cargo test --manifest-path bead-core/Cargo.toml` + `npm test` 两个 job；无 tag 触发、无打包/发版/签名 |
| `demo/{Cargo.toml,build.rs,tauri.conf.json,capabilities/default.json,src/main.rs,ui/index.html,icons/icon.ico}` | 独立平台能力探针，不依赖主程序、不自动清理（`demo/target/` 未纳入检索） |

**与先前依赖清单的偏差**：未引入 `vite` / `typescript` / `@tauri-apps/api`。前端改为 Tauri 直接托管 `web/` 静态目录，invoke 走 `withGlobalTauri` 注入的 `window.__TAURI__.core.invoke`。理由：避免 npm 构建链成为第二个网络阻塞点，单向数据流与视觉快照复用不变。这是减少依赖，不是新增。

## 二、验证结果（真实输出）

1. `cargo test --offline --manifest-path bead-core/Cargo.toml`
   → `24 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out`，doc-tests `0 passed`。
   覆盖：语言池 2/5 边界与双向、单次计划携带全部目标语言、fake 译文填充另一卡、过期 generation 整包丢弃、权威源不被覆盖、单卡失败保留旧文本并标错、剪贴板哈希去重与刷新强制、五态转移（含 200ms 防抖、800ms grace）、锁定/编辑态 hover 不读剪贴板、无 hover 的 tick 不读剪贴板。
2. `npm test`（vitest 2.1.9）
   → `Test Files 1 passed (1)` / `Tests 6 passed (6)`，耗时 564ms。
   首次运行有 1 条断言写错（`class="card` 正则误匹配子元素 class，得到 11 而非 2），已改为 `<article class="card` 并复跑通过。
3. `demo/` 探针（Windows / Tauri v2.12.5 / WebView2 156，第三次运行全绿）：
   ```
   BEAD_PROBE_JSON={"window_position_set_ok":true,"requested_position":[1180,260],"observed_position":[1180,260],"always_on_top_call_ok":true,"clipboard_read_ok":true,"clipboard_read_error":null,"clipboard_write_ok":true,"clipboard_readback_error":null,"clipboard_write_readback_ok":true,"clipboard_restored":true,"clipboard_write_probe":"full_with_restore","clipboard_content_hash":3476900567878811119,"hover_debounce_ms":200,"hover_observed_ok":true,"clipboard_listener_registered":false}
   ```
   结论：`set_position` 读回与请求一致、置顶调用成功、剪贴板读/写/读回/恢复原状全部成功、hover 200ms 防抖生效、未注册任何监听器。探针运行后自行退出，无残留进程/窗口/托盘项。
4. 视觉快照：`C:/Users/Kris/AppData/Local/Temp/bead-ui-01.png`（Chrome headless，420×300，复用同一套 `render.js` + `styles.css`）。**裁切疑点已定性（见下 7）**。
5. ~~`cargo check -p bead` 未确认~~ → **已确认**：`cargo check -p bead` 通过（14.5s），`cargo build -p bead` 通过（53s）。
6. **真机冒烟（深夜，CDP + 真鼠标，`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9222`）全链路通过**：
   - 启动 → 胶囊被动态 ✓；真鼠标 hover → `hover_pending` → 200ms 防抖 → `expanded` ✓（CDP 逐步实测）
   - 读取剪贴板一次 → zh 卡原文（权威源）、en 卡 `[en] 原文` ✓（证据 `smoke-4-expanded-filled.png`）
   - 点击 en 卡「复制」→ 剪贴板变为 `[en] Bead smoke test 2026` ✓
   - ESC → 立即回 `capsule` 且文档清空（Esc → Idle + ClearDocument）✓
   - 现场已清理：进程结束、原剪贴板 2 秒后复核仍在、光标还原、无托盘/自启残留
   - 证据目录：`%TEMP%\bead-measure\`（`smoke-*.png`、`atomic.ps1`、`cdp*.mjs`）
7. **快照裁切定性：截图工件，CSS 无缺陷**。headless Chrome 在 Windows 有最小窗口宽 500：`--window-size=420,300` 时实际视口 500×205，布局按 500 宽排、截图按 420 宽保存 → 右侧 80px 被裁。固定 420×300 几何实测：board 8..412、重读右缘 399、右卡复制右缘 389 全部在界内，`scroll==client` 无溢出。忠实快照：`%TEMP%\bead-ui-01-faithful.png`（420×300，真实组件+样式，经 IAB 视口截图 + GDI+ 裁切产出）。
8. 隐私基线静态检查：全仓 grep 无剪贴板监听注册、CI workflow 无打包/发版/签名步骤 ✓

## 三、未完成 / 未验证（如实标注）

- ~~主程序未确认可编译~~ → 已通过（见二.5）；~~手工冒烟~~ → 已完成（见二.6）；~~快照裁切~~ → 已定性（见二.7）；~~隐私基线 grep~~ → 已跑（见二.8）
- `git status` 全部为未跟踪文件，**没有提交、没有推送**；`Cargo.lock` / `package-lock.json` 也未入库
- 无用户 API key，LLM provider 端到端只能测请求构造与解析（工单 04 范围，当前不涉及）
- macOS / Linux / Arch 仅能由 CI 保证可构建，运行时手感与权限流程未验证
- 首次冒烟失败的两个干扰源已查明并排除（用户实时鼠标操作、PS 进程退出导致剪贴板 owner 清空），不影响结论；真机 UI 手感仍以用户日常使用为准

## 四、本轮遇到并已解决的问题

1. **crates.io index 卡住 6 分钟**（`cargo run` 停在 `Updating crates.io index`，且占住 package cache 锁导致 `cargo test` 被 SIGTERM）→ 终止进程，改用 rsproxy sparse 镜像 + `CARGO_HTTP_TIMEOUT=30` / `CARGO_HTTP_MULTIPLEXING=false`，Tauri 依赖全部下载并编译完成。curl 直连两个源均 200，问题在 cargo 的默认 index 拉取路径。
2. **tauri-build 缺 `icons/icon.ico`**（Windows Resource 生成失败，exit 101）→ 用标准库生成 32×32 PNG-in-ICO，demo 与主程序各放一份，190 字节。
3. **探针首次 `clipboard_read_ok=false`** → 错误信息为 `The clipboard contents were not available in the requested format or the clipboard is empty.`，是当时系统剪贴板为空/非文本，属预期空态而非权限问题；拆分写入与读回字段后第三次运行全绿。
4. **npm install 耗时 4m17s**（44 packages）→ 已完成，生成 `package-lock.json`。
5. **【已知异常，留工单 04】hover 后首次 `read_text` 偶发失败**：报「剪贴板为空或非文本」，但内容实际存在（读失败后 PowerShell 复查文本仍在），数秒后 `refresh` 重读成功；再下一次 hover 首读又成功。嫌疑：微信等前台应用轮询/占用剪贴板导致 OpenClipboard 竞争，或插件首调时序。影响：悬停偶发需要第二次悬停。T01 不深挖——工单 04 ProviderChain 的超时/重试天然兜底；若日常使用复现再定位。
6. **观察（留给工单 03）**：`document().slots()` 迭代顺序是 en 在前、zh 在后（池声明 [zh, en]），真实应用里 en 卡在左列。T01 无碍（纯展示顺序），工单 03 做语言检测落卡与增删语言时需注意顺序稳定性。
7. **测试工具链坑位记录**：① PowerShell `Set-Clipboard` 后进程退出会触发剪贴板 owner 清空——验证时需保活持有进程或即时消费；② headless Chrome 最小窗口宽 500；③ ZCode IAB 的 `screenshot({clip})` 在本机会产生 2×2 平铺坏图，须整视口截图后 GDI+ 裁切；④ `Set-Clipboard` 的内容第一次冒烟时被微信前台活动干扰过一次（表现为剪贴板变空），重设后恢复。

## 五、恢复时从这里接（已全部完成，仅剩提交）

1. ~~`cargo check -p bead`~~ ✓ 无编译错误
2. ~~最小冒烟~~ ✓ 全链路通过，现场已清理（见二.6）
3. ~~视觉快照比对~~ ✓ 定性为截图工件（见二.7）
4. ~~静态检查~~ ✓（见二.8）
5. **剩：改动点汇总待用户确认 → 按功能点分条提交（附 `Verified:` trailer），不推送**

## 六、待合并的文档冲突（按规范第 11 条，功能点完成后再修）

- `STATE.md` D18 与 `docs/naming.md` 标识清单写 `C:/Users/HP/WorkSpace/Bead`，实际工作区为 `D:/WorkSpace/Bead`。
- `STATE.md` / `Agents.md` 写 Rust 1.96.0，实测 `rustc 1.99.0`；Node 22.22.2 一致。
- `STATE.md` 基线写「无 Cargo.toml、0 tests」，那是 Stage 0 的有效历史事实，**不修改**；当前状态以本文件为准。

## 七、会话耗时

- 22:41 → 23:01，约 20 分钟。耗时点：crates.io index 卡住 6 分钟（含一次后台任务终止）、npm install 4m17s、Tauri 首次全量编译约 3 分钟、探针迭代 3 次。
- 优化建议：把 `.cargo/config.toml` 镜像配置与 `icons/icon.ico` 前置为第一步（可省约 8 分钟）；npm 也配置国内 registry；demo 与主程序可通过 `CARGO_TARGET_DIR` 共享一份编译产物，避免两次全量编译。
