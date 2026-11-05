# FloatTrans — tickets

One ticket per section, numbered in dependency order. A ticket is a **tracer bullet**: it cuts a narrow but complete path through every layer, is demoable on its own, and fits one fresh context window.

**Baseline reminder**: 仓库改动前 0 tests、无测试命令。因此 01 必须同时交付测试脚手架与第一条端到端闭环，否则后续工单的 `Verified:` 无法落地。

**通用规则（每张工单都适用）**：
- 提交前跑全量套件，与基线对比；不提交红灯。
- 提交信息正文写决策/假设，附 `Verified: <命令> — <真实结果>` trailer。
- 改共享模块前先 grep 调用点；未被测试覆盖的调用方先写 characterization test 并**单独提交**，再改代码。
- 每张工单单独一个提交，不与其他工单混合。

---

## 01 — 骨架与最小闭环：胶囊悬停 → 剪贴板 → 双语结果 → 复制

**What it delivers**: 一个能双击启动的 Tauri 应用：桌面边缘一条小胶囊，鼠标悬停 200ms 展开，读取一次剪贴板，经 fake translator 得到 2 种语言的结果，显示两张卡，每张可复制，ESC 收起。同时仓库第一次有了可运行的测试命令与 CI 测试 job。这是产品最小可演示形态。

**Blocked by**: None（可立即开始）

**Seam**: `SyncCoordinator` 公开接口 + `HoverStateMachine` 状态转移（均为纯 Rust，`Translator` / `ClipboardGateway` / 时钟注入 fake）

- [ ] `cargo test` 与前端 vitest 可运行；CI 有测试 job；真实输出写进提交 trailer
- [ ] 2 语池下 `commit` → `SyncPlan` → fake 结果 `apply` → 另一张卡被填充
- [ ] 相同剪贴板内容哈希去重，不重复发请求
- [ ] 状态转移覆盖 `idle → hover_pending → expanded → collapsing → idle`，含悬停防抖
- [ ] 手动冒烟：启动 → 胶囊 → 悬停 → 展开 → 两卡 → 复制 → ESC 收起
- [ ] 明确不注册任何剪贴板监听器（默认被动）

---

## 02 — 可编辑与联动同步（核心特性）

**What it delivers**: 任意语言卡可直接编辑（含中文输入法），改完其余语言自动同步；最后编辑的卡被标为权威源且不被覆盖，同步中保留旧值，连续快速编辑只应用最后一次结果，单卡失败单独标错可重试。这是本产品区别于普通翻译工具的能力。

**Blocked by**: 01

**Seam**: `SyncCoordinator::commit` / `apply`（fake Translator + fake 时钟）

- [ ] 400ms 防抖；Enter / 失焦立即提交
- [ ] `generation` 递增；晚到的旧 generation 结果整包丢弃，不回写
- [ ] 权威源卡永不被 `apply` 覆盖；其余卡同步中保留旧文本并带 `syncing` 状态
- [ ] 单卡失败置 `error` 且可单独重试，不影响其他卡；网络失败不清空已填内容
- [ ] 一次 `SyncPlan` 携带全部目标语言（不是每个目标一次请求）
- [ ] 手动冒烟：中文输入法编辑中文卡 → 英文/日文卡自动更新

---

## 03 — 语言池 2–5 与初始落卡

**What it delivers**: 设置里能配 2–5 种语言，选 2 种自然双向，卡片数量随池变化；初始剪贴板内容自动检测语言并落到对应卡，检测失败落主语言卡；增删语言时已有内容按规则保留。

**Blocked by**: 01

**Seam**: `LanguagePool` / 初始落卡解析（语言检测接口注入 fake）

- [ ] 池大小边界：1 与 6 被拒，2 与 5 合法
- [ ] 选 2 种时双向互译成立，无「双向模式」开关
- [ ] 检测成功落到对应卡；检测失败 / 空内容落主语言卡并给空态
- [ ] 语言增删：保留的 slot 文本不变，被移除的丢弃；权威源被移除时回落到主语言卡
- [ ] 每张卡标明语言、有复制与朗读入口（朗读行为由 06 填充）

---

## 04 — Provider 链与真实翻译

**What it delivers**: 零配置默认引擎（MyMemory）开箱能翻；设置里可选智谱 / 硅基流动 / 自定义 OpenAI 兼容 / DeepL / 腾讯 / 火山 / 阿里 / 百度 / 有道，填 `base_url`+`model`+`key` 后能测试连接、拉模型列表；主引擎失败自动回退，超时与重试有上限；单次请求一次返回所有目标语言。

**Blocked by**: 02, 03

**Seam**: `Translator` trait / `ProviderChain`（HTTP 客户端注入 fake）

- [ ] `ZeroKeyProvider` 默认可用；被限流时明确提示「尽力而为引擎」
- [ ] `OpenAICompatibleProvider` 预设可切换，模型清单由 `/v1/models` 运行时探测、不写死
- [ ] 单次多语言请求断言：一个 HTTP 调用含全部目标语言，响应按语言索引解析
- [ ] 回退顺序、超时、重试上限、熔断与错误映射有测试
- [ ] 各家签名算法/错误码差异被 provider 层吸收，上层只见统一错误类型
- [ ] key 不写入日志、不进崩溃报告、界面不明文回显；设置页有各家免费额度与申请入口
- [ ] **未验证项标注**：无用户 API key，LLM 端到端只能靠 fake 覆盖

---

## 05 — 完整状态机、隐私过滤与设置持久化

**What it delivers**: 图钉锁定、锁定态 hover 不覆盖内容、显式「读取剪贴板」、未锁定收起清空 / 锁定保留、ESC / 点击外部 / 离开 800ms 收起；敏感内容过滤、自动读剪贴板总开关、悬停延迟可配；设置持久化到用户配置目录且升级不丢。

**Blocked by**: 01, 03

**Seam**: `HoverStateMachine` + `SensitiveFilter` + `Settings` 读写（纯逻辑）

- [ ] 所有状态转移组合有测试，含锁定/编辑态下 `hover_enter` 不产生读取
- [ ] 锁定态仅 `refresh` 或解锁后可刷新；未锁定收起清空、锁定收起保留
- [ ] 密码/令牌形态/超长样本不进入 provider 链，界面提示「已跳过敏感内容」
- [ ] 关闭「自动读剪贴板」后，除手动粘贴/输入外无任何读取
- [ ] 设置读写往返、未知字段兼容、悬停延迟生效

---

## 06 — 复制与发音

**What it delivers**: 每张卡单独复制、单独按该语言朗读；默认联网 TTS，失败自动降级到系统离线 TTS，带音频缓存，失败只给轻提示；界面没有手动指定引擎的开关。

**Blocked by**: 01

**Seam**: `Speech` trait（在线/离线实现注入 fake）

- [ ] 在线成功、在线失败→离线降级、两者都失败的轻提示三种路径有测试
- [ ] 相同文本重复朗读命中缓存；失败不清空卡片状态
- [ ] 按卡片语言选择发音语种
- [ ] 设置页与 UI 中不存在引擎选择开关（防止范围漂移）

---

## 07 — 划词（按平台降级，默认关闭）

**What it delivers**: 开启划词后选中文本自动展开悬浮板并送入；Linux X11 读 PRIMARY selection，Windows / macOS 取选中文本（macOS 缺辅助功能权限给引导），Wayland 权限受限时降级为全局快捷键或手动复制；划词结果不覆盖正在编辑或锁定的内容；快捷键可配。

**Blocked by**: 05

**Seam**: `SelectionCapture` trait（fake）+ 与 `HoverStateMachine` 的集成

- [ ] 三种取词路径成功的送入行为有测试
- [ ] 权限缺失 / 合成器不支持时给出明确引导，不静默失败
- [ ] 划词结果与锁定态、编辑态不冲突
- [ ] 开关默认关闭；关闭时 01 的「默认被动」约束不被打破
- [ ] 快捷键可配置且冲突可感知

---

## 08 — 开机自启、单实例、托盘与窗口平台适配

**What it delivers**: 设置里可开关开机自启（Win 注册表 Run / macOS LaunchAgent / Linux XDG autostart），便携包被移动导致路径失效时能检测并提示重设；重复启动唤起已有实例；托盘菜单可开设置、显隐胶囊、退出；胶囊可拖动换边、位置记住、多显示器回落；Wayland 能力探测与降级提示。

**Blocked by**: 01

**Seam**: `Autostart` trait（系统写入注入 fake）

- [ ] `enable / disable / is_enabled` 与路径失效检测有测试
- [ ] 单实例唤起行为手动验收通过
- [ ] 托盘菜单三项动作可用
- [ ] 位置持久化、换边、显示器拔除回落主屏
- [ ] Wayland 下不支持定位/置顶时明确告知并提供手动方式，不静默 no-op

---

## 09 — 自动更新检查（只比较、只提示）

**What it delivers**: 启动时异步查 GitHub Releases latest tag 与当前编译版本号比较，不一致时在悬浮板显示可关闭轻提示 + 跳转 Release 页；一致 / 离线 / 超时 / 限流时静默；设置页有手动「检查更新」与「启动时自动检查」开关。**绝不自动下载或安装、绝不替换自身文件。**

**Blocked by**: 01

**Seam**: `UpdateChecker`（GitHub 响应与网络注入 fake）

- [ ] 版本相等不提示、不等提示、请求失败静默三条路径有测试
- [ ] 语义化版本比较（含预发布 tag 的明确规则）
- [ ] 提示可关闭、跳转链接指向本仓库 Release
- [ ] 静态检查：产物中不出现下载/替换自身文件的代码路径
- [ ] 复用 04 之外的独立轻量 HTTP 调用与超时设置，不阻塞启动

---

## 10 — 双轨分发与 GitHub Actions 发布

**What it delivers**: 公开 GitHub 仓库 + Actions matrix（windows / macos / ubuntu + Arch 容器），tag 触发构建并上传 Release：Windows NSIS + zip 目录、macOS dmg + zip `.app`、Linux deb + AppImage + tar.gz、Arch PKGBUILD；附 SHA256SUMS；README 写明未签名及原因、macOS 放行方式、Linux webkit2gtk 依赖、Wayland 降级。**不做单文件 exe、不用 UPX、不做任何签名或签名钩子。**

**Blocked by**: 02, 03, 04, 05, 06, 07, 08, 09（发布切片最后收口）

**Seam**: CI 产物清单校验（构建脚本/产物断言，非单元测试）

- [ ] 四个 job 全绿，产物清单与文件名约定齐全且可下载
- [ ] 便携包为目录形态，仓库内不存在 onefile 打包配置或 UPX 调用
- [ ] CI 中无签名步骤、无签名 secrets 分支、无公证脚本
- [ ] 每 Release 附 SHA256SUMS，README 覆盖四类首次使用说明
- [ ] **未验证项标注**：macOS / Linux / Arch 仅保证可构建，运行时手感与权限流程未本机验证，不得写成「已支持」

---

## Frontier

- **起始可执行**：01
- **01 完成后解锁**：02, 03, 05, 06, 08, 09
- **随后**：04（需 02+03）、07（需 05）
- **最后收口**：10（需 02–09 全部）

<!-- Wide refactor? 本项目无「expand → migrate batches → contract」型改动，全部工单均为 tracer bullet。 -->
