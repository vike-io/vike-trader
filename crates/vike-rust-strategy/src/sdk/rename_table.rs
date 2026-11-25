//! The RENAME TABLE: every name a strategy file may still write that the newer tree no longer has,
//! with its replacement where the SDK report found one (`xtask/src/sdk/report.rs`'s `rename_table`).
//! `vike-cli strategy migrate` (decision 0296, verdict 4) reads it to rewrite a file; this module is its
//! format, its ONE writer ([`RenameTable::render`]) and its ONE reader ([`RenameTable::parse`]).
//!
//! **One file, two crates.** It is std only so that xtask, which links no `vike-*` crate, compiles this
//! very file in by `#[path]` (`xtask/src/sdk/mod.rs`, as it does `build/source_hash.rs`) to write the
//! table, while the CLI reads it through this crate as `vike_rust_strategy::sdk::rename_table`.
//!
//! **Where it ships.** SDK N's snapshot carries the step from N - 1 to N as [`RENAME_TABLE_FILE`] at its
//! root, beside its marker (`<project>/vendor/vike-strategy-source/sdk-N/RENAMES`): the release writes it
//! with `xtask sdk-report --renames` and the packer (`scripts/package_strategy_source.sh`) puts it there.
//! An ABSENT file is an empty table, not a refusal: most cuts rename nothing.
//!
//! # The format, version [`RENAME_TABLE_VERSION`]
//!
//! UTF-8 text, one record per `\n`-ended line, fields split by ONE tab. A line starting with `#` is a
//! comment and a blank line is nothing; both may sit anywhere. The first three records, in this order
//! (each `\t` below is the one tab):
//!
//! ```text
//! vike-sdk-rename-table\t1
//! from\tsdk-3
//! to\t0123456789abcdef0123456789abcdef01234567
//! ```
//!
//! the format's name and version, the tree the names come FROM (an `sdk-N` tag) and the tree they go TO
//! (an `sdk-N` tag or a commit). Every later record is a row of four fields:
//!
//! ```text
//! rust\tfn\tvike_tactics::signal::cross_up\tvike_tactics::signal::crosses_up
//! rust\tstruct\tvike_model::Bar\tvike_marketdata::Bar
//! rhai\tindicator\tstochrsi\tstoch_rsi
//! rhai\tfn\tclose_all\t-
//! ```
//!
//! 1. the LANGUAGE the name is written in: `rust` (a `.rs` strategy file, a Rust path) or `rhai` (a
//!    `.rhai` file, a name the Rhai engine registers);
//! 2. the item's KIND: for `rust` one of the item kinds of `xtask/src/sdk/items.rs`'s `Item`; for `rhai`
//!    `fn`, `type`, `get`, `set` (what `vike-rhai` registers) or `indicator` (a registry row: its
//!    per-line accessors `<name>_<line>` move with it);
//! 3. the OLD name: a `::` path for `rust`, a bare name for `rhai`;
//! 4. the NEW name in the same spelling, or `-` when the name was removed and no replacement was found.
//!
//! No field is empty or holds whitespace; `(language, kind, old)` appears at most once. Rows are sorted
//! by language, then old name. A reader refuses any other version.

use std::collections::BTreeSet;

/// The table's file name at a snapshot's root, beside the marker `SDK` (`super::SDK_MARKER_FILE`).
pub const RENAME_TABLE_FILE: &str = "RENAMES";

/// The format's name, the first field of the first record.
pub const RENAME_TABLE_FORMAT: &str = "vike-sdk-rename-table";

/// The format's version, the second field of the first record.
pub const RENAME_TABLE_VERSION: u32 = 1;

/// What the fourth field holds for a name removed with no replacement found.
pub const REMOVED: &str = "-";

/// The two languages a row is written for.
pub const LANGUAGES: [&str; 2] = ["rust", "rhai"];

/// A rename table ([`RenameTable::render`] writes it, [`RenameTable::parse`] reads it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameTable {
    /// The tree the old names come from (an `sdk-N` tag).
    pub from: String,
    /// The tree the new names go to (a tag or a commit).
    pub to: String,
    pub rows: Vec<RenameRow>,
}

/// One row: a name the `from` tree has and the `to` tree does not.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RenameRow {
    /// One of [`LANGUAGES`].
    pub language: String,
    pub kind: String,
    pub old: String,
    /// `None`: removed, no replacement found.
    pub new: Option<String>,
}

impl RenameTable {
    /// The table as the format writes it, rows sorted.
    pub fn render(&self) -> String {
        let mut rows = self.rows.clone();
        rows.sort();
        let mut out = format!(
            "# The strategy SDK's rename table (crates/vike-rust-strategy/src/sdk/rename_table.rs has the format).\n\
             {RENAME_TABLE_FORMAT}\t{RENAME_TABLE_VERSION}\nfrom\t{}\nto\t{}\n",
            self.from, self.to
        );
        for row in rows {
            let new = row.new.as_deref().unwrap_or(REMOVED);
            out.push_str(&format!("{}\t{}\t{}\t{new}\n", row.language, row.kind, row.old));
        }
        out
    }

    /// Parse a table; `Err` in words for any line the format does not allow.
    pub fn parse(text: &str) -> Result<RenameTable, String> {
        let mut records = text
            .lines()
            .enumerate()
            .filter(|(_, line)| !line.is_empty() && !line.starts_with('#'))
            .map(|(at, line)| (at + 1, line.split('\t').collect::<Vec<&str>>()));
        let mut header = |key: &str| -> Result<String, String> {
            match records.next() {
                Some((_, fields)) if fields.len() == 2 && fields[0] == key && valid(fields[1]) => {
                    Ok(fields[1].to_string())
                }
                Some((at, fields)) => {
                    Err(format!("line {at}: expected `{key}<TAB><value>`, found {fields:?}"))
                }
                None => Err(format!("the table ends before its `{key}` record")),
            }
        };
        let version = header(RENAME_TABLE_FORMAT)?;
        if version != RENAME_TABLE_VERSION.to_string() {
            return Err(format!(
                "a rename table of version {version}: this reader reads version {RENAME_TABLE_VERSION}"
            ));
        }
        let from = header("from")?;
        let to = header("to")?;
        let mut rows = Vec::new();
        let mut seen = BTreeSet::new();
        for (at, fields) in records {
            let [language, kind, old, new] = fields[..] else {
                return Err(format!(
                    "line {at}: a row has 4 tab-separated fields, this one {}",
                    fields.len()
                ));
            };
            if !LANGUAGES.contains(&language) {
                return Err(format!(
                    "line {at}: language {language:?} is neither `rust` nor `rhai`"
                ));
            }
            if ![kind, old, new].iter().all(|f| valid(f)) {
                return Err(format!("line {at}: an empty field or one holding whitespace"));
            }
            if !seen.insert((language, kind, old)) {
                return Err(format!("line {at}: {language} {kind} {old} has a row already"));
            }
            let new = (new != REMOVED).then(|| new.to_string());
            rows.push(RenameRow {
                language: language.into(),
                kind: kind.into(),
                old: old.into(),
                new,
            });
        }
        Ok(RenameTable { from, to, rows })
    }
}

fn valid(field: &str) -> bool {
    !field.is_empty() && !field.chars().any(char::is_whitespace)
}
