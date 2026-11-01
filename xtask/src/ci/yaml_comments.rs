//! The YAML comment-only rule (decision 0148): when do two texts of a YAML file differ in comments and
//! blank lines alone?
//!
//! `scripts/yaml_comment_only.sh` is this module's twin in bash and awk (the workflows that trigger on
//! their own file run it as their first job). Both read ONE table of samples,
//! `scripts/yaml_comment_only_samples.txt`, and `xtask/tests/ci_plan_gate/yaml_comment_rule.rs` holds the two
//! equal: this module's [`normalize`] text against the script's `--normalize` text, byte for byte, for every
//! sample. Change one, change the other and a sample.
//!
//! # The rule
//!
//! Two texts are the same when [`normalize`] gives equal output: blank lines dropped, and WHOLE-LINE
//! comments (first non-blank character `#`) dropped. Everything else is compared byte for byte, a
//! carriage return included (LF turned CRLF is a change); one only decides whether a line is blank or a
//! comment. **Every doubt is "not comment-only":**
//!
//!   * A TRAILING comment (`key: value # note`) is never stripped from a kept line: a hash inside a quoted
//!     string (`"#fff"`) or a URL fragment (`page#top`) is data, and telling it from a comment needs a
//!     YAML parser.
//!   * A whole-line hash inside a BLOCK SCALAR is a comment only in the literal scalar of a `run` key (a
//!     shell script) that cannot emit the line as data. In any other block scalar (`>` folded text, a
//!     `description: |`, a `- |` item) the line is text, and so is a blank line there. In a `run: |`
//!     script the hash and blank lines are kept as data when the script holds a heredoc (`<<`), a quote
//!     left open at the end of a code line (an odd number of single quotes, or of double quotes), or a
//!     backslash-quote: the ways a later hash line becomes the text of a heredoc or of a multi-line quoted
//!     string. A plain `echo`, `printf`, `cat` or `tee` is no reason.
//!   * The whole text is compared RAW (nothing dropped) when a line is indented with a tab, or a value
//!     that opens a quote does not close it on its own line (a multi-line quoted scalar).

/// The text of `text` that [`comment_only`] compares: every line that is neither blank nor a droppable
/// whole-line comment, each followed by `\n`, in order. Total: a doubt returns every line.
pub fn normalize(text: &str) -> String {
    let lines: Vec<Line> = text.split_terminator('\n').map(Line::new).collect();
    let n = lines.len();
    let mut doubt = lines.iter().any(|l| l.s.trim_start_matches(' ').starts_with('\t'));

    // Pass 1: the block scalars (which line each owns), and the quote doubt on every line outside one.
    let mut scalar_of: Vec<Option<usize>> = vec![None; n];
    let mut scalars: Vec<Scalar> = Vec::new();
    let mut i = 0;
    while i < n {
        let l = &lines[i];
        if l.tr.is_empty() || l.tr.starts_with('#') {
            i += 1;
            continue;
        }
        let mut v = &l.s[l.ind..];
        let mut col = l.ind;
        while let Some(rest) = v.strip_prefix('-') {
            let after = rest.trim_start_matches([' ', '\t']);
            if after.len() == rest.len() {
                break;
            }
            col += 1 + (rest.len() - after.len());
            v = after;
        }
        let cc = strip_trailing_comment(v).trim_end_matches([' ', '\t']);
        if let Some(indicator) = block_indicator(cc) {
            let key = key_of(cc);
            let key_col = if key.is_empty() { l.ind } else { col };
            let id = scalars.len();
            scalars.push(Scalar { droppable: key == "run" && indicator == '|', body: Vec::new() });
            let mut j = i + 1;
            while j < n && (lines[j].tr.is_empty() || lines[j].ind > key_col) {
                scalar_of[j] = Some(id);
                scalars[id].body.push(j);
                j += 1;
            }
            i = j;
            continue;
        }
        // The value after `key:` and its blanks (a colon with no blank after it opens no value).
        let key = key_of(cc);
        let after_key = cc.get(key.len() + 1..).unwrap_or("");
        let value = if !key.is_empty() && after_key.starts_with([' ', '\t']) {
            after_key.trim_start_matches([' ', '\t'])
        } else {
            cc
        };
        if let Some(q) = value.chars().next().filter(|c| *c == '"' || *c == '\'')
            && value.matches(q).count() % 2 == 1
        {
            doubt = true;
        }
        i += 1;
    }

    // A `run: |` script whose hash and blank lines might be data.
    for s in &mut scalars {
        let sensitive = s
            .body
            .iter()
            .map(|&j| &lines[j])
            .filter(|l| !l.tr.is_empty() && !l.tr.starts_with('#'))
            .any(|l| can_emit_a_line(l.s));
        if sensitive {
            s.droppable = false;
        }
    }

    let mut out = String::new();
    for (i, l) in lines.iter().enumerate() {
        let drops = !doubt
            && (l.tr.is_empty() || l.tr.starts_with('#'))
            && scalar_of[i].is_none_or(|id| scalars[id].droppable);
        if !drops {
            out.push_str(l.raw);
            out.push('\n');
        }
    }
    out
}

/// True when the only difference between `old` and `new` is blank lines and whole-line comments, by
/// [`normalize`]'s rules. Two equal texts are comment-only; the callers ask it of a file git reports as
/// changed.
pub fn comment_only(old: &str, new: &str) -> bool {
    normalize(old) == normalize(new)
}

/// One text line: `raw` as read, `s` without its carriage return, `tr` without leading and trailing
/// blanks, `ind` the number of leading spaces.
struct Line<'a> {
    raw: &'a str,
    s: &'a str,
    tr: &'a str,
    ind: usize,
}

impl<'a> Line<'a> {
    fn new(raw: &'a str) -> Self {
        let s = raw.strip_suffix('\r').unwrap_or(raw);
        Line {
            raw,
            s,
            tr: s.trim_matches([' ', '\t']),
            ind: s.len() - s.trim_start_matches(' ').len(),
        }
    }
}

/// A block scalar: its body lines, and whether a whole-line hash in it is a droppable comment.
struct Scalar {
    droppable: bool,
    body: Vec<usize>,
}

/// `v` without a trailing ` # comment`: cut at the first hash that follows a blank, blanks included.
fn strip_trailing_comment(v: &str) -> &str {
    let b = v.as_bytes();
    let blank = |c: u8| c == b' ' || c == b'\t';
    let Some(k) = (1..b.len()).find(|&k| b[k] == b'#' && blank(b[k - 1])) else { return v };
    let mut s = k - 1;
    while s > 0 && blank(b[s - 1]) {
        s -= 1;
    }
    &v[..s]
}

/// The block scalar indicator character (`|` or `>`) a value ends with: the indicator, then up to two of
/// `-`, `+` and digits, after the start of the value, a blank or a colon.
fn block_indicator(cc: &str) -> Option<char> {
    let b = cc.as_bytes();
    let n = b.len();
    for k in 0..=2usize {
        if n < k + 1 {
            break;
        }
        let at = n - k - 1;
        let c = b[at];
        if c != b'|' && c != b'>' {
            continue;
        }
        if !b[at + 1..].iter().all(|x| matches!(x, b'-' | b'+' | b'0'..=b'9')) {
            continue;
        }
        if at == 0 || matches!(b[at - 1], b' ' | b'\t' | b':') {
            return Some(c as char);
        }
    }
    None
}

/// The mapping key a value opens: the leading run of `[A-Za-z0-9_.-]` when a colon follows, else `""`.
fn key_of(cc: &str) -> &str {
    let len = cc
        .bytes()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
        .count();
    if len > 0 && cc.as_bytes().get(len) == Some(&b':') { &cc[..len] } else { "" }
}

/// True when a code line of a script could make a later hash line data: a heredoc, an odd number of
/// quotes (a quote left open across lines), a backslash-quote.
fn can_emit_a_line(x: &str) -> bool {
    x.contains("<<")
        || x.matches('\'').count() % 2 == 1
        || x.matches('"').count() % 2 == 1
        || x.contains("\\\"")
        || x.contains("\\'")
}
