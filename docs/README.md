# Bead 文档索引

> **定位声明**：`docs/` 全部是**调研留档**，记录 2026-11-05 立项阶段的实测数据、外部资料与选型推理，**不是执行依据**。
> 执行依据只有两处：`.scratch/bead/SPEC.md`（已批准的规格）与 `.scratch/bead/TICKETS.md`（工单）；`.scratch/bead/STATE.md` 只记决策、假设与基线，不记进度（进度由 `git log` 推导）。
> 代码存在后，文档与代码冲突时**以代码与测试为准**。参见 `Agents.md` 第 10、11 条。

## 阅读顺序

| 文档 | 内容 | 什么时候读 |
|---|---|---|
| [competitive-landscape.md](competitive-landscape.md) | 竞品全景、定位象限、差异化结论 | 写 README、定 tagline、讨论要不要加功能时 |
| [naming.md](naming.md) | 命名排查、撞车清单、Bead 决策 | 改仓库名/产物名/域名/包名之前 |
| [translation-providers.md](translation-providers.md) | 端点实测、免费额度、provider 架构 | 实现 provider 层、加新翻译源、调免费策略时 |
| [platform-constraints.md](platform-constraints.md) | Wayland、签名、判毒、安装器对比 | 实现窗口层、划词、CI 与发布时 |
| `../.scratch/bead/SPEC.md` | 78 条用户故事与实现决策 | **执行前必读** |
| `../.scratch/bead/TICKETS.md` | 10 张 tracer-bullet 工单与阻塞边 | **执行前必读** |
| `../Agents.md` | 项目绝对规范、改动分级、不可动清单 | 每次动手前 |

## 事实的时间性

`docs/` 里的 star 数、免费额度、端点可达性、商标状态、框架限制都是 **2026-11-05 当天查证的结果**，会过期。引用前请重新核实，尤其是：

- 各家免费额度与模型可用性（厂商随时调整，智谱 GLM-4.5-Flash 已于 2026-01-30 下线）
- 端点可达性（本机网络环境下的实测，换网络会变）
- 竞品 star 与归档状态
- Tauri / Wayland 相关 issue 的开放状态

## 一句话结论

竞品的「多结果并排」是**多引擎**，本地化平台的「改一处其余同步」是**项目级流程**；Bead 站的是两者交集里的空位：**桌面悬浮 + 多语言并排 + 可编辑联动同步**。所有实现决策都应优先保护这条定位。
