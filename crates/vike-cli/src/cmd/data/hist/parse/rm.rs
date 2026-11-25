//! `data history rm` — the arm of the per-subcommand `match` in `super` that holds its
//! grammar.
//!
//! Moved here verbatim from that function (code-layout phase 2, task 11); see
//! `fetch` for how the pieces fit.

use vike_data::store::removal::{SeriesSelector, account_data_refusal, produced_by_refusal};

use super::super::{Filter, RmArgs, Window};

/// `rm`'s arm: MOVES `--kind`/`--venue` out of `filter` (they are the selector here, not a
/// substring filter) and builds the `RmArgs`. Returns the `(spec, window)` pair `parse` builds its
/// `Args` from, and the `RmArgs` it carries.
#[expect(clippy::type_complexity)] // the arm's `(spec, window)` pair plus the struct it builds
pub(in crate::cmd::data::hist) fn parse(
    filter: &mut Filter,
    symbol: Option<String>,
    group: Option<String>,
    interval: Option<String>,
    lane: Option<String>,
    produced_by: Option<String>,
    addr: &Option<String>,
    dry_run: bool,
    yes: bool,
) -> Result<(Option<String>, Option<Window>, Option<RmArgs>), String> {
    let rm;
    let (spec, window) = {
        // ⚠ `--kind`/`--venue` arrive in [`Filter`] because ONE flag set is read for every verb
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
        // The selector's rules that need no store, on BOTH routes and before anything is dialled or
        // spawned, through the ONE copy MCP's `delete_series` and the far side run too
        // (`vike_data::store::removal`): the kind by `StoreKindId`'s own `FromStr`, the shape, account
        // data. What needs the layout table's rows (an interval on a kind that does not sub-partition
        // by one, a group on a kind with no grouped form) is the far side's.
        let selector = SeriesSelector {
            kind: SeriesSelector::parse_kind(Some(kind.as_str()))?,
            venue,
            symbol,
            group,
            interval,
            source: lane,
        };
        selector.validate_shape().map_err(|refusal| refusal.rendered())?;
        if let Some(refusal) = account_data_refusal(selector.kind) {
            return Err(refusal.rendered());
        }
        // The assertion's OWN rules, on BOTH routes, before anything is dialled or spawned: a glob,
        // a producer path under `--addr` (`produced_by_refusal`'s compatibility guard), then the
        // engine's and the datahub's own resolver, so this side refuses a blank prefix (one that
        // matches every key and passes the provenance check vacuously) and a producer path
        // `STORE_KINDS` does not declare in the words they use. The spelling itself is forwarded
        // verbatim; the far side resolves it again.
        if let Some(p) = produced_by.as_deref() {
            if let Some(refusal) = produced_by_refusal(p, addr.is_some()) {
                return Err(refusal.rendered());
            }
            vike_data::store::store_kind::resolve_produced_by(p)?;
        }
        rm = Some(RmArgs { selector, produced_by, dry_run, yes });
        (None, None)
    };
    Ok((spec, window, rm))
}
