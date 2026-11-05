//! 签名原语与 UTC 时间格式化。
//!
//! 只做确定性计算：哈希、HMAC、civil 历法（从 Unix 秒推 UTC 年月日，零 chrono 依赖）。
//! 时间源由 provider 注入，测试用固定值，签名因此可复现断言。

use base64::Engine as _;
use hmac::Mac;
use md5::Md5;
use sha1::Sha1;
use sha2::{Digest, Sha256};

pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    Digest::update(&mut hasher, data);
    hex::encode(hasher.finalize())
}

pub fn md5_hex(data: &[u8]) -> String {
    let mut hasher = Md5::new();
    Digest::update(&mut hasher, data);
    hex::encode(hasher.finalize())
}

pub fn hmac_sha256_hex(key: &[u8], data: &[u8]) -> String {
    let mut mac = hmac::Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    hex::encode(mac.finalize().into_bytes())
}

pub fn hmac_sha256_raw(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = hmac::Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

pub fn hmac_sha1_base64(key: &[u8], data: &[u8]) -> String {
    let mut mac = hmac::Hmac::<Sha1>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

/// 把 Unix 秒拆成 UTC 的 (YYYY-MM-DD, YYYYMMDDTHHMMSSZ, YYYY-MM-DDTHH:MM:SSZ)。
pub fn utc_parts(unix_secs: u64) -> (String, String, String) {
    let days = (unix_secs / 86_400) as i64;
    let (year, month, day) = civil_from_days(days);
    let secs_of_day = unix_secs % 86_400;
    let (hour, minute, second) = (
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    );
    (
        format!("{year:04}-{month:02}-{day:02}"),
        format!(
            "{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z"
        ),
        format!(
            "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"
        ),
    )
}

/// Howard Hinnant 的 civil_from_days：days since 1970-01-01 → (y, m, d)。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_parts_matches_known_instants() {
        // 2026-11-06 00:00:00 UTC = 1791388800？用已知值核对：
        // 1970-01-01 00:00:00 → 1970-01-01
        assert_eq!(utc_parts(0).0, "1970-01-01");
        assert_eq!(utc_parts(0).1, "19700101T000000Z");
        // 2000-02-29 12:00:00 UTC = 951825600
        assert_eq!(utc_parts(951_825_600).0, "2000-02-29");
        assert_eq!(utc_parts(951_825_600).2, "2000-02-29T12:00:00Z");
        // 2026-11-06 00:00:00 UTC = 2026-01-01 (1767225600) + 309 天
        assert_eq!(utc_parts(1_793_923_200).0, "2026-11-06");
        assert_eq!(utc_parts(1_793_923_200).1, "20261106T000000Z");
    }

    #[test]
    fn signatures_are_deterministic_hex_or_base64() {
        assert_eq!(sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(md5_hex(b"abc"),
            "900150983cd24fb0d6963f7d28e17f72");
        let sig = hmac_sha256_hex(b"key", b"msg");
        assert_eq!(sig.len(), 64);
        assert_eq!(sig, hmac_sha256_hex(b"key", b"msg"));
        let b64 = hmac_sha1_base64(b"key", b"msg");
        assert!(!b64.is_empty());
        assert_eq!(b64, hmac_sha1_base64(b"key", b"msg"));
    }
}
