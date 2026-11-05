# Bead — 全平台悬浮窗多向翻译桌面应用

- **Slug**: `bead`
- **Tier**: `L` — 跨平台桌面应用，涉及 GUI 悬浮窗、剪贴板/划词系统钩子、翻译后端、TTS、多平台打包与自启动，跨多个模块且单个会话做不完
- **Stage**: `tickets`
- **Started**: 2026-11-05
- **Base**: `6da4428850436f9a617e2e603d265e0637edde9c` — diff range `6da4428..HEAD`
- **Repo**: `C:/Users/HP/WorkSpace/Bead`

## Goal

交付一款**免安装、点开即用、体积小巧**的全平台桌面翻译工具：平时只在桌面边缘显示一条小胶囊，鼠标主动悬停时读取剪贴板并展开悬浮板翻译，展开可锁定；语言池最多 5 种、一次展示全部语言结果卡，选 2 种即双向，任意卡片可编辑且其余语言自动同步；支持朗读发音、复制、划词取词（按平台降级）、开机自启可配置，覆盖 Windows / macOS / Linux / Arch Linux。验收标准是四类平台都有可直接运行的便携产物 + GitHub Actions 自动打包发布，且核心行为有自动化测试覆盖。

## Baseline

<!-- 记录于 Stage 0，任何代码改动之前。这是事后唯一无法用命令重建的事实。 -->

- **Command**: `python -m pytest -q` （仓库尚无测试命令，此为拟采用的命令）
- **Result**: `C:\Users\HP\.workbuddy\binaries\python\versions\3.13.12\python.exe: No module named pytest` — 0 tests, 0 passed, 0 failed；测试运行器未安装、测试目录为空、无既有失败用例
- **Also checked**: 无 `package.json` / `pyproject.toml` / `Makefile` / `Cargo.toml`；Python 3.13.14、Node 22.22.2、Rust 1.96.0 可用；PySide6/PyQt/tkinter/pyperclip/pytest/PyInstaller 均未安装
- **Recorded**: 2026-11-05 16:42

**推论（已作废，因 D1 排除 Qt/Electron、技术栈改为 Rust 系）**：~~第一张工单是 `pyproject.toml` + pytest 脚手架~~ → 现改为 `cargo test`（Rust 核心逻辑）+ 前端单元测试脚手架。基线本身仍是有效事实：改动前仓库没有任何测试命令、0 tests、0 failed。

## Decisions

<!-- Settled decisions only, one line each, newest last. A decision that changes later gets struck through, not deleted. This is intent, and intent is not in the code. -->

- 仓库落在 `C:/Users/HP/WorkSpace/Bead`（原 `FloatTrans`，2026-11-05 改名）而非 WorkBuddy 会话工作区 — 用户选择「换目录再做」，代码与状态文件都进新仓库
- 采用 `git init` 新建空仓库并做基线提交 — 用户明确授权；`gh auth` 已登录 KrisShin，SSH key 可用，后续如需远端由用户决定
- 规模定为 L — 用户确认按完整流程（盘问 → spec → 工单 → TDD → 三轴评审 → 收尾）
- **D1** 不做安装器，交付点开即用的独立 app，定位轻量小工具、体积尽量小巧精致；**明确排除 Qt/PySide6 与 Electron**（原 Q1 推荐作废，需重选型）
- **D2** 翻译源优先适配国内；用户希望整理各家免费 key/额度，并接受引入免费开源轻量大模型（点名 GLM-4.7-Flash）与 DeepL
- **D3** 采用 Q3-(a)：语言池最多 5 种，一次展示全部语言结果卡；只选 2 种语言即自然构成双向，不另设「双向模式」。**新增核心语义：任意一张语言卡的内容都可编辑，改任意语言后其余语言自动同步翻译**
- **D4** 支持锁定展开；**未开启划词翻译时不主动读取剪贴板，仅鼠标主动悬停胶囊才展开并读取**
- **D5** 划词采用 Q5-(d)：三种取词方式全做，按平台能力降级
- **D6** TTS 采用 Q6-(c)：默认联网、失败自动降级离线，**不向用户提供手动指定引擎的开关**
- **D7** 需要 GitHub 仓库 + 多平台自动打包发布 GitHub Actions
- **D8** 隐私与设置按推荐全做：敏感内容过滤、自动读剪贴板总开关、日志不落原文、开机自启开关
- **D9** Q11 编辑联动按推荐定案：400ms 防抖 + Enter/失焦立即提交；最后编辑的卡为权威源且不被覆盖；其余卡「同步中」保留旧值；`generation += 1` 丢弃过期响应；初始剪贴板语言检测落卡、失败落主语言
- **D10** Q12 划词/hover 按推荐定案：未开划词完全被动（仅 hover 读一次）；开启划词后划词完成自动展开并送入文本；锁定或编辑状态下 hover 不覆盖当前内容，仅显式点「读取剪贴板」或解锁后刷新；剪贴板内容哈希去重
- **D11** Q10 修正：GLM-4.7-Flash 不能免 key 直调，用户提出可走硅基流动 key。定案为 `OpenAICompatible` provider（base_url + model + key 全部可配），预设智谱 / 硅基流动 / 自定义；运行时探测 `/v1/models`，默认零 key 源仍为 MyMemory
- **OPEN — 待用户决定**：Q9 技术栈（Tauri v2 vs 纯 Rust 自绘）、Q13 是否做安装器。已给出对比与推荐，等用户拍板。→ **已定，见 D12/D13**
- **D12** Q9 定案：**Tauri v2（Rust + 前端 WebView）**。决定性理由是 Q3 的「中文可编辑多语言卡」需要完善 IME，WebView 与 egui/Slint 自绘是代差；且官方插件生态覆盖托盘/全局快捷键/剪贴板/单实例。Wayland 窗口定位限制两方案都存在，不构成改选 b 的理由。
- **D13** Q13 定案：**方案 C 双轨**，且用户补充「**不做单文件启动（易被判毒）**，改为安装器 + zip 便携目录」。
- **D14** 授权选 MIT/Apache 系，避免 Slint 的 GPLv3/商业双授权。
- **D15** 签名策略（**用户拍板，产品决定**）：**任何平台都不做代码签名**，不申请/购买证书、不接 macOS 公证、CI 不留签名钩子。理由：开源软件，觉得不安全自己看源码。信任来自源码可审计 + SHA256SUMS + 可复现 CI；README 如实写明首次运行放行方式。
- **D16** 自动更新从 Out of Scope 移入范围，但**只做版本比较提示**：启动时请求 GitHub Releases latest tag 与当前版本比较，不匹配显示可关闭轻提示 + 跳转 Release 页；一致/离线/超时静默；设置页有手动检查入口与开关。**绝不自动下载或安装、绝不替换自身文件**。
- **D17** SPEC.md 已获用户批准（附带 D15/D16 两处修改，已应用）。Stage 2 门通过。
- **D18** 项目定名 **Bead**（中文可作「齐译」，不作为交付标识）。用户拍板。标识统一：仓库 `KrisShin/Bead`、本地 `C:/Users/HP/WorkSpace/Bead`、会话工作区 `C:/Users/HP/WorkSpace/WorkBuddy/Bead`、`.scratch/bead/`、二进制 `bead`、PKGBUILD `bead`、crate `bead-core`。原名 FloatTrans 与 `krisir/floattrans`（40★ 同品类 macOS 工具）撞车故弃用。因 Windows 目录句柄占用导致 rename 被拒，改用 `cp -a` 完整拷贝迁移，7 条 git 提交与证据链原样保留。
- **D19** 仓库沿用 Stage 0 的 `git init`，**不重复初始化**，避免抹掉 ship 证据链；用户本次要求的「git init」按「确认仓库可用 + 补齐远端」理解执行。
- **D20** 远端仓库 `KrisShin/Bead` 已创建为 **PUBLIC**，默认分支 `main`，本地全部提交已推送，本地与远端 HEAD 一致（以 `git rev-parse HEAD` 与 `git ls-remote origin` 实时核对，不在此写死 SHA）。描述为「悬停即译、五语同步的轻量桌面多向翻译工具 (Tauri v2 + Rust)」。HTTPS 代理 `127.0.0.1:2154` 对 git 的 CONNECT 隧道返回 502，改用 SSH 远端 `git@github.com:KrisShin/Bead.git` 推送成功（`Hi KrisShin!` 认证通过）。本次推送由用户单次授权（改名 + Agents.md + 建仓推送为同一条指令）。
- **D21** 立项期全部调研结论已归档到 `docs/`（索引 `docs/README.md`）：竞品定位、命名排查、翻译源实测与免费额度、平台约束（Wayland / 签名 / 判毒 / 安装器对比 / CI）。定位为**调研留档、非执行依据**；执行依据仍是 `SPEC.md` 与 `TICKETS.md`，代码存在后以代码与测试为准。所有数据标注 2026-11-05 时间性。
- **D22** 用户确认 Stage 3 门与工单 01 的切片/阻塞边（按 L3 执行），并确认：CI 首刀只加测试 job、不触发发版/打包/签名；需要 `demo/` 探针；依赖清单确认后引入。工单 01 实现据此开工。
- **D23** 本机 crates.io index 经 cargo 拉取不可用（curl 直连 200，cargo 卡住超过 6 分钟并占住 package cache 锁）。定案改用项目级 `.cargo/config.toml` 的 rsproxy sparse 镜像 + 关闭 HTTP multiplexing，不改全局 cargo 配置。
- **D24** 工单 01 依赖定案：`tauri` 2.12.5、`tauri-build` 2.7.4、`tauri-plugin-clipboard-manager` 2.5.0、`serde`/`serde_json`；`bead-core` 保持零第三方依赖。前端**不引入** Vite/TypeScript/`@tauri-apps/api` 构建链，改由 Tauri 直接托管 `web/` 静态目录、用 `withGlobalTauri` 注入的全局 invoke —— 减少依赖，单向数据流与视觉快照复用不变。
- **D25** `demo/` 探针结论：Windows 上 Tauri v2 的窗口定位（读回与请求一致）、置顶、剪贴板读/写/读回/恢复、hover 200ms 防抖均可行，且未注册任何监听器。剪贴板为空或非文本时 `read_text` 报错属预期空态，不是权限配置缺陷。
- **D26** 远端无 dev 分支，定案从 `main` 新建**本地** `dev` 并切换，不推送。实施进度与验证输出另记 `.scratch/bead/STATUS.md`，本文件仍只记决策、假设与基线。
- **D27** 用户于 2026-11-06 授予全权：T04–T10 无须逐步确认，按我建议执行；所有决策与原因记入 STATE/STATUS 文档；MVP 完成后整理文档、提交并**推送 dev 分支**。**不打 tag**（tag 触发发版 CI，发布时机仍归用户）；不可动清单（不签名/不做单文件/更新只比较/默认被动/池上限 5/范围不扩张）继续有效。
- **D28** T04 依赖评估（规范第 6 条）：HTTP 客户端选 **`ureq` 2.x（rustls 特性）**——阻塞式、体积约 0.3MB、MIT/Apache-2.0，不需要为同步命令引入 tokio 运行时；替代方案 `reqwest`（重、拉 tokio 全家桶）与 `attohttpc/minreq`（生态弱）被否。签名用 RustCrypto 的 `sha2`/`hmac`/`md-5`/`hex`（MIT/Apache-2.0，各 <0.2MB）。provider 实现放**新 crate `bead-providers`**（依赖 bead-core 的 trait），`bead-core` 保持零依赖；HTTP 调用经 `Transport` trait 注入，测试用 fake 不触网。
- **D29** 规则 10 的解释定案：「一次请求返回全部译文」约束的是上层契约——provider 内部对按语言对设计的 REST API（MyMemory/DeepL）逐对调用属实现细节，对上层仍是一次 `translate()` 返回全部；OpenAI 兼容系则真单次调用携带全部目标。写入模块文档。
- **D30** T06 TTS 依赖届时评估；优先尝试 Windows SAPI（PowerShell System.Speech）零依赖离线路径 + 在线 TTS API 降级链，避免重依赖；若不可行再评估 crate。
- **D31** 许可定为 MIT 单许可（LICENSE 文件已入库）：全部依赖 MIT/Apache-2.0 兼容，未来需要时可加 Apache-2.0 双许可。
- **D32** MVP 完成后推送 dev 分支到 origin；**不打 tag**——tag 触发发版 CI（release.yml），发布时机归用户。release.yml 的三平台 tauri-action + Arch 容器构建 + SHA256SUMS job 需 tag 首跑验证，可能需小幅调整（已如实标注）。
- **D33** 敏感过滤与复制链路的运行时端到端验证被环境阻塞（本机剪贴板被外部进程整体锁死，PowerShell Get/Set-Clipboard 同报错误），逻辑由单元测试覆盖（47+ 项），运行时复验留待剪贴板恢复后执行。
- **D34** 会话收尾（2026-11-06 凌晨）：用户确认全权并指示「保存状态→提交推送→推送完成后关机」。终态核验：工作区干净、本地 HEAD 与 origin/dev 一致（ef1e1d3）、四套件 107 项测试全绿复跑确认、bead 进程与探针端口清零、无自启动残留、剪贴板无未恢复项（外部锁死与本会话无关）。按授权执行定时关机。

## Facts (Stage 1 实测 / 查证)

- 2026-11-05 本机网络实测：`translate.googleapis.com/translate_a/single` **超时不可达**；`libretranslate.com/translate` 返回 Cloudflare HTML 挑战页不可用；`api.mymemory.translated.net/get` **零密钥可用**（`你好世界` → `Hello world`）；`cn.bing.com/translator` / `ttranslatev3` / `fanyi.youdao.com` / `fanyi.baidu.com` 均 200；`open.bigmodel.cn/api/paas/v4/chat/completions` 返回 401（可达、需 key）；`api-free.deepl.com` 返回 403（需 key）。**结论：国内网络下「零配置且稳定」的翻译源不成立，免 key 源只能作为尽力而为的默认，稳定源必须用户自备免费 key。**
- 免费额度公开口径（2026，非厂商承诺）：腾讯翻译君 500 万字符/月、火山翻译 200 万/月、阿里翻译 100 万/月、百度标准版 5 万/月（高级版 100 万/月）、有道新用户 50 元体验金、DeepL Free 50 万字符/月、智谱 GLM-4.7-Flash **输入输出全免费 / 200K 上下文 / 仅 1 并发 / 需中国手机号注册 / OpenAI 兼容**。
- Tauri v2 用系统 WebView（Win WebView2 / macOS WKWebView / Linux WebKitGTK 4.1），产物 8–15MB、空闲内存 30–40MB；Linux 需 `libwebkit2gtk-4.1-dev`，发行版间 4.0/4.1 ABI 不兼容是已知坑。
- **Tauri v2 Wayland 硬限制（tauri-apps/tauri#14913，2026-02 提出、仍未关闭）**：`Window::set_position` 与 `always_on_top` 在 Wayland 下**静默 no-op**，连窗口初始位置都不生效。这是 xdg-shell 协议的设计而非框架 bug；GNOME 下可借合成器自己的窗口菜单让用户手动置顶（`tauri-plugin-window-manager` 有此能力），但位置仍无解。纯 Rust 自绘（winit/xdg-shell）同样受限，只有 `wlr-layer-shell` 能在 wlroots 系合成器（Sway/Hyprland）实现边缘吸附，GNOME 不支持。**结论：Linux Wayland 上「胶囊吸附边缘」两方案都要降级，与 UI 框架无关。**
- 硅基流动 `https://api.siliconflow.cn/v1` OpenAI 兼容，免费层 1000 RPM / 50K TPM、9B 以下模型永久免费、新用户 2000 万 token；智谱 `https://open.bigmodel.cn/api/paas/v4`。实测两家 `/v1/models` 与 completions **无 key 均返回 401**，故模型清单与免费模型 ID 需用户填入 key 后由 app 运行时探测，不能写死。
- Slint 为 GPLv3 / 商业双授权，体积约 5–9MB；egui 更小但中文 IME、圆角/模糊/富文本编辑支持弱。
- **签名与开源正交（2026）**：开源不豁免 Windows SmartScreen，也不豁免 macOS Gatekeeper。Windows 有 SignPath Foundation 免费 OSS 证书但需审核、发布者名显示基金会；macOS **无免费路线**（Apple Developer $99/年 + 公证）；Linux 无强制签名。zip 便携不绕过 SmartScreen（Mark-of-the-Web 会传递）。
- **误报判毒**：主要触发点是 onefile 运行时自解压到 `%TEMP%`、UPX 加壳、以及 PyInstaller 预编译 bootloader 的已知指纹（onefile 误报率实测约为 onedir 的 3–5 倍）。Tauri 产出的是普通原生 PE，无自解压、无加壳；「不做单文件 + 目录形态」进一步降低误报，方向正确。
- **竞品定位（2026-11-05 查证）**：Pot `pot-app/pot-desktop` **19,340★**、GPL-3.0、**Tauri + Rust**、同赛道且已于 2026-07 归档停更；Easydict **15,207★**、Swift、仅 macOS；Crow Translate 1,963★；krisir/floattrans 40★。这些工具的「并排」是**多引擎**（同一目标语言比服务商），不是**多语言**；「改一处其余语言同步」只存在于 Crowdin / Phrase / LexiSync / Framer 等**项目级本地化平台**。「桌面悬浮 + 多语言并排可编辑联动」是空位，D3 是唯一站得住的产品定位，README 首句须围绕它写。
- **命名排查**：`FloatTrans` / 中文「浮译」撞 krisir/floattrans；`Lilt` 撞 lilt.com 翻译公司与 LiLT 模型；`Perch` 撞 Perch AI、ConnectWise SIEM 与 pioneering.app 同气质桌面小工具；`Capsa` 撞 Colasoft Capsa 商业网络分析仪；`Verse` 撞 Epic Games 注册商标（USPTO 2023）；`Weft` / `Unison` / `Accord` / `Echo` / `Relay` 均有高星同名项目；`Wisp` 有 USPTO 注册商标。crates.io 上短单词几乎全被占，crate 名统一用 `bead-core` 规避。
- 本机：Windows，Python 3.13.14 / Node 22.22.2 / Rust 1.96.0 可用；`gh auth` 已登录 KrisShin（repo/workflow scope 齐全）。

## Assumptions

- 暂无。Stage 1 盘问后逐条补齐；任何未经用户确认的选择都写在这里并标 OPEN。

## Next action

Stage 3 门已确认，工单 01（L3）开工后按用户要求**暂停**。实施现状、验证输出、未完成项与恢复顺序见 `.scratch/bead/STATUS.md`；本文件不记进度，具备实现提交后进度由 `git log` 推导。当前尚无实现提交，所有文件均未跟踪、未推送。Stage 0 基线「无 `Cargo.toml`、0 tests」仍是有效历史事实，不修改。

## 文档地图

- `Agents.md` — 绝对规范、改动分级、不可动清单（每次动手前读）
- `docs/README.md` — 调研文档索引与「非执行依据」声明
- `docs/competitive-landscape.md` / `naming.md` / `translation-providers.md` / `platform-constraints.md` — 立项期查证
- `.scratch/bead/SPEC.md` — 已批准规格（78 条用户故事）
- `.scratch/bead/TICKETS.md` — 工单与 Frontier
- `.scratch/bead/STATE.md` — 本文件，只记决策/假设/基线
- `.scratch/bead/STATUS.md` — 工单 01 实施现状快照（进度、验证输出与未完成项，非执行依据）

## Later

- ~~GitHub 远端仓库创建与推送~~ → 已完成，见 D20（`KrisShin/Bead`，public，main）
- 后续可考虑：翻译历史、术语表、OCR 截图翻译、离线模型
- 旧目录 `C:/Users/HP/WorkSpace/FloatTrans` 与 `C:/Users/HP/WorkSpace/WorkBuddy/FloatTrans` 因 Windows 句柄占用未能删除，待关闭 WorkBuddy 与预览面板后手动清理（内容已完整拷贝到 Bead）
- README.md 尚未创建：按 SPEC 要求，首句须围绕「一次输入多语言并排、改任一语言其余自动同步」写，并说明未签名与各平台首次运行放行方式（属工单 10 范围）

## Close

- **Shipped**: —
- **Left out**: —
- **Next session needs**: —
- **Verify**: —
