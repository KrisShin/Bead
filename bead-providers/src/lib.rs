//! Bead 的真实翻译引擎实现。业务契约（`Translator` trait、`SyncPlan`、`TranslationSet`）
//! 全部来自 bead-core；本 crate 只负责「怎么跟各家 API 说话」。
//!
//! 结构性决定（D28/D29）：
//! - HTTP 经 `Transport` trait 注入——单元测试用 fake 回放响应体，请求构造与签名计算
//!   完全不触网；真实实现是 `UreqTransport`（10 秒硬超时，4xx/5xx 响应体照读）。
//! - 「一次请求返回全部译文」约束上层契约：OpenAI 兼容系真单次调用携带全部目标；
//!   按语言对设计的 REST（MyMemory/DeepL/各家签名 API）在 provider 内部逐对调用，
//!   对上层仍是一次 `translate()` 返回全部。
//! - key 只在内存/配置里流转，绝不写日志；`ProviderError` 的消息不携带 key。

pub mod aliyun;
pub mod baidu;
pub mod deepl;
pub mod google_tts;
pub mod mymemory;
pub mod openai;
pub mod settings;
pub mod signing;
pub mod tencent;
pub mod transport;
pub mod volc;
pub mod youdao;

pub use settings::{build_translator, list_models, openai_preset, Engine, ProviderConfig};
pub use google_tts::{ByteTransport, GoogleTts, UreqByteTransport};
pub use transport::{HttpMethod, HttpRequest, HttpResponse, Transport, UreqTransport};
