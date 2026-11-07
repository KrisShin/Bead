# 朗读（TTS）技术选型与设计

> 状态：**2026-11-07 已批准并实施 MVP**（工作区改动，未提交）。§9 五项均按建议获批；实施结果与真机验证缺口见 §7.2 / §7.3。
> 事实核查时间：2026-11-07。外部事实标注「已核实 / 估计 / 实测」，过期需重查。
> 当前实现：`bead-core/src/speech.rs`（音色匹配 / 切块 / 在线缓存）、`src-tauri/src/speech_local.rs`（SAPI 子进程）、`src-tauri/src/main.rs` 的 `SpeechService`、`web/app.js` 的 `speakCard` / `applySpeechEvent`。

## 0. 结论先行

1. **引擎**：MVP 继续用 **Windows 本机 SAPI5**（PowerShell `System.Speech` 子进程），补齐「按语言选音色、语速/音量、可停止、可抢占」。**本地优先**；在线只在该语言没有本地音色时作显式兜底，并在按钮上标「在线」。Edge 非官方朗读接口与 Piper 不进 MVP；`translate_tts` 已从默认路径降级为缺音色兜底（D30 与 README 已同步修订）。
2. **架构**：朗读放在 **Rust 侧**，新增独立 `SpeechService` 与独立的 `speech` 事件；**不进入** `Document`、`SyncCoordinator`、「翻译 epoch」与 `StateView` 快照。前端只负责 `<audio>` 播放与按钮状态，不用 `speechSynthesis` 做主路径。
3. **播放**：全进程一个朗读实例，**latest-wins 抢占**；同一张卡再点即停止；收起面板 / ESC / 点外部 / 清空 / 删卡 / 改该卡语言 / 该卡文本变化时立即停止；锁定、拖卡、其他卡编辑不打断。
4. **高亮**：MVP **不做逐句高亮**。卡片正文是 `<textarea>`，无法对子串着色；二期若要高亮，必须先改正文渲染形态，并只在能提供 word-boundary 的后端上做。
5. **划词线边界**：划词只负责把「文本 + 语言提示」送进同一个文档/面板；朗读入口复用面板卡片按钮，或调用同一个 `speak_text`，**不得另建引擎、队列或语音缓存**。

---

## 1. TTS 引擎选型对比

### 1.1 候选矩阵

| 方案 | 联网 | 中/英/日覆盖 | 首字延迟 | 离线 | 依赖与体积 | 许可 / 商用 | 失败与降级 |
|---|---|---|---|---|---|---|---|
| **SAPI5**（`System.Speech`，现状） | 否 | 本机实测：中 `Huihui/Kangkang/Yaoyao`、英 `Zira Desktop`；**无日语音色**（日 `Ichiro/Haruka/Ayumi` 需装日语包） | 未实测；PowerShell 进程启动 + 合成，量级 0.3–0.8s（估计） | 是 | 零新依赖，0MB | Windows 系统组件，随系统可用 | 该语言无音色 → 在线兜底或置灰 |
| **OneCore**（WinRT `Windows.Media.SpeechSynthesis`） | 否 | **本机探针实测**：`AllVoices` 只有 3 个 zh-CN 音色（Huihui / Yaoyao / Kangkang），**无 en/ja、无 Natural Voices** | zh 28 字：合成出流 80ms | 是 | WinRT feature 只在 `demo/tts-probe` 打开；主程序未接入，产品包 0MB 增量 | MIT/Apache | 失败 → SAPI5 |
| **WebView2 `speechSynthesis`** | 否 | 仅能拿到 **SAPI5** 音色（Chromium on Windows 走 SAPI5），拿不到 OneCore/Natural | 异步，`voiceschanged` 后可用 | 是 | 0 新依赖 | 随 WebView2 | Linux WebKitGTK 常无此 API；Chrome 71 起需用户手势 |
| **Edge 在线朗读（非官方 WS）** | 是 | 多语种、音质最好一档 | 几百 ms（估计） | 否 | 需 WebSocket 客户端 | **无官方授权文件**；2026-10 起经典端点大量 403，需重算签名 | 失败 → 本地 |
| **Google `translate_tts`（现状）** | 是 | 主流语种 | 本机实测首字节 1.96s（单段 9216 字节） | 否 | 现有 `ureq`，0MB | **非公开端点、无授权**；易 403/429；本实现按 200 字符切块 | 失败 → 明确报错 / 提示 |
| **Piper 本地神经 TTS** | 否 | 中文可用；日语模型 2026-10 才加入，质量未核实 | 0.1–0.3s（估计） | 是 | 单音色 20–60MB + onnxruntime ≈20MB → **超出 8–15MB 包体预算** | 旧 `rhasspy/piper` MIT 已归档；新 `piper1-gpl` **GPL-3.0**；各 voice 许可能含非商用 | 失败 → SAPI |
| **云 TTS**（Azure / 火山 / 阿里 / 腾讯 / Google Cloud） | 是 | 全语种、音质最好 | 200–800ms（估计） | 否 | 需账号与 key；0MB | 各厂商条款；国内需实名；免费额度有限（Azure F0 神经语音 0.5M 字符/月、20 请求/60s） | 失败 → 本地 |

### 1.1.1 OneCore 探针实测（2026-11-07，`demo/tts-probe`）

独立 crate（自带 `[workspace]`，不并入主程序），`windows 0.62.2` 打开 `Foundation / Storage_Streams / Media_Core / Media_SpeechSynthesis / Win32_System_WinRT`。`cargo run` 真实输出：

```text
{"phase":"voices","ok":true,"count":3,"natural_count":0,"voices":[
 {"display":"Microsoft Huihui","language":"zh-CN",...},
 {"display":"Microsoft Yaoyao","language":"zh-CN",...},
 {"display":"Microsoft Kangkang","language":"zh-CN",...}]}
{"phase":"synth","lang":"zh","ok":true,"voice":"Microsoft Huihui","stream_ready_ms":80,"read_ms":0,"bytes":179406,"content_type":"audio/wav","write_ok":true}
{"phase":"synth","lang":"en","ok":false,"error":"no_voice_for_language"}
{"phase":"synth","lang":"ja","ok":false,"error":"no_voice_for_language"}
```

结论（如实）：

- 本机 WinRT **只枚举到 3 个 zh-CN 音色，没有 en/ja，也没有任何 Natural Voices**；`AllVoices` 返回的 id 指向 `Speech_OneCore` 注册表。
- zh 合成可用：28 字 80ms 出流、WAV 179,406 字节、读取与落盘成功；产物是 **WAV**（若接入需让缓存扩展名跟随 `ContentType`）。
- 因此 **MVP 不接 OneCore**：语种覆盖反而不如 SAPI（同一台机器 SAPI 有 zh-CN + en-US）。OneCore 的价值（音质、延迟）在本机拿不到，接入只会多一套后端与格式分支。
- 同一脚本枚举 SAPI 的真实结果：`Microsoft Huihui Desktop`(zh-CN)、`Microsoft Zira Desktop`(en-US)、`Microsoft Huihui` / `Kangkang` / `Yaoyao`(zh-CN)；**无 ja** → ja 卡在该机器上只能走在线兜底或置灰。

### 1.2 推荐与理由

**推荐（已实施）：MVP = 本地 SAPI5 优先 + 在线仅做缺音色兜底；OneCore 暂不接入（探针结论见 1.1.1）。**

理由按权重排序：

1. **产品形态决定延迟与隐私优先**。朗读是「点一下立刻出声」的即时反馈；有本地音色时零网络，原文不出本机也符合仓库隐私基线。
2. **包体与依赖红线**。Piper 单音色就超过整个安装包预算，且新工具链是 GPL-3.0，与 MIT 单许可（D31）冲突；云 TTS 需要 key 与账号，破坏「零配置」。
3. **合规**。Edge 非官方接口与 Google `translate_tts` 都没有商用授权，且 Edge 端点 2026-10 起大面积 403。它们可以做实验，不能做默认。
4. **现有实现可复用**。SAPI5 已经在跑，改造集中在「选音色 / 可停止 / 不阻塞」，不需要新 crate。
5. **OneCore 的收益在本机不成立**：探针只看到 zh-CN legacy 音色，en/ja 缺失。等目标机器能枚举到对应语种（或 Natural Voices）再评估，不先写进承诺。

**明确不选**：

- Edge 非官方 WS：稳定性与授权双高风险，拒绝作为默认或商用路径。
- Google `translate_tts`：与 Edge 同属灰区；本机实测可达但单段约 2s（2026-11-07，HTTP 200 / audio/mpeg），只保留为「无本地音色时的在线兜底」，并在 UI 标示。
- Piper：体积、GPL、日语质量三项都不满足本阶段。
- 云 TTS：留作二期「用户自带 key」的可选增强，不进 MVP。

**现有决策的修订（用户 2026-11-07 批准，已落实）**：

- D6：由「默认联网、失败降级离线」改为 **本地优先、在线兜底**。
- README 第 17 行已同步改写。
- D30：在线链路限定为「缺本地音色才触发」；M9 设置结构与 M4 契约变更一并获批。

---

## 2. 语种匹配

### 2.1 语言码 → Windows culture

仓库里的语言码是 ISO-639-1（`zh/en/ja/...`，见 `web/components/index.js` 的 `LANG_NAMES`）。Windows 语音按 `Culture` 区分，需要显式映射，不能把 `zh` 直接扔给引擎。

| 卡片语言 | 首选 culture | 次选 | 典型音色 |
|---|---|---|---|
| zh | zh-CN | zh-TW → zh-HK → 任意 zh-* | Huihui / Kangkang / Yaoyao |
| en | en-US | en-GB → en-AU → 任意 en-* | Zira / David |
| ja | ja-JP | 任意 ja-* | Haruka / Ichiro / Ayumi（需语言包） |
| 其他池内语言 | 对应主 culture（fr-FR / de-DE / es-ES / ru-RU / pt-BR / it-IT / ar-SA / th-TH / vi-VN / id-ID / nl-NL / tr-TR / ko-KR） | 同主语言的其他 region | 视系统安装情况 |

### 2.2 选择算法（纯函数，可单测）

输入：卡片语言、当前已安装音色列表、用户覆盖配置（二期）。输出：`SelectedVoice | NoLocalVoice | Unavailable`。

1. 精确 culture 匹配且 `enabled`；
2. 同主语言标签的其他 region；
3. 无匹配 → 若允许在线兜底：`NoLocalVoice`（走在线）；否则 `Unavailable`；
4. **禁止**用英语音色读中文/日文来「凑合」——错误发音比不可用更糟。

`已安装音色列表` 必须在**每次朗读时或设置页打开时重新枚举**，不能启动时缓存：用户可能在应用运行期间安装语言包。

### 2.3 没有对应音色时

按规范第 8 条，**不静默 no-op**：

- 卡片朗读按钮置 `disabled`，`title` 写明「未安装 xx 语音 · 可在 Windows 设置 → 时间和语言 → 语音 添加」；
- 若在线兜底开启，按钮保持可用，但加一个 **「在线」角标**，并在首次使用时给一次性轻提示「此语言将使用在线朗读」；
- 设置页在「语音」分组展示每种池内语言的可用性（可用 / 仅在线 / 不可用），只读，不做引擎选择。

### 2.4 是否允许手动指定音色

- **MVP 不做**：自动按 culture 选第一个可用音色。理由：Windows 同语言多音色场景少，先避免设置结构膨胀。
- **二期可做**：按语言选择音色（仅列出系统已安装、该 culture 的音色），持久化到设置；这属于「音色覆盖」，**不是引擎开关**，不违反「不向用户提供手动指定引擎的开关」。
- 任何情况下都不暴露「SAPI / OneCore / 在线」的引擎单选。

---

## 3. 播放控制

### 3.1 状态机

```
Idle ──speak──> Resolving(查音色/查缓存/在线取字节) ──> Playing(local|file) ──> Idle
                       │                                     │
                       ├─ 失败 ──> Error(轻提示，2s 后回 Idle)
                       └─ 被新请求抢占 / stop ──> Cancelled ──> Idle
```

- 状态只存在于 Rust 的 `SpeechService`，前端不维护「我是不是在播」的业务判断。
- `Resolving` 允许被新请求抢占；晚到的旧结果按 ticket 丢弃，**绝不补播**。

### 3.2 互斥与抢占

- **全局单实例**：主面板、划词结果小窗共用同一个控制器。
- **latest-wins**：点 B 卡朗读 → 立即取消 A 的在线请求（若尚未开始）或 kill A 的本地进程 → 播 B。
- **同卡再点 = 停止**（toggle），按钮切到「停止」图标。
- 不做队列；连续朗读二期再做（见 §7）。

### 3.3 停止时机（策略表）

| 事件 | 行为 | 理由 |
|---|---|---|
| 点击同一张卡的朗读 | 停止 | 最简单的显式控制 |
| 点击另一张卡朗读 | 抢占 | 用户意图明确 |
| 收起面板（ESC / 点外部 / 离开宽限到期） | 停止 | 面板不可见后继续出声是「幽灵播放」 |
| 清空文档 | 停止 | 内容已不存在 |
| 删除正在朗读的卡 | 停止 | 同上 |
| 改正在朗读的卡的语言 | 停止 | 语言变了，音色与文本都不再对应 |
| 提交编辑且该卡文本变化 | 停止 | 读旧文本会误导 |
| 锁定 / 解锁、拖动卡片、编辑其他卡 | **继续** | 不改变正在读的内容 |
| 面板增删卡动画 | **继续** | 朗读不得反向影响窗口时序 |
| 应用退出 | 停止并 kill | 不留后台进程 |

### 3.4 语速与音量

- SAPI5 支持 `Rate`（-10..10）与 `Volume`（0..100）；映射为 UI 的 0.5×–2× 与 0–100%。
- 在线预渲染音频只能用 `<audio>.playbackRate` 与 `.volume`；`playbackRate` 会改变语速但不重合成，音质可接受；但**缓存键必须带语速与音色**，否则会播到旧参数生成的音频。
- 能力差异要在 UI 上体现：本地朗读可调语速，在线音频的语速调节属于播放层倍速（不承诺音色一致）。

### 3.5 长文本

现状 `GoogleTts::fetch` 用 `chars().take(200)` 静默截断；SAPI 走 PowerShell 时超长文本还受命令行长度限制。MVP 必须：

- 本地朗读按**句/标点**切块，顺序播放，块之间不重启引擎（或重启但保持进度）；
- 在线兜底按端点上限切块（≤200 字符），顺序请求播放；
- 任何截断都不能静默发生。

### 3.6 逐句高亮

- **MVP 不做**。两个硬约束：
  1. 卡片正文是 `<textarea class="card__text">`（`web/components/index.js`），`textarea` 不能对子串加样式；
  2. Google 音频文件没有边界信息，SAPI5 的 word boundary 事件只有走 COM 才能实时拿到（PowerShell 子进程跨进程拿不到可靠时间轴）。
- **二期路径**：先把正文改为「只读展示层（可高亮）+ 编辑态 textarea」双层结构；后端输出 `SpeechProgress { chunk_index, char_start, char_end }` 事件；只按事件高亮，前端**不做计时估算**。能提供边界的后端只有 SAPI COM（词级）与 Edge WS（词级），在线文件与 OneCore 都没有。

---

## 4. 与现有架构的关系

### 4.1 Rust 侧 vs 前端 `speechSynthesis`

**结论：Rust 侧负责策略、缓存、合成；前端只负责播放文件与展示状态。**

理由：

1. **状态单一**。朗读需要跨面板 / 划词小窗 / 托盘共用一个实例；放前端会让每个窗口各有一套 Audio，抢占规则无法保证。
2. **能力覆盖**。WebView2 的 `speechSynthesis` 在 Windows 上仍只映射 SAPI5 音色，拿不到 OneCore；Linux WebKitGTK 经常没有该 API。用它做主路径等于把已有后端优势丢掉。
3. **缓存与降级**。现有「在线取字节 → 落盘 → `<audio>`」链路必须留在 Rust；`speechSynthesis` 无法缓存，也无法统一在线/本地降级。
4. **可测试性**。语言匹配、抢占、停止策略是纯逻辑，放 `bead-core` 可被 `#[test]` 覆盖；放前端只能靠组件测试与真机。

`speechSynthesis` 仅保留为**未验证的能力探测项**（若未来 OneCore 无法接入，可作为低保真兜底），不写入 MVP。

### 4.2 新增状态

新增一个与文档状态完全平行的控制器：

```
SpeechService
  ├─ ticket: AtomicU64              // 最新请求号
  ├─ active: Mutex<Option<Active>>  // 本地进程句柄 / 在线请求令牌
  ├─ cache: Mutex<CacheIndex>       // 只在文件 IO 时短暂加锁
  └─ policy: SpeechPolicy           // 纯函数：选音色、停止判定、切块
```

对外只暴露：

- 命令：`speak_card { lang }`、`speak_text { text, lang, origin }`、`stop_speaking {}`
- 查询：`speech_state {}`（用于前端首次挂载与断线恢复）
- 事件：`speech`（`{ phase, lang, backend, chunk, error? }`），**不塞进 `StateView`**

### 4.3 与翻译链 / SyncCoordinator / 动画时序的解耦

- 朗读只做**一次同步读取**：在拿 `coordinator` 锁时复制 `slot.text` 与 `lang`，随后释放锁再做网络/合成；不得在合成期间持有 coordinator 锁。
- 朗读不递增、不读取 `translation_epoch`；翻译完成也不自动触发朗读。
- 朗读不改变 `generation`，不写 `Document`，不触发 `state` 事件。
- 前端收到 `speech` 事件只更新对应按钮的 class/aria 状态与音量条，**不调用 `draw()`**，不参与 `panelAnimating` 的 pending 队列，不影响窗口 resize 的任何一帧。

### 4.4 与现有实现的冲突点（必须一并处理）

| # | 冲突 | 位置 | 后果 | 处理 |
|---|---|---|---|---|
| 1 | `speak` 字段塞在 `StateView`，而 `snapshot()` 每次都写 `speak: None` | `main.rs` 的 `StateView` / `snapshot` | 任何无关 state 刷新都会清掉朗读结果；状态与文档快照耦合 | 从 `StateView` 移除，改独立 `speech` 事件 |
| 2 | `speak_card` 返回整份 `StateView`，前端 `send()` 会自动 `draw()` | `main.rs` / `web/app.js` 的 `send` | 每点一次朗读都整板重渲染；点击时 textarea 失焦，编辑态可能被打断 | 命令只返回 `{ kind, url }`，不返回视图 |
| 3 | `Mutex<SpeechManager>` 在 `spawn_blocking` 内被整段持有 | `main.rs` `speak_card` | 在线 2.5s 超时期间，第二次点击排队；SAPI 请求也会被串行拖慢 | 锁只保护缓存与 active 句柄，网络与合成在锁外 |
| 4 | `SapiSpeaker` 忽略 `_language` | `main.rs` | 中文卡可能用英文音色读，中文/日文基本不可懂 | 按 culture 枚举与选择音色 |
| 5 | 在线 `chars().take(200)` 静默截断 | `bead-providers/src/google_tts.rs` | 长卡只读前 200 字，用户以为读完了 | 切块 + 明确失败 |
| 6 | 缓存键只含 `(lang, text)` | `bead-core/src/speech.rs` | 改语速/音色后仍命中旧音频；不同后端产物混用 | 键含 `backend + voice_id + rate + text_hash` |
| 7 | 没有停止能力；SAPI 子进程句柄被丢弃 | `main.rs` | 连续点击会叠音；面板收起后仍在读 | 保存 `Child`，抢占/停止时 kill |
| 8 | 前端 `currentAudio` 只在换 src 时暂停，收起/清空/删卡不暂停 | `web/app.js` | 面板消失后继续播放 | 统一 stop 事件驱动 |
| 9 | 播放失败 `.catch(() => {})` 静默吞掉 | `web/app.js` `playSpeak` | 违反规范第 8 条「不得静默 no-op」 | 出轻提示并复位按钮 |
| 10 | 朗读读取 Rust 文档文本，编辑中未提交时读到旧值 | `main.rs` `speak_card` | 刚改的字读不出来 | 点击朗读先 flush 编辑（复用 `edit-sync`），再发 `speak_card`；不把 textarea 值直传 Rust 绕过同步链 |
| 11 | `assetProtocol.scope` 固定 `$TEMP/bead-tts/*`，缓存扩展名固定 `.mp3` | `tauri.conf.json` / `speech.rs` | 若 OneCore 产出 WAV 会播不了 | 格式随后端走，scope 保持目录级 |
| 12 | `api` 目前没有「朗读中」状态给按钮 | `web/components/index.js` | 用户无法停止，也不知道在播 | 按钮增加 `playing` 态与停止图标 |

**实施状态（2026-11-07）**：1–12 已逐项处理——`StateView` 已移除 `speak`（1）；`speak_card` / `speak_text` 只返回 `SpeakResult`（2）；锁只保护缓存与 active 句柄，网络/合成在锁外（3）；SAPI 按 culture 选音色（4）；超长文本由 `SpeechManager` 按 200 字切块，`GoogleTts::fetch` 对 >200 字符直接报错而非截断（5）；缓存键含 `backend+voice_id+rate+lang+text`（6）；`Child` 句柄保存并可 kill（7）；前端统一 `stopSpeaking`（8）；播放失败出轻提示并复位（9）；朗读前 flush 编辑（10）；在线产物仍为 MP3、asset scope 保持目录级（11）；按钮有 `is-speaking` 态与 `aria-pressed`（12）。

---

## 5. 划词 / 小窗口场景

### 5.1 接口边界（与另一条会话对齐）

划词线负责：全局热键、取词、选区旁图标、把文本送进面板。朗读线负责：从「已有文本 + 已知语言」到「出声」。

建议的边界：

```
SelectionCapture ── { text, lang_hint } ──> SyncCoordinator.accept_text()
                                                   │
                                                   ▼
                                          面板卡片（现有 UI）
                                                   │
                                          speak_card(lang)
                                                   │
                                    ┌──────────────┴──────────────┐
                                    ▼                             ▼
                        主面板内的朗读按钮            划词小窗内可选 speak_text()
                                    └──────────────┬──────────────┘
                                                   ▼
                                        同一个 SpeechService（单实例）
```

规则：

1. **朗读不参与取词**。不得在取词/图标链路里读选区或剪贴板；TTS 只接收上层已经拿到的文本。
2. **推荐形态：图标 = 打开/送入面板，朗读只在面板卡片内**。理由：语言上下文、编辑态、停止规则都已在面板内成立，不再造第二套 UI。
3. 若划词线最终做「小窗内直接朗读」，小窗必须调用同一个 `speak_text`，并复用同一套音色匹配、抢占、缓存、停止策略；**禁止**在小窗里单独初始化引擎或单独维护 Audio。
4. 小窗隐藏 / 失焦关闭时，按 §3.3 停止；不得出现「小窗关了声音还在」。
5. 图标出现时**不自动朗读**；自动朗读会与翻译链完成时机、面板展开动画竞争，且默认被动原则不支持。

### 5.2 与窗口链路的边界

朗读**不新增窗口、不改窗口几何、不走 `resize_window` / `panel_frame` / `place_panel`**。小窗的定位逻辑完全归划词线；朗读只是小窗里的一个按钮。

---

## 6. 隐私与系统行为

### 6.1 文本是否离开本机

- **本地 SAPI / OneCore**：不离开本机，无网络请求。
- **在线兜底（现状 Google）**：整段文本放在 **GET 查询串**里发往 `translate.googleapis.com`，本机代理、抓包工具、DNS/URL 日志都可能留存。2026-11-07 本机实测该端点**可达**（HTTP 200 / `audio/mpeg` / 约 2s），但只在缺本地音色时触发。这是现存的隐私事实，必须在设置页与 README 写清楚。
- **建议**：本地优先后，在线只在缺音色时触发；按钮显示「在线」角标；首次触发给一次性提示。二期接云 TTS 时改用 POST + 官方 SDK/端点，不把文本放 URL。

### 6.2 日志

- **不写原文、不写朗读文本、不写缓存文件名中的文本**（缓存名是 `lang + 哈希`，现状已满足）。
- 只记录结构化事件：`speech_start / speech_stop / speech_error`，错误码枚举（`NO_VOICE`、`NETWORK`、`SYNTH_FAILED`、`AUDIO_PLAY_FAILED`），不带文本。
- 不把 `SpeechError` 的 debug 字符串直接进用户可见日志；错误详情可保留在内存，随进程结束丢弃。

### 6.3 缓存

- 目录：`%TEMP%\bead-tts`（现状），只存音频，不存文本。
- 需要加**容量上限**（建议 50MB）与 LRU/按时间清理；否则长期使用会无限增长。
- 退出时是否清空：MVP 不建议清空（重复朗读命中缓存是体验的一部分），但提供设置项「清空朗读缓存」；文件随系统临时目录清理最终回收。

### 6.4 系统音量、混音与打断

- Windows 共享模式混音：SAPI 与其他播放器可同时发声，会叠音；本应用**不使用独占模式**，不抢设备。
- 系统「通信活动时降低其他声音音量」默认会让非通信流衰减约 80%，SAPI 属于普通流，可能被 Teams 等通信应用压低——这是系统行为，不是缺陷，UI 不承诺「永远清晰」。
- 多实例同时朗读是本应用自己的问题：靠单实例 latest-wins 解决，不依赖系统混音。
- 音量控制：本地走 SAPI `Volume`；在线走 `<audio>.volume`；不覆盖系统主音量。

---

## 7. MVP 切分

### 7.1 第一版必须做

| # | 内容 | 涉及文件 / 新增模块 | 依赖 | 真机验证 |
|---|---|---|---|---|
| M1 | 语言 → 音色匹配纯函数；无音色判定；停止事件判定；长文本切块 | `bead-core/src/speech.rs`（扩展现有模块，新增 `SpeechPolicy` / `SpeechTicket` / `chunk_text`） | 无 | 单测覆盖 zh/en/ja、多音色、无音色、空文本 |
| M2 | 独立 `SpeechService`：ticket 抢占、active 句柄、缓存锁分离 | `src-tauri/src/speech_local.rs`（Windows 本地朗读）；`main.rs` 的 `AppState` | 无 | 快速连点两卡不叠音；旧请求晚到不补播 |
| M3 | SAPI 按 culture 选音色 + Rate/Volume + 可 kill | `src-tauri/src/speech_local.rs` | 无（沿用 PowerShell） | 中/英/日各读一段；停止能立刻静音；语速/音量生效 |
| M4 | 朗读状态改为独立 `speech` 事件；`StateView` 移除 `speak`；命令不再返回整份视图 | `main.rs`、`web/app.js` | 无 | 朗读时翻译完成不会打断，也不重渲染整板 |
| M5 | 前端停止规则：收起/ESC/点外部/清空/删卡/改语言/改文本即停；按钮 playing 态 | `web/app.js`、`web/components/index.js`、`web/styles.css` | 无 | 逐条走 §3.3 表格 |
| M6 | 长文本切块，移除 200 字静默截断；缓存键加 `backend+voice+rate` | `bead-providers/src/google_tts.rs`、`bead-core/src/speech.rs` | 无 | 500 字中文卡完整读完；改语速后重新生成 |
| M7 | 无音色时按钮 disable + 明确提示；在线兜底显示「在线」角标 | `web/components/index.js`、`web/styles.css`、`main.rs` | 无 | 卸载日语语音后 ja 卡按钮置灰且提示可操作 |
| M8 | 错误轻提示，禁止静默吞错 | `web/app.js`、`main.rs` | 无 | 断网 + 无本地音色时出现提示且按钮复位 |
| M9 | 语速/音量设置（默认 1.0× / 系统音量） | `src-tauri/src/settings.rs`、`web/render.js`、`web/app.js` | 无 | 重启后设置保留 |
| M10 | 编辑未提交时点朗读先 flush | `web/app.js` | 无 | 输入后立刻点朗读，读到的是新文本 |

**M9 涉及设置结构，按 L3 事前确认。** 其余为 L2；M4 的 `StateView` 字段变化属于前后端契约，按 L3 处理并需要一次小型探针（真机确认事件路径）。

**依赖结论**：MVP **不新增任何 crate**；在线兜底沿用现有 `ureq`；SAPI 沿用 PowerShell。OneCore 探针是独立 `demo/tts-probe` crate，`windows` feature 只加在探针里，不进主程序。

### 7.2 实施结果（2026-11-07）

| # | 落地位置 | 自动验证 |
|---|---|---|
| M1 | `bead-core/src/speech.rs`：`cultures_for` / `choose_voice` / `chunk_text` | `cargo test -p bead-core`：78 passed，含音色精确/同主语言/禁用/缺失、切块每块 ≤200、超长硬切、短文本不切 |
| M2 | `src-tauri/src/main.rs` 的 `SpeechService`（ticket / active / voices / status） | 编译通过；latest-wins 与晚到结果丢弃为代码路径，未做并发真机测试 |
| M3 | `src-tauri/src/speech_local.rs`：临时文本文件 + PowerShell `System.Speech`、`Rate=-10..10`、`Volume`、`try_wait`、`kill`、非零退出上报 | `cargo test -p bead speech_local`：3 passed（rate 映射、单引号转义、音色 JSON 解析）；套件共 27 passed |
| M4 | `StateView` 移除 `speak`；`speak_card` / `speak_text` 返回 `SpeakResult`；独立 `speech` 事件 | `cargo check --all-targets` 通过；`npm test` 通过（前端套件） |
| M5 | `web/app.js`：`speakCard`（同卡再点停止）、`beginCollapse` 停止、clear / remove / change_lang / commit_edit 的 Rust 侧停止钩子 | 逻辑覆盖；真机逐条未验 |
| M6 | `SpeechManager` 切块拼 MP3；缓存键 `SpeechProfile{backend,voice_id,rate}+lang+text`；`GoogleTts::fetch` >200 报错 | `cargo test -p bead-providers`：36 passed（新增超长拒收用例） |
| M7 | `speech_capabilities` → 按钮 disable / `is-online` 角标；设置页隐私提示 | 命令逻辑编译通过；ja 置灰需真机确认（本机无日语包） |
| M8 | `speakFailed` → `.speech-hint` 轻提示 + 按钮复位 | 逻辑覆盖；断网真机未验 |
| M9 | `settings.rs`：`tts_rate`(0.5–2.0, 默认 1.0) / `tts_volume`(0–100, 默认 80) / `tts_online_fallback`(默认 true)；`web/render.js` 控件 | `cargo test` 通过；重启保留需真机确认 |
| M10 | `web/app.js` `speakCard`：先 `editDebouncer.cancel` + `commitEdit`，再 `speak_card` | 逻辑覆盖；真机未验 |

`cargo test --all` 实测：`bead`（src-tauri）27 passed / `bead-core` 78 passed / `bead-providers` 36 passed；`npm test` 实测 4 个文件 46 passed。另：工作区还有另一条会话（划词）的未提交改动，本轮未触碰其逻辑。

### 7.3 未验证 / 未实现（如实标注）

**需要真机听感/交互验证（无法自动断言）**：

- 停止/抢占是否**真的立刻静音**（SAPI 子进程 kill 后即时生效）；快速连点两张卡不叠音。
- 语速/音量是否按设置生效（`Rate=-10..10` 与 `Volume=0..100` 的听感映射）。
- 日语卡在无日语语音包时按钮置灰、在线兜底角标与轻提示的实际观感。
- `<audio>` 通过 Tauri asset 协议播放 `%TEMP%\bead-tts\*.mp3` 的真实链路（含打包后的 scope）。
- 面板收起 / ESC / 点外部 / 清空 / 删卡 / 改语言 / 改文本逐条停止。
- 编辑后立刻点朗读读到新文本；改语速后缓存键切换会重新请求。
- 在线兜底本机实测端点可达（HTTP 200 / audio/mpeg / 约 2s），但**未在应用内走完整链路**。

**设计中的建议项，本轮未实现**：

- 首次在线兜底触发的一次性提示（当前用按钮「在线」角标 + 设置页说明替代）。
- 缓存容量上限 50MB / LRU 清理、`speech_start/stop/error` 结构化日志（§6.2 / §6.3，MVP 未列入）。

### 7.4 可后置

| 项 | 原因 | 前置条件 |
|---|---|---|
| OneCore / Natural Voices（`windows` crate `Media_SpeechSynthesis`） | 音质与延迟收益明确，但 Natural 第三方可用性未验证 | 先在 `demo/` 做探针：能否枚举音色、能否合成、首字延迟、增量体积；业主批准新 feature |
| 按语言手动选音色 | 设置结构与 UI 成本 | 设置结构批准后 |
| 连续朗读 / 队列 | 需要「读到哪、下一张是谁、停止边界」完整规则 | 单卡体验稳定后再做 |
| 逐句高亮 | 依赖 textarea 改造 + 后端边界事件 | 先改正文渲染形态；SAPI COM 或 Edge WS（如获批） |
| 用户自带 key 的云 TTS | 账号、key、隐私披露、计费 | 单独选型；仅对缺音色的语言 |
| 缓存上限与清理 UI | 长期使用才暴露 | 缓存实际增长数据 |
| 划词小窗内 `speak_text` | 依赖划词线先落地小窗 | 与划词线共用 `SpeechService` |

### 7.5 明确不做

- Edge 非官方朗读接口作为默认或商用路径。
- Piper / onnxruntime 本地神经模型。
- 任何「选择 TTS 引擎」的用户开关。
- 翻译完成自动朗读（会与同步链、面板动画竞争，且违背默认被动）。
- 朗读直接读取剪贴板或选区（取词归划词线）。
- 为朗读新增窗口、修改 `resize_window` / `place_panel` / 面板时序。
- 朗读状态写入 `Document` / `SyncCoordinator` / 翻译 epoch。

---

## 8. 接口（已实现）

```rust
// bead-core/src/speech.rs（已实现）
pub struct SpeechProfile { pub backend: String, pub voice_id: String, pub rate: i32 }
pub struct VoiceInfo { pub id: String, pub culture: String, pub enabled: bool }
pub fn cultures_for(code: &str) -> &'static [&'static str];
pub fn choose_voice(lang: &Lang, voices: &[VoiceInfo]) -> Option<&VoiceInfo>;
pub fn chunk_text(text: &str, max: usize) -> Vec<String>;   // 每块 ≤ max 字符，不静默截断

impl<O: OnlineTts> SpeechManager<O> {
    pub fn request(&self, lang: &Lang, text: &str, profile: &SpeechProfile) -> Result<PathBuf, SpeechError>;
}
```

```rust
// src-tauri（已实现）
#[tauri::command] async fn speak_card(lang: String) -> SpeakResult;               // { kind, ticket, lang, backend, url, error, reason }
#[tauri::command] async fn speak_text(text: String, lang: String) -> SpeakResult; // 划词/小窗复用同一 Service
#[tauri::command] fn stop_speaking(reason: String, app) -> SpeechView;            // user / panel_hidden / ...
#[tauri::command] fn speech_state(app) -> SpeechView;                             // 首次挂载与断线恢复
#[tauri::command] async fn speech_capabilities(app) -> SpeechCapabilitiesView;   // 按语言的 local/online/none + 语速/音量
#[tauri::command] fn speech_finished(ticket: u64, app);                           // `<audio>` onended
#[tauri::command] fn speech_failed(ticket: u64, lang: String, message: String, app); // `<audio>` onerror
// 事件：app.emit("speech", SpeechView)
```

约束（均已落实）：

- `speak_text` 只接受上层已捕获的文本，长度上限由后端切块；空文本在 `begin` 前返回 `kind:"empty"`，不抢占正在播放的卡片。
- `stop_speaking` 的 `reason` 由 Rust 内部再校验为已知枚举值，前端只传 `user` / `panel_hidden`。
- `SpeechView` 不包含卡片正文，只含 `phase/lang/backend/ticket/error/reason`。
- 前端**不得**把 `speak_card` / `speak_text` 的返回值当作整板视图；只有 `speak_card` 的 `kind:"file"` 带 `url`，由前端 `<audio>` 播放。

---

## 9. 业主确认（2026-11-07 全部批准）

1. **D6/D30 修订**：已改为「本地优先、在线仅缺音色兜底」，README 第 17 行已同步；D6/D30 已在 STATE.md 改写。
2. **M9 设置结构**：已新增 `tts_rate` / `tts_volume` / `tts_online_fallback`；`tts_voice_overrides` 仍留二期。
3. **M4 契约变更**：`StateView` 已移除 `speak`，新增独立 `speech` 事件与 `SpeakResult`。
4. **OneCore 探针**：已在 `demo/tts-probe` 完成并实测（结果见 §1.1.1）；结论是不接入主程序。
5. **在线兜底**：保留且**默认开启**，仅缺音色时触发，按钮标「在线」。

按 `Agents.md` 改动分级：M9 设置结构、M4 契约变更、OneCore 探针均属 L3，已获批准后实施；本轮改动留在工作区，未提交、未推送、未打包。
