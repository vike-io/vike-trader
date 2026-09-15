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
//! # The five directions
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
//! 5. [`an_unconsumed_rows_reader_claim_is_checked`] — **the direction this file was MISSING**, and
//!    the one an audit had to find by hand.
//!
//! # ⚠ What direction 4 never checked, and what it cost
//!
//! Direction 4 holds a `Consumer::Not` row's `why` to a LENGTH and to the absence of "todo". It
//! never opened the file the `why` NAMES. So six of the eight rows said, in substance, *the file
//! key is inert, export the environment variable instead* — while the function reading that
//! variable sat inside a poller (or a recorder constructor) **no composition root ever builds**.
//! `vike-cli config show` printed those sentences verbatim, as its own paragraph, to an operator
//! who had configured the key. That is `Policy::max_total_exposure`'s defect one level down:
//! positive confirmation of something false, printed by the command added to prevent exactly it.
//! Worse, the tree already KNEW about one of them elsewhere —
//! `crates/vike-ops/tests/kill_switch_gate.rs` carries a row reading "nothing outside tests
//! constructs this poller" about the same code this table described as a live reader.
//!
//! Direction 5 turns that claim into `vike_config::Reader` data and checks it: a
//! [`vike_config::Reader::Live`] row must show a caller OUTSIDE the file that defines the read, and
//! a [`vike_config::Reader::Uncalled`] row must show that every entry point it names has NO caller
//! anywhere outside test code. The two claims are each other's opposites, so a row cannot sit in
//! the gap between them, and wiring one of these features turns its own row red rather than leaving
//! it to go on lying.
//!
//! **The residual, declared:** this is substring reachability, not call-graph analysis. It cannot
//! see a call through a trait object, an alias or a macro, and `Reader::Live`'s single hop is
//! evidence rather than proof of reachability from `main`. It pins the exact link that was missing
//! in all six bad rows — an entry point nothing outside its own file calls — and nothing wider.
//!
//! # ⚠ What directions 4 and 5 could not see AT ALL, and how far it reached
//!
//! Both scans, and the `Reader::Live` check beside them, used to stop at the first line whose
//! trimmed text starts with `#[cfg(test)]`. `#[cfg(test)]` is an attribute on an ITEM, not a
//! marker for a test module, so a `#[cfg(test)] use`, `fn` or `const` wore the same spelling and
//! ended the scan having cut nothing. `crates/vike-tradehub/src/tradehub_cli.rs` — the daemon's
//! composition root, and the file a mount would be WIRED in — carries
//! `#[cfg(test)] use crate::feeds::CexBars;` about 300 lines into 7,684, so the live mount, every
//! `FoldTier` row, `make_engine` and `spawn_recon` were all invisible to the search that makes a
//! `Reader::Uncalled` row mean anything. Tree-wide: 823 `crates/**/src/*.rs` files carry a
//! `#[cfg(test)]`, and 231,897 lines sat after the first one in their files.
//!
//! [`production_lines`] is the repair and the one notion of production code every direction now
//! shares — which also closes a second hole on the other side, [`contains_code_in`] having
//! accepted a `Reader::Live` caller that existed only inside a test module.
//!
//! ⚠ **That sentence was FALSE when it was first written here, and the gap was the wrong way
//! round.** Two sites kept their own rule: direction 2 — *THE* direction, the one that certifies a
//! key IS read — used a raw `source.contains(needle)` over the whole file, and
//! [`the_different_symbol_exemptions_have_not_rotted`]'s collision probe skipped `//` and nothing
//! else. So the hole was closed on the rows admitting that NOTHING reads a key, and left open on
//! the rows making the stronger claim that something does. Mutation-proved on
//! `config.tradehub_addr`: commenting its only read out, and moving that read into its file's own
//! `#[cfg(test)] mod tests`, BOTH left the gate green while the setting became genuinely unread and
//! `vike-cli config show` went on printing the file as its ORIGIN. Both sites go through
//! [`contains_code_in`] now, and the claim above is true as of that change rather than as of the
//! sentence. Both halves are
//! mutation-proved against real files rather than fixtures
//! ([`the_scanner_reads_past_a_cfg_test_attribute_in_a_real_file`],
//! [`a_reader_live_caller_inside_a_test_module_does_not_count`]), because the fixture-only proof
//! that stood here before passed green for the whole period the real check was off.

use std::path::{Path, PathBuf};

use vike_config::consumed::{CONSUMPTION, Consumer, Reader, keys_requiring_a_row};

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
/// ⚠ **EMPTY, and that is a state this table has reached rather than started in.** Its one row was
/// `config.state_dir` against `crates/vike-core/src/runtime/`, where `vike_core::CoreConfig`'s own
/// field of that name reads as `self.config.state_dir` and collided exactly. The unread-settings
/// sweep DELETED the settings key (nothing had read it since the desktop cut took `state_dir_path`
/// with the local core), so the exemption had no row to exempt and
/// [`the_different_symbol_exemptions_have_not_rotted`] would have failed on it — which is that
/// test working, not a reason to keep the row. `vike_core::CoreConfig::state_dir` is untouched and
/// still means what it always did.
const DIFFERENT_SYMBOL: &[(&str, &str, &str)] = &[];

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
        // ⚠ `contains_code_in`, NOT `source.contains`. THE direction was the last site in this file
        // still holding its own notion of production code, and the module doc above claimed the
        // opposite — that `production_lines` is "the one notion every direction now shares". It was
        // not, and the gap was the wrong way round: the hole was closed on the rows that admit
        // NOTHING reads a key, and left open on the rows that CERTIFY one does.
        //
        // Mutation-proved on `config.tradehub_addr`, whose only read is a single line in
        // `crates/vike-tradehub/src/tradehub_cli.rs`. With a raw `contains`, BOTH of these left the
        // gate green while the setting became genuinely unread:
        //   * commenting the read out — the needle still matches, inside a comment;
        //   * moving the identical text into that file's own `#[cfg(test)] mod tests`.
        // In each case `vike-cli config show` goes on printing the file as the key's ORIGIN, which
        // is `Policy::max_total_exposure`'s defect exactly — the one this file exists to prevent.
        //
        // The contrast was inside this very file: the same mutation against a `Reader::Live` row's
        // evidence went RED, because that direction had already been moved onto the shared rule.
        // Identical mutation shape, opposite verdicts, on the two halves of one question.
        if !contains_code_in(&source, needle) {
            failures.push(format!(
                "{} is DECLARED but NOT CONSUMED.\n  \
                 The row claims `{file}` reads it as `{needle}`, and that text is not in the \
                 file's PRODUCTION code (a match inside a comment or a `#[cfg(test)] mod` does not \
                 count — the setting would be unread in every shipped build).\n  \
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
/// ⚠ **TEST code is not consumption** and is excluded two ways — by path ([`is_test_path`]) and, in
/// a `src/` file, by [`production_lines`] cutting out each test MODULE. Both are needed and both
/// are real: `crates/vike-cli/tests/config_cli.rs` asserts on the literal row key
/// `flags.poly_exec`, and `crates/vike-tradehub/src/tradehub_cli.rs`'s in-`src` test module writes
/// `settings.flags.…` fields to drive its own resolvers. Neither is the program acting on a
/// setting. (The example here used to be `vike-cli`'s in-`src` test writing
/// `settings.preferences.rate_utilization` to exercise the policy clamp; that clamp and both its
/// fields are gone — a ceiling over a value nothing read.)
///
/// ⚠ **This paragraph used to describe a TRUNCATION at the first `#[cfg(test)]` and to defend it
/// as a heuristic resting on a convention.** It was not a heuristic about where test modules sit —
/// it was a mis-read of what the attribute means, and it disabled this scan (and the no-caller
/// search that shares it) over 231,897 lines of `src/`. [`production_lines`] carries the
/// measurement and the repair.
#[test]
fn an_unconsumed_setting_is_really_unconsumed() {
    let root = workspace_root();
    let sources = rust_sources(&root.join("crates"));
    let mut failures = Vec::new();

    for row in CONSUMPTION {
        let Consumer::Not { why, .. } = row.by else {
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
            for (n, line) in production_lines(&source) {
                if line.contains(row.key) {
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
                        "{} is marked Consumer::Not, but `{rel}:{n}` reads it:\n    {line}\n  \
                         Promote it to a Consumer::At row naming that read.",
                        row.key
                    ));
                }
            }
        }
    }

    assert!(failures.is_empty(), "\n\n{}\n", failures.join("\n\n"));
}

/// **Direction 5 — the `Consumer::Not` row's READER claim, checked instead of read.**
///
/// See the module doc for what the absence of this test cost. Per variant:
///
/// * [`Reader::Live`] — `file` must exist and contain `needle` (the read), and `caller` must be a
///   DIFFERENT existing file containing `call`. The second half is the whole point: a read with no
///   caller outside its own file is precisely the shape six rows claimed to be live while being
///   dead. `file` must also lie outside `crates/vike-config/` — this crate resolving the variable
///   into a field is not a consumer, the same rule direction 3 applies to `Consumer::At`.
/// * [`Reader::Uncalled`] — `file` must exist and contain `needle`, AND every entry point named
///   must have no call site in any non-test source. Each entry must be `Type::method`-qualified,
///   because a bare free-function name matches its own `fn name(` definition and would make the
///   search pass on every row forever. ⚠ That qualification rule leaves a DOOR the entry list can
///   never name — the read itself, when it is a `pub fn` its crate exports — so the read's own
///   name is searched too, outside its defining file and outside this crate. Without it a root
///   calling `vike_polymarket::heartbeat_enabled(&vars)` directly makes the variable live while
///   every `entry` stays uncalled and the row goes on saying neither spelling does anything.
/// * [`Reader::Nothing`] — nothing to open. Covered from the other side; see the variant's doc.
#[test]
fn an_unconsumed_rows_reader_claim_is_checked() {
    let root = workspace_root();
    let sources = rust_sources(&root.join("crates"));
    let mut failures = Vec::new();

    for row in CONSUMPTION {
        let Consumer::Not { reader, .. } = row.by else {
            continue;
        };
        // ⚠ FIRST, the cross-gate constraint, because getting it wrong reddens a gate in a
        // different crate with a message about something else entirely. A `needle` is a string
        // LITERAL living in `crates/vike-config/src/consumed.rs`, and
        // `crates/vike-ops/tests/settings_registry.rs` scans `src/` for env-read-shaped TEXT with
        // no call syntax to anchor on. An env-shaped needle therefore makes that gate believe THIS
        // crate reads the variable. Measured: a needle spelling the read of `RECORD_CHAINS_ENV`
        // failed `every_read_variable_is_declared`, `library_rows_do_not_grow` (a RATCHET, so the
        // repair is not merely adding a row) and `dynamic_sites_are_allowlisted` at once. Name the
        // reading function's SIGNATURE instead — see `Reader`'s own ⚠⚠ note.
        let needle = match reader {
            Reader::Live { needle, .. } | Reader::Uncalled { needle, .. } => Some(needle),
            Reader::Nothing => None,
        };
        if let Some(n) = needle
            && is_env_shaped(n)
        {
            failures.push(format!(
                "{}: the needle {n:?} is env-read-shaped. It is a string literal in \
                 `crates/vike-config/src/consumed.rs`, so `crates/vike-ops/tests/\
                 settings_registry.rs`'s literal sweep will read it as vike-config ITSELF reading \
                 the variable and fail three ways. Name the reading function's SIGNATURE (`pub fn \
                 foo() -> bool`) instead — it is also the stabler anchor.",
                row.key
            ));
        }
        match reader {
            Reader::Live { file, needle, caller, call } => {
                if file.starts_with(SELF_CRATE) {
                    failures.push(format!(
                        "{}: Reader::Live names `{file}`, inside the settings crate. This crate \
                         resolving the variable is not a reader — the same rule \
                         `a_claimed_consumer_is_outside_the_settings_crate` applies to \
                         Consumer::At.",
                        row.key
                    ));
                }
                if !contains_code(&root, file, needle) {
                    failures.push(format!(
                        "{}: Reader::Live claims `{file}` reads the variable at `{needle}`, and it \
                         does not (missing file, or the read moved). Either re-point the row or \
                         the variable has stopped being live and the row is now Uncalled/Nothing.",
                        row.key
                    ));
                }
                if caller == file {
                    failures.push(format!(
                        "{}: Reader::Live's `caller` is the file that DEFINES the read. A read \
                         called only from its own file is what every one of the six false rows \
                         looked like — name a caller outside it.",
                        row.key
                    ));
                } else if !contains_code(&root, caller, call) {
                    failures.push(format!(
                        "{}: Reader::Live claims `{caller}` reaches the read via `{call}`, and it \
                         does not. If that call site is gone, the variable may no longer work and \
                         the row must say so — `config show` tells operators to export it.",
                        row.key
                    ));
                }
            }
            Reader::Uncalled { file, needle, entry } => {
                if !contains_code(&root, file, needle) {
                    failures.push(format!(
                        "{}: Reader::Uncalled claims `{file}` holds the read `{needle}`, and it \
                         does not. A row may not describe a reader that is not there.",
                        row.key
                    ));
                }
                if entry.is_empty() {
                    failures.push(format!(
                        "{}: Reader::Uncalled names no entry point, so it asserts nothing.",
                        row.key
                    ));
                }
                for e in entry {
                    if !e.contains("::") {
                        failures.push(format!(
                            "{}: entry `{e}` is not `Type::method`-qualified. A bare name matches \
                             its own `fn {e}(` definition, so the no-caller search would pass \
                             forever and check nothing.",
                            row.key
                        ));
                        continue;
                    }
                    for path in &sources {
                        let rel = rel_path(&root, path);
                        if is_test_path(&rel) {
                            continue;
                        }
                        let Ok(source) = std::fs::read_to_string(path) else { continue };
                        if let Some(n) = call_line(&source, e) {
                            failures.push(format!(
                                "{}: Reader::Uncalled says nothing calls `{e}`, but `{rel}:{n}` \
                                 does. The feature has been MOUNTED — promote this row (the \
                                 variable is live now, and may be the file key too) rather than \
                                 leaving it claiming both spellings are dead.",
                                row.key
                            ));
                        }
                    }
                }
                // ⚠ …AND THE READ ITSELF, which an `entry` list structurally cannot name.
                //
                // Every `entry` must be `Type::method`-qualified, because a bare name matches its
                // own `fn name(` definition — so a read that IS a bare `pub fn` can never appear
                // there. Four of these reads are exactly that, and their crates EXPORT them:
                // `vike_polymarket::{heartbeat_enabled, auto_redeem_enabled, kill_switch_tripped,
                // pm_resolve_enabled}` are all in that crate's public API. A composition root can
                // therefore consult one DIRECTLY — `if vike_polymarket::heartbeat_enabled(&vars)`,
                // or a daemon checking the redeem halt switch without ever owning the poller it
                // was written for — at which point the variable is LIVE while every `entry` is
                // still uncalled and the row goes on telling an operator that neither spelling
                // does anything.
                //
                // So the read's own name is searched too, everywhere EXCEPT its defining file
                // (where its definition would satisfy the search and prove nothing).
                // Deliberately only for a `pub fn` needle: a PRIVATE read has no caller outside
                // its file by Rust's own rules, and its bare name collides freely — `fn
                // env_snapshot()` is a private method spelled identically in
                // `crates/vike-data/src/properties_rec.rs`, and `journal_env_snapshot(` in
                // `crates/vike-core/src/run_profile.rs` contains it as a substring. Hence both the
                // `pub fn` restriction and the word-boundary check in [`free_fn_call_line`].
                if let Some(name) = public_free_fn_name(needle) {
                    for path in &sources {
                        let rel = rel_path(&root, path);
                        // ⚠ `SELF_CRATE` too, and for a reason peculiar to THIS check: the needle
                        // is a string LITERAL in `crates/vike-config/src/consumed.rs`, and it
                        // spells the signature — `pub fn heartbeat_enabled(` — so the row's own
                        // text matches its own search. Measured: without this line all five rows
                        // fail naming `consumed.rs` itself. Direction 4 skips this crate for the
                        // same family of reason.
                        if is_test_path(&rel) || rel == file || rel.starts_with(SELF_CRATE) {
                            continue;
                        }
                        let Ok(source) = std::fs::read_to_string(path) else { continue };
                        if let Some(n) = free_fn_call_line(&source, name) {
                            failures.push(format!(
                                "{}: Reader::Uncalled says the read `{name}` is reached only from \
                                 {entry:?}, but `{rel}:{n}` calls it directly. The read is a \
                                 PUBLIC free function, so a root can consult the variable without \
                                 touching any of those entry points — which makes the variable \
                                 LIVE while every `entry` stays uncalled. Promote this row (or, if \
                                 that call site is itself unreachable, name its door in `entry` \
                                 and say why).",
                                row.key
                            ));
                        }
                    }
                }
            }
            // Nothing to open: there is no reader and no symbol. See `Reader::Nothing`'s doc for
            // what covers it instead — this arm is deliberately empty rather than absent, so the
            // gap is a written decision and not an oversight in a `match`.
            Reader::Nothing => {}
        }
    }

    assert!(failures.is_empty(), "\n\n{}\n", failures.join("\n\n"));
}

/// The no-caller search's ALGORITHM, proved to reject and to accept — a reachability check nobody
/// has watched reject anything is the shape `verify the tool before believing its answer` is for.
///
/// ⚠ **These are three-line synthetic strings and that is a declared limit, not an oversight.**
/// This test passed green through the entire period in which the real check was disabled on
/// `crates/vike-tradehub/src/tradehub_cli.rs` and 822 other files, because the cut that disabled
/// it is triggered by text no synthetic string contains. A kill proof over a fixture proves the
/// algorithm and nothing about the tree —
/// [`the_scanner_reads_past_a_cfg_test_attribute_in_a_real_file`] is the half that proves the
/// tree, and neither substitutes for the other.
#[test]
fn the_no_caller_search_can_actually_fail() {
    let call = "Poller::spawn";
    assert_eq!(
        call_line("fn main() {\n    Poller::spawn(cfg);\n}\n", call),
        Some(2),
        "a real call site must be found, or this gate rubber-stamps every Uncalled row"
    );
    // The three shapes that are NOT calls, each of which appears in the real tree today.
    assert_eq!(call_line("//! [`Poller::spawn`] returns None unless …\n", call), None, "doc link");
    assert_eq!(call_line("pub use settlement::{Poller, Deps};\n", call), None, "re-export");
    assert_eq!(call_line("    pub fn spawn(x: u8) {}\n", call), None, "its own definition");
    // …and the test module is below the cut, which is what makes "nothing but its own tests
    // constructs it" a statement the gate can hold.
    assert_eq!(
        call_line("fn live() {}\n#[cfg(test)]\nmod t {\n  fn x() { Poller::spawn(1); }\n}\n", call),
        None,
        "a call inside the file's own test module is not a production caller"
    );
    // ⚠ THE REGRESSION, in the smallest form it has: a `#[cfg(test)]` on a NON-module item cuts
    // nothing. This is the shape that blinded the gate over 231,897 lines of `src/`.
    assert_eq!(
        call_line("#[cfg(test)]\nuse crate::feeds::CexBars;\nfn m() { Poller::spawn(1); }\n", call),
        Some(3),
        "a `#[cfg(test)] use` is an attribute on an ITEM, not a module boundary — everything below \
         one is still production code"
    );
    // …the same for a `#[cfg(test)] fn`, and for the `all(test, …)` spelling this tree also uses.
    assert_eq!(
        call_line("#[cfg(test)]\nfn helper() {}\nfn m() { Poller::spawn(1); }\n", call),
        Some(3),
        "a `#[cfg(test)] fn` is not a module boundary either"
    );
    assert_eq!(
        call_line(
            "#[cfg(all(test, feature = \"x\"))]\nmod t {\n  fn x() { Poller::spawn(1); }\n}\n",
            call
        ),
        None,
        "`all(test, …)` can only hold in a test build, so the module it guards is test code"
    );
    // …but `any(test, …)` is NOT test-only: a `test-support` build ships that module.
    assert_eq!(
        call_line(
            "#[cfg(any(test, feature = \"test-support\"))]\nmod s {\n  fn x() { Poller::spawn(1); \
             }\n}\n",
            call
        ),
        Some(3),
        "`any(test, feature = …)` also compiles in a feature build, which is shipping code"
    );
    // …and an OUT-OF-LINE test module declaration steps over one line rather than ending the scan.
    assert_eq!(
        call_line("#[cfg(test)]\nmod t;\nfn m() { Poller::spawn(1); }\n", call),
        Some(3),
        "`#[cfg(test)] mod t;` has its body in a sibling FILE — nothing below it is test code"
    );
    // …and the scan RESUMES after an inline test module rather than stopping at it, so a test
    // module placed mid-file no longer hides the rest.
    assert_eq!(
        call_line("#[cfg(test)]\nmod t {\n  fn a() {}\n}\nfn m() { Poller::spawn(1); }\n", call),
        Some(5),
        "production code BELOW a test module is still production code"
    );
}

/// **The A/B proof, over a REAL file.**
///
/// [`the_no_caller_search_can_actually_fail`] exercises the algorithm on three-line synthetic
/// strings, and that is exactly how the hole it was written to close survived it: the scanner's
/// cut is triggered by text that appears 300 lines into a 7,684-line file, and no synthetic string
/// ever contains one. The proof that matters is therefore performed here, against
/// `crates/vike-tradehub/src/tradehub_cli.rs` itself — the daemon's composition root, the file a
/// mount would be wired in, and the one whose `#[cfg(test)] use` blinded the gate.
///
/// Three assertions, in the order they must hold:
///
/// 1. the TRAP SHAPE is really in that file — a test-only `cfg` attribute whose item is not a
///    module, with the file's own test module far below it. If somebody deletes that `use`, this
///    fails loudly rather than passing for a reason that has evaporated;
/// 2. a call planted in PRODUCTION code below the trap is FOUND (it was not, before this change);
/// 3. the same call planted INSIDE the file's own `#[cfg(test)] mod tests` is NOT.
#[test]
fn the_scanner_reads_past_a_cfg_test_attribute_in_a_real_file() {
    let root = workspace_root();
    let rel = "crates/vike-tradehub/src/tradehub_cli.rs";
    let source = std::fs::read_to_string(
        root.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR)),
    )
    .expect("the daemon's composition root must be readable — it is what this test is about");
    let lines: Vec<&str> = source.lines().collect();

    // 1. the trap: a test-only cfg attribute guarding something that is NOT a module.
    let trap = (0..lines.len())
        .find(|&i| {
            strip_test_only_cfg(lines[i].trim_start()).is_some()
                && test_item_at(&lines, i).is_none()
        })
        .expect(
            "`crates/vike-tradehub/src/tradehub_cli.rs` no longer carries a `#[cfg(test)]` on a \
             non-module item. That is the shape this test exists to prove the scanner survives — \
             point it at another file that has one rather than deleting the test.",
        );
    let module = (0..lines.len())
        .find_map(|i| match test_item_at(&lines, i) {
            Some(TestItem::Inline { at }) => Some((i, at)),
            _ => None,
        })
        .expect("that file's own inline `#[cfg(test)] mod tests` must still be there");
    let (module_attr, module_at) = module;
    assert!(trap < module_attr, "the trap must sit ABOVE the file's test module to blind anything");
    assert!(
        module_attr - trap > 500,
        "the blind span is down to {} lines, so this file no longer demonstrates the failure — \
         find one that does rather than lowering the number",
        module_attr - trap
    );

    // 2. production code below the trap is SEEN.
    let planted = plant(&lines, module_attr, "    let _ = AutoRedeemPoller::spawn(deps);");
    assert_eq!(
        call_line(&planted, "AutoRedeemPoller::spawn"),
        Some(module_attr + 1),
        "a call planted {} lines below the `#[cfg(test)]` trap was not found. That is the exact \
         state this gate shipped in: `flags.poly_auto_redeem` and `flags.poly_redeem_halt` both \
         claim nothing calls `AutoRedeemPoller::spawn`, and the daemon's whole mount was invisible \
         to the search backing that claim.",
        module_attr - trap
    );

    // 3. …and the file's own test module still is not.
    let in_tests = plant(&lines, module_at + 1, "    let _ = AutoRedeemPoller::spawn(deps);");
    assert_eq!(
        call_line(&in_tests, "AutoRedeemPoller::spawn"),
        None,
        "a call inside the file's own `#[cfg(test)] mod tests` counted as a production caller — \
         the cut moved too far the other way"
    );
}

/// **THE direction's half of the same rule — and it was the LAST site still holding its own.**
///
/// [`every_claimed_consumer_really_reads_it`] used a raw `source.contains(needle)` while the module
/// doc above already claimed [`production_lines`] was *"the one notion of production code every
/// direction now shares"*. It was not, and the gap was the wrong way round: the hole had been
/// closed on the rows that admit NOTHING reads a key, and left open on the rows that CERTIFY one
/// does — which is the stronger claim and the one an operator's `config show` repeats.
///
/// Measured on `config.tradehub_addr`, whose row names a single read in the daemon. With the raw
/// `contains`, BOTH of these left the gate green while the setting became genuinely unread:
/// commenting the read out (the needle still matches, inside a comment) and moving the identical
/// text into that file's own `#[cfg(test)] mod tests`. In each case `vike-cli config show` goes on
/// printing the file as the key's ORIGIN — `Policy::max_total_exposure`'s defect exactly.
///
/// This asserts the property directly rather than by mutation, because the mutation form would
/// have to edit a 7,600-line daemon file: a needle that exists ONLY in a comment, and one that
/// exists ONLY inside a test module, must both read as absent; one in production code must not.
#[test]
fn a_claimed_consumer_inside_a_comment_or_a_test_module_does_not_count() {
    let source = "\
fn live() {
    let a = settings.config.tradehub_addr.as_deref();
}
// settings.config.commented_out.as_deref()
#[cfg(test)]
mod tests {
    fn t() {
        let b = settings.config.only_in_tests.as_deref();
    }
}
";
    assert!(
        contains_code_in(source, "settings.config.tradehub_addr.as_deref()"),
        "a read in production code must count — otherwise THE direction fails every honest row"
    );
    assert!(
        !contains_code_in(source, "settings.config.commented_out.as_deref()"),
        "a needle that exists only inside a COMMENT must not satisfy a Consumer::At row: the \
         setting is unread in every shipped build while the row certifies it consumed"
    );
    assert!(
        !contains_code_in(source, "settings.config.only_in_tests.as_deref()"),
        "a needle that exists only inside `#[cfg(test)] mod tests` must not satisfy a Consumer::At \
         row: the read is compiled out of every shipped build, so the operator configures a key \
         that nothing they can run will read"
    );
}

/// **The [`Reader::Live`] half of the same rule, mutation-proved on the row that uses it.**
///
/// `preferences.sweep_threads` is the table's one `Reader::Live` row: it tells an operator that
/// `VIKE_SWEEP_THREADS` still works and that only the file key is inert, and its evidence is a
/// caller of `install_bounded` in `crates/vike-backtest/src/harness/optimize.rs`. Before the two
/// scans were unified, that evidence could have been a call inside that file's OWN test module —
/// which would make the advice `vike-cli config show` prints to an operator true of `cargo test`
/// and of nothing they can run.
///
/// The mutation edits PRODUCTION code, not the harness: every production `install_bounded(`
/// renamed away, one planted back inside `#[cfg(test)] mod tests`. The row's claim must then be
/// FALSE.
#[test]
fn a_reader_live_caller_inside_a_test_module_does_not_count() {
    let root = workspace_root();
    let rel = "crates/vike-backtest/src/harness/optimize.rs";
    let source =
        std::fs::read_to_string(root.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR)))
            .expect("the caller `preferences.sweep_threads` names must be readable");
    assert!(
        contains_code_in(&source, "install_bounded("),
        "the `Reader::Live` row's own caller is gone from {rel} — that is a real finding, not a \
         test fixture problem"
    );

    let renamed = source.replace("install_bounded(", "install_bounded_RENAMED_BY_THIS_TEST(");
    let lines: Vec<&str> = renamed.lines().collect();
    let module_at = (0..lines.len())
        .find_map(|i| match test_item_at(&lines, i) {
            Some(TestItem::Inline { at }) => Some(at),
            _ => None,
        })
        .expect("that file's own inline `#[cfg(test)] mod tests` must still be there");
    let mutated = plant(&lines, module_at + 1, "    let _ = install_bounded(|| ());");

    assert!(
        !contains_code_in(&mutated, "install_bounded("),
        "a call inside `{rel}`'s own `#[cfg(test)] mod tests` satisfied `Reader::Live`. That makes \
         `Live` and `Uncalled` stop being opposites: the same caller would count as evidence the \
         variable works while not counting as a caller that would redden the `Uncalled` row \
         opposite it, and `config show` would go on telling an operator to export a variable \
         nothing outside `cargo test` reaches."
    );
}

/// Insert `text` so it occupies 0-indexed line `at` of `lines`, and give back the whole source.
fn plant(lines: &[&str], at: usize, text: &str) -> String {
    let mut out: Vec<&str> = lines.to_vec();
    out.insert(at.min(out.len()), text);
    out.join("\n")
}

/// The env-shaped-needle refusal has to be able to FAIL, or it is decoration on a trap that has
/// already been sprung once.
///
/// ⚠ **The bad spellings are ASSEMBLED, never written out, and that is the test demonstrating its
/// own rule.** A literal read-shaped string in THIS file is the exact thing being refused, and
/// `crates/vike-ops/tests/settings_registry.rs` scans test sources too — so the first version of
/// this test, which spelled the two offenders as plain literals, reddened
/// `every_read_variable_is_declared` from inside a test whose whole subject is not doing that.
/// Measured, twice, which is why the note is here and not merely in `Reader`'s doc.
#[test]
fn the_env_shaped_needle_refusal_can_actually_fail() {
    let call = format!("std::{}::var", "env");
    for bad in [format!("{call}(RECORD_CHAINS_ENV)"), format!("{call}_os(\"SOME_HALT_FLAG\")")] {
        assert!(is_env_shaped(&bad), "{bad:?} must be caught — this shape reddened three gates");
    }
    // …and the signature needles every row carries now must NOT be caught, or the rule would have
    // no legal spelling at all. `fn env_snapshot()` is the interesting one: it CONTAINS "env".
    for good in ["pub fn sweep_threads() -> usize", "fn env_snapshot()"] {
        assert!(!is_env_shaped(good), "{good:?} was rejected — no legal needle would remain");
    }
}

/// `true` when `needle` would read as an environment access to `settings_registry.rs`'s literal
/// sweep. One check covers `var_os` too, since it contains the same prefix.
fn is_env_shaped(needle: &str) -> bool {
    needle.contains(&format!("{}::var", "env"))
}

// ---------------------------------------------------------------------------------------------
// the scanner — ONE notion of production code, shared by every direction above
// ---------------------------------------------------------------------------------------------

/// Which test ITEM a `#[cfg(test)]`-family attribute guards, and where that item sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestItem {
    /// `#[cfg(test)] mod name;` — the body is a sibling FILE and is not in this text at all, so
    /// only the declaration is stepped over.
    OutOfLine {
        /// 0-indexed line of the `mod name;` declaration.
        at: usize,
    },
    /// `#[cfg(test)] mod name { … }` — a module boundary: its whole body is test code.
    Inline {
        /// 0-indexed line the `mod name` opens on.
        at: usize,
    },
}

/// The PRODUCTION lines of a Rust source — what a NON-test build compiles, minus comment and
/// attribute lines — each paired with its 1-indexed number in the ORIGINAL file.
///
/// Every direction in this file goes through here, and that is the point rather than tidiness:
/// [`Reader::Live`] and [`Reader::Uncalled`] are supposed to be each other's opposites, so a
/// caller that counts for one and not the other puts a row in the gap between them.
///
/// ⚠ **This replaced a one-line rule that disabled the gate over most of the tree, and the shape
/// of that failure is why the rule is now spelled out.** Each scan used to stop at the FIRST line
/// whose trimmed text starts with `#[cfg(test)]`, on the stated assumption that a test module sits
/// at the bottom of its file. But `#[cfg(test)]` is an attribute on an ITEM, not a module marker,
/// and `#[cfg(test)] use`, `#[cfg(test)] fn` and `#[cfg(test)] const` all wear the identical
/// spelling while cutting nothing. `crates/vike-tradehub/src/tradehub_cli.rs` carries
/// `#[cfg(test)] use crate::feeds::CexBars;` about 300 lines into a 7,684-line file, so the whole
/// of the daemon's live mount — every `FoldTier` row, `make_engine`, `spawn_recon` — was invisible
/// to the no-caller search. A/B-measured against the real gate binary: one planted
/// `AutoRedeemPoller::spawn(` call read GREEN 1,700 lines below that `use` and RED above it.
/// Tree-wide, 823 `crates/**/src/*.rs` files carry a `#[cfg(test)]` and 231,897 lines sat after
/// the first one — MEASURED, and re-derivable: for each tracked `src/` file outside the vendored
/// `ibapi` copy, the first line matching `^\s*#\[cfg(test)\]` subtracted from that file's line
/// count, summed. [`the_scanner_reads_past_a_cfg_test_attribute_in_a_real_file`] is the A/B proof
/// kept as a test, over the real file rather than a synthetic string.
///
/// So the cut is at a test MODULE and at nothing else:
///
/// * `#[cfg(test)] mod name { … }` — skipped, body and all, and the scan RESUMES after it. The end
///   of the body is the first line equal to the module's own indentation plus `}`, which is exact
///   because `cargo fmt --check` is this repo's first CI gate; a module that never closes that way
///   runs to EOF, which is the old behaviour and therefore no worse.
/// * `#[cfg(test)] mod name;` — one declaration line stepped over
///   (`crates/vike-ops/tests/docs_constants_gate.rs`'s `code_only` is where this shape was already
///   solved, and this is deliberately the same answer rather than a second one).
/// * anything else the attribute guards — `use`, `fn`, `const`, `impl` — is NOT a boundary and the
///   scan continues straight through it.
///
/// ⚠ **Two residuals, declared.** A `#[cfg(test)] fn` body IS scanned, so a settings key written
/// inside one reads as production — that fails LOUDLY (a red gate naming the line) rather than
/// silently, which is the direction this file has to fail in. And the cfg predicate is read
/// literally: `#[cfg(test)]` and `#[cfg(all(test, …))]` are test-only, while
/// `#[cfg(any(test, feature = "test-support"))]` is NOT, because a `test-support` build ships it.
fn production_lines(source: &str) -> Vec<(usize, &str)> {
    let lines: Vec<&str> = source.lines().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        match test_item_at(&lines, i) {
            Some(TestItem::OutOfLine { at }) => {
                i = at + 1;
                continue;
            }
            Some(TestItem::Inline { at }) => {
                i = end_of_block(&lines, at);
                continue;
            }
            None => {}
        }
        let trimmed = lines[i].trim_start();
        // A comment line, a block-comment continuation, or an attribute: none of them is the
        // program reading a setting or calling an entry point.
        if !(trimmed.starts_with("//") || trimmed.starts_with('*') || trimmed.starts_with('#')) {
            out.push((i + 1, trimmed));
        }
        i += 1;
    }
    out
}

/// The test ITEM a test-only `cfg` attribute on line `i` guards, or `None` when line `i` is not
/// such an attribute or the item it guards is not a module.
///
/// The item may sit on the attribute's own line (`#[cfg(test)] mod tests;`) or below it, past
/// further attributes, comments and blank lines — `tradehub_cli.rs` spells a `#[path = "…"]`
/// between the two, which is why the look-ahead skips attributes rather than demanding that the
/// very next line be the item.
fn test_item_at(lines: &[&str], i: usize) -> Option<TestItem> {
    let rest = strip_test_only_cfg(lines[i].trim_start())?;
    let (at, item) = if rest.trim().is_empty() {
        let mut j = i + 1;
        loop {
            let t = lines.get(j)?.trim();
            if t.is_empty() || t.starts_with("//") || t.starts_with('*') || t.starts_with('#') {
                j += 1;
                continue;
            }
            break (j, t);
        }
    } else {
        (i, rest.trim())
    };
    // `pub mod` is not how a test module is written, but stripping visibility costs two lines and
    // removes a way for this to answer `None` about something that IS one.
    let item = item.strip_prefix("pub ").unwrap_or(item).trim_start();
    let item = match item.find(')') {
        Some(p) if item.starts_with("pub(") => item[p + 1..].trim_start(),
        _ => item,
    };
    if !item.starts_with("mod ") {
        return None;
    }
    if item.ends_with(';') {
        Some(TestItem::OutOfLine { at })
    } else {
        Some(TestItem::Inline { at })
    }
}

/// The text after a TEST-ONLY `cfg` attribute at the start of `line`, or `None` when `line` does
/// not open with one.
///
/// The predicate is matched with paren counting rather than a `find(")]")`, because
/// `#[cfg(all(test, not(fcsdk)))]` is a real spelling in this tree and its first `)]` is not the
/// attribute's.
fn strip_test_only_cfg(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("#[cfg(")?;
    let mut depth = 1usize;
    let mut end = None;
    for (idx, ch) in rest.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(idx);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end?;
    let after = rest.get(end + 1..)?.strip_prefix(']')?;
    is_test_only(&rest[..end]).then_some(after)
}

/// `true` when a `cfg` predicate can hold ONLY in a test build.
///
/// `all(test, …)` qualifies — every conjunct must hold and `test` is one of them. `any(test, …)`
/// deliberately does not: `#[cfg(any(test, feature = "test-support"))]` guards modules a
/// `test-support` build SHIPS, and treating those as test code would narrow the scan for no
/// reason. Erring towards scanning more is the safe direction here; erring the other way is the
/// defect this whole helper exists to repair.
fn is_test_only(pred: &str) -> bool {
    let p = pred.trim();
    if p == "test" {
        return true;
    }
    p.strip_prefix("all(")
        .and_then(|s| s.strip_suffix(')'))
        .is_some_and(|inner| inner.split(',').any(|t| t.trim() == "test"))
}

/// The 0-indexed line AFTER the block opening on line `at` — the first line equal to that line's
/// own indentation followed by `}`, or EOF when there is none.
fn end_of_block(lines: &[&str], at: usize) -> usize {
    let open = lines[at];
    // A one-line module (`mod tests {}`) closes on its own line.
    if open.contains('{') && open.matches('{').count() == open.matches('}').count() {
        return at + 1;
    }
    let indent: String = open.chars().take_while(|c| c.is_whitespace()).collect();
    let closing = format!("{indent}}}");
    for (j, line) in lines.iter().enumerate().skip(at + 1) {
        if *line == closing {
            return j + 1;
        }
    }
    lines.len()
}

/// A call site for `entry` in `source`, 1-indexed, or `None`.
///
/// Requires the `(` so a doc reference (``[`AutoRedeemPoller::spawn`]``), a re-export or a prose
/// mention is not mistaken for a call; everything else about what counts as code is
/// [`production_lines`]'s job, shared with [`contains_code_in`] so the two [`Reader`] variants
/// cannot disagree about what a caller is.
fn call_line(source: &str, entry: &str) -> Option<usize> {
    let needle = format!("{entry}(");
    production_lines(source).into_iter().find(|(_, l)| l.contains(&needle)).map(|(n, _)| n)
}

/// The bare name of a PUBLIC FREE function from a [`Reader`] `needle` (which is always a
/// signature), or `None` when the needle is not one.
///
/// `pub fn heartbeat_enabled(vars: &HashMap<String, String>) -> bool` -> `heartbeat_enabled`.
/// A needle that is not `pub` yields `None` deliberately — see the call site for why a private
/// read is neither checkable nor in need of checking.
fn public_free_fn_name(needle: &str) -> Option<&str> {
    let rest = needle.strip_prefix("pub fn ")?;
    let name = rest.split('(').next()?.trim();
    (!name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_')).then_some(name)
}

/// A call to the free function `name` in `source`, 1-indexed, or `None`.
///
/// Word-boundary-checked on the LEFT: `journal_env_snapshot(` must not read as a call to
/// `env_snapshot`, and `poly_heartbeat_enabled(` must not read as one to `heartbeat_enabled`. A
/// `::` before the name is fine and is the normal spelling from another crate
/// (`vike_polymarket::heartbeat_enabled(`).
fn free_fn_call_line(source: &str, name: &str) -> Option<usize> {
    let needle = format!("{name}(");
    for (n, line) in production_lines(source) {
        let mut from = 0usize;
        while let Some(off) = line[from..].find(&needle) {
            let at = from + off;
            let prev = line[..at].chars().next_back();
            if !prev.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                return Some(n);
            }
            from = at + 1;
        }
    }
    None
}

/// `true` when `source` holds `needle` in PRODUCTION code — the same rule [`call_line`] applies,
/// and that sharing is the fix for a second hole: this used to skip comment lines only, so a
/// [`Reader::Live`] row could be satisfied by a caller existing solely inside the file's own
/// `#[cfg(test)] mod tests`, making "the environment variable still works" true of test builds
/// alone. Mutation-proved both ways by
/// [`a_reader_live_caller_inside_a_test_module_does_not_count`].
fn contains_code_in(source: &str, needle: &str) -> bool {
    production_lines(source).iter().any(|(_, l)| l.contains(needle))
}

/// [`contains_code_in`] over a repo-relative path; a missing file is `false`.
fn contains_code(root: &Path, rel: &str, needle: &str) -> bool {
    let path = root.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
    std::fs::read_to_string(path).is_ok_and(|s| contains_code_in(&s, needle))
}

/// Repo-relative, forward-slashed — the spelling every table in this file is keyed on.
fn rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/")
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
                // ⚠ `contains_code_in`, for the same reason THE direction now uses it. This closure
                // kept the pre-repair rule verbatim — skip `//`, nothing else — which is the LAST
                // place in this file that disagreed about what production code is, and it
                // disagreed in the direction that keeps an exemption ALIVE: a collision surviving
                // only inside a `#[cfg(test)] mod` read as "still collides", so the exemption held,
                // and with it direction 4's silence over that whole directory. That is precisely
                // the rot this test's own message calls fatal.
                //
                // ⚠ INERT TODAY and stated as such rather than claimed proven: `DIFFERENT_SYMBOL`
                // is empty, so there is no row to exercise and no mutation that reaches this line
                // without editing the harness — which this workspace does not count as a proof.
                && std::fs::read_to_string(path).is_ok_and(|s| contains_code_in(&s, key))
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
