---
name: bead-build-screenshots
description: 用 Bead 构建产物里的前端 web 资源渲染界面截图（胶囊、2–5 卡、主副语言、同步中/失败、缩略图 OCR、设置各区块、历史、锁定、划词开关等），产出带 sidecar 索引的 PNG。用于文档、PR、发布说明。不启动桌面应用、不改用户配置；译文为固定 fixture 示例。
---

# Bead 构建产物截图

本 skill 从**编译产物实际嵌入的前端资源**渲染图片，不启动桌面应用，也不读写用户配置。
产物在 `.screenshots/build/`，每张图有同名 `.json` 侧车，另有 `index.json` 汇总。

## 与另外两条链路的区别

| 链路 | 输入 | 产物 | 位置 |
|---|---|---|---|
| **本 skill** | Tauri 构建时嵌入的 `web/` 逐字节副本 | 真实前端包渲染 | `.screenshots/build/` |
| 旧 DOM 拼页 | 源码里的 `render.js` + 自定义 HTML | 布局草稿 | `.screenshots/*.png` |
| 旧实机脚本（已移除） | 桌面进程 + 系统截屏 | 历史存档 | `.screenshots/live/`（仅保留旧图，不再生成） |

旧的 `bead.ps1` / `backdrop.ps1` / `seed-state.ps1` / `cdp.mjs` 已从 skill 删除：它们会启动应用、
改写 `%APPDATA%\dev.bead.app`，与本 skill 的默认路径冲突。历史实机图仍在 `.screenshots/live/`，
**不要**把它们混进构建产物索引。

## 为什么“构建产物”就是 `web/`

`src-tauri/tauri.conf.json` 的 `build.frontendDist` 指向 `../web`，仓库没有独立前端打包器；
Tauri 构建会把该目录逐字节嵌入可执行文件。因此：
- 先在构建流程里产出 exe（`npx tauri build --no-bundle`，或完整 `npm run build`）；
- `prepare-frontend.ps1` 把 `web/` 复制到 `.screenshots/.build/frontend-<version>-<hash>/web/`；
- 复制后重新计算整树 SHA-256，必须与源目录一致；
- 渲染器只读这个暂存目录，且启动前再校验一次树哈希。

### 如何确认用的是要发布的那一份

`prepare-frontend.ps1` 输出的 `manifest.json` 里有：

1. `appVersion`：与 `tauri.conf.json` 的 `version` 一致；
2. `artifact.version`：exe 的 ProductVersion 必须匹配该版本；
3. `artifact.mtime`：exe 构建时间不得早于 `web/` 里最新文件；
4. `artifact.sha256`：exe 内容哈希；
5. `treeHash` + `files[]`：每个 web 资源的路径、大小、SHA-256；
6. `gitCommit` / `gitDirtyWeb`：发布配图要求 `gitDirtyWeb=false`。

`capture.mjs` 启动前会重算 `web/` 树哈希，与 `manifest.treeHash` 不一致就拒绝出图。
换了 web 资源却没重新构建时，脚本会明确报“exe 早于最新 web 资源”，不会静默用旧产物。

## 前置条件

- Node 22+。
- Chrome 或 Edge（自动查找常见安装路径；也可用 `BEAD_CHROME` 指定）。
- 已构建的 `target/release/bead.exe`（推荐）或 `target/debug/bead.exe`。
- 不需要桌面交互、不需要解锁屏幕、不会启动 `bead.exe`。

## 用法

在仓库根目录执行：

```powershell
# 1. 构建（首次或 web/ 变更后；仅 exe，不打包安装器）
npx tauri build --no-bundle

# 2. 暂存并校验构建资源
pwsh -File skills/bead-build-screenshots/scripts/prepare-frontend.ps1

# 3. 渲染全部状态（或只跑指定状态）
node skills/bead-build-screenshots/scripts/capture.mjs
node skills/bead-build-screenshots/scripts/capture.mjs --state panel-3 --state settings-bottom

# 查看状态名
node skills/bead-build-screenshots/scripts/capture.mjs --list
```

也可以一步完成构建 + 暂存：

```powershell
pwsh -File skills/bead-build-screenshots/scripts/prepare-frontend.ps1 -Build
```

## 渲染方式

- 本地静态服务器只服务暂存目录里的 `index.html`、`app.js`、`render.js`、`styles.css`、`assets/`。
- headless Chrome 通过 CDP 打开真实 `index.html`，在页面脚本之前注入最小 Tauri IPC stub，
  让 `app.js` 走真实渲染路径（`draw` + `renderBoard`），而不是另拼一份 HTML。
- fixture 只提供状态数据（卡片、设置、历史、OCR 状态）；不修改任何业务代码。
- 窗口尺寸由 `Emulation.setDeviceMetricsOverride` 设为应用真实逻辑尺寸：
  胶囊 68×28，2/3/4/5 卡分别为 414/613/812/1011 × 300。
- 应用本体是透明窗口。面板截图用白底；胶囊是 55% 不透明度的白块，纯白背板会看不见，
  因此胶囊状态改用浅灰蓝背板（`#dfe6f0`）。背板色写在侧车 `backdrop` 字段里。
- 设置页与历史是内联面板，通过点击头部按钮打开（与真实交互一致），再按需滚动内部容器。

## 覆盖状态

| 文件 | 状态 | 触发 |
|---|---|---|
| `bead-capsule-idle.png` | 胶囊收起 | 初始快照 `view=capsule` |
| `bead-capsule-hover.png` | 胶囊悬停 | CDP 移动鼠标到 `#capsule` |
| `bead-panel-2/3/4/5.png` | 2–5 卡 | 初始快照给对应卡数 |
| `bead-card-hover-close.png` | 卡片悬停关闭按钮 | 3 卡 + 悬停英文卡 |
| `bead-card-authoritative-en.png` | 英文卡为权威源 | `authoritative` 移到 en |
| `bead-card-syncing.png` | 同步中 | `status=syncing` |
| `bead-card-error.png` | 同步失败 + 重试 | `status=error` |
| `bead-panel-empty.png` | 空文档 | 卡片文本为空 |
| `bead-panel-locked.png` | 锁定 | `locked=true` |
| `bead-settings-general.png` | 设置顶部 | 点击设置按钮，滚到顶 |
| `bead-settings-provider-deepl.png` | provider 选中 DeepL | `provider.engine=deepl, has_key=true` + 点击设置 |
| `bead-settings-bottom.png` | 设置底部 | 点击设置按钮，滚到底 |
| `bead-settings-ocr-bundled.png` | OCR 内置已安装 | `ocr_installed=true, ocr_engine=bundled` |
| `bead-settings-ocr-system.png` | OCR 系统方案 | `ocr_engine=system` |
| `bead-settings-ocr-not-installed.png` | OCR 未安装 | `ocr_installed=false` |
| `bead-settings-ocr-busy.png` | OCR 下载中 | `ocr_busy=true` + `ocr-progress` 事件 |
| `bead-settings-ocr-error.png` | OCR 错误 | `ocr_error` 非空 |
| `bead-settings-hotkey-error.png` | 非法快捷键 | `selection_hotkey_error` 非空 |
| `bead-settings-watch-error.png` | 划词监听失败 | `selection_watch_error` 非空 |
| `bead-settings-anchor-unsupported.png` | 划词图标不可用 | `selection_anchor_supported=false` |
| `bead-history-list.png` | 历史列表 | 点击历史按钮 + 3 条 fixture |
| `bead-history-empty.png` | 历史空态 | 点击历史按钮，列表为空 |
| `bead-selection-on.png` | 划词开关开启 | `settings.selection_enabled=true` |
| `bead-ocr-thumb-pending.png` | 缩略图 · 识别中 | 合成 paste 事件，OCR Promise 挂起 |
| `bead-ocr-thumb-empty.png` | 缩略图 · 未识别到文字 | OCR 返回空串 |
| `bead-ocr-thumb-failed.png` | 缩略图 · 识别失败 | OCR 抛错 |

示例图片来自 `assets/ocr-sample.png`，只在页面内合成 `paste` 事件，不读系统剪贴板。

## 不能从构建产物渲染的状态

`index.json` 里以 `captured:false` 列出并写明原因：

- **贴边 clamp**：窗口位置由 Rust 计算，前端画面没有屏幕边缘坐标，无法体现 clamp 后的屏幕位置。
- 卡片拖拽中、展开/收起动画中间帧：需要连续交互或窗口 resize 时序，静态渲染无稳定代表帧。
- 划词选区气泡：独立锚点窗口，不在主 web 包内。
- 更新徽标：后端 `update_latest` 没有任何写入路径，实际不可达。
- 测试连接结果：`test_provider` 不回写界面状态。

这些不要用旧图或 P 图补，索引保持 `captured:false`。

## 产物与索引

- 目录：`.screenshots/build/`（`.screenshots/` 已 gitignore）。
- 每张图：`bead-<state>.png` + `bead-<state>.json`。
  侧车包含 `state`、`trigger`、`viewport`、完整 `view`、`assets`（树哈希、exe 哈希/版本、git 状态）、
  `fixture=true`、`sampleContent=true`。
- `index.json`：本次渲染的全部状态、未覆盖状态及原因、资源溯源摘要、示例内容声明。
- 图内文案统一以“示例 / Sample / 例 / 예시 / Exemple …”开头；不要把这些图当作真实翻译结果。

## 改动边界

只改本 skill 目录与 `.screenshots/build/`；不修改 `src-tauri/`、`web/` 业务代码，不写入
`%APPDATA%\dev.bead.app`，不启动桌面应用。构建产物只读；`prepare-frontend.ps1` 只复制并计算哈希。
