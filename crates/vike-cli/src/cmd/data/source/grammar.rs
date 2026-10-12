//! `data source`'s grammar: the parser, and every refusal it can answer with.

use crate::cmd::args::{Flags, help_requested, no_value};

use super::roster::resolve;
use super::{Args, Format, SHOW_NEEDS_A_NAME, VERBS, Verb, is_a_hist_flag, parse_format};

/// The refusal `--addr` gets on `ls`, worded once.
///
/// ⚠ It is a REFUSAL rather than a silent ignore, for `super::refuse_foreign_flags`'s reason one
/// level up: every flag that function guards is one an operator typed because a SIBLING verb takes
/// it, so "unknown option" would be a lie. `data hist ls --addr` is a real line; this one is not,
/// and the message names the reach `ls` does not have and the verbs that do.
///
/// ⚠ **It covered BOTH verbs until the history-channels read landed** — `show` reached no server
/// either. The owner's Q3 answer of 2026-10-02
/// (`docs/superpowers/specs/2026-10-02-history-channels-step2-design.md` §7) gave `show VENUE` an
/// opt-in `--addr`, so the refusal narrowed to the verb that still reaches nothing.
const ADDR_REFUSAL: &str = "--addr does not apply to `data source ls` — it renders the axis this \
                            binary was BUILT with and reaches no server, so there is nothing for \
                            an address to select. `data source show VENUE --addr A` asks one \
                            datahub for a venue's history channels; the other verbs that ask one \
                            are under `data hist`.";

/// What `show NAME --addr A` is told when NAME is not a ROSTER venue: the address selects a
/// datahub to ask about a venue's history channels, and a source name or an unclassified venue
/// token has none to ask about.
fn addr_needs_a_roster_venue(name: &str) -> String {
    format!(
        "--addr asks a datahub for a ROSTER VENUE's history channels, and `{name}` is not one — \
         a source name or a venue this build has never classified has no channels to ask about. \
         Drop --addr to describe `{name}` locally."
    )
}

/// The refusal any OTHER flag gets — and it deliberately does not say "unknown".
///
/// ⚠ **[`ADDR_REFUSAL`] applied its own stated rule to exactly one flag, and this is the fix.**
/// `--store`, `--engine`, `--days`, `--from`/`--to`, `--venue`, `--kind` and `--source` are all
/// real, documented `data hist` flags, and every one of them fell through to
/// `unknown option '--store'` — which is, by the standard that const cites, a lie, and one that
/// sends an operator to check the spelling of a flag they typed correctly for a sibling.
///
/// ⚠ **It answered EVERY `--` token, so a TYPO was told it was spelt correctly.** The first version
/// of this said, unconditionally, *"If you typed `{flag}` for `data hist`, it is spelt correctly and
/// belongs there"* — so `data source ls --stroe /srv/vike/data` replied that `--stroe` is a correct
/// `data hist` flag. It is not a flag anywhere. That is the same defect the const above was written
/// to fix, wearing the other face: the old code lied by calling a real flag unknown, and the fix
/// lied by calling an unknown flag real.
///
/// So the claim is now made only for a flag `data hist` ACTUALLY takes, and the set is DERIVED
/// rather than typed — see [`is_a_hist_flag`]. Anything else gets the plain refusal, with this
/// group's own options printed under it by `crate::cmd::args::exit_for_parse_error`.
fn foreign_flag_refusal(flag: &str) -> String {
    if is_a_hist_flag(flag) {
        return format!(
            "`{flag}` is not a `data source` flag — the options this group takes are listed \
             below. ⚠ That is not a misspelling: `{flag}` is a real `data hist` flag, and it \
             means nothing to a group whose two verbs open no socket and read no store. Typed \
             for `data hist`, it belongs there."
        );
    }
    format!(
        "`{flag}` is not a `data source` flag, and it is not a `data hist` flag either — the \
         options this group takes are listed below."
    )
}

/// Parse a `data source …` line. `argv` is everything AFTER the group word.
pub(super) fn parse(argv: &[String]) -> Result<Args, String> {
    let Some(first) = argv.first() else {
        // DERIVED from [`VERBS`], for the reason that const carries.
        return Err(format!("`data source` needs a verb ({})", VERBS.join(" | ")));
    };
    let verb = match first.as_str() {
        "ls" => Verb::Ls,
        "show" => Verb::Show,
        "-h" | "--help" | "help" => return help_requested(),
        other => {
            return Err(format!("unknown `data source` verb '{other}' ({})", VERBS.join(" | ")));
        }
    };

    let mut name: Option<String> = None;
    let mut json_flag = false;
    let mut format: Option<Format> = None;
    let mut addr: Option<String> = None;

    let mut flags = Flags::new(argv[1..].iter().cloned());
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--addr" if verb == Verb::Ls => return Err(ADDR_REFUSAL.to_string()),
            "--addr" => {
                let value = flags.value(&flag, inline)?;
                if value.trim().is_empty() {
                    return Err("--addr was given an EMPTY value, so it names no datahub. Pass \
                                HOST:PORT, or drop --addr to describe the venue locally"
                        .to_string());
                }
                addr = Some(value);
            }
            "--json" => {
                no_value(&flag, inline)?;
                json_flag = true;
            }
            "--format" => format = Some(parse_format(&flags.value(&flag, inline)?)?),
            "-h" | "--help" => return help_requested(),
            // The `--` rule `crate::cmd::args`'s `is_flag_token` spells for this whole crate.
            other if other.starts_with("--") => return Err(foreign_flag_refusal(other)),
            positional => {
                // ⚠ `Flags::next_flag` splits EVERY token on its first `=`, which is right for a
                // FLAG and wrong for a positional: `show a=b` arrives as `("a", Some("b"))`, and
                // binding `a` discarded `b` in SILENCE — two answers for one value, from the group
                // whose stated job is that `show X` and `--source X` agree about X. Reassembling
                // is what makes that true for a value carrying an `=`.
                let token = match &inline {
                    Some(rest) => format!("{positional}={rest}"),
                    None => positional.to_string(),
                };
                match (&name, verb) {
                    (_, Verb::Ls) => {
                        return Err(format!(
                            "`data source ls` takes no argument, and got '{token}'. It lists \
                             every source; `data source show {token}` describes one"
                        ));
                    }
                    (None, Verb::Show) => name = Some(token),
                    (Some(already), Verb::Show) => {
                        return Err(format!(
                            "unexpected extra argument '{token}' (the source is already \
                             '{already}'); one source per `show`"
                        ));
                    }
                }
            }
        }
    }

    // ⚠ RESOLVED HERE, not at render time — so a value this group cannot describe is refused on the
    // USAGE rung rather than rendered as a working venue row. See [`resolve`].
    //
    // ⚠ An EMPTY positional is a MISSING name, not a source named "", and it gets
    // [`SHOW_NEEDS_A_NAME`] for that reason: `show ""` and `show` are one mistake, so they owe one
    // sentence. It used to fall through to [`resolve`] and print the AXIS's empty-value refusal,
    // which tells the operator to "omit the flag" — on the one rung that takes no flag, where
    // omitting the argument answers with this const instead. [`resolve`]'s doc carries the rest.
    let row = match (verb, name) {
        (Verb::Show, Some(n)) if !n.is_empty() => Some(resolve(&n)?),
        (Verb::Show, _) => return Err(SHOW_NEEDS_A_NAME.to_string()),
        (Verb::Ls, _) => None,
    };

    // ONE axis, two spellings, and the CONTRADICTION is refused rather than resolved — the rule
    // `crate::cmd::data`'s `parse` already follows.
    //
    // ⚠ The sentence below is a COPY of that function's, and
    // `the_output_axis_refuses_the_same_pair_the_hist_group_refuses` holds the two equal after
    // whitespace normalisation. Sharing it outright would mean editing that function, which two
    // sibling branches are inside; pinning two copies equal from a test is the seam
    // `crates/vike-bridge-core/tests/settings_dir_spellings.rs` already uses for a duplication that
    // could not be removed either.
    let json = match (format, json_flag) {
        (Some(Format::Table), true) => {
            return Err("--json and --format table ask for two different renderings. `--json` IS \
                        `--format json` — pass one"
                .to_string());
        }
        (Some(f), _) => f == Format::Json,
        (None, given) => given,
    };

    // `--addr` asks a datahub about a ROSTER venue's channels; any other row has none to ask about.
    if addr.is_some()
        && let Some(r) = row.as_ref().filter(|r| !r.declared)
    {
        return Err(addr_needs_a_roster_venue(&r.name));
    }

    Ok(Args { verb, row, json, addr })
}
