//! `data source`'s grammar: what happens to `clap`'s read of the line — the `data hist` flags this
//! group answers BY NAME, the verb resolution, and every value rule and per-verb refusal.

use std::sync::OnceLock;

use clap::parser::ValueSource;

use super::super::hist::HistVerb;
use super::roster::resolve;
use super::{Args, Format, SHOW_NEEDS_A_NAME, SourceFlags, SourceVerb, VERBS, Verb, parse_format};

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

/// The flags this group declares itself; every other long flag of `data hist` is registered hidden
/// by [`HistOnlyFlags`].
const OWN_FLAGS: &[&str] = &["addr", "json", "format"];

/// One `data hist` flag this group refuses by name: the word typed after the dashes, and the id it
/// is registered under.
///
/// ⚠ The id is NOT the word. A `clap` id is unique per command, and the hist verbs spell flags this
/// group has fields for (`--name` is a hist filter; the positional of `show` is `name`), so the
/// registered id carries a prefix no field of this group can have.
#[derive(Clone, Copy, Debug)]
struct HistFlag {
    long: &'static str,
    id: &'static str,
}

/// Every long flag of `data hist` that this group does NOT take, as the HIST parser itself declares
/// them.
///
/// ⚠ **DERIVED from the `data hist` verbs' own `clap` declarations, never typed here.** The roster
/// this replaces was read off the hand-written `data` usage page, and a list in this module would
/// be a second roster with nothing holding it in step — the copy-drift disease this module's own
/// doc opens on. Building the hist verbs' command to read their arguments is not a recursion:
/// `HistVerb` never names this group.
fn hist_only_flags() -> &'static [HistFlag] {
    static ROSTER: OnceLock<Vec<HistFlag>> = OnceLock::new();
    ROSTER.get_or_init(|| {
        let hist = <HistVerb as clap::Subcommand>::augment_subcommands(clap::Command::new("hist"));
        let mut flags: Vec<HistFlag> = Vec::new();
        for verb in hist.get_subcommands() {
            for arg in verb.get_arguments() {
                let Some(long) = arg.get_long() else { continue };
                if OWN_FLAGS.contains(&long) || flags.iter().any(|known| known.long == long) {
                    continue;
                }
                // Leaked once per flag, for the life of the process: `clap` takes its argument ids
                // and long names as `&'static str`.
                flags.push(HistFlag {
                    long: Box::leak(long.to_owned().into_boxed_str()),
                    id: Box::leak(format!("hist-{long}").into_boxed_str()),
                });
            }
        }
        flags
    })
}

/// The `data hist` flags, registered HIDDEN on every `data source` verb so that each is refused BY
/// NAME — `--store` on `ls` is not "unknown", it belongs to a sibling group — with the sentence
/// [`foreign_flag_refusal`] gives. `clap` would otherwise answer such a flag with its generic
/// unknown-argument error, which tells an operator the flag does not exist; that is false, and it
/// sends them to check the spelling of a flag they typed correctly for a sibling.
///
/// Registered as optional-valued so the value an operator typed beside it (`--store /srv/vike`) is
/// swallowed with it instead of being read as the source NAME.
#[derive(Debug, Default)]
pub(super) struct HistOnlyFlags {
    /// The flags (without their dashes) that were present.
    given: Vec<&'static str>,
}

impl clap::Args for HistOnlyFlags {
    fn augment_args(mut cmd: clap::Command) -> clap::Command {
        for flag in hist_only_flags() {
            cmd = cmd.arg(clap::Arg::new(flag.id).long(flag.long).hide(true).num_args(0..=1));
        }
        cmd
    }

    fn augment_args_for_update(cmd: clap::Command) -> clap::Command {
        <Self as clap::Args>::augment_args(cmd)
    }
}

impl clap::FromArgMatches for HistOnlyFlags {
    fn from_arg_matches(matches: &clap::ArgMatches) -> Result<Self, clap::Error> {
        let given = hist_only_flags()
            .iter()
            .filter(|flag| {
                matches.value_source(flag.id).is_some_and(|s| s != ValueSource::DefaultValue)
            })
            .map(|flag| flag.long)
            .collect();
        Ok(Self { given })
    }

    fn update_from_arg_matches(&mut self, matches: &clap::ArgMatches) -> Result<(), clap::Error> {
        *self = <Self as clap::FromArgMatches>::from_arg_matches(matches)?;
        Ok(())
    }
}

/// The refusal a `data hist` flag gets on this group — and it deliberately does not say "unknown".
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
/// `data hist` flag. It is not a flag anywhere. So the claim is made only for a flag `data hist`
/// ACTUALLY takes — [`HistOnlyFlags`] registers exactly those, DERIVED from the hist verbs — and a
/// token that is no flag of either group is `clap`'s ordinary unknown-argument refusal, with its
/// suggestion.
fn foreign_flag_refusal(flag: &str) -> String {
    format!(
        "`{flag}` is not a `data source` flag — the options this group takes are listed by \
         `data source --help`. ⚠ That is not a misspelling: `{flag}` is a real `data hist` flag, \
         and it means nothing to a group whose two verbs open no socket and read no store. Typed \
         for `data hist`, it belongs there."
    )
}

/// What `clap` read, resolved into the group's [`Args`]: the verb, then the post-parse rules every
/// verb shares.
pub(super) fn args_of(verb: &SourceVerb) -> Result<Args, String> {
    let (verb, flags) = match verb {
        SourceVerb::Ls(f) => (Verb::Ls, f),
        SourceVerb::Show(f) => (Verb::Show, f),
        SourceVerb::Other(words) => {
            let other = words.first().map(String::as_str).unwrap_or_default();
            // DERIVED from [`VERBS`], for the reason that const carries.
            return Err(format!("unknown `data source` verb '{other}' ({})", VERBS.join(" | ")));
        }
    };
    flags.args_for(verb)
}

impl SourceFlags {
    /// Resolve `clap`'s raw flags for one verb. PURE — no I/O, no socket.
    fn args_for(&self, verb: Verb) -> Result<Args, String> {
        if let Some(flag) = self.hist_only.given.first() {
            return Err(foreign_flag_refusal(&format!("--{flag}")));
        }
        if self.addr.is_some() && verb == Verb::Ls {
            return Err(ADDR_REFUSAL.to_string());
        }
        let format = self.format.as_deref().map(parse_format).transpose()?;
        let json_flag = self.json;

        let name = match (self.name.as_slice(), verb) {
            ([], _) => None,
            ([token, ..], Verb::Ls) => {
                return Err(format!(
                    "`data source ls` takes no argument, and got '{token}'. It lists every source; \
                     `data source show {token}` describes one"
                ));
            }
            ([only], Verb::Show) => Some(only.clone()),
            ([already, token, ..], Verb::Show) => {
                return Err(format!(
                    "unexpected extra argument '{token}' (the source is already '{already}'); one \
                     source per `show`"
                ));
            }
        };

        // ⚠ RESOLVED HERE, not at render time — so a value this group cannot describe is refused on
        // the USAGE rung rather than rendered as a working venue row. See [`resolve`].
        //
        // ⚠ A MISSING name is [`SHOW_NEEDS_A_NAME`]. An EMPTY one never gets here: the command-line
        // layer refuses a blank positional in its own words, before this rung's rules — see that
        // const for what it used to say about the two.
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
        // `crates/vike-bridge-core/tests/settings_dir_spellings.rs` already uses for a duplication
        // that could not be removed either.
        let json = match (format, json_flag) {
            (Some(Format::Table), true) => {
                return Err(
                    "--json and --format table ask for two different renderings. `--json` IS \
                            `--format json` — pass one"
                        .to_string(),
                );
            }
            (Some(f), _) => f == Format::Json,
            (None, given) => given,
        };

        // `--addr` asks a datahub about a ROSTER venue's channels; any other row has none to ask about.
        if self.addr.is_some()
            && let Some(r) = row.as_ref().filter(|r| !r.declared)
        {
            return Err(addr_needs_a_roster_venue(&r.name));
        }

        Ok(Args { verb, row, json, addr: self.addr.clone() })
    }
}
