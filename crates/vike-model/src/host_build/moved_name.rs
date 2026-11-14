//! The "this name moved" HINT for user Rust source: the market-data names a file still reaches
//! through `vike_model`, and the one-line fix, for an operator whose strategy or study stopped
//! compiling with rustc's bare "unresolved import". And its sibling, the "this name was renamed"
//! hint ([`RENAMED_NAMES`], [`renamed_name_hint`]): old name -> new name, each row naming the path a
//! file reaches the OLD name through: the seam names the one broker interface renamed through
//! `vike_model` (`HftBroker` is `OrderBroker` now), the order language's words plan 2b aligned
//! through `vike_tactics` (`Header` is `RuleDefaults`, `Side` is `vike_model::OrderSide`), and the
//! Heikin-Ashi candles through `vike_indicators::strategy_kit`, the names phase 4's abbreviation pass
//! spelled out on the strategy template's surface (`vike_model`'s `share_qty` is `share_quantity` now,
//! `vike_mm` is `vike_market_making`); and its market-data twin
//! ([`RENAMED_MARKETDATA_NAMES`], [`renamed_marketdata_hint`]) for a name renamed at
//! `vike_marketdata`'s root after it moved there (`QuoteTick` is `TickQuote`), a table of its own
//! because the moved hint reads it too ([`current_marketdata_name`]).
//!
//! The bar, tick, book and consolidator names live ONLY at `vike_marketdata`'s root
//! ([`MOVED_NAMES`] is the list). Nothing is migrated: a user file under `user_data` is the
//! operator's own and outside the repo, so the owner ruled that the tooling PRINTS A HINT and
//! changes nothing. Three entry points show every hint ([`source_hints`]), each next to rustc's own
//! error and never instead of it: the two compiled user-code hosts' build scripts (a
//! `cargo:warning=` line per hint per file, through [`folder_hints`] and the driver's
//! `Built::warnings`) and the strategy-file builder
//! (`crates/vike-strategy-builder/src/render.rs`'s `build_strategy_file`, appended to a `Compile` failure).
//!
//! ⚠ **[`RENAMED_NAMES`] holds BOTH spellings of each renamed name, so the scripted renames must
//! leave this file and its test alone** (they rewrite every other spelling): the old name is the
//! needle, the new one the fix.
//!
//! [`moved_name_hint`] reads source TEXT, not a syntax tree: comments, string and char literals are
//! blanked first, then every `vike_model` `::` path is followed — one name, a `{ .. }` group,
//! groups nested in groups, any layout across lines. A group item that goes on into a sub-module
//! (`params::Params`, `strategy::{..}`) names nothing at the root and is skipped; so are `self`,
//! `*` and an `as` rename's new name. A glob import names nothing, so it gets no hint.
//!
//! ⚠ **No literal in this module, its tests or its callers spells `vike_model` and a moved name
//! joined by `::`, nor `vike_tactics` and an old order-language word, nor an
//! old name of the abbreviation pass whole.** The codemods rewrite every
//! such spelling they find, string literals included, so a needle or a golden source written that
//! way would be rewritten into the NEW spelling and the hint would test nothing. Every one is built
//! from parts (`format!("{}::{}", "vike_model", name)`, `concat!("share", "_qty")`), and the message itself never needs the old
//! spelling.
//!
//! Pure except [`folder_hints`], which reads files; no environment and no clock (the
//! [`crate::host_build`] module doc says why that matters here).

use std::fs;
use std::path::{Path, PathBuf};

/// The names that left `vike_model`'s root for `vike_marketdata`'s: the market-data codemod's name
/// table, in its order. `crates/vike-model/tests/host_build/moved_name.rs` compiles a `use` of
/// every one from `vike_marketdata` and compares it with this list, so a name missing there or
/// spelled wrong here fails a test.
pub const MOVED_NAMES: [&str; 17] = [
    "Bar",
    "BarConsolidator",
    "BookLevel",
    "BookUpdate",
    "BookUpdateKind",
    "DeltaDecision",
    "FillSim",
    "L2Book",
    "OptionKind",
    "QuoteTick",
    "SeqPolicy",
    "TradeTick",
    "book_taker_price",
    "consolidate_quotes",
    "consolidate_trades",
    "quote_tick_to_bar",
    "trade_tick_to_bar",
];

/// The names a user file may still write that were RENAMED, `(root, old, new)`: `root` is the path
/// (a crate, or a crate's module) the file reaches the OLD name through, `new` the name there now,
/// or its full path when it left. Rooted at `vike_model`: the one broker interface (`Broker`,
/// `HftBroker` and the blueprint's `MultiHftBroker` are all `OrderBroker`), the seam names the
/// one-broker wave renames, the phase-4 floor renames and the order client id names the abbreviation
/// pass spelled out (owner 2026-11-11). Rooted at `vike_tactics`: the order
/// language's words plan 2b aligned (its section 8, approved by the owner 2026-11-13). Rooted at
/// `vike_indicators::strategy_kit`: the Heikin-Ashi candles, which a file derived from a gallery
/// rule may name (owner 2026-11-13). Also rooted at `vike_model`: the root names phase 4's abbreviation
/// pass spelled out; rooted at the old `vike_mm`:
/// every public root name of the crate, which is `vike_market_making` now (G14). Only names a user file can reach there are rows: the test
/// doubles are not, nor the rest of the strategy gallery's vocabulary, which the strategy template
/// never reached. `crates/vike-model/tests/host_build/renamed_targets.rs` holds the `vike_model` rows'
/// names against that root and `crates/vike-tactics/tests/renamed_names.rs` the `vike_tactics`
/// rows' and
/// `crates/vike-market-making/src/tests/renamed_names.rs` the maker crate's, so a misspelled one fails a test.
pub const RENAMED_NAMES: [(&str, &str, &str); 40] = [
    (OLD_ROOT, "Broker", "OrderBroker"),
    (OLD_ROOT, "HftBroker", "OrderBroker"),
    (OLD_ROOT, "MultiHftBroker", "OrderBroker"),
    (OLD_ROOT, "TaggedOrder", "OrderPlacement"),
    (OLD_ROOT, "OrderLifecycle", "OrderEvent"),
    (OLD_ROOT, "ClosedRow", "TradeClosedRow"),
    (OLD_ROOT, "BarrierHit", "TripleBarrierHit"),
    (OLD_ROOT, "BarrierKind", "TripleBarrierKind"),
    (OLD_ROOT, "ControllerParams", "PositionControllerParams"),
    (OLD_ROOT, "MarkTick", "TickMark"),
    (OLD_ROOT, "ResolvedGrid", "Grid"),
    (OLD_ROOT, "DataDelay", "StreamDelay"),
    (OLD_ROOT, "PegTo", "OrderPeg"),
    (OLD_ROOT, concat!("share", "_qty"), "share_quantity"),
    (OLD_ROOT, concat!("tif", "_expired"), "time_in_force_expired"),
    (OLD_ROOT, "InstanceOrigin", "OrderClientIdOrigin"),
    (OLD_ROOT, "OriginError", "OrderClientIdOriginError"),
    (OLD_ROOT, "is_valid_crypto_coid", "is_valid_crypto_order_client_id"),
    (LANGUAGE_ROOT, "Side", "vike_model::OrderSide"),
    (LANGUAGE_ROOT, "Header", "RuleDefaults"),
    (LANGUAGE_ROOT, "Commission", "RuleDefaultsFee"),
    (LANGUAGE_ROOT, "Run", "RuleRun"),
    (LANGUAGE_ROOT, "RunOptions", "RuleRunOptions"),
    (LANGUAGE_ROOT, "Sizing", "TacticsSizing"),
    (LANGUAGE_ROOT, "Input", "TacticsInput"),
    (LANGUAGE_ROOT, "Entry", "TacticsEntry"),
    (LANGUAGE_ROOT, "Order", "TacticsOrder"),
    (LANGUAGE_ROOT, "Close", "TacticsClose"),
    (LANGUAGE_ROOT, "Exit", "TacticsExit"),
    (LANGUAGE_ROOT, "Trail", "TacticsTrail"),
    (LANGUAGE_ROOT, "Lot", "TacticsBookLot"),
    (LANGUAGE_ROOT, "Binding", "TacticsBookBinding"),
    (LANGUAGE_ROOT, "Every", "HigherTimeframePeriod"),
    (LANGUAGE_ROOT, "HistoryBefore", "HigherTimeframeHistoryBefore"),
    (KIT_ROOT, "Candles", "HeikinAshiCandles"),
    (MAKER_ROOT, "reward", "vike_market_making::reward"),
    (MAKER_ROOT, "OwnBookFiltration", "vike_market_making::OwnBookFiltration"),
    (MAKER_ROOT, "SpreadMakerPreset", "vike_market_making::SpreadMakerPreset"),
    (MAKER_ROOT, "rearm_fee_floor", "vike_market_making::rearm_fee_floor"),
    (MAKER_ROOT, "MakerParamError", "vike_market_making::MakerParamError"),
];

/// The `vike_marketdata` root names RENAMED after they moved there, `(old, new)`: the `L2Book` and
/// `Tick` family roots of the phase-4 floor renames. A moved name's hint writes the CURRENT name
/// ([`current_marketdata_name`]), and a file still naming an old one through `vike_marketdata` draws
/// [`renamed_marketdata_hint`]. `crates/vike-model/tests/host_build/moved_name.rs` resolves every new
/// name at that root.
pub const RENAMED_MARKETDATA_NAMES: [(&str, &str); 8] = [
    ("BookLevel", "L2BookLevel"),
    ("BookUpdate", "L2BookUpdate"),
    ("BookUpdateKind", "L2BookUpdateKind"),
    ("DeltaDecision", "L2BookDeltaDecision"),
    ("FillSim", "L2BookFillSimulation"),
    ("QuoteTick", "TickQuote"),
    ("SeqPolicy", "L2BookSequencePolicy"),
    ("TradeTick", "TickTrade"),
];

/// The FIELDS the phase-4 renames gave a whole word on the types a user file reads (`Bar`, the ticks,
/// the book update, the fill and order events), `(old, new)`. Nothing answers to an old name: a file
/// that reads one stops compiling (or, for a Rhai file, is refused), and [`renamed_field_hint`] is the
/// one-line fix beside that error. ⚠ Each old needle is built from parts: the scripted rename
/// rewrites every field and literal spelling it finds, so a whole one here would become the new word.
pub const RENAMED_FIELDS: [(&str, &str); 2] =
    [(concat!("t", "s"), "timestamp"), (concat!("se", "q"), "sequence")];

/// The crate the market-data names left, a constant so no needle in this module is a joined literal.
const OLD_ROOT: &str = "vike_model";

/// The crate the names moved to.
const MARKETDATA_ROOT: &str = "vike_marketdata";

/// The order language's crate, a constant for the same reason.
const LANGUAGE_ROOT: &str = "vike_tactics";

/// The indicators' module of what a strategy file reads beside the registry (`Candles`' home).
const KIT_ROOT: &str = "vike_indicators::strategy_kit";

/// The market maker's crate under its OLD name (G14 renamed it `vike_market_making`), built from
/// parts so the crate-name rewrite cannot reach the needle.
const MAKER_ROOT: &str = concat!("vike", "_mm");

/// Every hint `source` draws, the moved one first, each ONE line: what each entry point prints.
pub fn source_hints(source: &str) -> Vec<String> {
    [
        moved_name_hint(source),
        renamed_name_hint(source),
        renamed_marketdata_hint(source),
        renamed_field_hint(source, None),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// The name `vike_marketdata`'s root spells today for a [`MOVED_NAMES`] entry: its
/// [`RENAMED_MARKETDATA_NAMES`] new name, or the entry itself.
pub fn current_marketdata_name(name: &'static str) -> &'static str {
    RENAMED_MARKETDATA_NAMES.iter().find(|(old, _)| *old == name).map_or(name, |(_, new)| new)
}

/// ONE operator-facing message for one file of user source, or `None` when the file reaches no
/// moved name through `vike_model`. Names come in the order the file first mentions them, each
/// once. The message is ONE line (a `cargo:warning=` directive cannot carry two):
///
/// ```text
/// `Bar` moved from `vike_model` to `vike_marketdata`: write `use vike_marketdata::Bar;`, keep the rest in `use vike_model::{…}`, and start any inline path to it with `vike_marketdata::`
/// ```
pub fn moved_name_hint(source: &str) -> Option<String> {
    let names = names_in(source, OLD_ROOT, &MOVED_NAMES);
    if names.is_empty() {
        return None;
    }
    let listed: Vec<String> = names.iter().map(|n| format!("`{n}`")).collect();
    let current: Vec<&str> = names.iter().map(|n| current_marketdata_name(n)).collect();
    let (import, pronoun) = match current.as_slice() {
        [one] => ((*one).to_string(), "it"),
        many => (format!("{{{}}}", many.join(", ")), "them"),
    };
    Some(format!(
        "{} moved from `{OLD_ROOT}` to `vike_marketdata`: write `use vike_marketdata::{import};`, \
         keep the rest in `use {OLD_ROOT}::{{…}}`, and start any inline path to {pronoun} with \
         `vike_marketdata::`",
        listed.join(", ")
    ))
}

/// ONE operator-facing line for one file of user source that still reaches a RENAMED name through
/// its row's root ([`RENAMED_NAMES`]), or `None`. Names come in the order the file first mentions
/// them, each once, one clause per root in the table's order:
///
/// ```text
/// `HftBroker` is now `OrderBroker` in `vike_model`: write the new name wherever the file names the old one
/// `HftBroker` is now `OrderBroker` in `vike_model`; `Side` is now `vike_model::OrderSide` in `vike_tactics`: write the new name wherever the file names the old one
/// ```
pub fn renamed_name_hint(source: &str) -> Option<String> {
    let mut roots: Vec<&'static str> = Vec::new();
    for (root, _, _) in RENAMED_NAMES {
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    let clauses: Vec<String> = roots
        .iter()
        .filter_map(|root| {
            let rows: Vec<(&'static str, &'static str)> = RENAMED_NAMES
                .iter()
                .filter(|(r, _, _)| r == root)
                .map(|(_, old, new)| (*old, *new))
                .collect();
            renamed_clause(source, root, &rows)
        })
        .collect();
    (!clauses.is_empty()).then(|| {
        format!("{}: write the new name wherever the file names the old one", clauses.join("; "))
    })
}

/// The same line for a file that still reaches a name RENAMED at `vike_marketdata`'s root
/// ([`RENAMED_MARKETDATA_NAMES`]), or `None`:
///
/// ```text
/// `QuoteTick` is now `TickQuote` in `vike_marketdata`: write the new name wherever the file names the old one
/// ```
pub fn renamed_marketdata_hint(source: &str) -> Option<String> {
    renamed_hint(source, MARKETDATA_ROOT, &RENAMED_MARKETDATA_NAMES)
}

fn renamed_hint(
    source: &str,
    root: &str,
    table: &[(&'static str, &'static str)],
) -> Option<String> {
    renamed_clause(source, root, table)
        .map(|clause| format!("{clause}: write the new name wherever the file names the old one"))
}

/// "`old` is now `new`, … in `root`" for the names of `table` that `source` reaches through `root`.
fn renamed_clause(
    source: &str,
    root: &str,
    table: &[(&'static str, &'static str)],
) -> Option<String> {
    let olds: Vec<&'static str> = table.iter().map(|(old, _)| *old).collect();
    let names = names_in(source, root, &olds);
    if names.is_empty() {
        return None;
    }
    let pairs: Vec<String> = names
        .iter()
        .filter_map(|old| table.iter().find(|(o, _)| o == old))
        .map(|(old, new)| format!("`{old}` is now `{new}`"))
        .collect();
    Some(format!("{} in `{root}`", pairs.join(", ")))
}

/// ONE line for a source that still READS a renamed field ([`RENAMED_FIELDS`]: `x.old`, never a
/// method call `x.old()`), or `None`. `receiver` narrows it to reads off one name (a Rhai indicator's
/// `on_bar` parameter); `None` takes every receiver, so a file's own field of that name draws the
/// hint too (it is a hint beside rustc's error, never instead of it). Names come in the order the
/// file first reads them, each once:
///
/// ```text
/// `.ts` is now `.timestamp` on vike's types: write the new name wherever the file reads the old one
/// ```
pub fn renamed_field_hint(source: &str, receiver: Option<&str>) -> Option<String> {
    let toks = tokens(source);
    let mut found: Vec<&(&str, &str)> = Vec::new();
    for (k, tok) in toks.iter().enumerate() {
        let (Tok::Dot, Some(Tok::Ident(name))) = (tok, toks.get(k + 1)) else { continue };
        if toks.get(k + 2) == Some(&Tok::Paren) {
            continue;
        }
        if let Some(r) = receiver
            && (k == 0 || toks[k - 1] != Tok::Ident(r))
        {
            continue;
        }
        if let Some(row) = RENAMED_FIELDS.iter().find(|(old, _)| old == name)
            && !found.contains(&row)
        {
            found.push(row);
        }
    }
    if found.is_empty() {
        return None;
    }
    let pairs: Vec<String> =
        found.iter().map(|(old, new)| format!("`.{old}` is now `.{new}`")).collect();
    Some(format!(
        "{} on vike's types: write the new name wherever the file reads the old one",
        pairs.join(", ")
    ))
}

/// Every `.rs` file under `dir` (recursively, in sorted path order) whose source draws a hint
/// ([`source_hints`]), as `<file>: <hint>` lines — one per hint, each a ready `cargo:warning=`
/// body. A file that cannot be read or is not UTF-8 is skipped: the hint is advisory, and rustc
/// reports such a file itself.
pub fn folder_hints(dir: &Path) -> Vec<String> {
    let mut files = Vec::new();
    rust_files(dir, &mut files);
    files.sort();
    files
        .iter()
        .flat_map(|f| {
            let source = fs::read_to_string(f).unwrap_or_default();
            source_hints(&source).into_iter().map(move |hint| format!("{}: {hint}", f.display()))
        })
        .collect()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(t) if t.is_dir() => rust_files(&path, out),
            Ok(t) if t.is_file() && path.extension().is_some_and(|e| e == "rs") => out.push(path),
            _ => {}
        }
    }
}

/// The names of `table` that `source` reaches through `root` (a crate, or `crate::module`), first
/// mention first, each once.
fn names_in(source: &str, root: &str, table: &[&'static str]) -> Vec<&'static str> {
    let toks = tokens(source);
    let segs: Vec<&str> = root.split("::").collect();
    let mut found = Vec::new();
    for k in 0..toks.len() {
        let Some(after) = path_after(&toks, k, &segs) else { continue };
        match toks.get(after) {
            Some(Tok::Ident(name)) => note(name, table, &mut found),
            Some(Tok::Open) => {
                group(&toks, after + 1, table, &mut found);
            }
            _ => {}
        }
    }
    found
}

/// The index just past `seg :: seg :: … ::` when the tokens from `k` spell `segs` so, else `None`.
fn path_after(toks: &[Tok<'_>], k: usize, segs: &[&str]) -> Option<usize> {
    let mut i = k;
    for &seg in segs {
        if toks.get(i) != Some(&Tok::Ident(seg)) || toks.get(i + 1) != Some(&Tok::PathSep) {
            return None;
        }
        i += 2;
    }
    Some(i)
}

fn note(name: &str, table: &[&'static str], found: &mut Vec<&'static str>) {
    if let Some(&hit) = table.iter().find(|m| **m == name)
        && !found.contains(&hit)
    {
        found.push(hit);
    }
}

/// Walk the items of a `{ .. }` group whose `{` is just before `i`; returns the index after its
/// `}`. An item that IS a name is noted, a nested group is walked as the same root, and anything
/// else (a sub-module path, `self`, `*`) is skipped to the next `,` or `}` at this depth.
fn group(
    toks: &[Tok<'_>],
    mut i: usize,
    table: &[&'static str],
    found: &mut Vec<&'static str>,
) -> usize {
    loop {
        match toks.get(i) {
            None => return i,
            Some(Tok::Close) => return i + 1,
            Some(Tok::Comma) => {
                i += 1;
                continue;
            }
            Some(Tok::Open) => i = group(toks, i + 1, table, found),
            Some(Tok::Ident(name)) if toks.get(i + 1) != Some(&Tok::PathSep) => {
                note(name, table, found);
                i += 1;
            }
            Some(_) => {}
        }
        i = item_end(toks, i);
    }
}

/// The index of the `,` or `}` that ends the group item containing `i`, at the item's own depth.
fn item_end(toks: &[Tok<'_>], mut i: usize) -> usize {
    let mut depth = 0usize;
    while let Some(t) = toks.get(i) {
        match t {
            Tok::Open => depth += 1,
            Tok::Close if depth == 0 => return i,
            Tok::Close => depth -= 1,
            Tok::Comma if depth == 0 => return i,
            _ => {}
        }
        i += 1;
    }
    i
}

/// The few token kinds a path walk needs; everything else is `Other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tok<'a> {
    Ident(&'a str),
    PathSep,
    Open,
    Close,
    Comma,
    /// `.`: a field read follows it ([`renamed_field_hint`]).
    Dot,
    /// `(`: after a name, a call rather than a field read.
    Paren,
    Other,
}

/// Lex `src` into [`Tok`]s, dropping comments (line, nested block, doc) and turning every string,
/// raw string, byte string and char literal into one `Other`, so neither can supply a path.
fn tokens(src: &str) -> Vec<Tok<'_>> {
    let b = src.as_bytes();
    let n = b.len();
    let at = |j: usize| b.get(j).copied().unwrap_or(0);
    let mut toks = Vec::new();
    let mut i = 0;
    while i < n {
        let c = b[i];
        if c == b'/' && at(i + 1) == b'/' {
            while i < n && b[i] != b'\n' {
                i += 1;
            }
        } else if c == b'/' && at(i + 1) == b'*' {
            i = block_comment_end(b, i + 2);
        } else if c == b'"' {
            i = quoted_end(b, i + 1, b'"');
            toks.push(Tok::Other);
        } else if c == b'\'' {
            i = char_or_lifetime_end(b, i);
            toks.push(Tok::Other);
        } else if c.is_ascii_digit() {
            while i < n && is_ident_byte(b[i]) {
                i += 1;
            }
            toks.push(Tok::Other);
        } else if is_ident_byte(c) {
            let start = i;
            while i < n && is_ident_byte(b[i]) {
                i += 1;
            }
            let word = &src[start..i];
            let next = at(i);
            if matches!(word, "r" | "br" | "cr")
                && (next == b'"' || next == b'#')
                && let Some(end) = raw_string_end(b, i)
            {
                i = end;
                toks.push(Tok::Other);
            } else if word == "r" && next == b'#' && is_ident_byte(at(i + 1)) {
                // A raw identifier, `r#name`: the name is the identifier.
                let s = i + 1;
                i = s;
                while i < n && is_ident_byte(b[i]) {
                    i += 1;
                }
                toks.push(Tok::Ident(&src[s..i]));
            } else if matches!(word, "b" | "c") && next == b'"' {
                i = quoted_end(b, i + 1, b'"');
                toks.push(Tok::Other);
            } else if word == "b" && next == b'\'' {
                i = quoted_end(b, i + 1, b'\'');
                toks.push(Tok::Other);
            } else {
                toks.push(Tok::Ident(word));
            }
        } else if c == b':' && at(i + 1) == b':' {
            toks.push(Tok::PathSep);
            i += 2;
        } else {
            match c {
                b'{' => toks.push(Tok::Open),
                b'}' => toks.push(Tok::Close),
                b',' => toks.push(Tok::Comma),
                b'.' => toks.push(Tok::Dot),
                b'(' => toks.push(Tok::Paren),
                _ if c.is_ascii_whitespace() => {}
                _ => toks.push(Tok::Other),
            }
            i += 1;
        }
    }
    toks
}

/// An identifier byte: ASCII alphanumeric, `_`, or any byte of a non-ASCII character (Rust admits
/// Unicode identifiers, and every byte boundary this leaves is an ASCII one).
fn is_ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

/// One past the `*/` closing a block comment whose `/*` ends just before `i` (nesting counted).
fn block_comment_end(b: &[u8], mut i: usize) -> usize {
    let mut depth = 1usize;
    while i < b.len() && depth > 0 {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            depth += 1;
            i += 2;
        } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
            depth -= 1;
            i += 2;
        } else {
            i += 1;
        }
    }
    i
}

/// One past the unescaped `quote` closing a literal whose body starts at `i`.
fn quoted_end(b: &[u8], mut i: usize, quote: u8) -> usize {
    while i < b.len() {
        if b[i] == b'\\' {
            i += 2;
        } else if b[i] == quote {
            return i + 1;
        } else {
            i += 1;
        }
    }
    b.len()
}

/// A raw string's end, where `i` is just after its `r`/`br`/`cr` prefix: `None` when what follows
/// is not `#`* then `"` (a raw identifier, say).
fn raw_string_end(b: &[u8], i: usize) -> Option<usize> {
    let mut j = i;
    while b.get(j) == Some(&b'#') {
        j += 1;
    }
    let hashes = j - i;
    if b.get(j) != Some(&b'"') {
        return None;
    }
    j += 1;
    while j < b.len() {
        let closes = b.get(j + 1..j + 1 + hashes).is_some_and(|h| h.iter().all(|c| *c == b'#'));
        if b[j] == b'"' && closes {
            return Some(j + 1 + hashes);
        }
        j += 1;
    }
    Some(b.len())
}

/// Past a char literal (`'x'`, `'\n'`, `'\u{..}'`, a multi-byte `'é'`) starting at the `'` at `i`,
/// or just past the `'` of a lifetime or label (`'a`), whose name is then lexed as an identifier.
fn char_or_lifetime_end(b: &[u8], i: usize) -> usize {
    match b.get(i + 1) {
        Some(b'\\') => quoted_end(b, i + 1, b'\''),
        Some(&c) => {
            let len = utf8_len(c);
            if b.get(i + 1 + len) == Some(&b'\'') { i + 2 + len } else { i + 1 }
        }
        None => i + 1,
    }
}

fn utf8_len(lead: u8) -> usize {
    match lead {
        0xF0..=0xFF => 4,
        0xE0..=0xEF => 3,
        0xC0..=0xDF => 2,
        _ => 1,
    }
}
