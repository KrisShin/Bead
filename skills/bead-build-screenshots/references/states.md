# 构建产物截图 · 状态手册

本手册只描述当前默认链路：**构建产物里的 web 资源 + headless 渲染**。
不启动 `bead.exe`，不读写 `%APPDATA%\dev.bead.app`，不需要可见桌面。

## 0. 一次完整流程

在仓库根目录执行：

```powershell
# 1) 构建（仅 exe，不打包安装器）。web/ 没变过可跳过。
npx tauri build --no-bundle

# 2) 把 tauri frontendDist（../web）逐字节暂存，并核对 exe 版本/时间/哈希
pwsh -File skills/bead-build-screenshots/scripts/prepare-frontend.ps1

# 3) 渲染全部状态到 .screenshots/build/
node skills/bead-build-screenshots/scripts/capture.mjs
```

也可以一条命令完成 1+2：

```powershell
pwsh -File skills/bead-build-screenshots/scripts/prepare-frontend.ps1 -Build
```

## 1. 确认“这就是要发布的那一份”

打开最近一次暂存目录的 `manifest.json`（`.screenshots/.build/frontend-<version>-<hash>/manifest.json`）：

| 字段 | 应该看到什么 |
|---|---|
| `appVersion` | 等于 `src-tauri/tauri.conf.json` 的 `version` |
| `artifact.version` | exe 的 ProductVersion，必须匹配 `appVersion` |
| `artifact.mtime` | 不早于 `web/` 里最新文件的修改时间 |
| `artifact.sha256` | 当前 exe 的内容哈希 |
| `treeHash` / `files[]` | 暂存 web 的整树哈希与逐文件 SHA-256 |
| `gitCommit` / `gitDirtyWeb` | 发布配图要求 `gitDirtyWeb=false` |

`capture.mjs` 每次启动前都会重算暂存 `web/` 的树哈希并与 `manifest.treeHash` 比对；
不一致直接拒绝出图。暂存目录被改动、或换了 web 却忘了重新构建，都会显式报错。

> 为什么“构建产物”就是 `web/`：本仓库 `build.frontendDist` 指向 `../web`，没有独立打包器，
> Tauri 把该目录逐字节嵌入可执行文件。暂存 = 复制同一份字节，再用哈希证明没有偏差。

## 2. 渲染是怎么发生的

- 本地静态服务器只服务暂存目录中的 `index.html`、`app.js`、`render.js`、`styles.css`、`assets/`。
- Page 级注入最小 `window.__TAURI__` stub，只替换 IPC 与事件：
  `get_state` 返回 fixture 状态，`panel_frame` 返回当前视口，`ocr_clipboard_image_for_lang`
  按状态返回挂起 / 空串 / 抛错，`event.listen` 注册 `state` 与 `ocr-progress`。
- 页面仍然执行真实的 `app.js` → `draw` → `renderBoard`，所以 DOM 结构、CSS、图标、
  卡片增删布局都来自构建产物，而不是另拼一份 HTML。
- 视口用 CDP `Emulation.setDeviceMetricsOverride` 设为应用真实逻辑尺寸。

## 3. 状态清单

`capture.mjs --list` 会同时打印可出图状态与 `captured:false` 的状态。

### 胶囊

| 状态 | 文件 | 触发 |
|---|---|---|
| 收起 | `bead-capsule-idle.png` | 初始快照 `view=capsule` |
| 悬停 | `bead-capsule-hover.png` | 同上，CDP 把鼠标移到 `#capsule` 中心 |

胶囊本身是 55% 不透明度的白块，纯白背景会和它混在一起；这两个状态使用浅灰蓝背板
（`#dfe6f0`）保证能看见轮廓，侧车里的 `backdrop` 字段会记录实际背板色。

### 面板与卡片

| 状态 | 文件 | 触发 |
|---|---|---|
| 2 / 3 / 4 / 5 卡 | `bead-panel-2.png` … `bead-panel-5.png` | fixture 直接给对应卡数，zh 为权威源 |
| 卡片悬停关闭按钮 | `bead-card-hover-close.png` | 3 卡 + CDP 悬停英文卡（`canClose` 仅 >2 卡成立） |
| 英文卡权威源 | `bead-card-authoritative-en.png` | `authoritative:true` 放在 en |
| 同步中 | `bead-card-syncing.png` | `status='syncing'` |
| 同步失败 | `bead-card-error.png` | `status='error'`，渲染重试按钮 |
| 空文档 | `bead-panel-empty.png` | 卡片文本为空 |
| 锁定 | `bead-panel-locked.png` | `locked=true` |

### 设置页

| 状态 | 文件 | 触发 |
|---|---|---|
| 顶部（provider + 通用开关） | `bead-settings-general.png` | `languagesOpen=true`，设置区滚到顶 |
| provider 选中 DeepL | `bead-settings-provider-deepl.png` | `provider.engine='deepl'`、`has_key=true` |
| 底部（TTS / OCR / 历史 / 更新） | `bead-settings-bottom.png` | 设置区滚到底 |
| OCR 内置已安装 | `bead-settings-ocr-bundled.png` | `ocr_installed=true`、`ocr_engine='bundled'` |
| OCR 系统方案 | `bead-settings-ocr-system.png` | `ocr_engine='system'` |
| OCR 未安装 | `bead-settings-ocr-not-installed.png` | `ocr_installed=false` |
| OCR 下载中 | `bead-settings-ocr-busy.png` | `ocr_busy=true` + 推一次 `ocr-progress` |
| OCR 错误 | `bead-settings-ocr-error.png` | `ocr_error` 非空 |
| 非法快捷键 | `bead-settings-hotkey-error.png` | `selection_hotkey_error` 非空 |
| 划词监听失败 | `bead-settings-watch-error.png` | `selection_watch_error` 非空 |
| 划词图标不可用 | `bead-settings-anchor-unsupported.png` | `selection_anchor_supported=false` |

### 历史、划词、缩略图

| 状态 | 文件 | 触发 |
|---|---|---|
| 历史列表 | `bead-history-list.png` | `historyOpen=true` + 3 条固定示例 |
| 历史空态 | `bead-history-empty.png` | `historyOpen=true`，列表为空 |
| 划词开启 | `bead-selection-on.png` | `settings.selection_enabled=true` |
| 缩略图 · 识别中 | `bead-ocr-thumb-pending.png` | 合成 `paste` 事件；OCR IPC 返回挂起 Promise |
| 缩略图 · 未识别到文字 | `bead-ocr-thumb-empty.png` | 合成 `paste`；OCR 返回空串 |
| 缩略图 · 识别失败 | `bead-ocr-thumb-failed.png` | 合成 `paste`；OCR 抛错 |

缩略图使用 `assets/ocr-sample.png`，只在页面内合成 `paste` 事件，不读系统剪贴板。

## 4. 不能从构建产物渲染的状态

索引中标记 `captured:false`，并写明原因，不用旧图或 P 图补：

- **贴边 clamp**：窗口位置与 clamp 在 Rust 侧计算，前端画面没有屏幕边缘坐标；
  只能看到面板本身，无法体现“贴边后被夹住”的屏幕位置。
- **卡片拖拽中**：需要按住 HTML5 drag 的瞬时中间态，静态渲染没有稳定代表帧。
- **展开/收起动画中间帧**：由窗口 resize 与内容补间共同驱动，单帧不具代表性。
- **划词选区气泡**：独立锚点窗口，需要真实选区与系统光标，不在主 web 包内。
- **更新徽标**：`update_latest` 没有写入路径，实际不可达。
- **测试连接结果**：`test_provider` 明确不回写界面状态。

## 5. 新增 / 修改状态

1. 在 `scripts/fixtures.mjs` 的 `STATES` 里加一项：
   `{ name, title, width, height, view, actions?, ocrMode?, trigger }`。
2. 需要点击 / 滚动 / 悬停 / 粘贴 / 进度事件时，用已有 `actions` 类型：
   `click`、`scroll`、`hover`、`paste`、`ocrProgress`。
3. 前端包确实无法表现的状态放进 `NOT_CAPTURABLE`，写清 `reason`。
4. 运行 `node ... --state <name>` 单独出图，检查 PNG 与同名 `.json`。

## 6. 产物与校验

- 目录：`.screenshots/build/`（`.screenshots/` 已在 `.gitignore` 中）。
- 每张图：`bead-<state>.png` + `bead-<state>.json`；索引：`index.json`。
- 侧车字段：`state`、`trigger`、`viewport`、`backdrop`、完整 `view`、
  `assets`（treeHash / appVersion / exe sha256 / exe version / git 状态）、
  `fixture=true`、`sampleContent=true`。
- 所有文案以“示例 / Sample / 例 / 예시 / Exemple …”开头，并在侧车与索引里声明为示例内容；
  不要把这些图当成实机翻译结果或真实用户数据。
- 旧实机图仍在 `.screenshots/live/`，属于历史存档，不进入本索引。
