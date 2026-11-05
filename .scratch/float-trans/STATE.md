# FloatTrans — 全平台悬浮窗多向翻译桌面应用

- **Slug**: `float-trans`
- **Tier**: `L` — 跨平台桌面应用，涉及 GUI 悬浮窗、剪贴板/划词系统钩子、翻译后端、TTS、多平台打包与自启动，跨多个模块且单个会话做不完
- **Stage**: `grill`
- **Started**: 2026-11-05
- **Base**: `6da4428850436f9a617e2e603d265e0637edde9c` — diff range `6da4428..HEAD`
- **Repo**: `C:/Users/HP/WorkSpace/FloatTrans`

## Goal

交付一款独立安装、点开即用的桌面翻译工具：平时只在桌面边缘显示一条小胶囊，鼠标悬停时自动读取剪贴板并展开悬浮板开始翻译；支持最多 5 种语言互相翻译（含自动识别源语言）、朗读发音、复制结果、划词取词翻译、开机自启可配置，覆盖 Windows / macOS / Linux (deb, AppImage) / Arch Linux (PKGBUILD, AUR)。验收标准是四平台都能拿到可安装/可运行的独立产物，且核心行为有自动化测试覆盖。

## Baseline

<!-- 记录于 Stage 0，任何代码改动之前。这是事后唯一无法用命令重建的事实。 -->

- **Command**: `python -m pytest -q` （仓库尚无测试命令，此为拟采用的命令）
- **Result**: `C:\Users\HP\.workbuddy\binaries\python\versions\3.13.12\python.exe: No module named pytest` — 0 tests, 0 passed, 0 failed；测试运行器未安装、测试目录为空、无既有失败用例
- **Also checked**: 无 `package.json` / `pyproject.toml` / `Makefile` / `Cargo.toml`；Python 3.13.14、Node 22.22.2、Rust 1.96.0 可用；PySide6/PyQt/tkinter/pyperclip/pytest/PyInstaller 均未安装
- **Recorded**: 2026-11-05 16:42

**推论**：Stage 3 的第一张工单必须是建立测试脚手架（`pyproject.toml` + pytest + 核心纯逻辑 seam），否则后续所有 `Verified:` 无法落地。

## Decisions

- 仓库落在 `C:/Users/HP/WorkSpace/FloatTrans` 而非 WorkBuddy 会话工作区 — 用户选择「换目录再做」，代码与状态文件都进新仓库
- 采用 `git init` 新建空仓库并做基线提交 — 用户明确授权；`gh auth` 已登录 KrisShin，SSH key 可用，后续如需远端由用户决定
- 规模定为 L — 用户确认按完整流程（盘问 → spec → 工单 → TDD → 三轴评审 → 收尾）

## Assumptions

- 暂无。Stage 1 盘问后逐条补齐；任何未经用户确认的选择都写在这里并标 OPEN。

## Next action

进入 Stage 1：等待用户回答第一轮前沿问题（技术栈、翻译来源、5 语言语义、悬停状态机、划词降级、TTS、交付物/CI、隐私与设置）。拿到确认后写 `SPEC.md` 并请用户批准；批准前不写实现代码。

## Later

- GitHub 远端仓库创建与推送（用户已备好 gh auth / SSH，但未要求现在做）
- 后续可考虑：翻译历史、术语表、OCR 截图翻译、离线模型

## Close

- **Shipped**: —
- **Left out**: —
- **Next session needs**: —
- **Verify**: —
