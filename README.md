# Bead

**一次输入、多语言并排：改任意一张语言卡，其余语言自动同步——桌面悬浮窗上的多向翻译工具。**

平时只在屏幕右上角挂着一条小胶囊（横置、紧贴顶缘），鼠标悬停 200ms 展开即读剪贴板翻译；
展开后任意卡片可直接编辑（含中文输入法），改一处，其余语言实时联动。选 2 种语言即天然双向，
最多支持 5 种并排。这是普通翻译工具做不到、目前只有项目级本地化平台才有的能力。

## 功能

- **多语言并排联动**：2–5 种语言，任意卡编辑、其余自动同步；最后编辑的卡为权威源，永不被覆盖
- **悬停即译**：悬停胶囊读一次剪贴板（默认被动——不注册任何剪贴板监听器、不后台轮询）
- **划词翻译**（默认关闭）：快捷键 Ctrl+Alt+B 取前台选中文本，取词后恢复用户剪贴板
- **引擎可换**：默认免费引擎（MyMemory）开箱能翻；可配智谱 GLM / 硅基流动 / 自定义 OpenAI 兼容 /
  DeepL / 腾讯云 / 火山引擎 / 阿里云 / 百度 / 有道（填 key 即用，模型清单运行时探测）
- **引擎回退链**：失败自动回退、重试有上限、连续失败熔断冷却
- **朗读**：在线 TTS 优先，自动降级系统离线语音（Windows SAPI），按语言与文本缓存
- **隐私**：敏感内容（密钥/口令/卡号/超长文本）不进任何翻译引擎；日志不落原文；key 不明文回显
- **更新检查**：只比较版本号并提示，**绝不自动下载或安装**（源码可审计这条路径）

## 下载与首次运行

所有产物**不做代码签名**——这是刻意的：信任来自开源可审计 + 每个版本附 SHA256SUMS。
首次运行请按平台放行：

- **Windows**：SmartScreen 可能提示「已保护你的电脑」——点「更多信息」→「仍要运行」。
  zip 目录形态可直接使用，无需安装。
- **macOS**：Gatekeeper 拦截未公证应用——右键 App →「打开」，或
  `xattr -cr /Applications/Bead.app`。
- **Linux**：需要系统 WebView：`libwebkit2gtk-4.1`（deb 系 `sudo apt install libwebkit2gtk-4.1-dev`
  仅构建需要，运行时只需 runtime 库）；AppImage 免安装；Arch 用仓库内 PKGBUILD 构建。
- **Wayland**：窗口定位与置顶受 xdg-shell 协议限制，可能无法贴边吸附——应用会明确提示，
  这是协议设计而非缺陷。

## 从源码构建

依赖：Rust 1.85+、Node 22+，Linux 另需 `libwebkit2gtk-4.1-dev libgtk-3-dev`。

```bash
cargo test --manifest-path bead-core/Cargo.toml   # 核心行为测试
npm test                                          # 前端渲染测试
cargo build -p bead                               # 调试构建
cargo tauri build                                 # 出包（各平台产物见 src-tauri/tauri.conf.json）
```

## 许可

MIT。见 [LICENSE](LICENSE)。
