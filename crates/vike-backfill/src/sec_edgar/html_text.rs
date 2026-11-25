//! **A filing document as plain text, one line per block**: what [`super::sections`] splits and what a
//! text row's `body` holds. A filing's HTML (inline XBRL included) is reduced to its words:
//!
//! - the `<head>`, `<script>`, `<style>`, comments and the inline XBRL header (`<ix:header>`, its hidden
//!   facts and contexts) are dropped whole;
//! - a block element (`p`, `div`, `br`, `tr`, `li`, `h1`..`h6`, `table`, `title`) ends a line, a table
//!   cell (`td`, `th`) is a tab, every other tag is dropped and its text kept;
//! - character references are decoded by `vike_utility::html::decode_entities`, a no-break space is a
//!   space, and inside a line a run of spaces and tabs is one space; blank lines go.
//!
//! A document with no markup (an old plain-text filing) comes out with its own lines, trimmed.

/// The elements whose whole content is dropped, closing tag included: the `head` (its `title` is the
/// file's name, `Document` in a real 8-K's exhibit, never the filing's words), scripts, styles and the
/// inline XBRL header.
const DROPPED: [&str; 4] = ["head", "script", "style", "ix:header"];

/// The elements that end a line where they open or close.
const BLOCKS: [&str; 15] = [
    "p", "div", "br", "tr", "li", "h1", "h2", "h3", "h4", "h5", "h6", "table", "title", "ul", "ol",
];

/// `html` as plain text, one line per block (the module doc).
pub fn html_to_text(html: &str) -> String {
    let mut raw = String::with_capacity(html.len() / 2);
    let lower = html.to_ascii_lowercase();
    let mut at = 0;
    while let Some(open) = html[at..].find('<').map(|i| at + i) {
        raw.push_str(&html[at..open]);
        if lower[open..].starts_with("<!--") {
            at = lower[open..].find("-->").map_or(html.len(), |i| open + i + 3);
            continue;
        }
        let Some(close) = html[open..].find('>').map(|i| open + i) else {
            at = html.len();
            break;
        };
        let name = tag_name(&lower[open + 1..close]);
        at = close + 1;
        if DROPPED.contains(&name) && !lower[open + 1..].starts_with('/') {
            let end = format!("</{name}");
            at = lower[at..].find(&end).map_or(html.len(), |i| {
                let end_at = at + i;
                lower[end_at..].find('>').map_or(html.len(), |j| end_at + j + 1)
            });
            continue;
        }
        if BLOCKS.contains(&name) {
            raw.push('\n');
        } else if name == "td" || name == "th" {
            raw.push('\t');
        }
    }
    if at < html.len() {
        raw.push_str(&html[at..]);
    }
    let decoded = vike_utility::html::decode_entities(&raw).replace('\u{a0}', " ");
    let mut out = String::with_capacity(decoded.len());
    for line in decoded.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        if words.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&words.join(" "));
    }
    out
}

/// The lower-case element name of a tag's inside (`p class="x"`, `/div`, `br/`), without the slash.
fn tag_name(inside: &str) -> &str {
    let inside = inside.trim_start_matches('/');
    let end =
        inside.find(|c: char| c.is_whitespace() || c == '/' || c == '>').unwrap_or(inside.len());
    &inside[..end]
}

#[cfg(test)]
#[path = "html_text_tests.rs"]
mod html_text_tests;
