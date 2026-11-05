# 命名排查与 Bead 决策

> 调研留档，2026-11-05 查证（GitHub search API、crates.io API、商标公开记录、产品官网）。
> 相关决策：STATE 的 D18；SPEC 的「项目名与标识」条。

## 一、为什么必须改名

原名 **FloatTrans / 中文「浮译」** 撞 [`krisir/floattrans`](https://github.com/krisir/floattrans)：40★、MIT、Swift，2026-08-27 创建，描述为「浮译（FloatTrans）：macOS 菜单栏实时中译英……以低打扰浮动框显示」。**英文名、中文名、品类、发布时间全面撞车**；搜「浮译」还牵出一堆低打扰悬浮翻译工具（Pico 皮可、SnapCut 简截等）。

## 二、逐一排查过的候选与撞车对象

| 候选 | 撞车 / 排除原因 |
|---|---|
| Lilt | `lilt.com` 是真实企业翻译公司；`LiLT` 还是文档理解模型（371★）——翻译赛道双重撞 |
| Weft | `WeaveMindAI/weft` 2025★、`jonesphillip/weft` 525★、`Weft-Rust/Weft` 307★ |
| Unison | `unisonweb/unison` 6771★ 编程语言 |
| Accord | `accord-net/framework` 4532★ |
| Tessera / Sill / Ledge | 搜索被 tesseract 77k★ / SillyTavern 35k★ / ledger 6k★ 淹没 |
| Relay / Echo | `meta/relay` 18,973★ / `labstack/echo` 32,790★ |
| Stitch / Kite / Flit / Parley | 精确同名 3715★ / 3256★ / `pypa/flit` 2261★ / `linebender/parley` 775★ |
| Gloss / Nook / Cove / Quilt / Nib / Folio | 精确同名 1606 / 2030 / 1404 / 1680 / 1880 / 877★ |
| **Perch** | `perchai.app` 的跨平台桌面 AI；ConnectWise SIEM（原 Perch Security）；`pioneering.app` 有个同名 Windows 本地优先小工具（2026-08，「small, sharp tools」、one file、no admin）——气质与发布形态几乎一样 |
| **Capsa** | **Colasoft Capsa**，2001 年公司的商业 Windows 网络分析仪，$295–995、1800+ 协议、有免费版 |
| **Verse** | Epic Games 的 USPTO 注册商标 #88749231（2023-07-11 注册），UE6 标准脚本语言 |
| **Wisp** | USPTO 注册商标 #79424254（2026-03-17 核准），Get Wisp Ltd |
| Echo / Speck / Fleck | `labstack/echo` 32,790★、`specklesystems`、`statianzo/Fleck` 2444★ |
| Synlingua / Hoverweave / Motelingo / Polyglide | 造词，GitHub 名称命中 0 / 0 / 0 / 3，但**语义不清晰、像 B2B SaaS**，用户否决 |
| Bead / Pane / Slip / Tuck / Brim / Moth | 均为**真实英文单词**（珠子 / 窗格 / 纸条 / 收起 / 边缘 / 蛾），非自造词；用的是冷僻义项，中文用户第一眼不会往「翻译」上想 |

## 三、crates.io 占用情况

以下名字**全部已被占用**：`mote` `lilt` `weft` `capsa` `perch` `accord` `tessera` `ledge` `sill` `gloss` `relay` `tuck` `cove` `nook` `idiom` `echo` `flit` `parley` `pane` `slip` `kite` `wren` `nib` `moth` `folio` `brim` `bead` `quilt` `stitch`。

返回 404（空）的只有：`verse`（但商标被 Epic 占）、`floattrans`、`float_trans`、`hovertranslate`、`polytranslate`、`synctranslate`。

**结论**：单个英文短词在 crates.io 几乎全被占，但这**不构成硬约束**——crate 名不必等于产品名，统一用 `bead-core` 规避。

## 四、最终决策：Bead

- 用户 2026-11-05 拍板。中文可作「**齐译**」（几种语言一齐呈现、一齐同步），但不作为交付标识。
- 意象：几颗珠子穿成一串 = 几张语言卡属于同一份内容；改一颗，整串重新对齐。与 `SyncCoordinator` 的语义直接对应，且「小、圆、精致」贴合轻量定位。
- 撞车极低：GitHub 精确同名最高 7★；不碰翻译赛道、不碰商业软件、不碰技术术语。
- 代价：名字本身不承担表意，**靠 tagline 与仓库 description 补**，这是有意的取舍（Pot / Bob / Crow 也都不含「翻译」）。

## 五、统一标识（改这些要一起改）

| 用途 | 值 |
|---|---|
| 本地仓库 | `C:/Users/HP/WorkSpace/Bead` |
| 会话工作区 | `C:/Users/HP/WorkSpace/WorkBuddy/Bead` |
| GitHub | `KrisShin/Bead`（public，`main`） |
| 状态目录 | `.scratch/bead/` |
| 二进制 / PKGBUILD | `bead` |
| Rust crate | `bead-core` |
| 更新检查 | 指向 `KrisShin/Bead` 的 Releases latest tag |
| 中文称呼 | 齐译（可选，不进产物名） |

建议 tagline：`One thought, five languages, always in step.` / 中文「悬停即译 · 五语同步」。
