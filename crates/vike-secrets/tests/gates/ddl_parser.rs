//! **The ONE reader for this crate's DDL**, shared by every gate in this binary.
//!
//! Three hand-written copies stood here before the gates were grouped into one binary, and they
//! were two algorithms, not three: `sqlite_sequence` and `store_link` both split the schema on the
//! literal `CREATE TABLE IF NOT EXISTS ` and cut each body at the first `) STRICT`, while
//! `null_discriminator` split on `;`, took every statement carrying the word `TABLE` and matched
//! the parentheses. This is the second, because it carries the stronger argument — a parser whose
//! own count is checked against a needle that shares none of its derivation (see [`indexes`]) can
//! be proven blind, and one that splits on the literal it then counts cannot.
//!
//! The two agree on everything the gates feed them (MEASURED: identical `(name, body)` lists over
//! [`vike_secrets::DDL`] and over `profile_ddl`), and differ only on input no gate supplies: the
//! first read nothing from a table spelled without `IF NOT EXISTS`, and panicked on a table
//! with no `STRICT` or no body. Neither guard is lost — [`assert_sees_every_table`] carries both
//! needles and a `STRICT` count, so a table that is dropped, double-counted or not `STRICT` still
//! reddens every gate that calls it.
//!
//! It lives in this binary and not in `tests/support`: `support` is the shared FIXTURE, and a
//! parser that only the schema gates read is not a fixture.

/// One `CREATE [UNIQUE] INDEX` statement, reduced to what the gates read.
#[derive(Debug, Clone)]
pub(crate) struct Index {
    pub(crate) name: String,
    pub(crate) table: String,
    /// The tokens after `WHERE`; empty for a full (non-partial) index.
    pub(crate) predicate: Vec<String>,
}

/// Strip the punctuation a token may carry from the SQL around it — `(venue,` -> `venue`.
pub(crate) fn bare(token: &str) -> &str {
    token.trim_matches(|c: char| !(c.is_alphanumeric() || c == '_'))
}

/// Every `CREATE … INDEX` statement in a schema.
///
/// ⚠ Driven by SPLITTING ON `;` and then classifying each statement, deliberately NOT by splitting
/// on a `"CREATE UNIQUE INDEX IF NOT EXISTS "` literal. A literal split makes the parser's own
/// count uncheckable — the only independent needle left would be a count of the same literal, which
/// agrees with the split by construction. The review of the sibling gate measured that shape;
/// [`the_parser_sees_every_index_in_the_ddl`] can check THIS one because its needle (the bare word
/// `INDEX`) and this split are different derivations.
pub(crate) fn indexes(ddl: &str) -> Vec<Index> {
    let mut out = Vec::new();
    for statement in ddl.split(';') {
        let tokens: Vec<&str> = statement.split_whitespace().collect();
        if !tokens.iter().any(|t| t.eq_ignore_ascii_case("INDEX")) {
            continue;
        }
        let on = tokens
            .iter()
            .position(|t| t.eq_ignore_ascii_case("ON"))
            .expect("a CREATE INDEX statement names the table it is ON");
        assert!(on >= 1, "a CREATE INDEX statement names the index before `ON`: {statement}");
        let name = bare(tokens[on - 1]).to_string();
        let table =
            bare(tokens.get(on + 1).expect("a CREATE INDEX statement names a table after `ON`"))
                .to_string();
        let predicate = match tokens.iter().position(|t| t.eq_ignore_ascii_case("WHERE")) {
            Some(w) => tokens[w + 1..].iter().map(|t| (*t).to_string()).collect(),
            None => Vec::new(),
        };
        out.push(Index { name, table, predicate });
    }
    out
}

/// `(table name, column/constraint body)` for every `CREATE TABLE` in a schema.
///
/// Driven by the same statement split [`indexes`] uses, for the same reason, and cross-checked the
/// same way by [`the_parser_sees_every_table_in_the_ddl`].
pub(crate) fn tables(sql: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for statement in sql.split(';') {
        let is_table = statement.split_whitespace().any(|t| t.eq_ignore_ascii_case("TABLE"));
        if !is_table {
            continue;
        }
        let Some(open) = statement.find('(') else { continue };
        let name = statement[..open]
            .split_whitespace()
            .next_back()
            .map(bare)
            .expect("a CREATE TABLE names the table before its body")
            .to_string();
        let Some(close) = matching_paren(statement, open) else { continue };
        out.push((name, statement[open + 1..close].to_string()));
    }
    out
}

/// The index of the `)` closing the `(` at `open`.
fn matching_paren(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in text.char_indices().skip_while(|(i, _)| *i < open) {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// A table body's top-level comma-separated parts — parenthesis-aware, so `CHECK (x IN (0, 1))`
/// stays one part instead of becoming three.
pub(crate) fn top_level_parts(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut cur = String::new();
    for c in body.chars() {
        match c {
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth = depth.saturating_sub(1);
                cur.push(c);
            }
            ',' if depth == 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// **The anti-vacuity guard for [`tables`]**, called by each gate's own
/// `the_parser_sees_every_table_in_the_ddl`.
///
/// A parser that quietly dropped a table would make every other test in the calling gate pass
/// blind to whatever it dropped, so the parsed count is held against three counts that share none
/// of its code: the bare word `TABLE` (the parser splits on `;`, this does not), the literal
/// `CREATE TABLE IF NOT EXISTS ` the shipped schema spells every table with, and `) STRICT`, which
/// every table in this store ends with. The last two are the guards the retired `store_link` and
/// `sqlite_sequence` copies carried; the first is `null_discriminator`'s.
pub(crate) fn assert_sees_every_table(ddl: &str) {
    let seen = tables(ddl).len();
    let by_word = ddl.split_whitespace().filter(|t| t.eq_ignore_ascii_case("TABLE")).count();
    let by_phrase = ddl.matches("CREATE TABLE IF NOT EXISTS ").count();
    let strict = ddl.matches(") STRICT").count();
    assert_eq!(
        seen, by_word,
        "the parser found {seen} table(s) but `DDL` uses the word TABLE {by_word} time(s) — a table \
         it drops is one every other test in the calling gate is blind to"
    );
    assert_eq!(
        seen, by_phrase,
        "the parser found {seen} table(s) but `DDL` contains {by_phrase} `CREATE TABLE IF NOT \
         EXISTS` statement(s) — the parser missed one (or double-counted), which would make every \
         OTHER test in the calling gate blind to whatever table it dropped"
    );
    assert_eq!(
        seen, strict,
        "the parser found {seen} table(s) but `DDL` closes {strict} body(ies) with `) STRICT` — \
         every table in this store is STRICT, so a table that is not is a table that accepts \
         anything in any column"
    );
}
