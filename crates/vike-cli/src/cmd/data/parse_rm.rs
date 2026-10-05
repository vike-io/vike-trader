//! `data hist rm` — the arm of the per-subcommand `match` in `super::parse` that holds its
//! grammar.
//!
//! Moved here verbatim from that function (code-layout phase 2, task 11); see
//! `parse_fetch` for how the pieces fit.

use super::{
    Filter, RmArgs, Window, refuse_a_blank_produced_by, refuse_a_producer_path_on_the_remote_route,
    refuse_an_account_kind,
};

/// `rm`'s arm: MOVES `--kind`/`--venue` out of `filter` (they are the selector here, not a
/// substring filter) and builds the `RmArgs`. Returns the `(spec, window)` pair `parse` builds its
/// `Args` from, and the `RmArgs` it carries.
#[allow(clippy::too_many_arguments)] // one parameter per local the arm read inside `parse`
#[allow(clippy::type_complexity)] // the arm's `(spec, window)` pair plus the struct it builds
pub(super) fn parse(
    filter: &mut Filter,
    symbol: Option<String>,
    group: Option<String>,
    interval: Option<String>,
    produced_by: Option<String>,
    addr: &Option<String>,
    dry_run: bool,
    yes: bool,
) -> Result<(Option<String>, Option<Window>, Option<RmArgs>), String> {
    let rm;
    let (spec, window) = {
        // ⚠ `--kind`/`--venue` arrive in [`Filter`] because ONE flag loop parses every flag
        // (see [`parse`]'s doc). For `rm` they are not a substring filter at all — they are the
        // two required path segments above every leaf — so they are MOVED here, and the
        // `Filter` this `Args` carries is left empty for the subcommand that has no listing.
        let kind = filter.kind.take().ok_or(
            "rm needs --kind: it is the first dimension a cleanup selects on, and the store \
                 holds bar/quote/trade/book/depth and more under one venue",
        )?;
        let venue = filter.venue.take().ok_or(
            "rm needs --venue: with --kind it bounds the blast radius to a subtree an \
                 operator can name and see, rather than to `the store`",
        )?;
        refuse_an_account_kind(&kind)?;
        // The SHAPE rules that need no store. Everything else — an unknown kind, an interval
        // on a kind that does not sub-partition by one, a group on a kind that has no grouped
        // form — is refused on the far side against `STORE_KINDS`, because a roster copied
        // into this crate would be a second list to keep in step. That is `fetch`'s own rule
        // (see the module doc) applied unchanged.
        if symbol.is_some() && group.is_some() {
            return Err(
                "--symbol and --group are ALTERNATIVES, not a pair: a GROUPED series has an \
                     EMPTY symbol and a per-symbol series has no group. Pass one."
                    .to_string(),
            );
        }
        if group.is_some() && interval.is_some() {
            return Err("--interval does not apply to --group: a grouped series' leaf has no \
                     `interval=` segment at all"
                .to_string());
        }
        // ⚠ **The SELECTOR dimensions only** — `--produced-by` is deliberately NOT in this
        // list any more, and taking it out is a correction rather than a refactor. It rode
        // here because both flags are strings that must not be blank, and it inherited a
        // sentence written for a DIMENSION: *"An empty `symbol=` is the store's GROUPED-series
        // sentinel … omit the flag to wildcard the dimension instead."* Both halves are false
        // of a provenance assertion. It is not a dimension, and OMITTING it does not wildcard
        // anything — it turns the assertion OFF, which on a sweep is refused outright. So the
        // one guard standing between a blank prefix and the wire told the operator to do the
        // thing that is forbidden. [`refuse_a_blank_produced_by`] carries the real reason.
        for (flag, value) in [
            ("--kind", Some(&kind)),
            ("--venue", Some(&venue)),
            ("--symbol", symbol.as_ref()),
            ("--group", group.as_ref()),
            ("--interval", interval.as_ref()),
        ] {
            let Some(value) = value else { continue };
            if value.trim().is_empty() {
                return Err(format!(
                    "{flag} was given an EMPTY value. An empty `symbol=` is the store's \
                         GROUPED-series sentinel, so an empty selector names neither layout — omit \
                         the flag to wildcard the dimension instead"
                ));
            }
            // A glob is a SECOND matcher with its own escaping rules, and omitting a dimension
            // already covers the shape a cleanup needs. Refused at the door rather than
            // half-implemented.
            if let Some(c) = value.chars().find(|c| "*?[".contains(*c)) {
                return Err(format!(
                    "{flag} value {value:?} contains the glob character {c:?}. Globs are \
                         refused here: omitting a dimension already wildcards it"
                ));
            }
        }
        // The assertion's OWN rules, on BOTH routes, before anything is dialled or spawned.
        if let Some(p) = produced_by.as_deref() {
            refuse_a_blank_produced_by(p)?;
            if addr.is_some() {
                refuse_a_producer_path_on_the_remote_route(p)?;
            }
            if let Some(c) = p.chars().find(|c| "*?[".contains(*c)) {
                return Err(format!(
                    "--produced-by value {p:?} contains the glob character {c:?}. A prefix is \
                         matched with `starts_with`, never globbed: pass the literal prefix the \
                         rows carry"
                ));
            }
        }
        rm = Some(RmArgs { kind, venue, symbol, group, interval, produced_by, dry_run, yes });
        (None, None)
    };
    Ok((spec, window, rm))
}
