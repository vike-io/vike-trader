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
//! `crates/vike-ops/tests/docs/kill_switch_gate.rs` carries a row reading "nothing outside tests
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

use vike_config::consumed::keys_requiring_a_row;
use vike_config::{CONSUMPTION, Consumer, Reader};

#[path = "settings_are_consumed/non_vacuity.rs"]
mod non_vacuity;
// The scanner is SHARED with `crates/vike-config/tests/policy_is_consumed.rs`, so the two
// consumption gates hold one notion of production code rather than one each.
#[path = "common/scanner.rs"]
mod scanner;
#[path = "common/workspace.rs"]
mod workspace;

use non_vacuity::is_env_shaped;
use scanner::{
    call_line, contains_code, contains_code_in, free_fn_call_line, is_test_path, production_lines,
    public_free_fn_name, rel_path, rust_sources,
};
use workspace::workspace_root;

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
///   calling `vike_polymarket::kill_switch_tripped(halted, &file)` directly makes the switch live
///   while every `entry` stays uncalled and the row goes on saying neither spelling configures
///   anything.
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
        // `crates/vike-ops/tests/settings_secrets/settings_registry.rs` scans `src/` for env-read-shaped TEXT with
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
                 settings_secrets/settings_registry.rs`'s literal sweep will read it as vike-config ITSELF reading \
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
                // there. Four of these reads were exactly that until decision 0095 turned three of
                // them into parameters, and the survivor is EXPORTED:
                // `vike_polymarket::kill_switch_tripped` is in that crate's public API. A
                // composition root can therefore consult it DIRECTLY — a daemon checking the
                // redeem halt switch without ever owning the poller it was written for — at which
                // point the switch is LIVE while every `entry` is still uncalled and the row goes
                // on telling an operator that neither spelling configures anything.
                //
                // So the read's own name is searched too, everywhere EXCEPT its defining file
                // (where its definition would satisfy the search and prove nothing).
                // Deliberately only for a `pub fn` needle: a PRIVATE read has no caller outside
                // its file by Rust's own rules, and its bare name collides freely — `fn
                // env_snapshot()` is a private method spelled identically in
                // `crates/vike-data/src/rec/properties_rec.rs`, and `journal_env_snapshot(` in
                // `crates/vike-core/src/run_profile/journal_env.rs` contains it as a substring. Hence both the
                // `pub fn` restriction and the word-boundary check in [`free_fn_call_line`].
                if let Some(name) = public_free_fn_name(needle) {
                    for path in &sources {
                        let rel = rel_path(&root, path);
                        // ⚠ `SELF_CRATE` too, and for a reason peculiar to THIS check: the needle
                        // is a string LITERAL in `crates/vike-config/src/consumed.rs`, and it
                        // spells the signature — `pub fn kill_switch_tripped(` — so the row's own
                        // text matches its own search. Measured: without this line all five rows
                        // (as they then were) failed naming `consumed.rs` itself. Direction 4
                        // skips this crate for the same family of reason.
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
