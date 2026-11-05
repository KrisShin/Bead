# 翻译源调研：端点实测、免费额度与 provider 架构

> 调研留档，2026-11-05 本机（中国大陆网络、Windows）实测。端点可达性与额度会变，引用前复核。
> 相关决策：STATE 的 D2、D11；SPEC 的「Provider 分层」条；TICKETS 04。

## 一、零密钥端点实测（`curl` 真实输出）

| 端点 | 结果 |
|---|---|
| `translate.googleapis.com/translate_a/single` (client=gtx) | **8 秒超时不可达** |
| `libretranslate.com/translate` | 返回 Cloudflare HTML 挑战页，不可用 |
| `api.mymemory.translated.net/get` | ✅ **零密钥可用**，`你好世界` → `Hello world`（`responseStatus: 200`） |
| `cn.bing.com/translator` | HTTP 200 |
| `cn.bing.com/ttranslatev3` | HTTP 200（需从页面取 token/IG，非纯匿名直调） |
| `fanyi.youdao.com` | HTTP 200 |
| `fanyi.baidu.com` | HTTP 200 |
| `translate.volcengine.com` | HTTP 404 |
| `api-free.deepl.com/v2/translate` | HTTP 403（需 key） |
| `open.bigmodel.cn/api/paas/v4/chat/completions` | HTTP 401（可达、需 key） |
| `api.siliconflow.cn/v1/models` | HTTP 401，`{"code":30014,"message":"Token is invalid."}` |

**核心结论：国内网络下「零配置 + 稳定」不成立。** 免 key 源只能作为尽力而为的默认，稳定源必须用户自备免费 key。因此：

- 默认 `ZeroKeyProvider` = MyMemory，UI 明确标注「尽力而为、可能被限流」。
- 所谓「帮我找免费 key」无法由程序代劳——各家都需手机号/实名注册，且共享 key 违反 ToS 并必然被封。设置页提供**各家申请入口与额度清单**，用户贴一次 key 即可解锁。

## 二、传统翻译 API 免费额度（2026 公开口径，非厂商承诺）

| 服务 | 免费额度 | 签名 / 备注 |
|---|---|---|
| 腾讯翻译君 TMT | **500 万字符/月** | TC3-HMAC-SHA256，默认 5 次/秒，国内最宽裕 |
| 火山翻译 | 200 万字符/月 | 国内 |
| 阿里翻译 | 100 万字符/月 | HMAC；专业版另计 |
| 百度翻译 | 标准版 5 万/月，高级版 100 万/月 | MD5(appid+q+salt+密钥)，标准版 QPS=1 |
| DeepL API Free | 50 万字符/月 | `DeepL-Auth-Key` 头，需信用卡验证 |
| 有道智云 | 新用户 50 元体验金（约 100 万字符） | SHA256 v3，一次性 |
| 彩云小译 | 新用户 100 万字符 / 1 个月 | 无固定免费额度 |
| 小牛翻译 | 每天 100 积分 | — |
| Google Cloud Translation v2 | 前 50 万字符/月 | 境内不可达 |

各家错误码风格差异大（有道 3 位数字、百度 5 位、腾讯/阿里字符串），这些差异由 `MachineTranslationProvider` 吸收，上层只见统一错误类型。

## 三、免费大模型（可作为翻译引擎）

### 智谱 GLM-4.7-Flash

- 2026 年发布并开源，**替代 GLM-4.5-Flash**（后者 2026-01-30 下线，请求自动路由到新版）。
- 混合思考模型，30B 总参 / 3B 激活，200K 上下文、128K 最大输出。
- 官方定价页：**输入免费、输出免费、缓存免费**。
- OpenAI 兼容：`https://open.bigmodel.cn/api/paas/v4`，需中国手机号注册、无信用卡。
- 并发口径有冲突：部分资料为 **1 concurrent request**，另有资料称 30 QPS。**按最保守的 1 并发设计**。
- 官方推荐场景含「中文写作、翻译、长文本」。

### 硅基流动 SiliconFlow

- `https://api.siliconflow.cn/v1`，OpenAI 兼容，免费层约 **1000 RPM / 50K TPM**，9B 以下模型永久免费，新用户赠 2000 万 token。
- 免费模型以 Qwen2.5-7B-Instruct、`GLM-4-9B-Chat`、DeepSeek-R1-Distill 系列为主；一个 key 打多家模型。
- **GLM-4.7-Flash（30B 总参）是否落在其免费列表里无法确认**——`/v1/models` 无 key 返回 401。因此 app 必须在用户填 key 后**运行时探测模型列表**，不能写死模型 ID。

### 其他国内免费 LLM API（备选）

阿里云百炼（每模型 100 万 token / 3 个月）、火山方舟（每日 200 万 token）、百度千帆 ERNIE-Speed（永久免费）、ModelScope（2000 RPD）、DeepSeek（新用户 300 万 token / 30 天）。

## 四、架构决策

1. **`Translator` trait**：`translate(request) -> Result<TranslationSet, ProviderError>`，`request` 一次携带**全部目标语言**，返回按语言索引的结果集。
   - 这是关键：免费模型只有 1 并发，若每个目标语言各发一次请求就会排队；单次请求 + JSON 返回全部译文既绕开并发限制，又天然支撑「改任一语言 → 其余同步」。
2. **Provider 分层**：`ZeroKeyProvider`（默认 MyMemory）、`OpenAICompatibleProvider`（`base_url`/`model`/`key` 全可配，预设智谱 / 硅基流动 / 自定义）、`MachineTranslationProvider`（吸收各家签名与错误码）、`ProviderChain`（优先级 + 失败回退 + 超时 + 重试上限 + 熔断）。
3. **运行时探测**：`/v1/models` 拉可用模型供用户选择；免费模型改版时用户自己能适配，不依赖我们发版。
4. **回退链**：主引擎失败 → 次引擎 → 免 key 引擎；失败给明确提示与切换入口，不静默。
5. **凭据安全**：key 不写日志、不进崩溃报告、界面不明文回显。

## 五、未验证项

没有用户 API key 时，LLM provider 只能测到「请求构造与响应解析」这一层，真实端到端需填 key 后补测。验收报告必须如实标注，不得写成「已支持」。
