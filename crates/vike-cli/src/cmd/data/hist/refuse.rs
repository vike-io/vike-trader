//! The refusals the `history` verbs share: a flag on the wrong verb, a malformed window or spec, a kind.
use vike_model::{parse_date_label, time::epoch_ms_to_utc_date};

use super::{Sub, Window, universe};

/// ⚠ **`repair` has no DATAHUB route, and `--addr` is refused rather than ignored.** The sentence,
/// and the argument behind it, live here because three separate facts each say no and a reader who
/// only meets one of them will try to add the route back.
///
/// 1. **The wire's own rule.** `docs/decisions/0058`'s decision 3 admits a write verb to the Observe
///    side only when it is additive, idempotent, operator-armed and bounded on every dimension the
///    request names, all four together. A rebuild fails three of them: its cost is however many
///    part footers the series has, it REPLACES an index rather than adding to one, and it clears a
///    delta log. `vike_datahub_client::proto`'s `FEATURE_DELETE_SERIES` is the precedent for the
///    Control side, and `docs/decisions/0050` is why a key-less datahub would serve no such verb
///    at all — so the datahub route would exist for a minority of deployments.
/// 2. **The datahub's vocabulary cannot name the broken series.** Every store-metadata RPC starts
///    from `HistoryStoreDataFusion::list_series`, which finds leaves by the presence of `_manifest.json` —
///    so the headline failure this verb repairs is absent from `inventory()` and from every answer
///    a datahub can give. A remote repair would be reachable for exactly the series that do not
///    need one.
/// 3. **It would be a SECOND writer on a hot series**, which is the reopening clause
///    `docs/decisions/0060` names by hand — and the process holding the store open over that socket
///    is usually the recorder itself.
///
/// So the route is `--store` (or the resolved default) and the engine, on the box the store is on,
/// which is also where somebody repairing a store already is.
pub(crate) fn refuse_the_remote_route_on_repair() -> String {
    "--addr asks a running vike-datahub about the store THAT process opened, and `repair` has no \
     datahub route. Three reasons: a rebuild is neither cost-bounded nor additive nor idempotent, \
     so it does not meet the bar a write-shaped wire verb has to clear; a datahub can only name \
     series it ENUMERATED, and a series whose base manifest is missing is in no enumeration — \
     which is exactly the series this repairs; and the process serving that socket is usually the \
     writer a rebuild must not collide with. Run it on the box, with --store DIR (or none, for the \
     resolved default)."
        .to_string()
}

/// ⚠ **`rename` has no DATAHUB route either, and `--addr` is refused rather than ignored.** It
/// writes whole series under the series lock beside the running datahub, which needs the store's own
/// box, and a wire verb that rewrites hundreds of megabytes is neither cost-bounded nor additive —
/// [`refuse_the_remote_route_on_repair`]'s first reason, unchanged.
pub(crate) fn refuse_the_remote_route_on_rename() -> String {
    "--addr asks a running vike-datahub about the store THAT process opened, and `rename` has no \
     datahub route: it runs on the store's own machine, beside the datahub, under the series lock. \
     Run it there (over ssh if the store is remote), with --store DIR or none for the resolved \
     default."
        .to_string()
}

/// Refuse a fetch WINDOW on a source whose span is fixed — `fetch-starter`'s published dataset.
///
/// Refused rather than ignored, for one reason both share: a `--days 30` that quietly did nothing
/// would leave an operator believing they had a month of history that is not there.
pub(super) fn refuse_a_window(
    sub: Sub,
    days: &Option<String>,
    from: &Option<String>,
    to: &Option<String>,
    why: &str,
) -> Result<(), String> {
    for (flag, present) in
        [("--days", days.is_some()), ("--from", from.is_some()), ("--to", to.is_some())]
    {
        if present {
            return Err(format!(
                "{flag} bounds a `fetch` window and does not apply to `{}` — {why}",
                sub.as_str()
            ));
        }
    }
    Ok(())
}

/// Refuse the first flag in `present` that was actually given, naming the subcommand it was given
/// to and WHY it belongs elsewhere.
///
/// ⚠ The `why` sentence is the product, not the refusal. Every flag this function guards is one an
/// operator typed because a SIBLING subcommand accepts it, so "unknown option" would be a lie and
/// a bare "not allowed here" would leave them guessing which sibling. The messages therefore name
/// the store the flag would have reached and the flag that reaches the other one.
pub(crate) fn refuse_foreign_flags(
    sub: Sub,
    present: &[(&str, bool)],
    why: &str,
) -> Result<(), String> {
    for (flag, given) in present {
        if *given {
            return Err(format!("{flag} does not apply to `{}` — {why}", sub.as_str()));
        }
    }
    Ok(())
}

/// `--store` on a verb that READS history — `export` and every [`Sub::is_read`] verb — refused with
/// `vike_model::run_request::flag_vocab::store_flag_removed`, the ONE sentence every history reader in
/// this workspace prints, naming the verb as the operator typed it.
///
/// ⚠ **The read verbs used to answer this mistake with a sentence of their own** (*"… reach it
/// with --addr"*), and the change that closed `export`'s local read on 2026-09-26 first extended
/// that sentence by hand-spelling the key-less datahub command a second time — so
/// `data history ls --store X` and `data history export --store X` answered the same mistake two ways,
/// and the copy could drift from the shared one without any test noticing. Both go through the
/// shared sentence now.
///
/// ⚠ **A read verb ADDS one clause the shared sentence cannot carry**: its `--addr` reaches a
/// datahub that already has a store open on another box, which is the read an operator who typed a
/// server's store path usually wanted. `export` does not get the clause, deliberately — its
/// `--addr` does not name another source for the same file, it selects a different ROUTE that
/// writes rows rather than Parquet (see [`export`]'s module doc), so pointing at it from here would
/// answer a question about WHERE with a change of WHAT.
pub(crate) fn store_refusal(sub: Sub) -> String {
    let sentence = vike_model::run_request::flag_vocab::store_flag_removed(&format!(
        "data history {}",
        sub.as_str()
    ));
    if sub.is_read() {
        format!(
            "{sentence} A store a datahub on another box already has open is read with \
             --addr HOST:PORT."
        )
    } else {
        sentence
    }
}

/// The spec's SHAPE: three non-empty `:`-separated parts. See this module's doc for why the venue
/// and interval themselves are the ENGINE's to judge.
pub(super) fn check_spec(spec: &str) -> Result<(), String> {
    let parts: Vec<&str> = spec.split(':').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.trim().is_empty()) {
        return Err(format!(
            "'{spec}' is not VENUE:SYMBOL:INTERVAL — three non-empty parts, e.g. binance:BTCUSDT:1h"
        ));
    }
    Ok(())
}

/// The window, as exactly one of the two forms.
///
/// ⚠ Mixing them is an error rather than a precedence rule. `--days 30 --from 2026-01-01` has
/// two readable meanings and no obviously right one, and whichever a precedence rule picked would
/// silently discard the other half of what the operator typed.
pub(super) fn window_from(
    days: Option<String>,
    from: Option<String>,
    to: Option<String>,
) -> Result<Window, String> {
    match (days, from, to) {
        (Some(d), None, None) => {
            let n: u32 = d
                .trim()
                .parse()
                .map_err(|_| format!("--days takes a whole number of days, got {d:?}"))?;
            if n == 0 {
                return Err("--days 0 covers no time at all".to_string());
            }
            Ok(Window::Days(d))
        }
        (None, Some(f), Some(t)) => Ok(Window::Range { from: f, to: t }),
        (None, Some(_), None) => Err("--from needs a matching --to".to_string()),
        (None, None, Some(_)) => Err("--to needs a matching --from".to_string()),
        (None, None, None) => Err(
            "fetch needs a window: --days N, or --from WHEN --to WHEN (epoch-ms, or a UTC date \
             YYYY-MM-DD such as 2024-01-01)"
                .to_string(),
        ),
        (Some(_), _, _) => {
            Err("--days and --from/--to are two ways to say the same thing — pass one".to_string())
        }
    }
}

/// `universe`'s window: BOTH bounds optional, BOTH independent, and BOTH parsed here.
///
/// ⚠ **The parse is the difference from [`ExportRange`], and it is the reason this verb has a
/// helper of its own.** An ENGINE export's bounds are forwarded to the engine as TEXT, and the
/// engine parses them its own way. Nothing is forwarded here: `universe` compares timestamps a
/// datahub already sent, in this process, so an unreadable bound has to be refused HERE or it would
/// be silently discarded into a window that means something else. It goes through
/// [`vike_model::parse_date_label`] — the SAME parser `fetch`'s bounds ([`fetch_window_ms`]) and a
/// remote export's go through, so those verbs cannot disagree about what `2026-01-01` means.
///
/// ⚠ This said "the SAME parser the engine's own bounds reach", and the engine's do not: an ENGINE
/// export hands its bounds to `crates/vike-backtest/src/run/profile.rs`'s `parse_timestamp`, which
/// takes epoch-ms, a plain date and also an hour label (`2026-01-01T00`), the one spelling
/// `parse_date_label` refuses (until 2026-11-14 it refused a plain date). The `--from` help
/// states both, by which process parses.
///
/// The four combinations are all well-formed (see [`ExportRange`] for that argument), so the only
/// refusals are an unreadable label and an INVERTED pair — and the second is refused rather than
/// swapped, for [`window_from`]'s reason: a range whose ends are the wrong way round has two
/// readable meanings, and picking one discards half of what the operator typed. An inverted window
/// would otherwise report every instrument in the store `absent`, which reads exactly like an
/// empty store.
pub(super) fn membership_window(
    from: Option<&str>,
    to: Option<&str>,
) -> Result<universe::MembershipWindow, String> {
    fn bound(flag: &str, raw: Option<&str>) -> Result<Option<i64>, String> {
        let Some(raw) = raw else { return Ok(None) };
        match parse_date_label(raw) {
            Ok(ms) => Ok(Some(ms)),
            Err(e) => Err(format!(
                "{flag} {raw:?} is not a timestamp this side can read ({e}). `universe` compares \
                 dates in THIS process rather than forwarding them, so the bound is parsed here: \
                 epoch-ms, or YYYY-MM-DD"
            )),
        }
    }
    let from_ms = bound("--from", from)?;
    let to_ms = bound("--to", to)?;
    match (from_ms, to_ms) {
        (Some(f), Some(t)) if f > t => Err(format!(
            "--from ({}) is AFTER --to ({}) — an inverted window contains nothing, so every \
             instrument in the store would be reported `absent`, which is indistinguishable from \
             an empty store. Pass them the other way round.",
            epoch_ms_to_utc_date(f),
            epoch_ms_to_utc_date(t)
        )),
        _ => Ok(universe::MembershipWindow { from: from_ms, to: to_ms }),
    }
}
