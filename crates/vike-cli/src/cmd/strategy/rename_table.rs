//! The rename table of ONE SDK step, as `migrate` reads it: the file the release side writes
//! (`xtask sdk-report --renames FILE`) and the packer ships at a snapshot's root, parsed by the format's
//! one reader (`vike_rust_strategy::sdk::rename_table`'s `RenameTable::parse`, whose module doc is the
//! contract). This file only finds the step's table and splits its rows by language: the `rust` rows
//! touch a `.rs` file (`kind` is not needed to rewrite a path), the `rhai` rows a `.rhai` one (`kind`
//! says which tokens are the name: `vike_model::host_build::source_edits`'s `RhaiRename`).
//!
//! An ABSENT table is an empty one, not a refusal: most cuts rename nothing, and the step's build is the
//! gate either way (a rename the table missed is a build error on its line, with the builder's hints
//! beside it).

use std::path::Path;

use vike_model::host_build::source_edits::{RhaiRename, SdkRename};
use vike_rust_strategy::sdk::rename_table::{RENAME_TABLE_FILE, RenameTable};

/// One step's rows, by the language a file is written in.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct StepTable {
    /// The `rust` rows.
    pub(super) rust: Vec<SdkRename>,
    /// The `rhai` rows.
    pub(super) rhai: Vec<RhaiRename>,
}

/// The rename table of the step INTO SDK `to`, read from its snapshot under `sdk_root`.
pub(super) fn step_into(sdk_root: &Path, to: u32) -> Result<StepTable, String> {
    let path = vike_rust_strategy::sdk::snapshot_dir(sdk_root, to).join(RENAME_TABLE_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            parse(&text, to).map_err(|e| format!("the rename table {}: {e}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(StepTable::default()),
        Err(e) => Err(format!("cannot read the rename table {}: {e}", path.display())),
    }
}

/// The rows of a table's text, which must be the step from SDK `to - 1`.
fn parse(text: &str, to: u32) -> Result<StepTable, String> {
    let table = RenameTable::parse(text)?;
    let step_from = format!("sdk-{}", to.saturating_sub(1));
    if table.from != step_from {
        return Err(format!("it is the step from {}, not from {step_from}", table.from));
    }
    let mut step = StepTable::default();
    for row in table.rows {
        let (old, new) = (row.old, row.new);
        match row.language.as_str() {
            "rust" => step.rust.push(SdkRename { old, new, hint: None }),
            // `RenameTable::parse` admits `rust` and `rhai` alone.
            _ => step.rhai.push(RhaiRename { kind: row.kind, old, new }),
        }
    }
    Ok(step)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A table as `xtask sdk-report --renames` writes it (its `RenameTable::render`).
    const TABLE: &str = "# The strategy SDK's rename table (crates/vike-rust-strategy/src/sdk/rename_table.rs has the format).\n\
                         vike-sdk-rename-table\t1\nfrom\tsdk-3\nto\t0123abcd\n\
                         rhai\tfn\tclose_all\t-\n\
                         rust\tfn\ta::old_fn\ta::new_fn\n\
                         rust\tstruct\ta::Gone\t-\n";

    #[test]
    fn the_rows_of_the_step_are_read_by_language() {
        assert_eq!(
            parse(TABLE, 4),
            Ok(StepTable {
                rust: vec![
                    SdkRename {
                        old: "a::old_fn".into(),
                        new: Some("a::new_fn".into()),
                        hint: None
                    },
                    SdkRename { old: "a::Gone".into(), new: None, hint: None },
                ],
                rhai: vec![RhaiRename { kind: "fn".into(), old: "close_all".into(), new: None }],
            })
        );
        assert!(parse(&TABLE.replace("rhai\tfn", "lua\tfn"), 4).is_err(), "an unknown language");
    }

    #[test]
    fn a_table_of_another_step_or_version_is_refused_and_an_absent_one_is_empty() {
        assert!(parse(TABLE, 5).is_err_and(|e| e.contains("not from sdk-4")));
        assert!(
            parse(&TABLE.replace("table\t1", "table\t2"), 4)
                .is_err_and(|e| e.contains("version 2"))
        );
        assert!(parse(&TABLE.replace("\ta::Gone\t-", "\ta::Gone"), 4).is_err());
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(step_into(dir.path(), 4), Ok(StepTable::default()));
    }

    /// The release side's writer and this reader are one format: a table `RenameTable::render` wrote reads
    /// back as its step, row for row.
    #[test]
    fn a_rendered_table_reads_back_as_its_step() {
        let rendered = RenameTable::parse(TABLE).expect("the sample table").render();
        assert_eq!(parse(&rendered, 4), parse(TABLE, 4));
    }
}
