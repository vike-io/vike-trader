//! `data hist repair` — the arm of the per-subcommand `match` in `super::parse` that holds its
//! grammar.
//!
//! Moved here verbatim from that function (code-layout phase 2, task 11); see
//! `parse_fetch` for how the pieces fit.

use super::{Filter, RepairArgs, Window};

/// `repair`'s arm: MOVES `--kind`/`--venue` out of `filter` and builds the `RepairArgs`. Returns the
/// `(spec, window)` pair `parse` builds its `Args` from, and the `RepairArgs` it carries.
#[allow(clippy::type_complexity)] // the arm's `(spec, window)` pair plus the struct it builds
pub(super) fn parse(
    filter: &mut Filter,
    symbol: Option<String>,
    group: Option<String>,
    interval: Option<String>,
    dry_run: bool,
    yes: bool,
) -> Result<(Option<String>, Option<Window>, Option<RepairArgs>), String> {
    let repair;
    let (spec, window) = {
        // `--kind`/`--venue` arrive in [`Filter`] because ONE flag loop parses every flag.
        // For `repair`, as for `rm`, they are not a substring filter — they are the two path
        // segments above the series leaf — so they are MOVED out of it here.
        let kind = filter.kind.take().ok_or(
            "repair needs --kind: it is the first path segment above every series leaf, and \
                 the store holds bar/quote/trade/book/depth and more under one venue",
        )?;
        let venue = filter.venue.take().ok_or(
            "repair needs --venue: with --kind it names the subtree the series leaf sits under",
        )?;
        // The SHAPE rules that need no store — `rm`'s two, plus one that is this verb's alone.
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
        // ⚠ **THE DIFFERENCE FROM `rm`.** An omitted dimension WILDCARDS there and names
        // nothing here: `repair` rebuilds exactly one series. ⚠ `--interval`'s requirement on
        // `bar` is deliberately NOT checked here and cannot be — which kinds sub-partition by
        // interval is `vike_data::store_kind::STORE_KINDS`, the table this crate does not
        // link — so the ENGINE refuses that half through `SeriesSelector::is_sweep`, which is
        // the same shape-vs-roster split `rm` already draws.
        if symbol.is_none() && group.is_none() {
            return Err(
                "repair needs --symbol S or --group G: it rebuilds ONE series' index, never a \
                     wildcard set. A rebuild holds that series' lock across every part footer it \
                     reads, and its verdict — what came back and what did not — is per-series, so \
                     a sweep would fold N unbounded critical sections and N verdicts into one exit \
                     code. For several series, run this verb several times."
                    .to_string(),
            );
        }
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
                         GROUPED-series sentinel, so an empty selector names neither layout — and \
                         `repair` has no wildcard to fall back to"
                ));
            }
            if let Some(c) = value.chars().find(|c| "*?[".contains(*c)) {
                return Err(format!(
                    "{flag} value {value:?} contains the glob character {c:?}. `repair` names \
                         ONE series exactly; there is no matcher here for a glob to feed"
                ));
            }
        }
        repair = Some(RepairArgs { kind, venue, symbol, group, interval, dry_run, yes });
        (None, None)
    };
    Ok((spec, window, repair))
}
