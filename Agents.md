# 绝对规范

1. 本绝对规范由项目所有者维护。你禁止修改其中任何内容，必须严格遵守。
2. 不得主动推送、不得执行任何改变仓库历史的写操作。推送单次授权有效，后续仍需另行确认。
3. 所有改动先分级（见「改动分级」），按级别决定是否事前确认、是否需要探针，确认后才执行。L1/L2 执行完必须主动提醒我复核，不得静默结束。
4. 未经我明确要求，不得主动执行完整 release 构建、四平台 Actions 发版、NSIS/dmg/deb/PKGBUILD 打包、安装与卸载测试或长时间 GUI 自动化；每次改动后做静态分析与最小必要验证。日常迭代用 `cargo check` / `cargo test`，`tauri build` 只用于出包。
5. 验证优先用机器可判定的手段，按此顺序：① `#[test]` 覆盖算得出的部分（`SyncCoordinator` 联动与竞态、`ProviderChain` 回退、`HoverStateMachine` 转移、`LanguagePool` 边界、`SensitiveFilter`）；② 涉及界面且 ① 覆盖不足的，用前端组件测试与视觉快照；③ 以上都不成立才申请 GUI 自动化。跑过 GUI 验证必须还原设置文件、清理自启动项与托盘残留、恢复剪贴板原状、结束测试进程，不留下测试状态。
6. Rust 核心尽量不引入新依赖；新增 crate 前须给出用途、体积、许可证与替代方案评估，经我确认后加入。本项目采用 Tauri v2（Rust 后端 + 系统 WebView 前端），不要按「无第三方 UI 框架」的旧习惯设计。
7. 竞品 Pot（GPL-3.0）、Easydict（GPL-3.0）、Crow Translate（GPL-3.0）功能思路可参照，**禁止复制其代码**；本项目用 MIT/Apache 系授权。
8. 界面展示与设置开关必须反映能力可用性：读不到的能力/数据一律不展示，对应开关置为 disable。适用于 Wayland 窗口定位与置顶、macOS 辅助功能权限、未配置 key 的 provider、不可用的 TTS 后端。平台能力探测失败必须给出明确提示，**不得静默 no-op**。
9. **隐私基线不可突破**：默认不注册任何剪贴板监听器；只有用户主动悬停胶囊（`hover_enter`）或已开启划词并捕获选中文本时才允许读取；不写原文日志、不写 API key、不进崩溃报告。任何改动若让应用变成主动监听剪贴板，一律视为 L3 且默认拒绝。
10. 单次翻译请求须携带全部目标语言、一次返回所有译文，不得改成「每个目标语言各发一次请求」。

补充：当你遇到问题，特别是权限相关问题时，终止并向我求助，不要反复尝试；其他问题尝试第二种方案仍未解决，立即终止并汇报现状，由我决定。记录每轮会话耗时，超过 10 分钟要在返回时说明耗时点并提出优化建议。

---

# 改动分级

0. 三级，只有 L3 需事前确认。
   - **L1 免确认、免探针**：UI 文案与布局、主题色值、动画曲线、日志措辞、注释、测试补充。改错了只影响观感。直接做，做完提醒我复核。
   - **L2 做完汇报、按需探针**：provider 容错分支、单模块内部重构、性能优化、非契约类设置项、构建配置。不确定可行性时先说明，由我决定是否要探针。
   - **L3 事前确认、通常需探针**：`SyncCoordinator` / `Document` / `LanguagePool`、`Translator` trait 与 `ProviderChain` 契约、`HoverStateMachine` 转移、剪贴板与划词平台适配层、设置结构、开机自启、`UpdateChecker`、CI / Actions / 打包脚本 / PKGBUILD、任何对外契约与升级路径。先在回复里给出改动点、影响面、风险、验证方式；涉及平台能力或未知 API 的，先做 demo 探针验证可行性。
   - 由我指定级别；未指定一律按 L3。
   - 探针约束：放根目录 `demo/`，完全独立运行、不依赖主程序、不自动清理；`demo/` 下编译产物不进入检索与阅读范围。
   - 「宽机械重构」不适用 tracer bullet：按 expand → 分批 migrate → contract 推进，contract 阻塞于所有批次完成。

---

# 改动收尾

9. 改动完成后按此顺序收尾，不跳步：
   ① 在回复里整理改动点：首行写「改了什么 + 为什么现在改」，其后写影响面、验证结果、「之前错在哪、现在为什么对」，这些信息同时要出现在 commit 中。不写「更新了 X 文件」这类无信息量描述。
   ② 若本次改动触及已批准文档所描述的用户可见行为、模块边界或已被证伪的内容，更新该文档；**不为此新建文档**。
   ③ 等我确认。
   ④ 确认后按功能点分条提交，**一条提交只做一件事**：不把多个无关改动塞进同一条，也不把一件事拆成一串中间态提交。每张工单单独提交，正文写决策或假设，附 `Verified: <命令> — <真实输出>` trailer；不提交红灯。
   ⑤ 不推送。
10. 改共享模块前先 grep 所有调用点；未被测试覆盖的调用方先写 characterization test 记录当前行为，**单独提交**，再改代码。测试转红时更新其期望值并保留，不得删除。
11. `.scratch/bead/SPEC.md`（已批准）与 `TICKETS.md`（Stage 3 门）在当前无代码的阶段是执行依据；一旦代码存在，**以代码与测试为准**，文档冲突时暂存冲突点并在功能点完成后合并修复。`STATE.md` 只记决策、假设与基线，不记进度（进度由 `git log` 推导）。

---

# 测试与验证

12. 测试即规格：测试名必须是一句可读的行为断言，不是被测函数的名字。
    - 好：`editing_the_chinese_card_updates_all_other_languages`、`stale_translation_never_overwrites_a_newer_edit`、`hover_does_not_read_clipboard_when_locked`、`a_failed_provider_falls_back_to_the_next_one`
    - 差：`test_sync`、`test_boundary`、`it_works`
    一句话说不清的行为拆成多个测试，不在测试体内堆注释。
13. 需真机、桌面会话、管理员权限或特定系统权限才能验证的，用 `#[ignore]` 标注并写明条件；默认测试集必须在无特权环境全绿。默认命令 `cargo test` + 前端 vitest。
14. 模块级 `//!` 写设计意图与取舍，回答「为什么这样而不那样」（为何单次请求返回全部目标语言、为何用 generation 丢弃过期结果、为何默认不监听剪贴板），不复述代码已表达的「做了什么」。
15. UI 改动的验证通道是**前端组件测试 + 视觉快照**（Vitest + 截图），不是离屏自绘渲染器：
    - 快照必须复用主程序同一套组件与样式，不得另写一份绘制逻辑；快照与真机不一致即为缺陷。
    - 涉及界面的改动若 ⑫ 覆盖不足，必须在回复中附截图路径供我比对；图片保留在临时目录，不进版本库、不进 release 产物。
    - 不得留下临时文件之外的进程、窗口、剪贴板或托盘状态。
16. 未验证项如实标注，不写成「已支持」：无用户 API key 时 LLM provider 只能测到请求构造与响应解析；macOS / Linux / Arch 在本机（Windows）只能由 CI 保证可构建，运行时手感与权限流程未验证。

---

17. 特殊要求：为兼容上下文读写限制，长文本读写请主动分小块操作，避免触发 `CC Switch local proxy failed ... EOF while parsing a string` 一类错误。

---

# 项目背景

Bead：轻量、免安装、点开即用的全平台桌面悬浮窗多向翻译工具。Tauri v2（Rust 后端持有全部系统能力与业务状态，WebView 前端只渲染与发 intent，单向数据流），产物为安装器 + zip 目录形态便携包，覆盖 Windows / macOS / Linux / Arch Linux。

- 核心域：`LanguagePool`（2–5 种语言）、`Document`（每语言一个 slot）、`authoritative_slot`、`SyncCoordinator`（400ms 防抖、`generation` 竞态、单卡错误隔离）。**「改任一语言卡，其余自动同步」是本产品唯一站得住的定位**，不得降级为可选功能。
- 服务层：`Translator` trait、`ZeroKeyProvider`（默认 MyMemory）、`OpenAICompatibleProvider`（智谱 / 硅基流动 / 自定义，运行时探测 `/v1/models`）、`MachineTranslationProvider`、`ProviderChain`（回退 + 超时 + 重试上限 + 熔断）。
- 平台层：`ClipboardGateway`、`SelectionCapture`（X11 PRIMARY / Win 模拟复制 / macOS 辅助功能 / Wayland 降级）、`Speech`（联网优先、离线自动降级、缓存）、`Autostart`（注册表 Run / LaunchAgent / XDG autostart）、`SensitiveFilter`、`UpdateChecker`（只比较 tag 与提示）。
- 交互基线：边缘小胶囊 → 主动悬停 200ms 展开 → 读一次剪贴板 → 多语言卡 → 可图钉锁定；未锁定 ESC / 点外部 / 离开 800ms 收起。
- 流程状态：`.scratch/bead/STATE.md`（stage `tickets`）、`SPEC.md`（78 条用户故事，已批准）、`TICKETS.md`（10 张 tracer-bullet 工单，等待 Stage 3 门确认）。基线：改动前 0 tests、无测试命令；工单 01 必须同时交付 `cargo test` 脚手架与最小闭环。
- 竞品定位：Pot（19.3k★，同 Tauri 技术栈，2026-07 已归档）、Easydict（15.2k★，仅 macOS）、Crow Translate 的「并排」是**多引擎**；「改一处其余语言同步」只存在于 Crowdin / Phrase 等**项目级本地化平台**。桌面悬浮 + 多语言联动是空位，README 首句须围绕它写。

## 交互与几何实现约定

以下由代码与测试保证；改交互或窗口几何时先对齐这里，细节见 `.scratch/bead/STATE.md` D35、D39–D42。

- **锚点单一**：位置记忆只有 `capsule_anchor`（胶囊左上角逻辑坐标），由 `Moved` 实时维护；胶囊态横向夹进多显示器可用区并集、纵向贴显示器上缘。展开 / 收起默认窗口左缘 = 锚点 − 272，贴边时整块面板 clamp 进当前显示器可用区，收起仍按锚点还原；**面板态禁止用窗口 `left + 272` 反推锚点**（贴边后不成立）。
- **宽度公式**：`board_width(n) = 26 + 189·n + 10·(n−1)`，2–5 卡对应 414 / 613 / 812 / 1011，窗口高固定 300；面板宽度只由语言池计数决定。
- **一帧一次几何提交**：`resize_window` / `sync_panel_width` 每次调用内部各一次 `SetWindowPos`；增删卡顺序为「加卡先窗口后内容、删卡先内容后窗口」，剩余卡用 FLIP，只有贴边才遮一帧内容。`panel_frame()` 是**只读**查询（不移动窗口、不改状态），几何与前两者同源。
- **卡片拖拽**：只能用卡片顶部 grip 起拖，不得把 `draggable` 放回卡片正文；拖动只做本地 DOM 重排 + FLIP，松手且顺序变化才调 `reorder_cards`；编辑态（textarea 聚焦 / 组字）全局禁拖；拖到面板外视为取消；卡片拖拽不得走 `start_drag` 链路（不产生 `Moved`）。
- **Windows 拖放**：窗口必须保持 `"dragDropEnabled": false`，否则 WebView2 接管拖放、HTML5 `dragstart` 不触发；改该项必须重新构建壳层。
- **图标尺寸**：SVG 放进 `<button>` 时先确认按钮内边距没有挤压图标（关闭按钮曾因 UA 默认 padding 从 14px 压成 6px 宽，看起来像一个点）；调图标先量 `getBoundingClientRect`，不要只改 path。
- **降级**：`prefers-reduced-motion` 下面板补间降为 1ms、卡片重排与增删动画时长 0，功能不变。

## 不可动

- **不做任何代码签名**：不申请/购买证书、不接 macOS 公证、CI 不含签名钩子或 secrets 分支。信任来自开源可审计 + SHA256SUMS + 可复现构建。
- **不做单文件 exe**、不用 UPX、不做 onefile 自解压（降低启发式误报）。
- **更新检查只比较与提示**：不得自动下载、自动安装或替换自身文件。
- **默认被动**：未开启划词时不主动读取剪贴板；锁定或编辑态下 hover 不覆盖当前内容。
- 语言池上限 5；选 2 种即双向，不另设「双向模式」；权威源卡永不被自动覆盖；过期 `generation` 结果整包丢弃。
- TTS 不向用户提供手动指定引擎的开关。
- 不承诺 Linux Wayland 的边缘吸附与置顶（xdg-shell 限制），只做能力探测与降级提示。
- Out of Scope，不得擅自扩张：OCR / 截图翻译、生词本与历史、术语表、账号与云同步、离线翻译模型、自建翻译中转服务、自动下载更新。

## 开发环境

Windows x64 本机开发；Rust 1.96.0、Node 22.22.2、Python 3.13（非主栈）。Tauri v2 依赖各平台系统 WebView：Windows WebView2、macOS WKWebView、Linux `webkit2gtk-4.1`（4.0/4.1 ABI 不兼容）。`gh auth` 账号 KrisShin，远端 `KrisShin/Bead`（公开）。CI 为 GitHub Actions matrix（windows / macos / ubuntu + Arch 容器），tag 触发发版。测试 `cargo test` + 前端 vitest；出包 `tauri build`。

## 未完成代办事项

如果我没有主动说要做什么而是问你要做什么，见 `.scratch/bead/TICKETS.md` 的工单与 Frontier；当前停在 Stage 3 门，**未开始写实现代码**。

## Agent skills

### Issue tracker

Issues 使用本仓库 GitHub Issues；工单粒度与阻塞边以 `TICKETS.md` 为准，不在 Issues 里另建一套并行进度。
