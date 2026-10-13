//! `chain`'s pure hex / ABI helpers: 32-byte words, addresses, uint256 token ids and log fields.

use serde_json::Value;

// ---------------------------------------------------------------------------------------------
// Pure hex / ABI helpers (no dependency beyond `hex`, already a crate dep)
// ---------------------------------------------------------------------------------------------

/// Strip `0x` and lowercase.
pub(super) fn clean(s: &str) -> String {
    s.strip_prefix("0x").unwrap_or(s).to_ascii_lowercase()
}

/// Split an ABI data blob into 32-byte words (hex, no `0x`). A trailing partial word is dropped —
/// real ABI data is always word-aligned, and a truncated body must not panic.
pub fn data_words(data: &str) -> Vec<String> {
    let d = clean(data);
    d.as_bytes()
        .chunks(64)
        .filter(|c| c.len() == 64)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect()
}

/// One 32-byte word → `u128`. The high 16 bytes MUST be zero: every value this module reads
/// (payouts, 6-decimal token amounts, slot counts, index sets) fits comfortably, and silently
/// truncating a genuinely-256-bit value would fabricate a number. Token IDS are the one true
/// 256-bit field and go through [`u256_word_to_decimal`] instead.
pub fn word_u128(w: &str) -> Result<u128, String> {
    let w = clean(w);
    if w.len() != 64 || !w.is_ascii() {
        return Err(format!("expected a 32-byte word, got {} hex chars", w.len()));
    }
    if w[..32].bytes().any(|b| b != b'0') {
        return Err(format!("word exceeds u128: 0x{w}"));
    }
    u128::from_str_radix(&w[32..], 16).map_err(|e| format!("bad hex word: {e}"))
}

/// One 32-byte word → a lower-case `0x…` 20-byte address (the right-aligned convention every
/// indexed `address` topic uses).
pub fn word_address(w: &str) -> Result<String, String> {
    let w = clean(w);
    if w.len() != 64 || !w.is_ascii() {
        return Err(format!("expected a 32-byte word, got {} hex chars", w.len()));
    }
    Ok(format!("0x{}", &w[24..]))
}

/// One 32-byte word → the DECIMAL string form of the uint256 — which is exactly how Polymarket
/// spells an ERC-1155 outcome token id everywhere else in this crate (`/positions.asset`, the CLOB
/// `asset_id`, and therefore the vike `symbol`). Schoolbook base-256 → base-10 division, so no
/// bigint dependency is added for the one genuinely 256-bit field on this wire.
pub fn u256_word_to_decimal(w: &str) -> Result<String, String> {
    let w = clean(w);
    if w.len() != 64 {
        return Err(format!("expected a 32-byte word, got {} hex chars", w.len()));
    }
    let mut bytes = hex::decode(&w).map_err(|e| format!("bad hex word: {e}"))?;
    let mut digits = Vec::new();
    while bytes.iter().any(|b| *b != 0) {
        let mut rem = 0u32;
        for b in bytes.iter_mut() {
            let cur = (rem << 8) | u32::from(*b);
            *b = (cur / 10) as u8;
            rem = cur % 10;
        }
        digits.push(b'0' + rem as u8);
    }
    if digits.is_empty() {
        return Ok("0".to_string());
    }
    digits.reverse();
    Ok(String::from_utf8(digits).expect("ascii digits"))
}

/// A dynamic `uint256[]` living at `words[head]`'s offset → its elements. The offset is measured in
/// BYTES from the start of the data section (so `/32` is the word index), which is what makes the
/// decoders self-checking: a wrong layout assumption lands on a nonsense offset and errors instead
/// of returning plausible garbage.
pub(super) fn dyn_array(words: &[String], head: usize) -> Result<Vec<u128>, String> {
    let off = word_u128(words.get(head).ok_or("array head word missing")?)? as usize;
    if !off.is_multiple_of(32) {
        return Err(format!("array offset {off} is not word-aligned"));
    }
    let at = off / 32;
    let len = word_u128(words.get(at).ok_or("array length word missing")?)? as usize;
    let mut out = Vec::new();
    for i in 0..len {
        out.push(word_u128(words.get(at + 1 + i).ok_or("array element word missing")?)?);
    }
    Ok(out)
}

/// The same, for a `uint256[]` whose elements are 256-bit token IDs (decimal strings).
pub(super) fn dyn_array_ids(words: &[String], head: usize) -> Result<Vec<String>, String> {
    let off = word_u128(words.get(head).ok_or("array head word missing")?)? as usize;
    if !off.is_multiple_of(32) {
        return Err(format!("array offset {off} is not word-aligned"));
    }
    let at = off / 32;
    let len = word_u128(words.get(at).ok_or("array length word missing")?)? as usize;
    let mut out = Vec::new();
    for i in 0..len {
        out.push(u256_word_to_decimal(words.get(at + 1 + i).ok_or("array element word missing")?)?);
    }
    Ok(out)
}

pub(super) fn log_str(l: &Value, key: &str) -> String {
    l.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string()
}

pub(super) fn log_topic(l: &Value, i: usize) -> Result<String, String> {
    l.get("topics")
        .and_then(|t| t.as_array())
        .and_then(|t| t.get(i))
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("log has no topic[{i}]"))
}

pub(super) fn log_u64(l: &Value, key: &str) -> u64 {
    l.get(key)
        .and_then(|v| v.as_str())
        .and_then(|s| u64::from_str_radix(clean(s).as_str(), 16).ok())
        .unwrap_or(0)
}
