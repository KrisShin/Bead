# 竞品全景与定位结论

> 调研留档，2026-11-05 查证。star 数与归档状态会变，引用前复核。
> 相关决策：STATE 的 D3、D12、D18；SPEC 的「项目名与标识」条。

## 一、直接竞品（GitHub 实测 star）

| 项目 | Star | 授权 | 栈 / 平台 | 做什么 |
|---|---|---|---|---|
| `pot-app/pot-desktop` (Pot) | **19,340** | GPL-3.0 | **Tauri + Rust**，Win/mac/Linux | 划词翻译、截图 OCR、**多引擎结果并排**、TTS、生词本、本地 Ollama |
| `tisfeng/Easydict` | **15,207** | GPL-3.0 | Swift，仅 macOS | 多引擎同屏并排、划词、OCR、系统词典、多 LLM 引擎 |
| `crow-translate/crow-translate` | 1,963 | GPL-3.0 | C++ / Qt 6，Win/mac/Linux | 轻量划词 + OCR + TTS + 托盘自启，闲置约 20MB 内存 |
| `krisir/floattrans`（浮译） | 40 | MIT | Swift，仅 macOS | 菜单栏实时中译英、低打扰浮窗、读当前输入框 |
| TTime | — | — | 跨平台 | 截图 / 划词 / 多翻译服务聚合 |
| Bob | — | 闭源付费 | 仅 macOS | 划词、OCR、插件引擎 |
| hcfy 划词翻译 | — | — | 浏览器扩展为主 | 多服务、可选套餐包 |
| 其他同类 | — | — | — | Pico 皮可、SnapCut 简截、ow-translate-lite、Nani Translate |

**Pot 的关键事实**：`archived=true`，最后 push `2026-07-04`。它和我们**技术栈完全相同**（Tauri + Rust）、同为轻量开源跨平台划词翻译，还多了 OCR 与生词本。若把自己讲成「轻量划词翻译工具」，就是 Pot 的功能缩减版。

## 二、「改一处，其余语言同步」在哪儿

这个能力在**项目级本地化平台**里是成熟功能，但都要建项目、导文件、走 CI：

| 平台 | 形态 | 联动方式 |
|---|---|---|
| Crowdin / Phrase / POEditor / Localazy / LingoHub | 本地化 SaaS | 源字符串变更后按工作流同步各 locale 状态 |
| LexiSync | 桌面 + Web 协作 | 实时协作同步、Myers diff 解冲突、25+ 种 i18n 格式 |
| Framer Auto Translate | 建站平台 | 「改一处，其余语言立即翻译」，手动编辑优先于自动译文 |
| Tolgee / SimpleLocalize / Locize | 开发者向 | in-context 编辑 + CI 状态同步 |
| LanguageX / 传统 CAT | 译者工具 | 翻译记忆、术语库、多人协作 |

它们都不是「随手悬停、改一句、五语齐变」的桌面小工具。

## 三、定位象限

两个轴：

- 纵轴：**桌面悬浮 · 随手即用** ↔ **项目级 · 流程化**
- 横轴：**单结果 / 多引擎并排** ↔ **多语言并排 · 可编辑联动**

| | 单结果 / 多引擎并排 | 多语言并排 · 可编辑联动 |
|---|---|---|
| **桌面悬浮** | Pot、Easydict、Crow Translate、Bob、TTime、浮译（**拥挤**） | **尚无产品** |
| **项目级** | 传统 CAT / 词典 | Crowdin、Phrase、LexiSync、Framer（**成熟**） |

两个容易混淆、但本质的区别：

1. Pot / Easydict 的「并排」是**多引擎**——同一句英文，Google 怎么说、DeepL 怎么说；Bead 的「并排」是**多语言**——同一段内容，中/英/日/法/德同时呈现。
2. 「改一处其余同步」在右下角象限里是工作流，在 Bead 里是一次 `SyncCoordinator::commit` + 单次多语言请求。

## 四、结论与约束

- **D3（任意语言卡可编辑、其余自动同步）不是一个 feature，是唯一能站得住的产品定位。**
- README 首句与 tagline 必须围绕它写，不以「轻量划词翻译」定位，不跟 Pot 正面比功能数量。
- 不要为了「看起来完整」去补 OCR、生词本、翻译历史——那些是 Pot 的战场，补了也只是更差的复刻。SPEC 的 Out of Scope 是刻意划的。
- 竞品多为 GPL-3.0，本项目用 MIT/Apache 系；功能思路可参照，**禁止复制代码**。
- Pot 已归档（2026-07 停更），「轻量 Tauri 开源翻译工具」的心智位置正在空出来，但不能靠功能重合去接。
