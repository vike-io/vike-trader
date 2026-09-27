//! **Which venues of this process have more than one ACCOUNT mounted, and what to say about it** —
//! the pure half of the account-routing seam
//! (`docs/superpowers/specs/2026-09-13-the-wire-names-an-account-from-the-misroute.md`, Stage 0).
//!
//! # The fact these functions are about
//!
//! `vike_mount::make_engine_accounts` mounts one `vike_exec::ExecutionEngine` per ACTIVE account of
//! a venue, stamping each with a distinct `route_key` (`vike_model::account_keys::route_key_of` —
//! the bare venue id for the default account, `venue#LABEL` for a labelled one) while leaving every
//! engine's `venue` the canonical exchange id. So a process holding two binance accounts holds two
//! engines whose `venue` is `"binance"`, and a command that names `"binance"` and nothing else
//! names BOTH of them.
//!
//! That is the ambiguity. `CoreThread::route_of`'s fallthrough resolved it by picking the venue's
//! DEFAULT account, silently, for the whole life of the command plane — which is the one
//! disposition the spec's ONE RULE names by name: *"not through a default that was correct when
//! there was one account."*
//!
//! # Why the WARNING is a pure function and the `tracing::warn!` is elsewhere
//!
//! The shape is `vike_mount::paper_fallback`'s `venue_arming_migration_message` /
//! `venue_arming_migration` pair, copied deliberately: *"Returns the message rather than logging
//! it, so the decision is testable with no subscriber."* A message builder that takes
//! `(venue, route_key)` pairs and NOTHING else can be driven by a unit test at full strength, while
//! an emission that reaches a real two-account core needs two credential sets and two live sockets.
//!
//! # ⚠ The denominator is the ENGINE set, and it has to be the SAME one the refusal counts
//!
//! §4.2 of the spec states the count as `engines_of_venue(venue).len()`, and Stage 1's refusal
//! reads exactly that. This warning reads the same set — it is built from the assembled core's own
//! engines rather than from the mount's arming rows — because the alternative has already cost this
//! tree an incident: `vike_exec::ReconcileReports::route_key`'s producer counted LEGS while
//! `CoreThread::reconcile_reports` counted ENGINES, and a venue with two engines and one leg
//! refused the surviving account's own legitimate pass every interval until the predicate was
//! corrected. One set, one answer, or the warning promises a refusal that does not come (or stays
//! silent before one that does).
//!
//! ⚠ **A consequence worth stating rather than discovering**: dukascopy is NOT named by this
//! warning on any box today, even though it is the venue with two credential sets in the owner's
//! own store. `vike_mount::dukascopy::sidecar_holder` picks ONE account before anything is mounted
//! and the other declines to paper by name, so the venue never produces two ENGINES in one process
//! — and a warning scoped to armed CREDENTIALS rather than to mounted engines would name a venue
//! Stage 1 can never refuse. The venues this reaches are the ones whose arm addresses several
//! accounts through the `__LABEL` key grammar (`vike_mount::arming`'s `arm_addresses_accounts`).

use std::collections::BTreeMap;

/// **Every venue this process runs MORE THAN ONE account of**, and each one's route keys.
///
/// `engines` is `(venue, route_key)` in the core's own engine order — primary first, then the
/// extras in registration order, which is the order `vike_run::build_node` binds them in and the
/// order an operator reading a snapshot sees.
///
/// Venues are returned in a stable (sorted) order so a log line and a test cannot disagree about
/// it; the route keys inside a venue keep ENGINE ORDER, so the venue's default account is listed
/// first — it is the account every account-less command reached before Stage 1, which is the one
/// an operator reading the refusal is most likely to have meant.
///
/// Empty for every single-account process, which is every process with no `policy.accounts` rows.
#[must_use]
pub fn ambiguous_venues<'a>(
    engines: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Vec<(String, Vec<String>)> {
    let mut by_venue: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for (venue, route_key) in engines {
        by_venue.entry(venue).or_default().push(route_key.to_string());
    }
    by_venue
        .into_iter()
        .filter(|(_, keys)| keys.len() > 1)
        .map(|(venue, keys)| (venue.to_string(), keys))
        .collect()
}

/// **The Stage 0 startup warning** — one paste-ready block naming every ambiguous venue, every one
/// of its accounts, the FILE and the KEY that armed them, and what will happen to an account-less
/// command once Stage 1 lands.
///
/// `None` when nothing is ambiguous, which is the self-silencing property
/// `venue_arming_migration_message` has for the same reason: a deployment with nothing to say never
/// sees a line, so the line means something when it appears.
///
/// # ⚠ Route keys and settings keys only — never a credential key NAME
///
/// The same rule `venue_arming_migration_message` states: *"this text is designed to be pasted, and
/// a paste-ready block that carries key names invites the reply that carries values."* A route key
/// is already public — it is the `LIVE-<route_key>.lock` sentinel filename on disk and the string
/// the snapshot publishes — and `policy.accounts.<venue>.<LABEL>` is a settings path, not a secret.
#[must_use]
pub fn ambiguity_warning(ambiguous: &[(String, Vec<String>)]) -> Option<String> {
    if ambiguous.is_empty() {
        return None;
    }
    let mut out = String::from(
        "SEVERAL ACCOUNTS OF ONE VENUE ARE MOUNTED: a command that names only the EXCHANGE no \
         longer names one book on this node.\n\n\
         Each venue below runs more than one account, armed from the settings database's \
         `policy.accounts` rows. Every account has its own wallet, its own positions and its own \
         `LIVE-<route key>.lock` sentinel.\n\n",
    );
    for (venue, keys) in ambiguous {
        out.push_str(&format!("  {venue} — {} accounts:\n", keys.len()));
        for key in keys {
            let label = vike_model::account_keys::label_of_route_key(venue, key);
            let arming = match label.as_ref().and_then(vike_model::account_keys::AccountLabel::text)
            {
                None => format!("policy.venues.{venue}"),
                Some(l) => format!("policy.accounts.{venue}.{l}"),
            };
            out.push_str(&format!("      {key}   (armed by {arming})\n"));
        }
    }
    out.push_str(
        "\nUntil now an ORDER naming one of these venues and no account reached the venue's \
         DEFAULT account — the first route key listed above — whatever the sender meant. That is \
         the misroute this node now REFUSES: a risk-INCREASING command (a submit, a bracket, a \
         combo, a conditional arm) that names one of these venues and no account is refused by \
         name, and the refusal lists the candidates above.\n\n\
         A risk-REDUCING one is NOT refused and gets WIDER instead: `market_exit`, `mass_cancel` \
         and `flatten` naming no account reach EVERY account of the venue named, because \"close \
         everything on this exchange\" is the only reading of those verbs that is not a trap; \
         naming an account narrows any of the three to that account's book. (The trading-state \
         switch names no venue at all and has always reached every engine.)\n\n\
         A strategy MOUNT already names its account (policy.accounts + the mount's own `account` \
         field), so mounted strategies are unaffected. `vike-cli config show --filter policy` \
         prints what the binaries will read.\n",
    );
    Some(out)
}

/// **The refusal text for ONE ambiguous command** — the verb, the venue, and EVERY account it could
/// have meant.
///
/// ⚠ **Both candidates, always.** A refusal that says "ambiguous" without saying which books it
/// could have meant tells an operator that something is wrong and not what to do about it, and this
/// message is the whole of what they see: it lands in `CoreThread::note` (the snapshot's
/// recent-events ring, which the GUI, the Telegram channel and `vike-cli` all render) as well as on
/// `tracing::error!`.
///
/// `verb` is the intent's own name (`"submit"`, `"bracket"`) so the line reads as a sentence about
/// the thing the operator did, and `candidates` is `ambiguous_venues`' list for the venue — engine
/// order, so the account the command USED to reach is named first.
///
/// ⚠ **The advice at the end has to be true for every verb that reaches here** (`submit`,
/// `bracket`, `ArmConditional`, `combo`, and a restored conditional's fire, which is a `submit`).
/// A submit's ticket carries `account`, while the other three cannot name one yet. The reducing
/// verbs narrow to a NAMED account and fan out over every account only when they name none.
///
/// Until 2026-09-26 this text said an operator command "cannot name one yet" and that the
/// reducing verbs "reach EVERY account of the venue", unqualified. That steered an operator who
/// wanted out of one book toward an account-less `flatten`, which closes every book, on a node
/// that honours the labelled one. The test
/// `a_refusal_steers_a_one_account_reduce_to_the_verb_that_names_it` pins the corrected advice.
#[must_use]
pub fn refusal(verb: &str, venue: &str, candidates: &[String]) -> String {
    let named = candidates.join(", ");
    format!(
        "{verb} REFUSED: it names venue `{venue}` and no account, and this process runs \
         {n} accounts of it — {named}. Nothing was sent. This command used to reach `{first}` \
         (the venue's default account) whichever account was meant; naming the exchange is not \
         naming a book. A strategy mount names its account, and so may an order ticket (a \
         submit's own `account`); a bracket, a combo and a conditional arm cannot name one yet. \
         To close or reduce ONE account, name it on `market_exit` / `mass_cancel` / `flatten`, \
         which narrows the verb to that account's book — the same verbs naming NO account reach \
         EVERY account of the venue.",
        n = candidates.len(),
        first = candidates.first().map_or("", String::as_str),
    )
}

/// **The refusal text for a command naming an account this process runs no engine for** — the
/// sibling of [`refusal`], for the other way a destination can fail to be determined.
///
/// [`refusal`] is about a sender who named too LITTLE (an exchange this node runs several books
/// of); this is about one who named something that is not here at all.
///
/// # ⚠ It deliberately does NOT list the accounts this node DOES hold, and [`refusal`] does
///
/// The asymmetry is a ruling, not an oversight. An AMBIGUOUS command can only be repaired by
/// seeing the options, and the core's ring is the whole of what that operator sees — so
/// [`refusal`] must carry them. An UNHELD account on a SUBMIT is answered at the node's EDGE
/// instead, before the Ack, where the wire handler refuses the ticket outright and renders the
/// accounts it could have meant; a client learns `NOSUCH` is wrong there rather than from an
/// `accepted` followed by a log line. Rendering the same list twice would be two spellings of one
/// answer, and the one here is the one nobody is reading.
///
/// ⚠ **"At the edge" is narrower than it sounds, and this sentence used to claim all of it.** The
/// edge gate reads the roster the core PUBLISHES, so it covers a submit on a node that has
/// published at least once — and deliberately refuses nothing before that first publish, because a
/// core publishes when its state goes dirty and a refused command never reaches the core, so
/// refusing on an empty roster would refuse every command for ever. In that case this text is what
/// an operator gets, which is the next paragraph's point rather than an exception to it.
///
/// ⚠ **This used to name a SECOND hole — `MountStrategy`, "an account that nothing at the edge
/// checks yet" — and that hole is CLOSED.** `vike_tradehub::server::account_refusal` checks the
/// mount plane too now, in the same sentence the submit plane gets with its subject changed, so a
/// mount naming an unheld account is refused before the Ack like an order is. The core's own copy
/// (`CoreThread::mount_strategy_runtime`, which folds this refusal's sibling wording into a
/// recent-events note) stays and is not made redundant: an in-process mount, a topology replay and
/// a strategy-minted order all reach the core without passing that edge.
///
/// What this text is for is the case the edge cannot cover: the core has callers besides the wire
/// handler, and what it owes all of them is that an unheld account is never quietly turned into
/// the venue's default book. So the message states the refusal and the two facts that identify it,
/// and stops.
#[must_use]
pub fn unheld_account_refusal(
    verb: &str,
    venue: &str,
    account: &vike_model::account_keys::AccountLabel,
) -> String {
    format!(
        "{verb} REFUSED: it names account `{account}` of venue `{venue}`, and this process runs no \
         engine for that account. Nothing was sent. It is NOT routed to the venue's default \
         account instead: an account nobody mounted is not a spelling of the one that is, and \
         resolving it that way would place the order in a book the sender never named."
    )
}

/// **The refusal text for a command naming an account and NO venue** — the third way a
/// destination can fail to be determined, and the one only a risk-REDUCING verb can reach
/// (`MassCancel`/`MarketExit` are the two whose venue is optional).
///
/// An account label names one book OF a venue, so on its own it names nothing. The hazard is where
/// such a command would otherwise LAND: a venue-less `MarketExit` or `MassCancel` is the GLOBAL
/// verb, every engine this process runs. Reading the account as ignorable would turn the narrowest
/// request an operator can make into the widest action the core can take, so the command is
/// refused and the sentence says what to send instead.
///
/// Like [`unheld_account_refusal`] it lists no candidates: the node's EDGE refuses this frame
/// before the Ack whatever its roster, and this is the core's own record for the callers that do
/// not pass through that edge.
#[must_use]
pub fn account_without_venue_refusal(
    verb: &str,
    account: &vike_model::account_keys::AccountLabel,
) -> String {
    format!(
        "{verb} REFUSED: it names account `{account}` and no venue. Nothing was sent. An account \
         label names one book OF a venue, so on its own it resolves to nothing — and it is NOT \
         widened to every engine instead: name the venue the account belongs to, or name neither \
         for the whole-process verb."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_account_process_has_nothing_to_say() {
        let engines = [("binance", "binance"), ("bybit", "bybit"), ("okx", "okx")];
        assert!(ambiguous_venues(engines).is_empty());
        assert_eq!(ambiguity_warning(&[]), None);
    }

    /// ⚠ **The ambiguity is scoped to the VENUE, never to the node** — §4.2: *"a box running
    /// forty-nine binance accounts and one okx account keeps serving every account-less okx command
    /// unchanged, and refuses only the binance ones."*
    #[test]
    fn ambiguity_is_per_venue_not_per_node() {
        let engines = [("binance", "binance"), ("okx", "okx"), ("binance", "binance#ALT")];
        let found = ambiguous_venues(engines);
        assert_eq!(found.len(), 1, "okx has one account and must not appear: {found:?}");
        assert_eq!(found[0].0, "binance");
        assert_eq!(
            found[0].1,
            vec!["binance".to_string(), "binance#ALT".to_string()],
            "engine order, so the DEFAULT account is listed first"
        );
    }

    #[test]
    fn the_warning_names_the_rows_the_keys_and_every_account() {
        let found = ambiguous_venues([
            ("binance", "binance"),
            ("binance", "binance#ALT"),
            ("bybit", "bybit"),
        ]);
        let text = ambiguity_warning(&found).expect("two binance accounts is something to say");
        assert!(!text.contains(".toml"), "no settings file may be named: {text}");
        assert!(text.contains("policy.accounts"), "names the ROW space: {text}");
        assert!(text.contains("policy.venues.binance"), "names the default account's KEY: {text}");
        assert!(text.contains("policy.accounts.binance.ALT"), "names the labelled KEY: {text}");
        assert!(text.contains("binance#ALT"), "names the second ACCOUNT: {text}");
        assert!(!text.contains("bybit"), "a single-account venue is not named at all: {text}");
        assert!(text.contains("REFUSES"), "says what will happen: {text}");
        assert!(text.contains("market_exit"), "…and what still works: {text}");
    }

    /// The paste-safety rule, asserted rather than trusted: no credential key name can appear,
    /// because nothing but a route key and a settings path is ever interpolated.
    #[test]
    fn the_warning_prints_no_credential_key_name() {
        const VENUE: &str = "binance";
        let found = ambiguous_venues([(VENUE, VENUE), (VENUE, "binance#ALT")]);
        let text = ambiguity_warning(&found).expect("something to say");
        // ⚠ The venue's credential-key PREFIX is DERIVED rather than spelled out. Written as a
        // literal it would be an ENV-SHAPED string — the upper-cased venue id with a trailing
        // underscore — which `vike-ops`' settings-registry scanner reads as a credential key this
        // module reads. It reads none, and that gate's exception tables are for the gate's OWN
        // machinery (`scan.rs`, `settings.rs`, its own test file), never for a test needle.
        // Deriving it also keeps the needle correct if `VENUE` ever changes.
        let key_prefix = format!("{}_", VENUE.to_uppercase());
        for needle in ["_API_KEY", "_API_SECRET", "_API_PASSPHRASE", key_prefix.as_str()] {
            assert!(!text.contains(needle), "{needle} must never be pasted: {text}");
        }
    }

    #[test]
    fn a_refusal_names_every_candidate_and_the_one_it_used_to_pick() {
        let text = refusal(
            "submit",
            "binance",
            &["binance".to_string(), "binance#ALT".to_string(), "binance#HEDGE".to_string()],
        );
        assert!(text.contains("binance#ALT"), "{text}");
        assert!(text.contains("binance#HEDGE"), "{text}");
        assert!(text.contains("3 accounts"), "{text}");
        assert!(text.contains("Nothing was sent"), "{text}");
        assert!(
            text.contains("used to reach `binance`"),
            "names the account the misroute reached, so an operator can tell whether they were \
             relying on it: {text}"
        );
    }

    /// ⚠ **The refusal's advice has to match what this node does with the reducing verbs.** Naming an
    /// account narrows them to that account's book, and only naming NONE reaches every account.
    ///
    /// The old text said they "reach EVERY account of the venue", unqualified, and that an operator
    /// command "cannot name" an account. An operator who wanted out of `ALT` alone followed it, sent
    /// an account-less `flatten`, and closed the DEFAULT account too. That happened on a node that
    /// honours `flatten binance BTCUSDT ALT` and advertises the capability. "Cannot name one yet" is
    /// also false for a submit, whose ticket carries `account`, though it is still true for a
    /// bracket, a combo and a conditional arm.
    #[test]
    fn a_refusal_steers_a_one_account_reduce_to_the_verb_that_names_it() {
        let text =
            refusal("submit", "binance", &["binance".to_string(), "binance#ALT".to_string()]);
        assert!(
            text.contains("name it on `market_exit` / `mass_cancel` / `flatten`"),
            "names the narrowing: {text}"
        );
        assert!(text.contains("that account's book"), "…and what it narrows to: {text}");
        assert!(
            text.contains("naming NO account reach EVERY account"),
            "the fan-out is stated with its condition, never bare: {text}"
        );
        assert!(
            !text.contains("an operator command cannot name one yet"),
            "a submit CAN name its account: {text}"
        );
        assert!(text.contains("a submit's own `account`"), "…and the text says so: {text}");
        assert!(
            text.contains("a bracket, a combo and a conditional arm cannot name one yet"),
            "…while the three that cannot are named: {text}"
        );
    }

    /// The unheld-account refusal names the two facts that identify it — and, asserted just as
    /// hard, does NOT name the books this node holds. That omission is the ruling (the edge
    /// renders that list, before the Ack), so it is pinned rather than left to drift back in.
    #[test]
    fn an_unheld_account_refusal_names_the_account_and_lists_no_others() {
        let alt = vike_model::account_keys::AccountLabel::parse("NOSUCH").expect("a legal label");
        let text = unheld_account_refusal("submit", "binance", &alt);
        assert!(text.contains("NOSUCH"), "names the account that was asked for: {text}");
        assert!(text.contains("binance"), "names the venue: {text}");
        assert!(text.contains("Nothing was sent"), "{text}");
        assert!(
            !text.contains("binance#"),
            "no route key of another account may appear — enumerating the node's books is the \
             EDGE's rendering, and a second copy here is the one nobody reads: {text}"
        );
    }

    /// …and the DEFAULT account has a spelling here too, through [`AccountLabel`]'s `Display`.
    /// It is reachable: a payload may name `DEFAULT` on a venue this core runs no engine for at
    /// all, which is an unheld account like any other.
    #[test]
    fn the_default_account_is_nameable_in_an_unheld_refusal() {
        let text = unheld_account_refusal(
            "submit",
            "okx",
            &vike_model::account_keys::AccountLabel::Default,
        );
        assert!(text.contains("DEFAULT"), "the wire's spelling of the unlabelled account: {text}");
        assert!(text.contains("okx"), "{text}");
    }

    /// The venue-less refusal names the account, says the venue is what is missing, and says the
    /// command was NOT widened — the one sentence an operator who sent a narrowed panic button
    /// needs in order to know their other books were not touched.
    #[test]
    fn an_account_without_a_venue_refusal_names_the_account_and_refuses_the_widening() {
        let alt = vike_model::account_keys::AccountLabel::parse("ALT").expect("a legal label");
        let text = account_without_venue_refusal("market_exit", &alt);
        assert!(text.contains("market_exit REFUSED"), "{text}");
        assert!(text.contains("`ALT`"), "names the account: {text}");
        assert!(text.contains("no venue"), "names what is missing: {text}");
        assert!(text.contains("Nothing was sent"), "{text}");
        assert!(text.contains("NOT widened"), "{text}");
    }
}
