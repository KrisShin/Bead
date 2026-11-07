//! 语言检测：先按书写系统分流，再对拉丁语系做特征打分。
//!
//! 旧实现只看两件事：汉字/假名/谚文是否出现，以及拉丁字母是否占多数；除了东亚三种，
//! 其余一律返回 `en`。法语、德语、西班牙语、葡萄牙语、意大利语、荷兰语等都用拉丁字母，
//! 于是全部被当成英语去翻译，卡片里就会出现「把法语当英语」的错误结果。
//!
//! 新实现的原则：
//! 1. 先看书写系统。假名、谚文、西里尔、阿拉伯、泰文、希伯来、天城文、希腊文、汉字
//!    各自对应语言；汉字与拉丁混排时看哪一侧占主导，只有真正均衡的混排才判为「混合文本」。
//! 2. 拉丁语系用特征打分：高频功能词、语言特有字母/变音符号、语言特有字母组合。
//!    英语不再作为兜底，而是和其它语言一起参与打分。
//! 3. 低置信度不硬判：单个词、品牌名、缩写、纯数字、URL、代码片段、真正均衡的中英混排等
//!    统一返回 `Undetermined`，由调用方安静丢弃，绝不回落成 en。
//!
//! 不引入第三方语言检测库：本项目只需要 2～5 种卡池语言，且要求离线、零依赖、可单测；
//! 常见库（lingua 等）模型体积大、对短文本也经常强行给答案，和「低置信度不翻译」的产品
//! 语义相反。这里用确定性的特征表，体积和许可证都没有额外负担。

use std::collections::HashSet;

use crate::document::Lang;

/// 检测器接口。返回 `None` 表示「无法确定语言」，调用方不应把它当成任何具体语言。
pub trait LanguageDetector: Send {
    fn detect(&self, text: &str) -> Option<Lang>;
}

/// 检测置信度。低置信度不会以 `Detected` 返回，而是走 `Undetermined`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confidence {
    High,
    Medium,
}

/// 无法确定语言的原因，便于测试和诊断；产品上这些情况一律安静不处理。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UndeterminedReason {
    Empty,
    NoLetters,
    Url,
    Code,
    Abbreviation,
    SingleWord,
    MixedScript,
    WeakEvidence,
}

/// 检测结果。`Detected` 只代表「有足够把握」，不代表一定正确。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Detection {
    Detected { lang: Lang, confidence: Confidence },
    Undetermined(UndeterminedReason),
}

/// 基于字符集与特征打分的启发式检测器。
#[derive(Debug, Default, Clone, Copy)]
pub struct HeuristicDetector;

impl HeuristicDetector {
    /// 返回带有置信度/失败原因的详细结果；产品路径用 `detect`，测试与诊断用这个。
    pub fn analyze(&self, text: &str) -> Detection {
        analyze(text)
    }
}

impl LanguageDetector for HeuristicDetector {
    fn detect(&self, text: &str) -> Option<Lang> {
        match analyze(text) {
            Detection::Detected { lang, .. } => Some(lang),
            Detection::Undetermined(_) => None,
        }
    }
}

/// 检测入口。
pub fn analyze(text: &str) -> Detection {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Detection::Undetermined(UndeterminedReason::Empty);
    }

    // URL、邮箱、裸域名常夹在句子里。先按空白 token 剥离，再检测剩余正文；
    // 只有正文被剥空（整段都是链接）才返回 `Url`，避免「一句话里有链接就整段放弃」。
    let body = strip_links(trimmed);
    if body.is_empty() {
        return Detection::Undetermined(UndeterminedReason::Url);
    }
    if looks_like_code(&body) {
        return Detection::Undetermined(UndeterminedReason::Code);
    }

    let scripts = count_scripts(&body);
    if scripts.total_letters() == 0 {
        return Detection::Undetermined(UndeterminedReason::NoLetters);
    }

    // 假名、谚文是排他书写系统，混入少量拉丁术语也仍可判定。
    if scripts.kana > 0 {
        return detected("ja", Confidence::High);
    }
    if scripts.hangul > 0 {
        return detected("ko", Confidence::High);
    }

    // 汉字同时被中文和日文使用。中英混排时按「词单位」比较：一个汉字算一个词，
    // 一个拉丁 token 算一个词；任一侧达到 2/3 即视为该侧主导，只有势均力敌才不判。
    if scripts.han > 0 {
        if scripts.latin > 0 {
            let latin_units = latin_tokens(&body).len().max(1);
            let (han_units, total) = (scripts.han, scripts.han + latin_units);
            if is_balanced_mix(han_units, latin_units) {
                return Detection::Undetermined(UndeterminedReason::MixedScript);
            }
            // 拉丁一侧占 2/3 以上：按英文等人名/术语的处理方式走拉丁打分，而不是判中文。
            if latin_units * 3 >= total * 2 {
                return analyze_latin(&body);
            }
        }
        let confidence = if scripts.han >= 2 {
            Confidence::High
        } else {
            Confidence::Medium
        };
        return detected("zh", confidence);
    }

    // 其它排他书写系统：取字符数最多的那个；与拉丁混排时同样按 2/3 支配度判定。
    let strong_script = [
        (scripts.cyrillic, "ru"),
        (scripts.arabic, "ar"),
        (scripts.thai, "th"),
        (scripts.hebrew, "he"),
        (scripts.devanagari, "hi"),
        (scripts.greek, "el"),
    ];
    let (count, code) = strong_script
        .iter()
        .copied()
        .max_by_key(|(count, _)| *count)
        .unwrap_or((0, "en"));
    if count > 0 {
        if scripts.latin > 0 && is_balanced_mix(count, scripts.latin) {
            return Detection::Undetermined(UndeterminedReason::MixedScript);
        }
        return detected(code, Confidence::High);
    }

    if scripts.other >= 2 && scripts.latin >= 2 {
        return Detection::Undetermined(UndeterminedReason::MixedScript);
    }
    if scripts.latin > 0 {
        return analyze_latin(&body);
    }

    Detection::Undetermined(UndeterminedReason::WeakEvidence)
}

fn detected(code: &'static str, confidence: Confidence) -> Detection {
    Detection::Detected {
        lang: Lang::new(code),
        confidence,
    }
}

/// 两种书写系统是否势均力敌：任一侧占 2/3 以上即视为该侧主导，只有双方都不足 2/3
/// 才算真正的平衡混排。这样「这个 API 返回 JSON」判中文，而「please see 文档」仍不硬判。
fn is_balanced_mix(first: usize, second: usize) -> bool {
    let total = first + second;
    if total == 0 {
        return false;
    }
    first * 3 < total * 2 && second * 3 < total * 2
}

#[derive(Default, Clone, Copy)]
struct Scripts {
    latin: usize,
    han: usize,
    kana: usize,
    hangul: usize,
    cyrillic: usize,
    arabic: usize,
    thai: usize,
    hebrew: usize,
    devanagari: usize,
    greek: usize,
    other: usize,
}

impl Scripts {
    fn total_letters(&self) -> usize {
        self.latin
            + self.han
            + self.kana
            + self.hangul
            + self.cyrillic
            + self.arabic
            + self.thai
            + self.hebrew
            + self.devanagari
            + self.greek
            + self.other
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Script {
    Latin,
    Han,
    Kana,
    Hangul,
    Cyrillic,
    Arabic,
    Thai,
    Hebrew,
    Devanagari,
    Greek,
    Other,
}

fn count_scripts(text: &str) -> Scripts {
    let mut scripts = Scripts::default();
    for ch in text.chars() {
        match script_of(ch) {
            Some(Script::Latin) => scripts.latin += 1,
            Some(Script::Han) => scripts.han += 1,
            Some(Script::Kana) => scripts.kana += 1,
            Some(Script::Hangul) => scripts.hangul += 1,
            Some(Script::Cyrillic) => scripts.cyrillic += 1,
            Some(Script::Arabic) => scripts.arabic += 1,
            Some(Script::Thai) => scripts.thai += 1,
            Some(Script::Hebrew) => scripts.hebrew += 1,
            Some(Script::Devanagari) => scripts.devanagari += 1,
            Some(Script::Greek) => scripts.greek += 1,
            Some(Script::Other) => scripts.other += 1,
            None => {}
        }
    }
    scripts
}

fn script_of(ch: char) -> Option<Script> {
    // 只统计字母；数字、标点、空白、组合附加符号都不参与书写系统判断。
    if !ch.is_alphabetic() {
        return None;
    }
    let code = ch as u32;
    let script = if is_kana(code) {
        Script::Kana
    } else if is_hangul(code) {
        Script::Hangul
    } else if is_han(code) {
        Script::Han
    } else if is_cyrillic(code) {
        Script::Cyrillic
    } else if is_arabic(code) {
        Script::Arabic
    } else if is_thai(code) {
        Script::Thai
    } else if is_hebrew(code) {
        Script::Hebrew
    } else if is_devanagari(code) {
        Script::Devanagari
    } else if is_greek(code) {
        Script::Greek
    } else if is_latin(code) {
        Script::Latin
    } else {
        Script::Other
    };
    Some(script)
}

fn is_latin(code: u32) -> bool {
    // × (U+00D7) 和 ÷ (U+00F7) 不是字母，必须排除。
    if code == 0x00D7 || code == 0x00F7 {
        return false;
    }
    matches!(
        code,
        0x0041..=0x005A
            | 0x0061..=0x007A
            | 0x00C0..=0x00FF
            | 0x0100..=0x017F
            | 0x0180..=0x024F
            | 0x1E00..=0x1EFF
            | 0xFF21..=0xFF3A
            | 0xFF41..=0xFF5A
    )
}

fn is_han(code: u32) -> bool {
    matches!(
        code,
        0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xF900..=0xFAFF
            | 0x20000..=0x2A6DF
            | 0x2A700..=0x2B73F
            | 0x2B740..=0x2B81F
            | 0x2B820..=0x2CEAF
    )
}

fn is_kana(code: u32) -> bool {
    matches!(
        code,
        0x3040..=0x309F | 0x30A0..=0x30FF | 0x31F0..=0x31FF | 0xFF66..=0xFF9F
    )
}

fn is_hangul(code: u32) -> bool {
    matches!(
        code,
        0x1100..=0x11FF | 0x3130..=0x318F | 0xAC00..=0xD7AF
    )
}

fn is_cyrillic(code: u32) -> bool {
    matches!(
        code,
        0x0400..=0x04FF | 0x0500..=0x052F | 0x2DE0..=0x2DFF | 0xA640..=0xA69F
    )
}

fn is_arabic(code: u32) -> bool {
    matches!(
        code,
        0x0600..=0x06FF
            | 0x0750..=0x077F
            | 0x08A0..=0x08FF
            | 0xFB50..=0xFDFF
            | 0xFE70..=0xFEFF
    )
}

fn is_thai(code: u32) -> bool {
    matches!(code, 0x0E00..=0x0E7F)
}

fn is_hebrew(code: u32) -> bool {
    matches!(code, 0x0590..=0x05FF | 0xFB1D..=0xFB4F)
}

fn is_devanagari(code: u32) -> bool {
    matches!(code, 0x0900..=0x097F | 0xA8E0..=0xA8FF)
}

fn is_greek(code: u32) -> bool {
    matches!(code, 0x0370..=0x03FF | 0x1F00..=0x1FFF)
}

fn lowercased(text: &str) -> String {
    text.chars().flat_map(|ch| ch.to_lowercase()).collect()
}

/// 把拉丁字母切成小写词；单字母 ASCII 词（a/l/c/s 等）噪音太大直接丢弃，
/// 带变音符号的单字母（法语 à、意大利语 è）保留，它们是有效特征。
fn latin_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_alphabetic() && is_latin(ch as u32) {
            current.extend(ch.to_lowercase());
        } else if !current.is_empty() {
            flush_token(&mut current, &mut tokens);
        }
    }
    if !current.is_empty() {
        flush_token(&mut current, &mut tokens);
    }
    tokens
}

fn flush_token(current: &mut String, tokens: &mut Vec<String>) {
    let keep = current.chars().count() >= 2 || !current.is_ascii();
    if keep {
        tokens.push(std::mem::take(current));
    } else {
        current.clear();
    }
}

/// 按空白切 token，把其中的 URL、邮箱、裸域名整个剥掉，剩余 token 重新拼成正文。
/// 句中链接只影响它自己，不会让整段文本被当成 URL 放弃。
fn strip_links(text: &str) -> String {
    let kept: Vec<&str> = text
        .split_whitespace()
        .filter(|token| !is_link_token(token))
        .collect();
    kept.join(" ")
}

fn is_link_token(token: &str) -> bool {
    let token = trim_link_punct(token);
    if token.is_empty() {
        return false;
    }
    if token.contains("://") || token.starts_with("www.") {
        return true;
    }
    if is_email_token(token) {
        return true;
    }
    is_bare_domain(token)
}

fn trim_link_punct(token: &str) -> &str {
    token.trim_matches(|ch: char| {
        matches!(
            ch,
            '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | '"' | '\'' | '“' | '”' | '‘'
                | '’' | ',' | ';' | ':' | '!' | '?' | '.' | '。' | '，' | '；' | '：' | '！'
                | '？' | '（' | '）' | '【' | '】' | '「' | '」'
        )
    })
}

fn is_email_token(token: &str) -> bool {
    let Some((local, domain)) = token.rsplit_once('@') else {
        return false;
    };
    !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

fn is_bare_domain(token: &str) -> bool {
    let Some((host, tld)) = token.rsplit_once('.') else {
        return false;
    };
    if host.is_empty() {
        return false;
    }
    let tld = tld
        .trim_matches(|ch: char| !ch.is_alphabetic())
        .to_ascii_lowercase();
    const TLDS: &[&str] = &[
        "com", "org", "net", "io", "cn", "jp", "kr", "de", "fr", "es", "pt", "it", "nl", "ru",
        "ai", "dev", "app", "co", "me", "edu", "gov", "info", "xyz", "uk", "us", "ca", "au",
        "br", "in", "ch", "se", "no", "fi", "dk", "pl", "tr", "vn", "id", "th", "ar", "he",
    ];
    TLDS.contains(&tld.as_str())
}

fn looks_like_code(text: &str) -> bool {
    // 只认强代码标记，避免把普通句子里的分号/括号误杀。
    const MARKERS: &[&str] = &[
        "=>",
        "->",
        "::",
        "&&",
        "||",
        "==",
        "!=",
        "{",
        "}",
        "</",
        "/>",
        "```",
        "#include",
        "def ",
        "fn ",
        "function ",
        "const ",
        "var ",
        "console.log",
        "println(",
        "printf(",
        "public static",
        "system.out",
        "();",
        ");",
    ];
    let lower = text.to_ascii_lowercase();
    MARKERS.iter().any(|marker| lower.contains(marker))
}

struct LatinFeatures {
    code: &'static str,
    strong_words: &'static [&'static str],
    shared_words: &'static [&'static str],
    strong_chars: &'static [(char, u32)],
    patterns: &'static [(&'static str, u32)],
}

/// 特征表。`strong_words` 基本是该语言独有或高度常用的功能词；
/// `shared_words` 是跨语言碰撞词，只给低权重，靠其它特征打破平局。
static LATIN_FEATURES: &[LatinFeatures] = &[
    LatinFeatures {
        code: "en",
        strong_words: &[
            "the", "this", "that", "these", "those", "with", "from", "have", "has", "had",
            "are", "is", "were", "be", "been", "being", "you", "your", "they", "their", "them",
            "there", "here", "what", "when", "where", "which", "who", "whom", "whose", "how",
            "why", "not", "but", "and", "or", "for", "to", "of", "in", "on", "at", "as", "do",
            "does", "did", "will", "would", "can", "could", "should", "may", "might", "must",
            "my", "we", "us", "he", "she", "her", "it", "its", "an", "a", "if", "then", "than",
            "because", "while", "about", "into", "through", "over", "under", "between", "after",
            "before", "also", "just", "only", "very", "more", "most", "some", "any", "all",
            "each", "every", "both", "either", "neither", "please", "hello", "good", "morning",
            "world", "see", "thanks", "thank",
            "today", "tomorrow", "yesterday", "let", "use", "using", "am", "go",
        ],
        shared_words: &["die", "me", "no", "so", "man", "was"],
        strong_chars: &[],
        patterns: &[("th", 2), ("wh", 2), ("sh", 1), ("tion", 1), ("ing", 1), ("ly", 1), ("ed", 1)],
    },
    LatinFeatures {
        code: "fr",
        strong_words: &[
            "le", "les", "des", "une", "est", "sont", "dans", "pour", "avec", "sur", "pas",
            "qui", "plus", "aux", "elle", "elles", "nous", "vous", "mon", "ton", "son", "mais",
            "au", "ce", "cette", "ces", "tout", "tous", "toute", "toutes", "être", "avoir",
            "fait", "comme", "aussi", "très", "encore", "toujours", "jamais", "quand", "comment",
            "pourquoi", "leur", "leurs", "notre", "votre", "nos", "vos", "moi", "toi", "lui",
            "eux", "ne", "ni", "alors", "donc", "et", "je", "tu", "il", "ou", "oui", "ça",
            "noël", "déjà", "voilà", "bonjour", "merci", "beaucoup", "monde", "chat", "maison",
        ],
        shared_words: &["la", "de", "un", "que", "bien", "entre", "non", "en"],
        strong_chars: &[
            ('œ', 8),
            ('æ', 8),
            ('ÿ', 8),
            ('ç', 4),
            ('à', 3),
            ('â', 2),
            ('è', 3),
            ('ê', 2),
            ('ë', 2),
            ('î', 2),
            ('ï', 2),
            ('ô', 2),
            ('ù', 2),
            ('û', 2),
            ('é', 1),
        ],
        patterns: &[
            ("eau", 3),
            ("eux", 3),
            ("ique", 4),
            ("ment", 3),
            ("tion", 1),
            ("gn", 1),
            ("oi", 1),
            ("qu", 1),
            ("eu", 1),
        ],
    },
    LatinFeatures {
        code: "de",
        strong_words: &[
            "der", "die", "das", "den", "dem", "ein", "eine", "einen", "einem", "einer", "und",
            "ist", "sind", "war", "waren", "sein", "seine", "seinem", "seinen", "nicht", "ich",
            "du", "er", "sie", "wir", "ihr", "wie", "als", "auch", "aber", "oder", "bei", "aus",
            "mit", "für", "auf", "von", "zu", "zum", "zur", "im", "wird", "werden", "wurde",
            "wurden", "kann", "können", "muss", "müssen", "soll", "sollen", "hat", "habe",
            "haben", "hatte", "hatten", "dieser", "diese", "dieses", "diesen", "diesem", "mein",
            "dein", "kein", "keine", "sehr", "noch", "nur", "schon", "jetzt", "hier", "dort",
            "wenn", "weil", "dass", "ob", "durch", "gegen", "ohne", "um", "über", "unter",
            "zwischen", "nach", "vor", "seit", "will", "haus", "hund", "sondern", "schläft",
            "draußen", "katze", "guten", "morgen", "tag",
        ],
        shared_words: &["es", "was", "des", "so", "man", "in", "her", "kind"],
        strong_chars: &[('ß', 8), ('ä', 2), ('ö', 2), ('ü', 2)],
        patterns: &[
            ("sch", 4),
            ("tsch", 5),
            ("keit", 5),
            ("heit", 4),
            ("lich", 4),
            ("ung", 3),
            ("tz", 2),
            ("ck", 2),
            ("pf", 2),
            ("ei", 1),
            ("eu", 1),
            ("ng", 1),
        ],
    },
    LatinFeatures {
        code: "es",
        strong_words: &[
            "el", "los", "las", "una", "unas", "unos", "del", "para", "por", "sin", "más",
            "como", "al", "se", "su", "sus", "este", "esta", "estos", "estas", "pero", "hay",
            "estás", "estoy", "somos", "ser", "soy", "era", "fueron", "fue", "también", "muy",
            "ahora", "aquí", "allí", "cuando", "donde", "porque", "quien", "quienes", "nuestro",
            "nuestra", "vuestro", "tu", "tus", "mi", "mis", "te", "nos", "os", "les", "lo",
            "con", "es", "verdad", "quiero", "quiere", "tiene", "tienen", "hacer", "puede",
            "pueden", "todo", "todos", "nada", "algo", "uno", "otro", "otra", "gente", "niño",
            "niña", "casa", "jugar", "libro", "no", "hola", "mundo",
        ],
        shared_words: &[
            "la", "de", "un", "que", "bien", "entre", "sobre", "está", "están", "son", "a",
            "en", "para", "por", "como", "este", "esta", "todo", "ser", "era", "también",
            "porque", "cuando", "tiene", "gente",
        ],
        strong_chars: &[
            ('ñ', 8),
            ('¿', 6),
            ('¡', 6),
            ('á', 1),
            ('é', 1),
            ('í', 1),
            ('ó', 1),
            ('ú', 1),
        ],
        patterns: &[
            ("ción", 6),
            ("ciones", 6),
            ("dad", 4),
            ("iendo", 3),
            ("ll", 2),
            ("ía", 2),
            ("ado", 1),
            ("ido", 1),
            ("que", 1),
        ],
    },
    LatinFeatures {
        code: "pt",
        strong_words: &[
            "os", "as", "um", "uma", "uns", "umas", "do", "da", "dos", "das", "são", "em", "no",
            "na", "nos", "nas", "não", "mais", "ao", "aos", "seu", "sua", "seus", "suas", "isto",
            "você", "vocês", "ele", "ela", "eles", "elas", "nós", "há", "foi", "foram", "muito",
            "agora", "aqui", "ali", "onde", "quem", "nosso", "nossa", "teu", "tua", "com", "sem",
            "quer", "brincar", "menino", "menina", "livro", "casa", "sobre", "olá", "mundo",
        ],
        shared_words: &[
            "la", "de", "un", "que", "bien", "entre", "está", "estão", "ser", "era", "também",
            "porque", "quando", "como", "este", "esta", "todo", "para", "por", "a",
        ],
        strong_chars: &[
            ('ã', 8),
            ('õ', 8),
            ('ç', 3),
            ('â', 2),
            ('ê', 2),
            ('ô', 2),
            ('á', 1),
            ('é', 1),
            ('í', 1),
            ('ó', 1),
            ('ú', 1),
        ],
        patterns: &[
            ("ão", 6),
            ("ões", 6),
            ("ção", 6),
            ("ções", 6),
            ("nh", 3),
            ("lh", 3),
            ("mente", 2),
            ("dade", 4),
            ("ado", 1),
            ("ido", 1),
        ],
    },
    LatinFeatures {
        code: "it",
        strong_words: &[
            "il", "lo", "i", "gli", "uno", "di", "del", "della", "dei", "delle", "dello", "degli",
            "è", "sono", "nel", "nella", "nello", "negli", "nelle", "per", "senza", "che", "non",
            "più", "al", "allo", "alla", "agli", "alle", "si", "suo", "sua", "suoi", "sue",
            "questo", "questa", "questi", "queste", "io", "tu", "lui", "lei", "noi", "voi",
            "loro", "ma", "anche", "ancora", "molto", "bene", "adesso", "ora", "qui", "qua",
            "dove", "perché", "chi", "nostro", "nostra", "vostro", "vostra", "erano", "essere",
            "c'è", "bambino", "vuole", "giocare", "libro", "casa", "gente", "stai",
            "ciao", "mondo", "come",
        ],
        shared_words: &[
            "la", "le", "un", "una", "in", "con", "si", "lo", "a", "quando", "era", "tiene",
            "gente",
        ],
        strong_chars: &[('ì', 4), ('ò', 4), ('à', 1), ('è', 1), ('é', 1), ('ó', 1), ('ù', 1)],
        patterns: &[
            ("zione", 6),
            ("zioni", 6),
            ("ggi", 3),
            ("gli", 3),
            ("gn", 2),
            ("che", 2),
            ("chi", 1),
            ("ità", 4),
            ("mente", 1),
        ],
    },
    LatinFeatures {
        code: "nl",
        strong_words: &[
            "het", "een", "en", "zijn", "op", "voor", "met", "van", "te", "niet", "ik", "jij",
            "hij", "zij", "wij", "jullie", "dit", "dat", "deze", "die", "maar", "ook", "als",
            "bij", "uit", "wordt", "worden", "kan", "kon", "heb", "hebt", "heeft", "hebben",
            "had", "hadden", "mijn", "jouw", "haar", "ons", "hun", "wat", "wie", "waar",
            "wanneer", "waarom", "hoe", "omdat", "terwijl", "na", "over", "onder", "tussen",
            "zonder", "tegen", "sinds", "thuis", "spelen", "wil", "goed",
        ],
        shared_words: &["de", "is", "was", "waren", "in", "of", "kind", "als"],
        strong_chars: &[('ë', 2), ('ï', 2)],
        patterns: &[
            ("ij", 4),
            ("lijk", 5),
            ("tje", 3),
            ("sch", 2),
            ("aa", 1),
            ("ee", 1),
            ("oo", 1),
            ("uu", 1),
            ("ng", 1),
        ],
    },
    LatinFeatures {
        code: "vi",
        strong_words: &[
            "và", "là", "của", "có", "không", "trong", "được", "cho", "với", "này", "đó",
            "người", "các", "những", "một", "hai", "để", "khi", "như", "từ", "đến", "tôi",
            "bạn", "anh", "chị", "em", "chúng", "họ", "rất", "cũng", "nhưng", "vì", "nếu",
            "sau", "trước", "trên", "dưới", "giữa", "ngoài", "chào", "khỏe", "muốn", "làm",
            "biết", "thấy", "đi", "nói",
        ],
        shared_words: &[],
        strong_chars: &[
            ('đ', 8),
            ('ơ', 8),
            ('ư', 8),
            ('ă', 6),
            ('ạ', 6),
            ('ả', 6),
            ('ấ', 6),
            ('ầ', 6),
            ('ẩ', 6),
            ('ẫ', 6),
            ('ậ', 6),
            ('ắ', 6),
            ('ằ', 6),
            ('ẳ', 6),
            ('ẵ', 6),
            ('ặ', 6),
            ('ẻ', 6),
            ('ẽ', 6),
            ('ẹ', 6),
            ('ế', 6),
            ('ề', 6),
            ('ể', 6),
            ('ễ', 6),
            ('ệ', 6),
            ('ỉ', 6),
            ('ĩ', 6),
            ('ị', 6),
            ('ỏ', 6),
            ('ố', 6),
            ('ồ', 6),
            ('ổ', 6),
            ('ỗ', 6),
            ('ộ', 6),
            ('ớ', 6),
            ('ờ', 6),
            ('ở', 6),
            ('ỡ', 6),
            ('ợ', 6),
            ('ủ', 6),
            ('ứ', 6),
            ('ừ', 6),
            ('ử', 6),
            ('ữ', 6),
            ('ự', 6),
            ('ỷ', 6),
            ('ỹ', 6),
            ('ỵ', 6),
        ],
        patterns: &[("nh", 2), ("th", 1), ("ch", 1), ("tr", 1), ("ng", 1)],
    },
    LatinFeatures {
        code: "id",
        strong_words: &[
            "dan", "yang", "di", "ke", "dari", "untuk", "dengan", "pada", "ini", "itu", "adalah",
            "tidak", "ada", "akan", "dalam", "saya", "kamu", "dia", "kami", "kita", "mereka",
            "bisa", "karena", "juga", "tapi", "tetapi", "atau", "jika", "kalau", "saat", "ketika",
            "sudah", "belum", "sangat", "lebih", "banyak", "semua", "setiap", "antara", "apa",
            "kabar", "harus", "ingin", "tahu", "dilakukan",
        ],
        shared_words: &[],
        strong_chars: &[],
        patterns: &[("ng", 1), ("ny", 1), ("sy", 1)],
    },
    LatinFeatures {
        code: "tr",
        strong_words: &[
            "ve", "bir", "bu", "şu", "için", "ile", "olarak", "çok", "daha", "gibi", "var", "yok",
            "ben", "sen", "biz", "siz", "onlar", "ama", "fakat", "veya", "ise", "kadar", "sonra",
            "önce", "her", "bazı", "tüm", "şey", "zaman", "gün", "yıl", "bugün", "nasıl", "güzel",
            "kitap", "merhaba",
        ],
        shared_words: &["de", "da"],
        strong_chars: &[('ğ', 8), ('ş', 8), ('ı', 8), ('ç', 2), ('ö', 2), ('ü', 2)],
        patterns: &[],
    },
];

fn analyze_latin(text: &str) -> Detection {
    let tokens = latin_tokens(text);
    if tokens.is_empty() {
        // 只剩单字母（如 "a"），按单个词处理，不做硬判。
        return Detection::Undetermined(UndeterminedReason::SingleWord);
    }
    let unique: HashSet<&str> = tokens.iter().map(String::as_str).collect();
    let lower = lowercased(text);
    let lower_chars: HashSet<char> = lower.chars().collect();

    let mut scores: Vec<(&'static str, u32, bool)> = LATIN_FEATURES
        .iter()
        .map(|features| score_features(features, &unique, &lower_chars, &lower))
        .collect();
    scores.sort_by(|left, right| right.1.cmp(&left.1));

    let (best_code, best_score, best_has_strong_char) = scores[0];
    let second_score = scores.get(1).map(|(_, score, _)| *score).unwrap_or(0);

    if tokens.len() == 1 {
        if is_abbreviation(text) {
            return Detection::Undetermined(UndeterminedReason::Abbreviation);
        }
        // 单个词只有在出现排他性变音符号时才算可靠证据（如 Grüße、señor、ação），
        // 否则 Bonjour / Hello / OpenAI 这类词都可能被误判。
        if best_has_strong_char && best_score >= 6 && best_score >= second_score + 4 {
            return detected(best_code, Confidence::Medium);
        }
        return Detection::Undetermined(UndeterminedReason::SingleWord);
    }

    let margin = best_score - second_score;
    let confidence = if best_score >= 10 && margin >= 4 {
        Confidence::High
    } else if best_score >= 4 && margin >= 2 {
        Confidence::Medium
    } else {
        return Detection::Undetermined(UndeterminedReason::WeakEvidence);
    };
    detected(best_code, confidence)
}

fn score_features(
    features: &LatinFeatures,
    unique: &HashSet<&str>,
    lower_chars: &HashSet<char>,
    lower: &str,
) -> (&'static str, u32, bool) {
    let mut score = 0u32;
    for word in features.strong_words {
        if unique.contains(*word) {
            score += 4;
        }
    }
    for word in features.shared_words {
        if unique.contains(*word) {
            score += 1;
        }
    }

    let mut has_strong_char = false;
    for (ch, weight) in features.strong_chars {
        if lower_chars.contains(ch) {
            score += weight;
            if *weight >= 6 {
                has_strong_char = true;
            }
        }
    }

    for (pattern, weight) in features.patterns {
        let occurrences = lower.matches(*pattern).count().min(2) as u32;
        if occurrences > 0 {
            score += weight * occurrences;
        }
    }

    (features.code, score, has_strong_char)
}

fn is_abbreviation(text: &str) -> bool {
    if text.split_whitespace().count() != 1 {
        return false;
    }
    let letters: Vec<char> = text.chars().filter(|ch| ch.is_alphabetic()).collect();
    (2..=6).contains(&letters.len()) && letters.iter().all(|ch| ch.is_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(text: &str) -> Option<String> {
        match analyze(text) {
            Detection::Detected { lang, .. } => Some(lang.code().to_string()),
            Detection::Undetermined(_) => None,
        }
    }

    #[test]
    fn script_exclusive_languages_are_identified() {
        assert_eq!(code("你好世界，今天天气很好。"), Some("zh".to_string()));
        assert_eq!(code("これはテストです。"), Some("ja".to_string()));
        assert_eq!(code("안녕하세요, 반갑습니다."), Some("ko".to_string()));
        assert_eq!(code("Привет, как дела?"), Some("ru".to_string()));
        assert_eq!(code("مرحبا، كيف حالك؟"), Some("ar".to_string()));
        assert_eq!(code("สวัสดีครับ"), Some("th".to_string()));
        assert_eq!(code("שלום, מה שלומך?"), Some("he".to_string()));
        assert_eq!(code("नमस्ते, आप कैसे हैं?"), Some("hi".to_string()));
    }

    #[test]
    fn latin_languages_are_scored_instead_of_defaulting_to_english() {
        let samples = [
            ("The weather is nice today, and we will go to the park.", "en"),
            ("Le chat est dans la maison et il dort.", "fr"),
            ("Der Hund ist nicht im Haus, sondern er schläft draußen.", "de"),
            ("El niño está en la casa y quiere jugar.", "es"),
            ("O menino está em casa e não quer brincar.", "pt"),
            ("Il bambino è a casa e non vuole giocare.", "it"),
            ("Het kind is thuis en wil niet spelen.", "nl"),
            ("Xin chào, bạn khỏe không?", "vi"),
            ("Saya tidak tahu apa yang harus dilakukan.", "id"),
            ("Bu kitap çok güzel.", "tr"),
        ];
        for (text, expected) in samples {
            assert_eq!(code(text), Some(expected.to_string()), "text: {text}");
        }
    }

    #[test]
    fn non_english_latin_text_never_falls_back_to_english() {
        let samples = [
            ("Le chat est dans la maison et il dort.", "fr"),
            ("Der Hund ist nicht im Haus, sondern er schläft draußen.", "de"),
            ("El niño está en la casa y quiere jugar.", "es"),
            ("O menino está em casa e não quer brincar.", "pt"),
            ("Il bambino è a casa e non vuole giocare.", "it"),
            ("Het kind is thuis en wil niet spelen.", "nl"),
        ];
        for (text, expected) in samples {
            assert_ne!(code(text), Some("en".to_string()), "text: {text}");
            assert_eq!(code(text), Some(expected.to_string()), "text: {text}");
        }
    }

    #[test]
    fn high_confidence_for_clear_sentences() {
        assert!(matches!(
            analyze("Der Hund ist nicht im Haus, sondern er schläft draußen."),
            Detection::Detected {
                confidence: Confidence::High,
                ..
            }
        ));
    }

    #[test]
    fn isolated_words_without_function_words_stay_undetermined() {
        for text in ["Hello", "Bonjour", "OpenAI", "iPhone"] {
            assert!(
                matches!(analyze(text), Detection::Undetermined(_)),
                "isolated word should not be forced: {text}"
            );
        }
        assert_eq!(HeuristicDetector.detect("Hello"), None);
        assert_eq!(HeuristicDetector.detect("Bonjour"), None);
    }

    #[test]
    fn common_short_phrases_are_detected_instead_of_dropped() {
        let samples = [
            ("Good morning", "en"),
            ("Hello world", "en"),
            ("Hola mundo", "es"),
            ("Guten Tag", "de"),
            ("Merci beaucoup", "fr"),
            ("Bonjour le monde", "fr"),
            ("Ciao mondo", "it"),
        ];
        for (text, expected) in samples {
            assert_eq!(code(text), Some(expected.to_string()), "text: {text}");
        }
    }

    #[test]
    fn single_word_with_exclusive_diacritics_is_detected() {
        assert_eq!(code("Grüße"), Some("de".to_string()));
        assert_eq!(code("señor"), Some("es".to_string()));
        assert_eq!(code("ação"), Some("pt".to_string()));
    }

    #[test]
    fn abbreviations_and_non_linguistic_text_are_undetermined() {
        assert!(matches!(
            analyze("NASA"),
            Detection::Undetermined(UndeterminedReason::Abbreviation)
        ));
        assert!(matches!(
            analyze("API"),
            Detection::Undetermined(UndeterminedReason::Abbreviation)
        ));
        assert!(matches!(
            analyze("1234567890"),
            Detection::Undetermined(UndeterminedReason::NoLetters)
        ));
        assert!(matches!(
            analyze("!@#$%^&*()"),
            Detection::Undetermined(UndeterminedReason::NoLetters)
        ));
        assert!(matches!(
            analyze("https://example.com/path?q=1"),
            Detection::Undetermined(UndeterminedReason::Url)
        ));
        assert!(matches!(
            analyze("www.example.org"),
            Detection::Undetermined(UndeterminedReason::Url)
        ));
        assert!(matches!(
            analyze("const value = compute(input);"),
            Detection::Undetermined(UndeterminedReason::Code)
        ));
    }

    #[test]
    fn dominant_script_decides_mixed_text() {
        // 中文为主 + 英文术语，仍按中文翻译。
        for text in [
            "这个 API 返回 JSON",
            "请使用 AI 进行翻译",
            "OpenAI 发布了新模型 GPT-5",
            "Hello 你好",
            "Tokyo 東京",
            "Bonjour, 你好",
        ] {
            assert_eq!(code(text), Some("zh".to_string()), "text: {text}");
        }
        // 英文为主 + 少量中文人名/术语，仍按英文翻译。
        assert_eq!(
            code("Please update the document and send it to 张伟 before the meeting."),
            Some("en".to_string())
        );
    }

    #[test]
    fn balanced_mix_stays_undetermined() {
        assert!(matches!(
            analyze("please see 文档"),
            Detection::Undetermined(UndeterminedReason::MixedScript)
        ));
    }

    #[test]
    fn links_are_stripped_before_detection() {
        assert_eq!(
            code("See https://example.com for details"),
            Some("en".to_string())
        );
        assert_eq!(
            code("这是文档 https://example.com/guide 请参考"),
            Some("zh".to_string())
        );
        assert_eq!(
            code("请联系 alice@example.com 获取帮助"),
            Some("zh".to_string())
        );
        assert_eq!(
            code("contact alice@example.com for help"),
            Some("en".to_string())
        );
        assert_eq!(
            code("See example.com for details"),
            Some("en".to_string())
        );
        assert!(matches!(
            analyze("alice@example.com"),
            Detection::Undetermined(UndeterminedReason::Url)
        ));
        assert!(matches!(
            analyze("https://example.com/path?q=1"),
            Detection::Undetermined(UndeterminedReason::Url)
        ));
        // 裸域名被剥离后只剩 See，不再与葡萄牙语功能词 `com` 碰撞。
        assert!(matches!(
            analyze("See example.com"),
            Detection::Undetermined(UndeterminedReason::SingleWord)
        ));
    }

    #[test]
    fn known_script_boundaries_are_kept() {
        // 纯汉字日语与中文共用书写系统，只能判 zh；出现假名时才判 ja。
        assert_eq!(code("日本語"), Some("zh".to_string()));
        assert_eq!(code("これはテストです"), Some("ja".to_string()));
    }

    #[test]
    fn mixed_cyrillic_and_latin_is_undetermined() {
        assert!(matches!(
            analyze("Привет world"),
            Detection::Undetermined(UndeterminedReason::MixedScript)
        ));
    }

    #[test]
    fn english_short_phrase_with_function_word_is_still_detected() {
        assert_eq!(code("translate this"), Some("en".to_string()));
        assert_eq!(code("Hello world"), Some("en".to_string()));
        assert_eq!(code("How are you?"), Some("en".to_string()));
    }
}
