//! provider 设置与装配：引擎枚举、配置结构、工厂。
//!
//! 设置面板只产出这个结构；壳层把 `build_translator` 的结果放进 ProviderChain。
//! key 只存在内存与（工单 05 起）用户配置文件里，不写日志、不进崩溃报告。

use bead_core::Translator;

use crate::aliyun::AliyunProvider;
use crate::baidu::BaiduProvider;
use crate::deepl::DeepLProvider;
use crate::mymemory::MyMemoryProvider;
use crate::openai::OpenAiCompatibleProvider;
use crate::tencent::TencentProvider;
use crate::transport::{Transport, UreqTransport};
use crate::volc::VolcProvider;
use crate::youdao::YoudaoProvider;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    ZeroKey,
    Zhipu,
    Siliconflow,
    Custom,
    DeepL,
    Tencent,
    Volc,
    Aliyun,
    Baidu,
    Youdao,
}

impl Engine {
    pub fn code(&self) -> &'static str {
        match self {
            Engine::ZeroKey => "zerokey",
            Engine::Zhipu => "zhipu",
            Engine::Siliconflow => "siliconflow",
            Engine::Custom => "custom",
            Engine::DeepL => "deepl",
            Engine::Tencent => "tencent",
            Engine::Volc => "volc",
            Engine::Aliyun => "aliyun",
            Engine::Baidu => "baidu",
            Engine::Youdao => "youdao",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        Some(match code {
            "zerokey" => Engine::ZeroKey,
            "zhipu" => Engine::Zhipu,
            "siliconflow" => Engine::Siliconflow,
            "custom" => Engine::Custom,
            "deepl" => Engine::DeepL,
            "tencent" => Engine::Tencent,
            "volc" => Engine::Volc,
            "aliyun" => Engine::Aliyun,
            "baidu" => Engine::Baidu,
            "youdao" => Engine::Youdao,
            _ => return None,
        })
    }

    /// 需要用户填 key 才能工作的引擎（界面据此提示与置灰）。
    pub fn needs_key(&self) -> bool {
        !matches!(self, Engine::ZeroKey)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    pub engine: Engine,
    /// 主凭据：OpenAI 系与 DeepL 是 API key；腾讯/火山/阿里是 AccessKeyId；百度/有道是应用 ID。
    pub api_key: String,
    /// 第二凭据（密码框，绝不回显）：腾讯/火山/阿里的 SecretKey，百度/有道的应用密钥。
    pub secret: String,
    /// 仅 OpenAI 兼容引擎使用；其余引擎忽略。
    pub model: String,
    /// 仅 Custom 引擎使用；其余引擎的 base_url 由预设决定。
    pub base_url: String,
}

impl ProviderConfig {
    pub fn has_key_stored(&self) -> bool {
        !self.api_key.is_empty()
    }

    pub fn has_secret_stored(&self) -> bool {
        self.engine_needs_secret() && !self.secret.is_empty()
    }

    /// 双凭据引擎（界面据此渲染两个密码框）。
    pub fn engine_needs_secret(&self) -> bool {
        matches!(self.engine, Engine::Tencent | Engine::Volc | Engine::Aliyun | Engine::Baidu | Engine::Youdao)
    }
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            engine: Engine::ZeroKey,
            api_key: String::new(),
            secret: String::new(),
            model: String::new(),
            base_url: String::new(),
        }
    }
}

/// 各引擎预设的 OpenAI 兼容 base_url 与默认模型（工单 04：模型清单运行时探测，不写死）。
pub fn openai_preset(engine: Engine) -> Option<(&'static str, &'static str)> {
    match engine {
        Engine::Zhipu => Some(("https://open.bigmodel.cn/api/paas/v4", "glm-4.7-flash")),
        Engine::Siliconflow => Some(("https://api.siliconflow.cn/v1", "")),
        _ => None,
    }
}

/// 按配置装配 provider。引擎选择错误或缺 key 返回带说明的错误。
pub fn build_translator(
    config: &ProviderConfig,
    transport: Box<dyn Transport>,
) -> Result<Box<dyn Translator + Send>, String> {
    Ok(match config.engine {
        Engine::ZeroKey => Box::new(MyMemoryProvider::new(transport)),
        Engine::Zhipu => {
            let (base, default_model) = openai_preset(Engine::Zhipu).expect("preset exists");
            require_key(config)?;
            Box::new(OpenAiCompatibleProvider::new(
                transport,
                base.into(),
                pick_model(config, default_model),
                config.api_key.clone(),
            ))
        }
        Engine::Siliconflow => {
            let (base, _) = openai_preset(Engine::Siliconflow).expect("preset exists");
            require_key(config)?;
            Box::new(OpenAiCompatibleProvider::new(
                transport,
                base.into(),
                pick_model(config, ""),
                config.api_key.clone(),
            ))
        }
        Engine::Custom => {
            if config.base_url.trim().is_empty() {
                return Err("自定义引擎需要填写 base_url".into());
            }
            require_key(config)?;
            Box::new(OpenAiCompatibleProvider::new(
                transport,
                config.base_url.clone(),
                pick_model(config, ""),
                config.api_key.clone(),
            ))
        }
        Engine::DeepL => {
            require_key(config)?;
            Box::new(DeepLProvider::new(transport, config.api_key.clone()))
        }
        Engine::Tencent => {
            require_key(config)?;
            Box::new(TencentProvider::new(
                transport,
                config.api_key.clone(), // SecretId
                config.secret.clone(),  // SecretKey
                Box::new(now_secs)
            ))
        }
        Engine::Volc => {
            require_key(config)?;
            Box::new(VolcProvider::new(
                transport,
                config.api_key.clone(), // AccessKeyId
                config.secret.clone(),  // SecretKey
                Box::new(now_secs),
            ))
        }
        Engine::Aliyun => {
            require_key(config)?;
            Box::new(AliyunProvider::new(
                transport,
                config.api_key.clone(), // AccessKeyId
                config.secret.clone(),  // AccessKeySecret
                Box::new(|| {
                    use std::time::{SystemTime, UNIX_EPOCH};
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.subsec_nanos().to_string())
                        .unwrap_or_default()
                }),
                Box::new(now_secs),
            ))
        }
        Engine::Baidu => {
            require_key(config)?;
            Box::new(BaiduProvider::new(
                transport,
                config.api_key.clone(), // APP ID
                config.secret.clone(),  // 密钥
                Box::new(|| {
                    use std::time::{SystemTime, UNIX_EPOCH};
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_millis().to_string())
                        .unwrap_or_default()
                }),
            ))
        }
        Engine::Youdao => {
            require_key(config)?;
            Box::new(YoudaoProvider::new(
                transport,
                config.api_key.clone(), // 应用 ID
                config.secret.clone(),  // 应用密钥
                Box::new(|| {
                    use std::time::{SystemTime, UNIX_EPOCH};
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.subsec_nanos().to_string())
                        .unwrap_or_default()
                }),
                Box::new(now_secs),
            ))
        }
    })
}

/// Unix 秒时间源：签名系 provider 的默认时钟，测试里各 provider 用注入的固定值。
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn require_key(config: &ProviderConfig) -> Result<(), String> {
    if config.api_key.trim().is_empty() {
        return Err(format!("{} 引擎需要先填写密钥", config.engine.code()));
    }
    if config.engine_needs_secret() && config.secret.trim().is_empty() {
        return Err(format!("{} 引擎还需要填写第二密钥", config.engine.code()));
    }
    Ok(())
}

/// 运行时拉取 OpenAI 兼容引擎的模型清单（工单 04：不写死模型 id）。其他引擎返回空表。
pub fn list_models(config: &ProviderConfig) -> Vec<String> {
    match config.engine {
        Engine::Zhipu | Engine::Siliconflow | Engine::Custom => {
            let base = match config.engine {
                Engine::Custom => config.base_url.clone(),
                other => openai_preset(other)
                    .map(|(base, _)| base.to_string())
                    .unwrap_or_default(),
            };
            let provider = OpenAiCompatibleProvider::new(
                Box::new(UreqTransport),
                base,
                String::new(),
                config.api_key.clone(),
            );
            provider.list_models().unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

fn pick_model(config: &ProviderConfig, default: &str) -> String {
    if config.model.trim().is_empty() {
        default.to_string()
    } else {
        config.model.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::HttpRequest;
    use std::sync::{Arc, Mutex};
    

    struct FakeTransport {
        captured: Arc<Mutex<Vec<HttpRequest>>>,
        response: String,
    }

    impl Transport for FakeTransport {
        fn send(&self, request: &HttpRequest) -> Result<crate::transport::HttpResponse, bead_core::ProviderError> {
            self.captured.lock().unwrap().push(request.clone());
            Ok(crate::transport::HttpResponse {
                status: 200,
                body: self.response.clone(),
            })
        }
    }

    fn plan() -> bead_core::SyncPlan {
        bead_core::SyncPlan {
            generation: 1,
            source_lang: bead_core::Lang::new("zh"),
            source_text: "你好".into(),
            targets: vec![bead_core::Lang::new("en")],
            created_at_ms: 0,
        }
    }

    #[test]
    fn zerokey_builds_without_any_credentials() {
        let config = ProviderConfig::default();
        let translator = build_translator(&config, Box::new(FakeTransport {
            captured: Arc::new(Mutex::new(Vec::new())),
            response: r#"{"responseData":{"translatedText":"Hello"},"responseStatus":200}"#.into(),
        }))
        .unwrap();
        assert!(translator.translate(&plan()).is_ok());
    }

    #[test]
    fn engines_without_keys_are_rejected_with_a_readable_message() {
        let config = ProviderConfig {
            engine: Engine::DeepL,
            ..Default::default()
        };
        let error = match build_translator(&config, Box::new(FakeTransport {
            captured: Arc::new(Mutex::new(Vec::new())),
            response: "{}".into(),
        })) {
            Err(error) => error,
            Ok(_) => panic!("缺 key 的引擎应当被拒绝"),
        };
        assert!(error.contains("需要先填写密钥"));
    }
}
