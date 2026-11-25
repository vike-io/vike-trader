//! **HTML character references** (decision 0141): [`decode_entities`], the ONE decoder of `&#NNN;`,
//! `&#xHHHH;` and the five common named references, shared by the datahub's RSS summaries
//! (`crates/vike-datahub/src/tool_feeds/news.rs`'s `clean_html`) and the SEC filing text
//! (`crates/vike-backfill/src/sec_edgar/html_text.rs`). Tags, whitespace and layout are each caller's.

/// Decode every numeric character reference (decimal `&#8217;` and hex `&#x2019;`), then `&apos;`,
/// `&quot;`, `&nbsp;` (to a plain space), `&lt;`, `&gt;` and, LAST, `&amp;`, so a decoded `&amp;`
/// never re-creates a reference head. A reference that is not well formed stays as written.
pub fn decode_entities(s: &str) -> String {
    decode_numeric_entities(s)
        .replace("&apos;", "'")
        .replace("&quot;", "\"")
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Decode every `&#NNN;` (decimal) and `&#xHHHH;` (hex) numeric character reference.
fn decode_numeric_entities(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find("&#") {
        result.push_str(&rest[..at]);
        let after = &rest[at + 2..];
        if let Some(semi) = after.find(';') {
            let code = &after[..semi];
            let cp = if let Some(hex) = code.strip_prefix(['x', 'X']) {
                u32::from_str_radix(hex, 16).ok()
            } else {
                code.parse::<u32>().ok()
            };
            if let Some(ch) = cp.and_then(char::from_u32) {
                result.push(ch);
                rest = &after[semi + 1..];
                continue;
            }
        }
        result.push_str("&#"); // not a valid entity — keep the literal and move on
        rest = after;
    }
    result.push_str(rest);
    result
}

#[cfg(test)]
#[path = "html_tests.rs"]
mod tests;
