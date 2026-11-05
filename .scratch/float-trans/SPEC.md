# FloatTrans — 轻量悬浮窗多向翻译工具

## Problem Statement

在多语言阅读和写作时，人需要在「看到一段外文 / 写出一句中文」和「拿到其他语言的说法」之间反复切换：复制、切到浏览器、粘贴、选语言、等结果、再复制回来。现有翻译工具要么要求你先打开一个窗口、要么把整块屏幕占住、要么在你没打算翻译时就把剪贴板内容送出去。更麻烦的是，当一段话需要同时以几种语言呈现时，改了其中一版，其余版本不会跟着变，只能逐句手动重翻。用户需要的是一个不打断当前工作、只在自己主动唤起时才出现、并且能把多种语言当作同一份内容来编辑的小东西。

## Solution

桌面边缘常驻一条小胶囊。鼠标主动悬停上去时，它读一次剪贴板并展开成悬浮板，把内容同时翻译到语言池里的所有语言（2–5 种，选 2 种即双向），每种语言一张可编辑卡片。改任意一张卡，其余卡自动同步翻译；最后编辑的那张是权威源，不会被覆盖。每张卡可单独复制、朗读。展开可以图钉锁定。划词翻译可选开启，按平台能力自动降级。开机自启可配置。应用免安装、点开即用，也提供安装器；四平台产物由 GitHub Actions 自动构建发布。

## User Stories

### A. 启动与常驻

1. As a 用户, I want 安装器或解压后双击即可运行、无需预装运行时或配置环境, so that 拿到就能用。
2. As a 用户, I want 启动后只在桌面边缘出现一条小胶囊, so that 不遮挡我正在看的内容。
3. As a 用户, I want 重复启动只唤起已有实例而不是开出第二个窗口, so that 托盘和剪贴板状态不会错乱。
4. As a 用户, I want 系统托盘/菜单栏图标能打开设置、显示隐藏胶囊、退出, so that 不依赖悬浮板本身也能管理应用。
5. As a 用户, I want 胶囊默认吸附屏幕右边缘垂直居中、可拖动换边且位置被记住, so that 它待在我顺手的地方。
6. As a 用户, I want 多显示器下可指定吸附哪块屏、该屏拔掉后回落到主屏, so that 外接屏变化不会让胶囊消失。
7. As a 用户, I want 在 Wayland 下若窗口定位或置顶不被合成器支持，应用明确告知并提供手动定位方式，而不是静默失败, so that 我知道发生了什么。

### B. 悬停 → 读取 → 展开

8. As a 用户, I want 鼠标悬停胶囊约 200ms 后展开悬浮板, so that 路过不会误触、主动停留才触发。
9. As a 用户, I want 展开时读取一次剪贴板文本作为输入, so that 复制完直接悬停就能翻。
10. As a 用户, I want 未开启划词时应用绝不主动监听剪贴板、绝不因剪贴板变化而弹窗, so that 复制密码或无关内容时不被打扰、不外发。
11. As a 用户, I want 剪贴板为空或非文本时显示空态而不是报错, so that 不会莫名其妙弹一个错误。
12. As a 用户, I want 相同内容不重复发起翻译请求, so that 不浪费额度也不闪烁。
13. As a 用户, I want 想重翻时点「刷新」能强制忽略去重, so that 换引擎或改设置后能立刻看到新结果。
14. As a 用户, I want 在设置里关掉「自动读剪贴板」总开关，关掉后只能手动粘贴或输入, so that 我能完全控制哪些内容被读取。
15. As a 用户, I want 疑似密码/令牌/超长内容被过滤且不外发，并在界面说明「已跳过敏感内容」, so that 误复制密钥不会泄漏。
16. As a 用户, I want 悬停延迟可在设置里调整, so that 能按自己的鼠标习惯校准。

### C. 语言池与多语言卡片

17. As a 用户, I want 配置 2–5 种语言组成语言池, so that 只出现我需要的语言。
18. As a 用户, I want 只选 2 种语言时自然形成双向互译、无需切换任何「双向模式」, so that 心智模型最简单。
19. As a 用户, I want 一次输入同时看到语言池里所有语言的结果卡, so that 不用逐一切换目标语言。
20. As a 用户, I want 初始输入自动检测语言并落到对应语言卡，检测失败落到主语言卡, so that 中英混用时不需手动指定。
21. As a 用户, I want 一眼看出哪张卡是权威源, so that 我知道改动会从哪里传播出去。
22. As a 用户, I want 增删语言时卡片数量随之变化，已有内容按明确规则保留, so that 调整语言池不会丢掉我已编辑的文字。
23. As a 用户, I want 每张卡标明语言、可单独复制、可单独朗读, so that 每个语种都能独立使用。
24. As a 用户, I want 设置页提供常用语言列表与搜索, so that 不必记语言代码。

### D. 可编辑与联动同步（核心）

25. As a 用户, I want 能编辑任意一张语言卡的内容（含中文输入法）, so that 译文可以人工润色。
26. As a 用户, I want 改完一张卡，其余语言自动同步翻译, so that 多语言版本始终一致。
27. As a 用户, I want 连续输入时 400ms 防抖、按 Enter 或失焦立即提交, so that 打字过程不会刷屏发请求。
28. As a 用户, I want 我最后编辑的那张卡永不被自动覆盖, so that 我写的东西不会被机器改回去。
29. As a 用户, I want 同步中的卡片保留旧内容并显示「同步中」, so that 界面不闪空、我知道在等哪几张。
30. As a 用户, I want 连续快速编辑时只应用最后一次提交的结果，旧结果不回写, so that 不会出现「打字快了内容倒退」。
31. As a 用户, I want 某张卡同步失败时单独标错并可单独重试，不影响其他卡, so that 一个语种挂了不拖垮整板。
32. As a 用户, I want 网络失败时已填内容保持原样, so that 断网不会清空我的文字。
33. As a 用户, I want 单次翻译请求携带源文本和全部目标语言、一次返回所有译文, so that 在 1 并发的免费模型上也能快速完成同步。
34. As a 用户, I want 请求有超时与重试上限，界面不会无限转圈, so that 卡住时我能重来。

### E. 翻译源

35. As a 用户, I want 零配置首次启动就能翻译, so that 不用先注册账号也有基本能力。
36. As a 用户, I want 默认免 key 引擎被明确标注「尽力而为、可能被限流」, so that 我对质量有正确预期。
37. As a 用户, I want 设置里可选 provider 预设：智谱、硅基流动、自定义 OpenAI 兼容端点、DeepL、腾讯、火山、阿里、百度、有道, so that 优先用国内服务。
38. As a 用户, I want 自定义 provider 可填 `base_url` / `model` / `key`，填完点「测试连接」看到明确结果, so that 换服务不用改代码。
39. As a 用户, I want 能拉取所选 provider 的可用模型列表并挑选, so that 免费模型改版时我自己能适配。
40. As a 用户, I want 设置页列出各家免费额度与申请入口, so that 我知道去哪拿免费 key。
41. As a 用户, I want 主引擎失败时自动回退到次引擎、再回退到免 key 引擎, so that 单点故障不至于让工具不可用。
42. As a 用户, I want API key 不写入日志、不进崩溃报告、不在界面明文回显, so that 凭据不泄漏。
43. As a 用户, I want 各家签名算法与错误码差异被 provider 层吸收，上层只看到统一的错误类型, so that 新增一家不改业务代码。

### F. 复制与发音

44. As a 用户, I want 每张卡有复制按钮，只复制该卡文本, so that 能精确取用某个语种。
45. As a 用户, I want 每张卡有朗读按钮，按该卡语言发音, so that 能确认读法。
46. As a 用户, I want 默认走联网 TTS、失败自动降级到系统离线 TTS，且没有手动指定引擎的开关, so that 不用操心、也不怕断网。
47. As a 用户, I want 朗读失败只给轻提示、不弹模态框, so that 不打断流程。
48. As a 用户, I want 相同文本重复朗读走缓存, so that 不重复下载音频。

### G. 展开 / 锁定 / 收起状态机

49. As a 用户, I want 展开后能点图钉锁定常驻, so that 可以边看边改不收起。
50. As a 用户, I want 锁定或正在编辑时 hover 一律不覆盖当前内容, so that 鼠标划过不会毁掉我写的东西。
51. As a 用户, I want 锁定态下只有显式点「读取剪贴板」或解锁后才刷新, so that 刷新完全由我决定。
52. As a 用户, I want 未锁定时 ESC、点击外部或鼠标离开 800ms 收起回到胶囊, so that 用完不占地方。
53. As a 用户, I want 锁定态收起/隐藏后重新展开时内容仍在，未锁定态收起则清空, so that 行为可预期。

### H. 划词（默认关闭）

54. As a 用户, I want 开启划词后选中文本自动展开悬浮板并送入, so that 阅读时不用先复制再悬停。
55. As a 用户, I want Windows 与 Linux X11 下能自动取到选中文本, so that 主流环境开箱可用。
56. As a 用户, I want macOS 缺辅助功能权限时给出引导而不是静默失败, so that 知道去哪授权。
57. As a 用户, I want Wayland 权限受限时降级为「选中 + 全局快捷键」或手动复制, so that 不支持自动取词时仍有可用路径。
58. As a 用户, I want 划词结果同样不覆盖正在编辑或锁定的内容, so that 两种入口不打架。
59. As a 用户, I want 划词快捷键可配置, so that 避开系统和其他软件占用。
60. As a 用户, I want 划词开关默认关闭, so that 默认行为完全被动、不读剪贴板。

### I. 设置与开机自启

61. As a 用户, I want 在设置里开关开机自启, so that 不用手动改系统配置。
62. As a 用户, I want 自启在 Windows 走注册表 Run、macOS 走 LaunchAgent、Linux 走 XDG autostart, so that 各平台用各平台的标准方式。
63. As a 用户, I want 便携包被移动导致自启路径失效时能自动检测并提示重设, so that 移动文件夹后不会静默失效。
64. As a 用户, I want 设置持久化在用户配置目录、升级不丢, so that 不用每次重配。
65. As a 用户, I want 敏感内容过滤可开关、日志不记录原文, so that 隐私策略由我掌握。

### J. 分发与构建

66. As a 用户, I want Windows 能下载到 NSIS 安装器 **和** zip 便携目录, so that 装或解压随我选。
67. As a 用户, I want macOS 能下载到 dmg 安装镜像 **和** zip 便携 `.app`, so that 两种用法都覆盖。
68. As a 用户, I want Linux 能下载到 deb / AppImage **和** tar.gz 便携目录, so that 依赖可自动装也可手动放。
69. As a 用户, I want Arch 有 PKGBUILD / AUR 包, so that 一行命令装好。
70. As a 用户, I want **没有单文件 exe 形态**, so that 降低杀软启发式误报。
71. As a 维护者, I want push tag 触发 GitHub Actions matrix 自动构建四平台并上传 Release, so that 发版不用手工跑四台机器。
72. As a 维护者, I want CI 产物明确**不签名**、也不保留签名钩子或 secrets 分支, so that 构建流程保持简单、fork 与源码构建完全等价。
73. As a 用户, I want 每个 Release 提供 SHA256SUMS 与构建来源, so that 能验证下载物确实来自该仓库。
74. As a 用户, I want README 写明「本软件未签名及原因」、macOS 首次运行放行方式、Linux 的 webkit2gtk 依赖、Wayland 降级说明, so that 首次使用不被卡住、也不被误导。
75. As a 用户, I want 应用启动时默认异步检查仓库最新 release tag 并与当前版本比较, so that 有新版本时能及时知道。
76. As a 用户, I want 版本不一致时只显示一条可关闭的轻提示与跳转 Release 页的按钮, so that 不被弹窗打断、装不装由我决定。
77. As a 用户, I want 版本一致、离线、请求超时或被限流时完全静默，且更新检查绝不自动下载或安装, so that 便携包不会自改文件、断网也不报错。
78. As a 用户, I want 设置页有手动「检查更新」入口和「启动时自动检查」开关, so that 我能自己控制这个行为。

## Implementation Decisions

- **技术栈**：Tauri v2。Rust 侧持有全部系统能力与业务状态；WebView 只负责渲染与发出 intent。选它的决定性理由是中文 IME 完善的编辑体验与官方插件生态（托盘、全局快捷键、剪贴板、单实例、自启）。授权走 MIT/Apache 系。
- **前后端契约**：单向数据流。前端只发 `dispatch(intent)`、订阅 `state` 快照；Rust 侧 `Reducer` 计算新状态。所有可测行为因此都落在 Rust，前端不含业务判断。
- **核心域对象**：`LanguagePool`（2–5 个语言）、`Document`（每个语言一个 slot，含 `text` 与 `status: idle|syncing|error`）、`authoritative_slot`。语言池变动时，被移除的 slot 丢弃、保留的 slot 文本不变。
- **`SyncCoordinator`**（本项目主 seam）：`commit(slot, text)` 把该 slot 设为权威源、`generation += 1`、产出 `SyncPlan`（源文本 + 目标语言集合 + generation）；`apply(result)` 仅在 `result.generation == current` 时写入，否则整包丢弃。防抖（400ms）、Enter/失焦立即提交、过期响应丢弃、单卡错误隔离都归它。
- **`Translator` trait**：`translate(request) -> Result<TranslationSet, ProviderError>`，`request` 一次携带全部目标语言，返回按语言索引的结果集。这样只发一次请求，绕开免费模型 1 并发限制，也让「改任一语言 → 其余同步」成为单次调用。
- **Provider 分层**：`ZeroKeyProvider`（默认免 key，实测国内可达）、`OpenAICompatibleProvider`（`base_url`/`model`/`key` 全可配，预设智谱 / 硅基流动 / 自定义）、`MachineTranslationProvider`（吸收各家签名与错误码差异）、`ProviderChain`（优先级 + 失败回退 + 超时 + 重试上限 + 熔断）。模型清单运行时由 `/v1/models` 探测，不写死。
- **`ClipboardGateway` trait**：`read_clipboard()`、`read_selection()`、`content_hash()`，平台实现通过注入替换，测试用 fake。未开划词时只允许由 hover intent 触发一次 `read_clipboard()`，不注册任何剪贴板监听。
- **`HoverStateMachine`**：状态 `idle → hover_pending → expanded(locked|unlocked) → collapsing → idle`；事件 `hover_enter / hover_leave / pin / esc / outside_click / selection_captured / refresh`。锁定或编辑态下 `hover_enter` 不产生读取。未锁定收起清空 Document，锁定收起保留。
- **`SelectionCapture` 平台适配**：Linux X11 读 PRIMARY selection；Windows / macOS 用选中后取词（macOS 需辅助功能权限并给引导）；Wayland 与权限受限时降级为全局快捷键或手动复制。能力探测结果暴露给设置页。
- **`Speech` trait**：`speak(text, lang)`，在线优先 → 离线系统 TTS 降级，带音频缓存；不暴露引擎选择开关。
- **`Autostart` trait**：`enable / disable / is_enabled`，额外做路径有效性校验，路径变更时置为失效并提示。
- **`SensitiveFilter`**：长度阈值 + 令牌/密码形态识别 + 可配置开关；命中后不进入 provider 链。日志与崩溃报告一律不写原文、不写 key。
- **平台窗口适配**：无边框 + 置顶 + 边缘吸附在 Win/macOS/Linux X11 用标准路径；Wayland 做能力探测，不支持时降级为普通窗口 + 手动定位提示，不承诺边缘吸附。
- **产物与 CI**：`tauri build` 的 bundle 产出 NSIS（Win）、dmg（mac）、deb + AppImage（Linux）、PKGBUILD（Arch）；另打包 zip/tar.gz **目录形态**便携版。明确不产 onefile 单文件 exe、不用 UPX。GitHub Actions matrix（windows / macos / ubuntu + Arch 容器）在 tag 时构建并上传 Release。签名以可选 secrets 钩子实现，无 secrets 时构建不失败、产物未签名。每 Release 附 SHA256SUMS。
- **签名策略（产品决定，非阶段性妥协）**：**任何平台、任何形式都不做代码签名**，也不申请/购买证书、不接 macOS 公证、CI 不留签名钩子或 secrets 分支。定位是开源软件，信任来自源码可审计 + 每 Release 的 SHA256SUMS + 可复现的 CI 构建来源；用户若对二进制不放心，可自行看源码或从源码构建。README 需直白写明各平台首次运行的放行方式（Windows SmartScreen「更多信息 → 仍要运行」、macOS `xattr -dr com.apple.quarantine` 或系统设置放行），不回避、不美化为「已签名」。
- **`UpdateChecker`**：启动时异步请求 GitHub Releases 的 latest tag，与编译期注入的当前版本号做语义化比较；不一致时在悬浮板内显示一条可关闭的轻提示，附跳转 Release 页的按钮；一致、离线、超时或 API 限流时静默不提示。设置页另有手动「检查更新」入口与「启动时自动检查」开关。**只比较并提示，绝不自动下载、绝不自动安装、绝不替换自身文件**（便携包尤其不能自改）。复用 provider 层的 HTTP 客户端与超时策略。
- **配置存储**：用户配置目录单一 JSON，包含语言池、provider 配置、悬停延迟、划词开关与快捷键、自启开关、敏感过滤开关；升级兼容未知字段。

## Testing Decisions

- **Seams**：主 seam 是 `SyncCoordinator` 的公开接口（`commit` → `SyncPlan`、`apply` → 新 `Document`）。纯 Rust、无 GUI、无网络，`Translator` 与时钟均注入 fake。次要 seam 为 `ProviderChain`（回退与错误映射）和 `HoverStateMachine`（状态转移），同样是纯逻辑。
- **What makes a good test here**：给定语言池与一串提交事件，断言最终 `Document` 各 slot 的文本/状态以及发出的 `SyncPlan`；特别是「快速连续编辑后旧 generation 结果抵达」必须被丢弃。不断言私有字段，不 mock 框架内部。GUI 薄层与系统钩子不写单元测试，改由手工验收清单覆盖。
- **Falsification**：用 fake provider 跑一次端到端冒烟（悬停 → 读剪贴板 → 展开 → 多语言卡 → 编辑联动 → 复制），确认状态机在真实 UI 事件流下与纯逻辑测试一致。
- **Prior art**：无。基线为 0 tests、无测试命令；第一张工单建立 `cargo test` 与前端 vitest（仅测纯展示逻辑），并把测试命令写进 CI。
- **不可测边界（明确记录）**：无用户 API key 时，LLM provider 只能测到「请求构造与响应解析」这一层，真实端到端需填 key 后补测；macOS / Linux 运行时手感在本机（Windows）无法验证，CI 只保证能构建。

## Out of Scope

- 离线翻译模型、本地 LLM 推理、自建翻译中转服务
- OCR / 截图翻译、图片内文字翻译
- 翻译历史、生词本、术语表、导出、批量文件与整页翻译
- 账号系统、云同步、多设备同步、额度看板
- 移动端、浏览器扩展、命令行模式
- 主题皮肤商店、插件市场、自定义布局、多窗口
- 自动**下载或安装**更新、静默替换自身文件、后台自升级守护进程（本次只比较与提示）
- 代码签名（任何平台、任何形式）、证书申请与购买、CI 签名钩子、macOS 公证与 Apple Developer Program 接入
- 对 Linux Wayland 边缘吸附/置顶做承诺（平台协议不支持）
- 手动指定 TTS 引擎的开关
- 单独的「双向模式」（选 2 种语言即双向，已由语言池覆盖）

## Further Notes

- **签名不做，是产品决定**：开源解决「源码可审计」，签名解决「发布者身份与完整性」，两者正交；本项目主动选择前者，代价是 Windows SmartScreen 与 macOS Gatekeeper 的首次警告由用户承担，README 必须如实说明。zip 便携版同样带着浏览器下载的 Mark-of-the-Web，不能绕过 SmartScreen。macOS 未签名还会与划词所需的辅助功能/输入监听权限叠加，升级后可能需重新授权。
- **更新检查只做比较**：latest tag 与当前版本号不匹配就提示，不碰文件系统。这条边界防止便携目录被自更新改坏，也避免未签名产物在后台替换自身带来更大的信任问题。
- **误报判毒的规避依据**：主要触发点是 onefile 运行时自解压到临时目录、UPX 加壳、以及打包器 bootloader 的已知指纹。本项目用原生编译产物 + 目录形态 + 不压缩，天然避开了这几条。
- **默认被动**：整个产品默认不监听剪贴板、不弹窗、不联网翻译，直到用户主动悬停。这是隐私基线，任何后续改动都不得静默打破。
- **待用户提供的验证条件**：一个可用的免费 API key（智谱或硅基流动）用于 LLM provider 端到端验证；此项未完成时相关行为只能以 fake 覆盖，验收报告须标注。
- **平台验证责任划分**：Windows 本地实测；macOS / Linux / Arch 由 CI 保证可构建，运行时手感与权限流程列为未验证项，不写成「已支持」。
