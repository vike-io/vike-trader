//! The [`Config`]/[`Preferences`]/[`Flags`] consumption gate — the twin of
//! `policy_is_consumed.rs`, for the three types that were never gated.
//!
//! `policy_is_consumed.rs` exists because `Policy::max_total_exposure` shipped as a field, was
//! validated on load, was accepted by `deny_unknown_fields`, and was **read by nothing**. That gate
//! covers exactly one of the four settings types. The other three had the identical defect and no
//! gate at all: a clean-install validation found `flags.tradehub_control`, `config.tradehub_addr`,
//! `config.log_dir`, `config.state_dir` and `preferences.log_file_level` all displayed as effective
//! — by `vike-cli config show`, which attributes a value to a file and prints its origin — while
//! nothing read any of them. No control server. Nothing listening. The trace log in the wrong
//! directory, at `trace`, which once wrote 341 GB onto the disk hosting a live trading node.
//!
//! Positive confirmation of something false is worse than an unimplemented feature, and this gate is
//! what makes it un-shippable. The table it checks is `vike_config::CONSUMPTION` — `pub` data rather
//! than a fixture, because a test alone would have left `config show` still lying; the command reads
//! the same rows and names the unread keys.
//!
//! # The four directions
//!
//! 1. [`every_setting_has_a_consumption_row`] — the row set equals `setting_keys()` minus `policy.*`.
//!    A new `Config`/`Preferences` field or a new `Flags` entry fails here until its author states
//!    where it is consumed or admits that it is not.
//! 2. [`every_claimed_consumer_really_reads_it`] — THE direction. Each `Consumer::At` row's file must
//!    exist and contain its needle. This is what turns red on the day the read is deleted, and it is
//!    what was red for all five keys above before they were wired.
//! 3. [`a_claimed_consumer_is_outside_the_settings_crate`] — a needle inside `crates/vike-config/`
//!    does not count. This crate parses, validates, clamps and serializes every field; if that were
//!    consumption, every row could claim `At` and the table would assert nothing.
//! 4. [`an_unconsumed_setting_is_really_unconsumed`] — a `Consumer::Not` row must carry a real
//!    argument, and must not have quietly GAINED a consumer since it was written.

use std::path::{Path, PathBuf};

use vike_config::consumed::{CONSUMPTION, Consumer, keys_requiring_a_row};

/// Workspace root from `CARGO_MANIFEST_DIR` (never CWD) — the same idiom every other source-walking
/// gate in this workspace uses.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// The declaring crate. A read here is the settings machinery reading itself.
const SELF_CRATE: &str = "crates/vike-config/";

/// **A textual match that is a DIFFERENT SYMBOL** — `(key, path prefix, why)`.
///
/// ⚠ The unread-direction search below is `line.contains(row.key)`, and a settings key is
/// `<section>.<field>`. That is section-qualified, which is what makes it narrow enough to be
/// useful — but it still matches any Rust expression ending in the same two path segments. A struct
/// with a field named `state_dir`, reached through a binding named `config`, reads as
/// `self.config.state_dir` and collides exactly.
///
/// The collision is REAL rather than hypothetical, so this table exists instead of the search being
/// loosened: loosening it would let a genuine reader hide, which is the failure the whole file is
/// for. A row here must name the OTHER symbol, not merely assert innocence.
const DIFFERENT_SYMBOL: &[(&str, &str, &str)] = &[(
    "config.state_dir",
    "crates/vike-core/src/runtime/",
    "`vike_core::CoreConfig::state_dir` — a field of the CORE's own config struct, reached as \
     `self.config.state_dir`, not the `config.state_dir` settings key. ⚠ And the two are not even \
     connected any more: `crates/vike-desktop/src/main.rs`'s deleted `state_dir_path` was the ONLY \
     production code that ever fed the settings value into `CoreConfig`, so since the desktop cut \
     nothing outside `vike-core`'s own tests sets that field at all. Its readers here — the \
     strategy-state save timer's arming checks — therefore see `None` in every shipped binary. \
     That is a pre-existing gap in the DAEMON (it never set the field either), widened rather than \
     caused by the cut, and it is recorded here because this gate is the only place the two facts \
     meet.",
)];

#[test]
fn every_setting_has_a_consumption_row() {
    let mut required = keys_requiring_a_row();
    let mut rows: Vec<String> = CONSUMPTION.iter().map(|c| c.key.to_string()).collect();
    required.sort();
    rows.sort();
    assert_eq!(
        required, rows,
        "\n\nCONSUMPTION and the real settings keys disagree.\n\
         A new Config/Preferences field, or a new Flags entry, needs a row saying WHERE it is \
         consumed (Consumer::At {{ file, needle }}) or an explicit written admission that it is \
         not (Consumer::Not {{ why }}).\n\
         A removed setting needs its row deleted.\n\
         `policy.*` keys belong to crates/vike-config/tests/policy_is_consumed.rs and must NOT \
         appear here.\n"
    );
}

/// THE direction that catches a declared-but-unread setting: a row may claim a consumer, and this
/// opens the file and looks.
#[test]
fn every_claimed_consumer_really_reads_it() {
    let root = workspace_root();
    let mut failures = Vec::new();

    for row in CONSUMPTION {
        let Consumer::At { file, needle } = row.by else {
            continue;
        };
        let path = root.join(file);
        let Ok(source) = std::fs::read_to_string(&path) else {
            failures.push(format!(
                "{} claims a consumer in `{file}`, but that file does not exist (looked in {}). \
                 Point the row at the real consumer, or downgrade it to Consumer::Not with a \
                 reason.",
                row.key,
                path.display()
            ));
            continue;
        };
        if !source.contains(needle) {
            failures.push(format!(
                "{} is DECLARED but NOT CONSUMED.\n  \
                 The row claims `{file}` reads it as `{needle}`, and that text is not in the \
                 file.\n  \
                 A setting nothing reads is a setting the operator believes they configured and \
                 has not: the file validates, `deny_unknown_fields` accepts the key, and \
                 `vike-cli config show` prints the file as its ORIGIN — positive confirmation of \
                 something false.\n  \
                 Fix it by WIRING the setting (thread it from the binary that loads settings to \
                 the code that acts on it, and point the needle at that read), by DELETING the \
                 field, or by downgrading the row to Consumer::Not with a written reason naming \
                 the reader that owns the variable today.",
                row.key
            ));
        }
    }

    assert!(failures.is_empty(), "\n\n{}\n", failures.join("\n\n"));
}

/// A needle inside `crates/vike-config/` is the settings system reading itself, which every field
/// gets for free from `apply`/`apply_env`/`serialize`. Without this rule the gate above could be
/// satisfied by pointing at the loader, and it would then assert nothing at all.
#[test]
fn a_claimed_consumer_is_outside_the_settings_crate() {
    let offenders: Vec<&str> = CONSUMPTION
        .iter()
        .filter_map(|row| match row.by {
            Consumer::At { file, .. } if file.replace('\\', "/").starts_with(SELF_CRATE) => {
                Some(row.key)
            }
            _ => None,
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "these rows claim a consumer inside the declaring crate, which is not consumption — \
         parsing, validating, clamping and serializing a field is what this crate does to EVERY \
         field: {offenders:?}"
    );
}

/// The other direction: an admitted-unread setting whose `why` is not an argument, or which has
/// quietly gained a real reader, must be corrected rather than left with its excuse.
///
/// The reader search is deliberately narrow and anchored on the SECTION-qualified read
/// (`config.log_dir`, `flags.poly_exec`) that a wired consumer necessarily spells, skipping comment
/// lines so the prose EXPLAINING why a setting is unread does not read as the read it describes,
/// and skipping this crate for the reason above.
///
/// ⚠ **TEST code is not consumption** and is excluded two ways — by path ([`is_test_path`]) and, in a
/// `src/` file, by truncating the scan at the first `#[cfg(test)]`. Both are needed and both are
/// real: `crates/vike-cli/tests/config_cli.rs` asserts on the literal row key `flags.poly_exec`, and
/// `crates/vike-tradehub/src/tradehub_cli.rs`'s in-`src` test module writes `settings.flags.…` fields to
/// drive its own resolvers. Neither is the program acting on a setting. (The example here used to be
/// `vike-cli`'s in-`src` test writing `settings.preferences.rate_utilization` to exercise the policy
/// clamp; that clamp and both its fields are gone — a ceiling over a value nothing read.)
/// The truncation is a HEURISTIC resting on this workspace's convention that a
/// `#[cfg(test)]` module sits at the bottom of its file — the same assumption
/// `crates/vike-ops/tests/settings_registry.rs` makes, and stated here rather than hidden: a test
/// module placed mid-file would hide real reads BELOW it, which fails safe in the direction of the
/// `At` gate above (a claimed read is still verified) and unsafe only for this one.
#[test]
fn an_unconsumed_setting_is_really_unconsumed() {
    let root = workspace_root();
    let sources = rust_sources(&root.join("crates"));
    let mut failures = Vec::new();

    for row in CONSUMPTION {
        let Consumer::Not { why } = row.by else {
            continue;
        };
        // An excuse has to be an ARGUMENT naming the reader that owns the variable. "TODO" or a
        // one-liner is how a setting with no consumer and no defence gets waved through, which is
        // the exact shape being gated.
        assert!(
            why.len() > 60 && !why.to_lowercase().contains("todo"),
            "{} is marked Consumer::Not, but its `why` does not name what reads the variable \
             today: {why:?}",
            row.key
        );

        for path in &sources {
            let rel = path.strip_prefix(&root).unwrap_or(path).to_string_lossy().replace('\\', "/");
            if rel.starts_with(SELF_CRATE) || is_test_path(&rel) {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(path) else { continue };
            for (n, line) in source.lines().enumerate() {
                let trimmed = line.trim_start();
                // Everything from the file's own test module down is test code — see the doc.
                if trimmed.starts_with("#[cfg(test)]") {
                    break;
                }
                if trimmed.starts_with("//") || trimmed.starts_with('*') || trimmed.starts_with('#')
                {
                    continue;
                }
                if trimmed.contains(row.key) {
                    // ⚠ …unless this file is a declared collision for this key — see
                    // [`DIFFERENT_SYMBOL`]. Checked HERE rather than by skipping the file earlier,
                    // so the exemption is scoped to the ONE key that collides: every other key
                    // still gets a full read of this file.
                    if DIFFERENT_SYMBOL
                        .iter()
                        .any(|(key, prefix, _)| *key == row.key && rel.starts_with(prefix))
                    {
                        continue;
                    }
                    failures.push(format!(
                        "{} is marked Consumer::Not, but `{rel}:{}` reads it:\n    {}\n  \
                         Promote it to a Consumer::At row naming that read.",
                        row.key,
                        n + 1,
                        line.trim()
                    ));
                }
            }
        }
    }

    assert!(failures.is_empty(), "\n\n{}\n", failures.join("\n\n"));
}

/// **The exemption table is the one thing here that can hide a real reader, so it is gated too.**
///
/// A [`DIFFERENT_SYMBOL`] row silences a whole directory for one key. Left unattended that is
/// exactly how the next genuine consumer goes unnoticed — the failure this file exists to prevent,
/// wearing the costume of its own fix. Three ways a row rots, all fatal:
///
/// * its KEY stopped being `Consumer::Not` — the row was promoted, and the exemption is now
///   silencing a directory for a key that claims a reader elsewhere;
/// * its PATH no longer exists — the code it was written about moved or went;
/// * it stopped being NEEDED — nothing under that path matches the key any more, so the exemption
///   is pure unexamined licence and must be deleted.
#[test]
fn the_different_symbol_exemptions_have_not_rotted() {
    let root = workspace_root();
    let sources = rust_sources(&root.join("crates"));
    let mut failures = Vec::new();

    for (key, prefix, why) in DIFFERENT_SYMBOL {
        let row = CONSUMPTION.iter().find(|r| r.key == *key);
        match row.map(|r| r.by) {
            Some(Consumer::Not { .. }) => {}
            Some(Consumer::At { .. }) => failures.push(format!(
                "{key} is exempted for `{prefix}` but its row is `Consumer::At` now — a promoted \
                 key needs no exemption, and leaving one silences that directory for nothing. \
                 Delete the row."
            )),
            None => failures.push(format!("{key} is exempted but has no CONSUMPTION row at all.")),
        }

        assert!(
            why.len() > 60,
            "{key}'s exemption must NAME the other symbol, not assert innocence: {why:?}"
        );

        let dir = root.join(prefix.replace('/', std::path::MAIN_SEPARATOR_STR));
        if !dir.is_dir() {
            failures.push(format!("{key} is exempted for `{prefix}`, which is not a directory."));
            continue;
        }

        // …and it must still be LOAD-BEARING: something under that path must actually match, or the
        // exemption is licence nobody is using.
        let still_collides = sources.iter().any(|path| {
            let rel = path.strip_prefix(&root).unwrap_or(path).to_string_lossy().replace('\\', "/");
            rel.starts_with(prefix)
                && !is_test_path(&rel)
                && std::fs::read_to_string(path).is_ok_and(|s| {
                    s.lines().any(|l| {
                        let t = l.trim_start();
                        !t.starts_with("//") && t.contains(*key)
                    })
                })
        });
        if !still_collides {
            failures.push(format!(
                "{key}'s exemption for `{prefix}` matches nothing any more — the collision it was \
                 written for is gone. Delete the row; the search can see that directory again."
            ));
        }
    }

    assert!(failures.is_empty(), "\n\n{}\n", failures.join("\n\n"));
}

/// A repo-relative path that holds TEST code rather than program code: an integration-test
/// directory, a benchmark, or an example. A setting read in one of these is a test exercising the
/// loader, never the program acting on the value.
fn is_test_path(rel: &str) -> bool {
    rel.contains("/tests/") || rel.contains("/benches/") || rel.contains("/examples/")
}

/// Every `.rs` file under `dir`, minus build output and the vendored trees this workspace does not
/// own.
fn rust_sources(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                if name == "target" || name == "vendor" {
                    continue;
                }
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    out
}
