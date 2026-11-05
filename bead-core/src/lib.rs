//! Bead 的业务状态全部放在这里，WebView 与系统能力一层都不参与判断。
//!
//! 这样切分不是为了让 Tauri 好写，而是为了让「改任一语言卡，其余自动同步」这条唯一的产品定位
//! 能在没有窗口、没有网络、没有剪贴板的环境里被完整断言：前端只发 intent、只订阅状态快照，
//! 所有会产生分歧的行为（防抖、竞态、权威源、失败隔离）都收敛成纯函数。
//!
//! 本 crate 刻意不依赖任何第三方库：语言池是 2–5 个元素，用 BTreeMap 就够；去重哈希用标准库的
//! DefaultHasher 足够（它只判断「和上次是否一样」，不承担安全用途）；HTTP 客户端属于工单 04，
//! 现在引入只会把测试拖进网络。时钟、翻译器、剪贴板都是 trait，测试注入 fake。

pub mod detector;
pub mod document;
pub mod hover;
pub mod sync;

pub use detector::{HeuristicDetector, LanguageDetector};
pub use document::{Document, DocumentError, Lang, LanguagePool, PoolError, Slot, SlotStatus, MAX_LANGUAGES, MIN_LANGUAGES};
pub use hover::{
    ClipboardError, ClipboardGateway, HoverAction, HoverEvent, HoverState, HoverStateMachine,
    DEFAULT_HOVER_DELAY_MS, DEFAULT_LEAVE_GRACE_MS,
};
pub use sync::{
    content_hash, ApplyOutcome, Clock, ProviderError, SyncCoordinator, SyncPlan, TranslationItem,
    TranslationSet, Translator,
};
