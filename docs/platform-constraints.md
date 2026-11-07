# 平台约束：Wayland、签名、判毒与分发形态

> 调研留档，2026-11-05 查证。相关决策：STATE 的 D12/D13/D15/D16；SPEC 的「平台窗口适配」「产物与 CI」条；TICKETS 07/08/10。

## 一、Tauri v2 与运行时依赖

| 平台 | 系统 WebView | 备注 |
|---|---|---|
| Windows | Edge WebView2（Evergreen，Win11 自带） | 目标机普遍已有，无需打包运行时 |
| macOS | WKWebView | 随系统 |
| Linux | **WebKitGTK 4.1** | 需 `libwebkit2gtk-4.1-dev`；1.x 用 4.0，**4.0/4.1 ABI 不兼容**，装错编译期就报找不到头文件 |

产物 8–15MB、空闲内存 30–40MB（对比 Electron 120MB+ / 200–300MB）。Linux 打包完整依赖：`libwebkit2gtk-4.1-dev build-essential curl wget file libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev`；缺 `libxdo-dev` / `libayatana-appindicator3-dev` 时全局快捷键与托盘可能**编译通过但运行时静默失效**。

Rust release 体积优化：`strip = true`、`lto = true`、`codegen-units = 1`、`panic = "abort"`。

### Windows WebView2 会替换 HTML5 拖放处理器

Tauri v2 的 `WindowConfig.dragDropEnabled` 默认 `true`。在 Windows 上它会接管 WebView2 的原生拖放，结果页面的 `dragstart` / `drop` 不按 WebView 标准事件触发——前端用 HTML5 拖拽做卡片排序时会「完全拖不动」，且没有任何报错。官方文档明确要求：Windows 上要使用前端 HTML5 拖放，必须把该项设为 `false`（底层对应 `WebviewBuilder::disable_drag_drop_handler`）。

- 代价：窗口不再接收系统「文件拖入窗口」事件（`onDragDropEvent`）。Bead 没有文件拖放需求，关闭无功能损失。
- 该项与前端资源一起在编译期嵌入，改配置后必须重新 `tauri build` 才生效；只替换 `web/` 目录里的文件不会让真机产物改变。
- macOS / Linux 不经过这条 Windows 专用处理器，但本轮未实测；跨平台行为以 CI 构建 + 实机为准，不写成「已支持」。

## 二、Wayland 硬限制（影响「胶囊吸附边缘」）

- [`tauri-apps/tauri#14913`](https://github.com/tauri-apps/tauri/issues/14913)（2026-02 提出、仍未关闭）：Wayland 下 `Window::set_position` 与 `always_on_top` **静默 no-op**，连窗口初始位置都不生效；`skip_taskbar` 同样失效。官方回复是「xdg-shell 的刻意设计，我们无能为力」。
- 这是**协议限制，不是框架缺陷**：纯 Rust 自绘（winit / xdg-shell）同样受限。
- 可能的缓解：
  - GNOME 下借合成器自己的窗口菜单让用户手动置顶（`tauri-plugin-window-manager` 通过 `xdg_toplevel.show_window_menu` 提供此能力），但**位置仍无解**。
  - `wlr-layer-shell` 能实现边缘层窗口，但只在 wlroots 系合成器（Sway / Hyprland）有效，GNOME 不支持。
  - Qt 的 `QClipboard::Selection` 是 Linux 划词最稳入口（X11 等价 `xclip -o -selection primary`，Wayland 等价 `wl-paste --primary`）；但 Shift+方向键这类「非鼠标放词」的选中拿不到。
  - 全局快捷键在 Wayland 需 `GlobalShortcuts` portal 或**交给桌面自定义绑定**（Click'n'Translate 的做法：提供 `app --selection` 等命令，让用户在 GNOME/KDE/XFCE 设置里绑快捷键）。
- **SPEC 结论**：Windows / macOS / Linux X11 完整实现；Linux Wayland 做能力探测、不支持时降级为普通窗口 + 手动定位提示，**不承诺边缘吸附与置顶**，且不静默失败。

## 三、签名：开源不豁免（D15：本项目主动不做）

| 平台 | 开源能豁免吗 | 现实 |
|---|---|---|
| Windows SmartScreen | ❌ | 检查 Mark-of-the-Web + 文件/证书声誉。未签名下载会弹「Windows 已保护你的电脑」；每个新 Release 都是新文件，声誉重新累积 |
| macOS Gatekeeper | ❌ | 只认 Developer ID 签名 + 公证；Sequoia 起取消「右键打开」捷径，须进系统设置 → 隐私与安全性放行 |
| Linux | ✅ 基本无要求 | deb/rpm 用 GPG 签仓库元数据，AUR 靠 PKGBUILD checksum |

可选路径与成本（2026 口径）：

- **SignPath Foundation**：给合格 OSS **免费 OV Authenticode 证书**，私钥在 HSM、可接 GitHub Actions。限制：发布者名显示 "SignPath Foundation"、要求项目已发布且活跃维护、OSI 许可、每次 release 人工审批、全员 MFA、审核可自由拒绝（有 2026-10 因 star/下载量不足被拒的案例）。
- **Azure Trusted Signing**（原 Artifact Signing）：约 $9.99/月，短有效期证书 + 时间戳，个人需美/加身份验证。
- **Certum Open Source Code Signing**：约 €69，密钥在智能卡，本地签名。
- 商业 OV $100–200/年、EV ~$300/年；**2024 起 EV 也不再直接跳过 SmartScreen 声誉阶段**。自签名无效。
- macOS：**无免费路线**，唯一正规解是 Apple Developer Program $99/年 → `codesign --options runtime --timestamp` → `notarytool submit --wait` → `stapler staple` → `spctl -a -vvv`。ad-hoc 签名（`codesign -s -`）只证完整性不证身份，仍会显示「已损坏」。
- `xattr -dr com.apple.quarantine` 只是移除隔离属性，**不补签名、不公证、不证明安全**。
- 本项目额外风险：划词需辅助功能/输入监听权限，签名身份不稳定会导致**每次升级后重新授权**。

**D15 决定**：任何平台都不签名、不申请证书、不接公证、CI 不留签名钩子或 secrets 分支。信任来自源码可审计 + 每 Release 的 SHA256SUMS + 可复现 CI。README 必须直白写明各平台首次运行放行方式，不美化。

**注意**：从下载的 zip 解压会把 Mark-of-the-Web 传给内部文件，**zip 便携同样绕不过 SmartScreen**。

## 四、误报判毒（D13：不做单文件 exe）

杀软启发式命中的主要原因：

1. **onefile 运行时自解压到 `%TEMP%`**（`_MEIXXXXXX`），行为模式接近恶意载荷；
2. **UPX 加壳**改变 PE 结构，被当作可疑；
3. **PyInstaller 预编译 bootloader 的已知指纹**——和真实恶意软件共用同一二进制签名；
4. PE 结构异常（非标准节名、空 IAT、大量重定位）。

实测样本：VirusTotal 上 Python 打包 exe 常见 4–19/71 命中；某样本 `--onefile` 误报率约为 `--onedir` 的 **3–5 倍**（样本集 N=1247）；自编译 bootloader 可把 4/71 降到 1/71。

**Bead 的规避**：Tauri 产出普通原生 PE，**没有自解压、没有加壳、没有 PyInstaller bootloader**；再叠加「不做单文件、只做安装器 + 目录形态 zip、不用 UPX」，天然避开上述几条。附加手段：exe 带完整版本信息与图标、提交误报申诉、Release 附 SHA256。

## 五、安装器 vs 便携（Q13 结论：方案 C 双轨）

| 维度 | A 纯便携 | B 安装器 |
|---|---|---|
| 体积 / 免管理员 / 零系统污染 | ✅ | ❌ +2–5MB，MSI 要 UAC |
| 点开即用 | ✅ | ❌ 多一步向导 |
| 开机自启稳定性 | ⚠️ 注册表 Run / LaunchAgent / XDG `.desktop` 写死 exe 绝对路径，**移动文件就断** | ✅ 路径固定 |
| 快捷方式 / 卸载入口 | ❌ | ✅ 标准 |
| Linux 依赖自动解决 | ❌ 用户自装 webkit2gtk | ✅ deb / PKGBUILD 声明 depends |
| 自动更新 | ❌ 需自建 | ✅ 可接 updater |

三个反直觉点：

1. 「不做安装器」**省不掉签名问题**——那是身份问题，不是安装方式问题。
2. 便携 + 自启是唯一真脆弱组合；缓解方式是写入自启项时校验路径，路径变更自动置为失效并提示重设。
3. 安装器真正的价值在 **Linux**（webkit2gtk 依赖自动装），Win/mac 上主要只是自启与卸载规范。

**定案**：用户先要求「不做可安装、点开即用」，后修正为「不做单文件启动（易判毒），做安装器 + zip 便携目录」。CI 用 `tauri build` 一套产物同时给：Windows NSIS + zip、macOS dmg + zip `.app`、Linux deb + AppImage + tar.gz、Arch PKGBUILD。Tauri 顺带产出这些 bundle，**双轨额外成本接近零**。

## 六、更新策略（D16）

只比较、只提示：启动时异步请求 `KrisShin/Bead` 的 latest tag，与编译期注入的当前版本做语义化比较；不一致则显示可关闭轻提示 + 跳转 Release 页；一致 / 离线 / 超时 / 限流时静默。**绝不自动下载、绝不自动安装、绝不替换自身文件**——便携目录被自更新改坏、未签名产物在后台替换自身，都是更大的信任问题。

## 七、CI 与发布

- GitHub Actions matrix：`windows-latest` / `macos-latest` / `ubuntu-latest` + Arch 容器；tag 触发，用 `tauri-action` 构建并上传 Release。
- 本机只有 Windows：**CI 只保证能构建**，macOS / Linux / Arch 的运行时手感与权限流程列为未验证项，不写成「已支持」。
- 本机 HTTPS 代理 `127.0.0.1:2154` 对 git 的 CONNECT 隧道会返回 502；`origin` 已改用 SSH `git@github.com:KrisShin/Bead.git`（`ssh -T git@github.com` → `Hi KrisShin!`）。
