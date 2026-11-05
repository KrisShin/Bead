//! 一份内容、多个语言 slot。
//!
//! 用「每语言一个 slot」而不是「一条原文 + 若干译文」是因为任意卡片都可编辑，任何一张卡都可能
//! 成为权威源；若把原文和译文建成两种类型，编辑译文后就要搬动身份，联动逻辑会立刻长出特例。
//! 语言池上限 5 是产品决定（并排卡片在悬浮窗里放得下），不是技术限制；选 2 种即双向，不另建模式。

use std::collections::{BTreeMap, BTreeSet};

pub const MIN_LANGUAGES: usize = 2;
pub const MAX_LANGUAGES: usize = 5;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Lang(pub String);

impl Lang {
    pub fn new(code: impl Into<String>) -> Self {
        Self(code.into())
    }

    pub fn code(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PoolError {
    InvalidSize(usize),
    DuplicateLanguage(Lang),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanguagePool {
    languages: Vec<Lang>,
}

impl LanguagePool {
    pub fn new(languages: Vec<Lang>) -> Result<Self, PoolError> {
        let size = languages.len();
        if !(MIN_LANGUAGES..=MAX_LANGUAGES).contains(&size) {
            return Err(PoolError::InvalidSize(size));
        }
        let mut seen = BTreeSet::new();
        for language in &languages {
            if !seen.insert(language.clone()) {
                return Err(PoolError::DuplicateLanguage(language.clone()));
            }
        }
        Ok(Self { languages })
    }

    pub fn languages(&self) -> &[Lang] {
        &self.languages
    }

    pub fn primary(&self) -> &Lang {
        &self.languages[0]
    }

    pub fn contains(&self, language: &Lang) -> bool {
        self.languages.contains(language)
    }

    pub fn targets_excluding(&self, source: &Lang) -> Vec<Lang> {
        self.languages
            .iter()
            .filter(|language| *language != source)
            .cloned()
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotStatus {
    Idle,
    Syncing,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slot {
    pub text: String,
    pub status: SlotStatus,
}

impl Slot {
    pub fn empty() -> Self {
        Self {
            text: String::new(),
            status: SlotStatus::Idle,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocumentError {
    UnknownLanguage(Lang),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Document {
    slots: BTreeMap<Lang, Slot>,
    authoritative: Option<Lang>,
}

impl Document {
    pub fn for_pool(pool: &LanguagePool) -> Self {
        let slots = pool
            .languages()
            .iter()
            .map(|language| (language.clone(), Slot::empty()))
            .collect();
        Self {
            slots,
            authoritative: None,
        }
    }

    pub fn slot(&self, language: &Lang) -> Option<&Slot> {
        self.slots.get(language)
    }

    pub fn slots(&self) -> impl Iterator<Item = (&Lang, &Slot)> {
        self.slots.iter()
    }

    pub fn authoritative(&self) -> Option<&Lang> {
        self.authoritative.as_ref()
    }

    pub fn set_authoritative(&mut self, language: &Lang) -> Result<(), DocumentError> {
        if !self.slots.contains_key(language) {
            return Err(DocumentError::UnknownLanguage(language.clone()));
        }
        self.authoritative = Some(language.clone());
        Ok(())
    }

    pub fn set_text(&mut self, language: &Lang, text: impl Into<String>) -> Result<(), DocumentError> {
        let slot = self
            .slots
            .get_mut(language)
            .ok_or_else(|| DocumentError::UnknownLanguage(language.clone()))?;
        slot.text = text.into();
        Ok(())
    }

    pub fn set_status(&mut self, language: &Lang, status: SlotStatus) -> Result<(), DocumentError> {
        let slot = self
            .slots
            .get_mut(language)
            .ok_or_else(|| DocumentError::UnknownLanguage(language.clone()))?;
        slot.status = status;
        Ok(())
    }

    pub fn clear(&mut self) {
        for slot in self.slots.values_mut() {
            slot.text.clear();
            slot.status = SlotStatus::Idle;
        }
        self.authoritative = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(codes: &[&str]) -> Result<LanguagePool, PoolError> {
        LanguagePool::new(codes.iter().map(|code| Lang::new(*code)).collect())
    }

    #[test]
    fn a_pool_of_one_or_six_languages_is_rejected() {
        assert_eq!(pool(&["zh"]), Err(PoolError::InvalidSize(1)));
        assert_eq!(
            pool(&["zh", "en", "ja", "fr", "de", "ko"]),
            Err(PoolError::InvalidSize(6))
        );
    }

    #[test]
    fn a_pool_of_two_or_five_languages_is_accepted() {
        assert!(pool(&["zh", "en"]).is_ok());
        assert!(pool(&["zh", "en", "ja", "fr", "de"]).is_ok());
    }

    #[test]
    fn two_languages_are_bidirectional_without_a_separate_mode() {
        let languages = pool(&["zh", "en"]).expect("two languages are valid");
        assert_eq!(
            languages.targets_excluding(&Lang::new("zh")),
            vec![Lang::new("en")]
        );
        assert_eq!(
            languages.targets_excluding(&Lang::new("en")),
            vec![Lang::new("zh")]
        );
    }

    #[test]
    fn clearing_a_document_keeps_the_slot_layout_but_drops_text_and_authority() {
        let languages = pool(&["zh", "en"]).expect("two languages are valid");
        let mut document = Document::for_pool(&languages);
        document.set_text(&Lang::new("zh"), "你好").unwrap();
        document.set_authoritative(&Lang::new("zh")).unwrap();
        document.clear();

        assert_eq!(document.slot(&Lang::new("zh")).unwrap().text, "");
        assert_eq!(document.authoritative(), None);
        assert_eq!(document.slots().count(), 2);
    }
}
