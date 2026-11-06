//! `data catalog`'s grammar: what happens to `clap`'s read of the line — the two help strings that
//! carry a runtime fact, the verb resolution, and every value validator and per-verb refusal.

use vike_datahub_client::catalog::validate_catalog_venue;
use vike_model::AssetClass;

use super::{Args, CatalogFlags, CatalogVerb, Format, InstrumentRef, VERBS, Verb, parse_format};

/// The roster as the unknown-verb refusal renders it.
fn verb_roster() -> String {
    VERBS.iter().map(|v| v.as_str()).collect::<Vec<_>>().join(" | ")
}

/// The asset-class vocabulary as `--class`'s refusal and its help render it.
///
/// DERIVED from `vike_model::AssetClass::SQL_WORDS`, never typed: the taxonomy has already MOVED
/// crates once (`docs/decisions/0061` was accepted while it still lived in `vike-catalog`) and
/// GROWN once (decision 0126 added `Bond`, `Fund`, `Warrant`, `Commodity` and `Combo`), and a
/// hand-copied list in a usage string is exactly the shape this repository has watched rot.
pub(super) fn class_roster() -> String {
    AssetClass::SQL_WORDS.join(" | ")
}

/// `--class`'s one-line help: the vocabulary is [`class_roster`]'s, rendered when the command is
/// built rather than typed into an attribute.
///
/// ⚠ A FUNCTION, because the `--class` vocabulary is `vike_model::AssetClass::SQL_WORDS` (a word
/// list whose taxonomy has already MOVED crates and GROWN) and a copy typed into a `help = "…"` is
/// exactly the shape this repository has watched rot.
pub(super) fn class_help() -> String {
    format!(
        "ls: keep only instruments of this asset class, case-insensitively ({})",
        class_roster()
    )
}

/// `--addr`'s one-line help; the default is [`vike_config::DEFAULT_DATAHUB_ADDR`]'s to answer, never
/// typed here.
pub(super) fn addr_help() -> String {
    format!(
        "every verb: the datahub to ask (default {}). It binds localhost, so reach a remote one \
         over `ssh -L 7878:localhost:7878`. On `venues` an unreachable one is REPORTED rather than \
         fatal",
        vike_config::DEFAULT_DATAHUB_ADDR
    )
}

/// What a word that is none of the four verbs is told: the replacement when it is the retired
/// spelling, the roster otherwise.
///
/// The spelling an operator arrives with, because the sibling group renamed the same verb: `data
/// list` became `data hist ls`, so `data catalog list` is what a reader who learned that rename
/// types next. Naming the replacement costs one arm and saves a support round trip —
/// `crate::cmd::data`'s `RETIRED_SPELLINGS` makes the same trade one group over.
fn refuse_a_non_verb(words: &[String]) -> String {
    match words.first().map(String::as_str).unwrap_or_default() {
        "list" => "`data catalog list` is `data catalog ls`".to_string(),
        other => format!("unknown `data catalog` verb '{other}' ({})", verb_roster()),
    }
}

/// What `clap` read, resolved into the group's [`Args`]: the verb, then the post-parse rules every
/// verb shares.
pub(super) fn args_of(verb: &CatalogVerb, configured_addr: Option<&str>) -> Result<Args, String> {
    let (verb, flags) = match verb {
        CatalogVerb::Ls(f) => (Verb::Ls, f),
        CatalogVerb::Show(f) => (Verb::Show, f),
        CatalogVerb::Refresh(f) => (Verb::Refresh, f),
        CatalogVerb::Venues(f) => (Verb::Venues, f),
        CatalogVerb::Other(words) => return Err(refuse_a_non_verb(words)),
    };
    flags.args_for(verb, configured_addr)
}

impl CatalogFlags {
    /// Resolve `clap`'s raw flags for one verb. PURE — no I/O, no socket.
    ///
    /// ⚠ Every flag is accepted by `clap` for every verb and refused PER VERB here, rather than
    /// being routed by a per-verb struct. That ordering is `crate::cmd::data`'s and it buys the
    /// same thing: an inapplicable flag is named in a message that says which verb it DOES belong
    /// to, where an unknown-option error would tell an operator the flag does not exist — which is
    /// false, and sends them looking in the wrong place.
    fn args_for(&self, verb: Verb, configured_addr: Option<&str>) -> Result<Args, String> {
        let positional = match self.instrument.as_slice() {
            [] => None,
            [only] => Some(only.clone()),
            // Named BOTH, for `crate::cmd::data`'s reason: an operator who typed two instruments
            // cannot tell which one this parser kept unless the refusal says.
            [had, flag, ..] => {
                return Err(format!(
                    "two positional arguments ('{had}' and '{flag}') — `data catalog show` \
                     takes exactly one VENUE:SYMBOL"
                ));
            }
        };
        let venue = self.venue.clone();
        let class = self.class.as_deref().map(parse_class).transpose()?;
        // ⚠ **An empty needle is not a narrow filter, it is NO filter wearing one.** `contains("")`
        // is true for every row, so the whole listing would render under a summary that says a
        // filter selected it, and a TRUNCATED listing would earn [`truncation_warning`]'s ⚠ for a
        // filter that selected nothing out. The command-line layer refuses a blank value for every
        // flag, so the needle that reaches this point is text.
        let search = self.search.clone();
        let format = self.format.as_deref().map(parse_format).transpose()?;
        let json_flag = self.json;

        // ⚠ THE CONTRADICTION IS REFUSED RATHER THAN RESOLVED, and the sentence is deliberately the
        // one `crate::cmd::data`'s `parse` already gives: `--json` IS `--format json`, so the two can
        // disagree in exactly one way. Picking a winner would silently discard half of what the
        // operator typed. The two spellings are held EQUAL by
        // `the_two_groups_refuse_the_json_format_contradiction_in_the_same_words`, which is what stops
        // one group's wording drifting away from the other's without a shared const to import.
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

        let instrument = match (verb, positional) {
            (Verb::Show, Some(spec)) => Some(parse_instrument(&spec)?),
            (Verb::Show, None) => {
                return Err(
                    "`data catalog show` needs an instrument: VENUE:SYMBOL (e.g. binance:BTCUSDT)"
                        .to_string(),
                );
            }
            (v, Some(spec)) => {
                return Err(format!(
                    "`{}` takes no positional argument ('{spec}') — an instrument is `data catalog \
                     show VENUE:SYMBOL`",
                    v.as_str()
                ));
            }
            (_, None) => None,
        };

        match verb {
            Verb::Ls | Verb::Refresh => {
                let Some(slug) = venue.as_deref() else {
                    return Err(format!(
                        "`{}` needs --venue: a catalog is per VENUE, and there is no roster-wide \
                         listing on this wire. `data catalog venues` is the roster",
                        verb.as_str()
                    ));
                };
                // ⚠ **The SHAPE is refused HERE, by this binary, before a socket is opened.**
                // `vike_datahub_client::catalog::validate_catalog_venue` is the same function the
                // server's own door calls, and that module's doc says the client copy exists "for the
                // message and never for the enforcement" — i.e. to be called exactly here. Until it
                // was, `--venue BINANCE` (or `bin@nce`, or the empty string a shell variable expanded
                // to) opened a connection and the operator's diagnosis depended on who was on the
                // port: with no datahub up, exit 3 `cannot connect to datahub at …` — a connect-class
                // rung a WRAPPER RETRIES — and against a server with no catalog lane, the `does not
                // advertise venue_catalog` message. Neither ever mentioned the slug.
                //
                // The sentence is the client's own rather than one minted here, for
                // [`refuse_an_unlistable_venue`]'s reason: one refusal, one wording, wherever it is
                // reached from. ⚠ It deliberately never echoes the value back — that validator's doc
                // argues why, and prefixing the flag name is all this side adds.
                //
                // ⚠ `show`'s venue is NOT put through this: that half of the VENUE:SYMBOL positional
                // reaches `properties_as_of`, a different wire verb with its own door, and coercing it
                // through the CATALOG validator would refuse a spelling that verb serves.
                validate_catalog_venue(slug).map_err(|why| format!("`--venue`: {why}"))?;
            }
            Verb::Show => {
                if venue.is_some() {
                    return Err(
                        "--venue does not apply to `show` — the venue is the FIRST half of the \
                         VENUE:SYMBOL positional, and a second spelling of it could disagree with it"
                            .to_string(),
                    );
                }
            }
            Verb::Venues => {
                if venue.is_some() {
                    return Err(
                        "--venue does not apply to `venues` — the whole roster IS this verb's answer. \
                         One venue's instruments are `data catalog ls --venue V`"
                            .to_string(),
                    );
                }
            }
        }

        if verb != Verb::Ls {
            for (flag, given) in [("--class", class.is_some()), ("--search", search.is_some())] {
                if given {
                    return Err(format!(
                        "{flag} does not apply to `{}` — it narrows a LISTING, which is `data \
                         catalog ls --venue V`",
                        verb.as_str()
                    ));
                }
            }
        }

        Ok(Args {
            verb,
            venue,
            instrument,
            class,
            search,
            addr: self
                .addr
                .clone()
                .or_else(|| {
                    // A BLANK rung is skipped rather than honoured, the same rule
                    // `crate::cmd::data`'s `parse` applies: an `Environment=` line that set nothing
                    // must not aim this at an empty address.
                    configured_addr.filter(|s| !s.trim().is_empty()).map(str::to_string)
                })
                .unwrap_or_else(|| vike_config::DEFAULT_DATAHUB_ADDR.to_string()),
            json,
        })
    }
}

/// `--class`'s value, resolved against the model's own stored words, case-insensitively.
///
/// ⚠ **The vocabulary is `vike_model::AssetClass`'s, not this file's**, and the comparison is over
/// `AssetClass::sql_word` — the same word the store persists and the same word
/// `crate::cmd::data`'s `class_cell` renders. A second spelling minted here (`crypto-perp`,
/// `perp`) would be a private vocabulary an operator could not carry between the two groups.
///
/// An EMPTY or blank value never reaches this function: the command-line layer refuses it for every
/// flag (`--class ""` is `--class needs a non-empty value`), which is the rule this function used to
/// state itself in a sentence of its own.
fn parse_class(value: &str) -> Result<AssetClass, String> {
    AssetClass::ALL
        .iter()
        .copied()
        .find(|c| c.sql_word().eq_ignore_ascii_case(value))
        .ok_or_else(|| format!("unknown `--class {value}` ({})", class_roster()))
}

/// The `VENUE:SYMBOL` positional's SHAPE — two non-empty parts and nothing else.
///
/// ⚠ **A three-part spelling is refused BY NAME rather than as a shape error**, because it is not
/// a typo: `VENUE:SYMBOL:INTERVAL` is the grammar `crate::cmd::data`'s `check_spec` enforces one
/// group over, and an operator arriving from `data hist fetch` will type it. An interval is a
/// property of a SERIES; an instrument has none, so the refusal says which verb wants the third
/// part rather than telling them their line is malformed.
///
/// Neither half is validated further, for the reason `crate::cmd::data`'s module doc gives: the
/// reachable set is a property of a remote process this crate cannot see at parse time.
fn parse_instrument(spec: &str) -> Result<InstrumentRef, String> {
    let parts: Vec<&str> = spec.split(':').collect();
    if parts.len() == 3 {
        return Err(format!(
            "'{spec}' names a SERIES, not an instrument — the third part is a bar INTERVAL and an \
             instrument has none. `data catalog show VENUE:SYMBOL`; the interval belongs to the \
             `data hist` verbs"
        ));
    }
    if parts.len() != 2 || parts.iter().any(|p| p.trim().is_empty()) {
        return Err(format!(
            "'{spec}' is not VENUE:SYMBOL — two non-empty parts, e.g. binance:BTCUSDT"
        ));
    }
    Ok(InstrumentRef { venue: parts[0].to_string(), symbol: parts[1].to_string() })
}
