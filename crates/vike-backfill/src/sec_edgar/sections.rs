//! **A filing's standard sections**: the `Item` headings of a 10-K, a 10-Q, an 8-K and a 20-F, each
//! with its text, split from the plain text [`super::html_text::html_to_text`] gives.
//!
//! A heading is a LINE that opens with `Item` and an item number (`1`, `1A`, `7A`, `2.02`), in any
//! case, then an optional `.`, `:` or dash and the item's title. A 10-Q numbers its items again in
//! each part, so there the key carries the part the last `PART I`/`PART II` line opened
//! (`part_2_item_1a`); elsewhere it is the item alone (`item_1a`, `item_2_02`).
//!
//! ⚠ **A table of contents repeats every heading.** Each key's text runs from its heading to the next
//! heading of any key, and the occurrence kept is the one with the LONGEST text: a contents line is
//! followed at once by the next contents line, the real heading by the item itself. One run is no
//! item's text: the contents' LAST line, followed by a preamble and then the body's FIRST item, which
//! goes back in the items' order (the order their first lines give). Such a run counts as its own line
//! only (measured on Apple's 10-K of 2024-11-01, whose contents end at `Item 16.` before a page of
//! forward-looking statements). The kept headings are then cut at each other, in document order; the
//! text before the first (the cover page and the contents) is not a section.
//!
//! A form with no standard items (`DEF 14A`, `S-1`, `F-1`, `6-K`, the 13D and 13G schedules), or a
//! document in which no heading is found, is ONE section, [`WHOLE_DOCUMENT`].

/// The section key of a document kept whole.
pub const WHOLE_DOCUMENT: &str = "document";

/// The base forms whose primary document is split at its `Item` headings.
const ITEMIZED_FORMS: [&str; 4] = ["10-K", "10-Q", "8-K", "20-F"];

/// The form whose items are numbered again in each part.
const PART_NUMBERED_FORM: &str = "10-Q";

/// One section of a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// Its key: `item_1a`, `part_1_item_2`, `item_2_02`, or [`WHOLE_DOCUMENT`].
    pub key: String,
    /// Its heading line as written (`Item 1A. Risk Factors`); for a whole document, empty.
    pub title: String,
    /// Its text, the heading line first.
    pub text: String,
}

/// The sections of `text`, the plain text of the primary document of a `base_form` filing (no `/A`).
pub fn split_sections(base_form: &str, text: &str) -> Vec<Section> {
    let whole = || {
        vec![Section {
            key: WHOLE_DOCUMENT.to_string(),
            title: String::new(),
            text: text.to_string(),
        }]
    };
    if !ITEMIZED_FORMS.contains(&base_form) {
        return whole();
    }
    let by_part = base_form == PART_NUMBERED_FORM;
    let lines: Vec<&str> = text.lines().collect();
    // Byte offset of each line's start, and one past the end.
    let mut starts = Vec::with_capacity(lines.len() + 1);
    let mut offset = 0;
    for line in &lines {
        starts.push(offset);
        offset += line.len() + 1;
    }
    starts.push(text.len() + 1);

    let mut part = 0u32;
    let mut headings: Vec<(String, usize)> = Vec::new(); // (key, line index)
    for (i, line) in lines.iter().enumerate() {
        if let Some(p) = part_number(line) {
            part = p;
        } else if let Some(item) = item_number(line) {
            let key =
                if by_part { format!("part_{part}_item_{item}") } else { format!("item_{item}") };
            headings.push((key, i));
        }
    }
    if headings.is_empty() {
        return whole();
    }
    // The canonical order of the keys: the order they first appear in (the contents' order).
    let mut order: Vec<&str> = Vec::new();
    for (key, _) in &headings {
        if !order.contains(&key.as_str()) {
            order.push(key);
        }
    }
    let rank = |key: &str| order.iter().position(|k| *k == key).unwrap_or(0);
    // The occurrence of each key with the longest run to the next heading of any key. A run that the
    // next heading ends by going BACK in the order (the contents' last line, then the body's first
    // item) is no item's text: it counts as its own line only.
    let mut best: Vec<(String, usize, usize)> = Vec::new(); // (key, line, length)
    for (n, (key, line)) in headings.iter().enumerate() {
        let length = match headings.get(n + 1) {
            Some((next_key, next)) if rank(next_key) > rank(key) => starts[*next] - starts[*line],
            Some(_) => starts[*line + 1] - starts[*line],
            None => starts[lines.len()] - starts[*line],
        };
        match best.iter_mut().find(|(k, _, _)| k == key) {
            Some(kept) if kept.2 >= length => {}
            Some(kept) => *kept = (key.clone(), *line, length),
            None => best.push((key.clone(), *line, length)),
        }
    }
    best.sort_by_key(|(_, line, _)| *line);
    best.iter()
        .enumerate()
        .map(|(n, (key, line, _))| {
            let end_line = best.get(n + 1).map_or(lines.len(), |(_, l, _)| *l);
            let end = (starts[end_line] - 1).min(text.len());
            Section {
                key: key.clone(),
                title: lines[*line].to_string(),
                text: text[starts[*line]..end].to_string(),
            }
        })
        .collect()
}

/// The part a `PART I` … `PART IV` line opens (`Part II — Other Information`), or `None`.
fn part_number(line: &str) -> Option<u32> {
    let rest = strip_word(line, "part")?;
    let numeral: String = rest.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    let after = &rest[numeral.len()..];
    if !(after.is_empty() || after.starts_with([' ', '.', ':', '-', '\u{2014}', '\u{2013}'])) {
        return None;
    }
    match numeral.to_ascii_uppercase().as_str() {
        "I" => Some(1),
        "II" => Some(2),
        "III" => Some(3),
        "IV" => Some(4),
        _ => None,
    }
}

/// The longest line read as a heading: a sentence that happens to open with "Item 7 of …" is a
/// paragraph, many times longer than the longest standard heading (about a hundred characters).
const MAX_HEADING_BYTES: usize = 300;

/// The key form of the item number an `Item` heading line opens with (`1a`, `7`, `2_02`), or `None`.
fn item_number(line: &str) -> Option<String> {
    if line.len() > MAX_HEADING_BYTES {
        return None;
    }
    let rest = strip_word(line, "item")?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits > 2 {
        return None;
    }
    let mut number = rest[..digits].to_string();
    let mut tail = &rest[digits..];
    if let Some(sub) = tail.strip_prefix('.')
        && sub.len() >= 2
        && sub.as_bytes()[..2].iter().all(u8::is_ascii_digit)
    {
        number.push('_');
        number.push_str(&sub[..2]);
        tail = &sub[2..];
    } else if let Some(letter) = tail.chars().next().filter(char::is_ascii_alphabetic)
        && !tail[1..].starts_with(|c: char| c.is_ascii_alphabetic())
    {
        number.push(letter.to_ascii_lowercase());
        tail = &tail[1..];
    }
    let heading_ends = tail.is_empty()
        || tail.starts_with([' ', '.', ':', '-', '\u{2014}', '\u{2013}', '\t', '(']);
    heading_ends.then_some(number)
}

/// The rest of `line` after its first word, when that word is `word` in any case and a space follows.
fn strip_word<'a>(line: &'a str, word: &str) -> Option<&'a str> {
    let head = line.get(..word.len())?;
    let rest = line[word.len()..].strip_prefix(' ')?;
    head.eq_ignore_ascii_case(word).then(|| rest.trim_start())
}

#[cfg(test)]
#[path = "sections_tests.rs"]
mod sections_tests;
