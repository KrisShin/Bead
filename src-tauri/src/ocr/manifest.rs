//! OCR 资产清单：远端拉取优先，编译期内置兜底（完全离线也能装）。
//!
//! 清单本身不含机密；每个文件带 `size` 与 `sha256`，下载器按这份数据逐文件校验。
//! 远端顺序固定为魔搭 → Gitee → GitHub：魔搭匿名直链最快且国内可达，
//! Gitee 与 GitHub 只作为发布源兜底。所有失败都返回 `None`，由调用方继续用内置清单。

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// 清单地址（按顺序尝试）。远端未发布时直接落到内置清单。
pub const MANIFEST_URLS: [&str; 3] = [
    "https://modelscope.cn/models/KrisShin/Bead-OCR/resolve/master/ocr-manifest.json",
    "https://gitee.com/KrisShin/Bead/raw/dev/scripts/ocr/ocr-manifest.json",
    "https://raw.githubusercontent.com/KrisShin/Bead/dev/scripts/ocr/ocr-manifest.json",
];

/// 内置兜底：直接复用仓库里的清单，发布资产时两边天然一致。
const BUILTIN_MANIFEST: &str = include_str!("../../../scripts/ocr/ocr-manifest.json");

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub version: String,
    #[serde(default)]
    pub tag: String,
    #[serde(default)]
    pub bundles: Vec<Bundle>,
    pub files: Vec<Asset>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bundle {
    pub id: String,
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub total_size: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Asset {
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub size: u64,
    pub sha256: String,
    #[serde(default)]
    pub sources: Vec<String>,
}

impl Manifest {
    /// 编译期内置清单：解析失败属于构建错误，测试会覆盖。
    pub fn builtin() -> Self {
        Self::parse(BUILTIN_MANIFEST).expect("内置 OCR 清单损坏")
    }

    pub fn parse(text: &str) -> Option<Self> {
        let manifest: Self = serde_json::from_str(text).ok()?;
        manifest.is_valid().then_some(manifest)
    }

    /// 远端清单：6 秒超时、逐个源尝试，全部失败返回 `None`。
    pub fn fetch() -> Option<Self> {
        for url in MANIFEST_URLS {
            let Ok(response) = ureq::get(url)
                .timeout(Duration::from_secs(6))
                .call()
            else {
                continue;
            };
            let Ok(text) = response.into_string() else {
                continue;
            };
            if let Some(manifest) = Self::parse(&text) {
                return Some(manifest);
            }
        }
        None
    }

    /// 只做结构性校验：关键文件齐全、哈希是 64 位十六进制、体积非零、至少一个源。
    fn is_valid(&self) -> bool {
        if self.schema_version == 0 || self.version.trim().is_empty() || self.files.is_empty() {
            return false;
        }
        let valid_assets = self.files.iter().all(|file| {
            !file.name.trim().is_empty()
                && file.size > 0
                && file.sha256.len() == 64
                && file.sha256.chars().all(|c| c.is_ascii_hexdigit())
                && !file.sources.is_empty()
        });
        if !valid_assets {
            return false;
        }
        ["det", "rec", "cls"].iter().all(|kind| self.asset(kind).is_some())
    }

    pub fn asset(&self, kind: &str) -> Option<&Asset> {
        self.files.iter().find(|file| file.kind == kind)
    }

    pub fn total_size(&self) -> u64 {
        self.files.iter().map(|file| file.size).sum()
    }

    /// 当前 bundle 是否覆盖该语言族（`zh-Hans` 只取 `zh`）。
    pub fn supports_language(&self, language: &str) -> bool {
        let family = language.split('-').next().unwrap_or(language);
        self.bundles.iter().any(|bundle| {
            bundle
                .languages
                .iter()
                .any(|candidate| candidate == "*" || candidate == family)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_manifest_is_valid() {
        assert_eq!(Manifest::builtin().version, "1.0.0");
    }

    #[test]
    fn malformed_manifests_are_rejected() {
        assert!(Manifest::parse("{}").is_none());
        assert!(Manifest::parse("not json").is_none());
        let mut manifest = Manifest::builtin();
        manifest.files[0].sha256 = "abc".into();
        assert!(Manifest::parse(&serde_json::to_string(&manifest).unwrap()).is_none());
    }

    #[test]
    fn language_family_is_matched() {
        let manifest = Manifest::builtin();
        assert!(manifest.supports_language("zh-Hans"));
        assert!(manifest.supports_language("en-US"));
        assert!(!manifest.supports_language("ja"));
    }
}
