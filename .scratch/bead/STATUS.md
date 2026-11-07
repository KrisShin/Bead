# Bead — 工单实施现状快照

- **记录时间**：2026-11-05 23:01 (GMT+8)；2026-11-06 凌晨 T01–T10 全部完成并推送（ef1e1d3）；2026-11-06 晚间同步至 `4ac886a` 并回填 MVP 后改动；**2026-11-07 真机验收后收尾：面板几何 / 增删动画、卡片拖拽换序、把手与关闭图标共 4 条功能提交 + 1 条文档提交**
- **状态**：MVP 已完成；MVP 后的交互重构与缺陷修复（D35–D42）已全部落库并推送 `dev`。当前基线四套件 **158 项全绿**；用户真机复验确认本轮拖拽、把手、关闭按钮与面板增删动画效果良好。**未打 tag**（发布时机归用户）。
- **当前基线（2026-11-07 本轮复跑）**：vitest 45、shell 20、bead-core 65、bead-providers 28，合计 **158**（上一轮 113）。
- **恢复指针**：下次会话从「十二 · 未验证 / 风险」开始——多显示器与混合 DPI 的 clamp（仅单测）、补间落窗时机的低性能设备表现、reduced-motion 真机、`release.yml` tag 首跑（归用户）、敏感过滤运行时复验（需剪贴板恢复）、macOS/Linux 运行时（仅 CI 保证可构建）。
- **说明**：下方「零～十一」是各自时间点的历史快照，当前状态以「十二」为准。

## 十、MVP 终验记录（2026-11-06 凌晨）

**测试基线**：bead-core 61（链/检测/敏感/语音/更新/状态机/协调器）、bead-providers 28（八家引擎请求构造/签名/解析/错误映射 + Google TTS + GitHub 拉取器）、shell settings 3（往返/兼容/损坏回落）、vitest 15（渲染/面板/边界锁定/朗读入口）。

**真机冒烟（CDP）**：真 MyMemory 翻译（「今天天气不错」→ en/ja 真译文）、朗读自动降级 SAPI（played）、自动读开关生效（关闭后重读按钮也拦截）、图钉切换（pin→locked→leave 不收起→unpin）、设置持久化真机生效（settings.json 加载）、窗口贴顶右上、日语剪贴板检测落卡、三卡等宽不压缩。

**静态合规**：无剪贴板监听器、无签名/公证、无 onefile/UPX、无下载/替换自身代码路径、test workflow 无发版步骤。

**环境阻塞项（如实标注，非代码缺陷）**：
1. 敏感过滤/复制的运行时端到端验证——本机剪贴板被外部进程锁死（D33），逻辑单元级已验证
2. 真实中文输入法手感——需用户实机打字验证（composition 守卫已实现）
3. release.yml 四平台构建——需用户打 tag 后首跑验证（本会话按约不打 tag）
4. macOS/Linux/Arch 运行时手感——仅 CI 保证可构建

**工单→提交对照**：T01 六条提交、T02 三条、T03 三条、T04 三条+docs、T05 两条、T06 两条、T07 一条、T08 一条、T09 一条、T10 一条（详见 git log）。

**已知边界（文档已声明）**：MyMemory 免费引擎质量一般（尽力而为定位）；日语纯汉字句判 zh；检测拉丁系不判别；同步命令下「同步中」状态不可见（工单 04 异步化后可见）；改划词键位重启生效。

## 十一、MVP 后交互重构与缺陷修复（2026-11-06 日间，回填）

**范围**：`87adbbb`（MVP 文档收尾）→ `4ac886a`，5 条提交，无新工单；属对已交付 MVP 的形态打磨、交互修复与打包/文案补齐。决策见 STATE.md D35–D38。

**已落地**：

1. `c804b99` 胶囊态透明区不再拦截点击（用户实测反馈）：窗口形态跟随状态，胶囊 / HoverPending / Collapsing 收缩为顶部胶囊条，展开恢复面板尺寸；`bundle.active=true`（nsis/dmg/app/deb/appimage）；新增 `build.ps1` 一键打包；`package.json` 加 `@tauri-apps/cli`。该提交自记真机冒烟「胶囊 36px / 展开 309px」、NSIS `Bead_0.1.0_x64-setup.exe` 2.89 MiB。
2. `cbf0ba5` 胶囊贴边与面板重构：胶囊纯 div + logo 词组、贴屏幕上缘、未悬停降透明度；拖拽改系统级 `start_dragging`，`Moved` 实时 clamp；窗口固定 68×28 / 613×300（3 语池），位置以 `capsule_anchor` 一次 `SetWindowPos` 提交；开合动画严格对称、resize 只在内容不可见帧执行；新增 `sync_panel_width` 命令（前端先淡出 → resize → 淡入），卡片增删不左右滑移；卡片顺序跟随语言池槽位；设置页移除语言选项（语言入口移到面板内「语言」开关）；图标集补齐多分辨率 + 托盘图标，`shadow=false`；设置结构新增 `capsule_anchor`、默认 `hover_delay_ms` 200→400。
3. `4bebb0b` 行为修正：拉丁字母过半判 `en`；识别到池外语言改为**不粘贴**（不再回落主语言卡）；`clear()` 递增 `generation`，清空前的在途结果整包丢弃；`replace_pool` 只替换槽位内容、保持顺序；腾讯/火山/阿里/百度/有道第二密钥改读 `secret`；在线朗读超时 8s→2.5s。
4. `8e5ce7e` `build.ps1` 依赖自举（缺 `node_modules` 自动 `npm ci`）、错误可读化、产物清单尊重 `CARGO_TARGET_DIR`；README 补一键打包说明。
5. `4ac886a` `.gitignore` 忽略 HTML 分享包等本地工具产物。

**当前测试基线（本轮真跑，2026-11-06 21:31）**：

- `cargo test --offline --manifest-path bead-core/Cargo.toml` → `65 passed; 0 failed`（MVP 基线 61，+4：拉丁字母判 en、非拉丁不猜、英文落 en 卡、清空作废在途结果、换池保持槽位等新增/改写用例）
- `cargo test --offline -p bead-providers` → `28 passed; 0 failed`（不变）
- `cargo test --offline -p bead` → `3 passed; 0 failed`（不变）
- `npm test` → `Tests 17 passed (17)`（MVP 基线 15，+2；覆盖新增：设置页无语言选项、每卡语言自动完成选择器、朗读图标等）
- 合计 **113**（MVP 终验 107）

**验证状态变化**：

- 本轮只跑测试与静态阅读，**未做 CDP / 真机冒烟**。`cbf0ba5` 的动画对称性、拖拽 clamp、面板宽度同步、多显示器/缩放边界**未见冒烟记录**（该提交未附 Verified），仅由编译与单元测试间接保证；手感需实机复核。
- `c804b99` 的 NSIS 产物与胶囊/展开尺寸为其提交自记的冒烟结果，本轮未复跑打包（按约束不跑长任务）。
- MVP 遗留的四项未验证项全部保持：① 敏感过滤/复制运行时端到端（剪贴板被外部进程锁死，D33）；② 真实中文 IME 手感；③ `release.yml` 四平台 tag 首跑；④ macOS/Linux/Arch 运行时（仅 CI 保证可构建）。未打 tag、未 push 本次文档改动。
- 新增未验证：胶囊拖拽与贴边的真实手感、多显示器与 DPI 缩放下锚点回落、`sync_panel_width` 在真实渲染帧中的表现。


## 十二、面板几何 / 动画、卡片拖拽与图标修复（2026-11-07 收尾）

**范围**：`92ac328` 之后本轮，4 条功能提交 + 1 条文档提交，无新工单；属对已交付 MVP 的交互补全与缺陷修复。用户真机复验确认效果良好。决策见 STATE.md D39–D42。

**已落地**：

1. 面板几何 clamp 与只读 `panel_frame()`：`place_panel` 把面板整体收进当前显示器可用区（贴边时允许平移以保证完整可见，收起仍按锚点还原）；新增只读命令只返回 `{left, top, width, height}`，不移动窗口，几何与 `resize_window` / `sync_panel_width` 同源；胶囊锚点改由 `Moved` 实时维护（`start_dragging` 是 `PostMessage` 立即返回），面板态禁止用 `left + 272` 反推锚点；`geometry_tests` 覆盖左右贴边、顶部任务栏、超窄可用区、双屏并集、150% 缩放。
2. 面板增删的扩展 / 收缩补间：`web/panel-sync.js` 纯函数（`planPanelResize` / `flipOffsets` / `createPanelWidthTracker`）；一帧一次 `SetWindowPos`、加卡先窗口后内容、删卡先内容后窗口、剩余卡 FLIP、只有贴边才遮一帧内容（方案 A）；补间期间到达的状态快照走 `pendingView`，不打断窗口时序；卡片数基线展开时 `prime`、收起时 `clear`。
3. 卡片拖拽换序：`web/card-drag.js` 纯函数；只有顶部 grip 能起拖、复用 `reorder_cards`、编辑态全局禁拖、拖到面板外视为取消、`prefers-reduced-motion` 下时长 0。
4. 拖拽把手、关闭图标与 Windows 拖放配置：`[::::]` 抓握条（`role="button"` + `aria-roledescription="拖拽手柄"`）、close 图标 12→14 并改为撑满 viewBox 的粗叉、`padding: 0` + `flex: none` 修掉 `<button>` UA 内边距把图标压成 6px、窗口设 `dragDropEnabled: false`（Windows + WebView2 否则吞掉 HTML5 拖放）。

**测试基线（本轮真跑，2026-11-07）**：

- `npm test -- --run` → `Test Files 4 passed (4)`、`Tests 45 passed (45)`（panel-sync 12 / edit-sync 5 / card-drag 9 / render 19）
- `cargo test -p bead` → `20 passed; 0 failed`（settings 3 + geometry 17；同时验证 `tauri.conf.json` 被 `generate_context!` 接受）
- bead-core 65 / bead-providers 28 本轮未改动，沿用上一轮基线；合计 **158**（上一轮 113）

**已验证**：

- 用户真机（重新 `tauri build` 后）：从把手拖动换序正常、关闭按钮为清晰叉形、面板增删动画观感通过。
- 浏览器实测（临时静态页复用真 `styles.css`）：关闭按钮 SVG 修复前 `6×14` / computed 宽 6px，修复后 `14×14`；四卡均有 12px 高、`cursor: grab` 的把手。
- 视觉快照（本地临时目录，未入库）：`card-drag-panel.png`（四卡 + 把手 + 拖拽占位虚线 + 插入线）、`card-close-zoom.png`（关闭按钮 8 倍放大）。

**未验证 / 风险（不写成「全部完美」）**：

- macOS / Linux / Arch 运行时仍只有 CI 保证可构建；`dragDropEnabled` 是 Windows 专属问题，其他平台未实测。
- 多显示器 + 混合 DPI 的 clamp 只有 `geometry_tests`（按各屏 scale_factor 折算），未见真机；胶囊跨屏手感受 `Moved` 事件频率影响。
- 补间用固定 160ms + 40ms 等待收尾，低性能设备掉帧时落窗时机可能提前或延后，未量化。
- 拖动期间前端不整板重绘（`pendingDragView` 暂存），若此时 Rust 推送新顺序，会被下一次非拖动刷新覆盖；该竞态未构造运行时用例。
- 面板补间与卡片重排的 reduced-motion 分支只有单测，未在真机「减少动态效果」下观察。
- MVP 遗留四项继续有效：敏感过滤 / 复制运行时端到端（剪贴板被外部进程锁死，D33）、真实中文 IME 手感、`release.yml` 四平台 tag 首跑、macOS/Linux/Arch 运行时。
- `.screenshots/` 是本地视觉验证产物，`.gitignore` 未排除，本轮未入库，待所有者决定是否加入忽略规则。


## 零、T04–T10 自动执行总计划（恢复锚点）

- **T04** Provider 链与真实翻译：bead-core 增 `ProviderChain`（回退/超时/重试上限/熔断，纯逻辑 fake 测试）；新 crate `bead-providers`（HTTP provider 实现，bead-core 保持零依赖）：ZeroKey(MyMemory)、OpenAICompatible(智谱/硅基流动/自定义)、DeepL、腾讯、火山、阿里、百度、有道——请求构造/响应解析用 Transport trait 注入 fake 测试；设置面板加引擎选择与 key/model/base_url + 测试连接。规则 10 解释：单次 translate() 对上层返回全部译文，REST 按语言对的 API 在 provider 内部逐对调用属实现细节（模块文档声明）。
- **T05** 状态机补全（图钉锁定已有基础）、SensitiveFilter、设置持久化（Tauri app_config_dir JSON，升级兼容未知字段）。
- **T06** 复制（已有）+ Speech trait（在线优先/离线降级/缓存；Windows 离线走 SAPI PowerShell 后备或 tts crate——按依赖评估定）；界面不出现引擎选择开关。
- **T07** 划词默认关闭：Windows 取选中文本（模拟 Ctrl+C 读剪贴板，需前台窗口焦点判断）、权限缺失明确提示；快捷键可配。
- **T08** 开机自启（注册表 Run）、单实例、托盘菜单（设置/显隐/退出）、位置记忆与显示器回落；Wayland 探测降级提示（CI 层面）。
- **T09** UpdateChecker：GitHub Releases latest tag 比较、可关闭提示、手动检查；绝不下载/替换自身。
- **T10** README（首句围绕「一次输入多语言并排、改任一语言其余自动同步」）+ CI 发版 workflow（四平台 matrix、SHA256SUMS、无签名）+ 各项静态检查；**只提交不打 tag**。
- 每张工单：测试先行（能先红的先红）→ 实现 → 全量套件 → CDP/静态冒烟 → STATUS 记录 → 分条提交。真实 IME 手感与 macOS/Linux 运行时按规范如实标注未验证。
- 已知技术事实：headless Chrome 最小窗宽 500；IAB clip 截图有平铺 bug（整视口截图+GDI+ 裁切）；PowerShell Set-Clipboard 进程退出会清空剪贴板；竖排→横排胶囊变更后 Chromium 会对替换 DOM 补发 mouseenter（渲染层已防）；Document 用 BTreeMap，卡按语言码字典序展示。
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
5. ~~T01 提交~~ ✓ 6 条已落库（见 git log）；**剩：T02 改动点待用户确认 → 分条提交，不推送**

## 八、T02（可编辑与联动同步）实施记录（2026-11-06 凌晨）

**已实现**：
- `bead-core`：`SyncCoordinator::retry_failed()`——只重试 apply 标错的卡，源为权威源卡现文本，一次请求只携带失败语言；无失败卡返回 None 不递增 generation。新增 2 个行为测试，先红后绿。
- `web/`：卡片文本改 `textarea`（400ms 防抖、Enter/失焦立即提交、Shift+Enter 换行）；**IME 守卫**（isComposing 的 Enter/Esc 不提交不收起，compositionend 重新排程）；error 卡渲染「重试」按钮；`edit_start`/`edit_end` 接状态机（编辑态 hover 不产出读取，复用既有转移）。
- **渲染保留策略**：编辑中（textarea 持有焦点且 expanded）与渲染结果不变时跳过 DOM 替换；失焦后统一刷新。
- `src-tauri`：新命令 `commit_edit` / `edit_start` / `edit_end` / `retry_failed`；固定池 [zh,en,ja]（`Document` 用 BTreeMap，slot 按语言码字典序展示：en,ja,zh——顺序确定性有保证）。
- **窗口布局（用户指定）**：宽度随语言数增长（40+185n+9(n-1)，3 语 613px），启动定位屏幕顶缘右缘（`visible:false` → setup 内定尺寸/定位后 `show()`）；胶囊横置为标签形，平顶贴屏幕顶缘。

**发现并修复的潜在渲染竞态（T01 已带病、T02 重构暴露）**：真鼠标悬停时，重绘替换光标下的胶囊 DOM 会触发 Chromium 对新节点补发 `mouseenter` → `hover_enter` → 再重绘……同时 `scheduleTick` 每次重绘 `clearTimeout` 重排 200ms，tick 永远不期（实测 600ms 内排程 129 个定时器）。修复：渲染结果不变不替换 DOM + 同类 tick 计时不重排 + 胶囊补 `mouseleave`（带 `isConnected` 守卫，顺带修复 T01 的「短暂掠过后 tick 仍展开」缺陷）。T01 竖置胶囊带 transform 合成层恰好抑制补发，故未暴露。

**验证**：
- `cargo test`：26 passed / 0 failed（24+2）
- `npm test`：8 passed / 8（6+2 组件测试：textarea 渲染、error 卡独有重试按钮）
- CDP 冒烟：三卡顺序 en,ja,zh ✓；防抖路径一次编辑联动双卡（zh 原文权威源、en/ja fake 填充）✓；回车立即提交 ✓；blur 后整板重绘 DOM 实测同步值 ✓；窗口 613×300 贴屏幕右上（client origin 1955,1）✓；横置胶囊截图 ✓
- 证据：`%TEMP%\bead-measure\smoke-t02-capsule.png`、`smoke-t02-expanded.png`、`%TEMP%\bead-ui-02-faithful.png`（613×300）、`cdp6/7/8.mjs`
- 现场已清理：进程清零、光标还原、无剪贴板改动（本轮未触碰）、探针服务器已终止

**未验证 / 留待**：
- **真实中文输入法手感未验证**（composition 守卫只能实机 IME 验证）——需用户实机在中文卡打一句中文
- 重试按钮的运行时链路未真机触发（fake translator 不产生失败；语义由 bead-core 2 测试覆盖，真实失败路径属工单 04）
- Esc 冒烟被「光标停在胶囊上会立即重新悬停展开」干扰，语义由状态机测试覆盖
- 同步命令下「同步中」状态不可见（fake 瞬回），可视表现属工单 04 异步化

## 九、T03（语言池 2–5 与初始落卡）实施记录（2026-11-06）

**已实现**：
- `bead-core/detector.rs`：`LanguageDetector: Send` trait + `HeuristicDetector`（假名→ja、谚文→ko、汉字→zh、其余 None；拉丁系不判别，宁可落主语言）。3 个检测测试。
- `SyncCoordinator`：构造函数注入 `Box<dyn LanguageDetector>`；`accept_clipboard` 检测命中池内语言落对应卡（该卡即权威源），检测不出/不在池内回落主语言卡；`replace_pool` 换池保留重叠卡文本、丢弃移除卡、权威源被移除回落主语言（还在则保持）、槽位状态回 Idle。5 个新测试。
- `src-tauri`：`set_languages` 命令（非法池给 notice；换池后窗口宽度按 `board_width(n)` 重算并保持贴顶右）；`HeuristicDetector` 接入；`board_width` 提取复用。
- `web/`：header 新增「语言」开关；面板 8 候选（zh/en/ja/ko/fr/de/es/ru），勾选即时生效；**2–5 边界在渲染层锁定**（2 卡禁移除、5 卡禁新增）；面板开合为前端本地状态（`renderBoard({...view, languagesOpen})`，保持纯函数签名）。
- **偏差声明（规范第 8 条 > 工单描述）**：工单要求「朗读入口」，TTS 未实现故不展示，入口留 T06。

**验证**：
- `cargo test`：34 passed / 0 failed（+8：检测 3、落卡 3、换池 2）
- `npm test`：10 passed / 0 failed（+2：面板开合、2–5 边界锁定）
- CDP 冒烟：8 候选渲染 ✓；取消 ja → 2 卡 [en,zh] + 窗口 613→419 ✓；加回 → 3 卡 + 613 ✓（面板保持打开）；日语链路端到端——编辑 zh 卡为「テストです」→ 复制 ja 卡（剪贴板 "[ja] テストです"）→ ESC 清空 → 再悬停 → 检测假名 → **权威源落 ja 卡**，en/zh fake 填充 ✓
- 快照：`%TEMP%\bead-ui-03-faithful.png`（613×300，含「语言」按钮）
- 现场已清理：进程清零、光标还原、剪贴板经应用自身链路变动后未还原（内容为应用产出的译文，无隐私风险）、探针服务器已终止

**未验证 / 留待**：
- 检测启发式边界：日语纯汉字句判为 zh（文档已声明）；真实多语种检测质量留后续优化
- 设置**不持久化**（会话内有效）——持久化属工单 05
- 真实 IME 手感仍待用户实机验证（T02 遗留）

## 六、待合并的文档冲突（按规范第 11 条，功能点完成后再修）

- `STATE.md` D18 与 `docs/naming.md` 标识清单写 `C:/Users/HP/WorkSpace/Bead`，实际工作区为 `D:/WorkSpace/Bead`。
- `STATE.md` / `Agents.md` 写 Rust 1.96.0，实测 `rustc 1.99.0`；Node 22.22.2 一致。
- `STATE.md` 基线写「无 Cargo.toml、0 tests」，那是 Stage 0 的有效历史事实，**不修改**；当前状态以本文件为准。
- `SPEC.md` / `TICKETS.md` 的 T03 写「设置里能配 2–5 种语言」，MVP 后实现改为**面板内「语言」开关**、设置页不再提供语言选项（`cbf0ba5`）；运行时语义（2–5 边界、双向）不变，属入口位置偏差。
- `SPEC.md` 交互基线写「悬停 200ms 展开」，MVP 后默认改为 **400ms**（`cbf0ba5`，`hover_delay_ms` 默认值）；此外前端设置项读取的缺省回退仍写 200ms，与 Rust 默认 400ms 存在遗留不一致，待后续统一。

## 七、会话耗时

- 22:41 → 23:01，约 20 分钟。耗时点：crates.io index 卡住 6 分钟（含一次后台任务终止）、npm install 4m17s、Tauri 首次全量编译约 3 分钟、探针迭代 3 次。
- 优化建议：把 `.cargo/config.toml` 镜像配置与 `icons/icon.ico` 前置为第一步（可省约 8 分钟）；npm 也配置国内 registry；demo 与主程序可通过 `CARGO_TARGET_DIR` 共享一份编译产物，避免两次全量编译。
- 2026-11-06 21:18 → 21:42，约 24 分钟（本轮为同步 + 文档回填）。耗时点：逐条阅读 5 条提交的正文与关键 diff（`main.rs` 单条 +1094 行）、四套件复跑（debug 缓存命中，编译约 1–2 分钟）、两份文档改写与一致性核对。
- 优化建议：下次先看 `git log --format` 摘要再按需展开 diff；测试基线可只跑 bead-core + vitest（providers/shell 在本轮改动中未变）。
