//! `RestoreRow` — the per-venue declaration of whether, after a restart, the orders a PREVIOUS
//! session left resting on the venue may be put BACK into the engine registry at the first
//! successful reconcile pass.
//!
//! # What the feature does, and why it is a table
//!
//! A restart loses the engine registry. Orders the old session left resting are still on the venue
//! and nothing in the new process knows them. The restore adopts the ones the venue still reports
//! and delivers a synthetic cancel for the ones the venue no longer reports. That is a WRITE into
//! live order state (the root `CLAUDE.md`'s rollout rule: a position-shaped write is only for a
//! venue whose report is live-proven complete), so it is enabled PER VENUE from this table and a
//! venue that is not NAMED here as [`RestoreState::Enabled`] never restores. The same shape as
//! `crate::venues::venue_tif` and `crate::venues::venue_amend`: declare today's reality first,
//! flip one row per change behind a demo smoke (the playbook in `crates/vike-model/CLAUDE.md`).
//!
//! # The two conditions a row needs before it may say `Enabled`
//!
//! 1. **The venue's open-orders report carries OUR client order id.** Adoption maps a reported
//!    order back to the engine's own record by coid; a report that hashes the id, mints its own
//!    reference or has no id cannot be mapped, and an order adopted under a wrong key is worse than
//!    one left alone. That condition is `crate::venues::venue_coid_budget::CoidWire::echoes_coid`,
//!    and the cross-pin `an_enabled_venue_echoes_the_coid` EXECUTES it against this table so the
//!    two cannot drift. ONE named exception: a venue whose report carries a wire id that is a pure
//!    function of the coid (Hyperliquid's `keccak` cloid) and whose client answers
//!    `ExecutionClient::wire_id_for` is listed in [`WIRE_ID_RESTORE_VENUES`]; the core translates
//!    its reports back to the coid before planning (decision 0121), and a test pins the list.
//! 2. **A demo smoke proved the round trip** (place, restart, adopt, cancel). Nothing in the tree
//!    proves that for a venue until its row moves; the first four planned rows therefore started in
//!    [`RestoreState::AwaitingSmoke`], and flipping one is a one-line edit (`git grep
//!    awaiting_smoke` lists them).
//!
//! Neither condition is sufficient alone, and several venues pass the first and still stay off:
//! oanda, ibkr and aster echo the coid but are not live-proven (aster's passes are authenticated
//! MAINNET reads), so their rows say [`RestoreState::Never`].
//!
//! # Contradictions are PINNED, not fixed
//!
//! `crate::venues::venue_coid_budget::coid_budget_for` says `NotOnTheWire` for ctrader and alpaca,
//! yet both recon clients' ORDER parsers read a coid (see their rows below). That table is a
//! separate decision with its own consumers (origin recognition); this table does not paper over
//! it, it just declines to enable either venue, which is the safe direction either way.
//! `a_venue_whose_coid_row_is_not_on_the_wire_is_never_enabled` pins the consequence.
//!
//! An unrecognised venue string is never restored (fail-CLOSED). Every ROSTER venue is a NAMED arm
//! — `every_roster_venue_has_a_restore_row` enforces it.

/// Where a venue stands on restoring a previous session's resting orders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreState {
    /// The restore runs on this venue's first successful reconcile pass.
    Enabled,
    /// Planned for the first set, but its demo smoke has not proven the round trip yet. Behaves as
    /// off; the row is flipped to [`RestoreState::Enabled`] by the change that carries the smoke.
    AwaitingSmoke,
    /// Not enabled and not planned: the reason on the row says why (a report that cannot be mapped
    /// back to our coid, no order query, or a venue the rollout rule keeps off).
    Never,
}

/// One venue's declaration: the state and the adapter code it was read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestoreRow {
    /// Whether the restore may run.
    pub state: RestoreState,
    /// Why, citing the adapter code (a `path`'s `symbol`, never a line number).
    pub reason: &'static str,
}

impl RestoreRow {
    /// A venue whose restore runs. Only a row whose smoke has landed may use this.
    #[must_use]
    pub const fn enabled(reason: &'static str) -> Self {
        Self { state: RestoreState::Enabled, reason }
    }

    /// A planned venue still waiting for its demo smoke. `git grep awaiting_smoke` finds the rows
    /// to flip.
    #[must_use]
    pub const fn awaiting_smoke(reason: &'static str) -> Self {
        Self { state: RestoreState::AwaitingSmoke, reason }
    }

    /// A venue that is not enabled and is not planned.
    #[must_use]
    pub const fn never(reason: &'static str) -> Self {
        Self { state: RestoreState::Never, reason }
    }

    /// True only for [`RestoreState::Enabled`].
    #[must_use]
    pub const fn restores(&self) -> bool {
        matches!(self.state, RestoreState::Enabled)
    }
}

/// The venues whose open-orders report does NOT echo our coid (`coid_budget_for(v).echoes_coid()`
/// is false) and which may still be planned or enabled, because their wire id is a pure function of
/// the coid and their client names it through `ExecutionClient::wire_id_for`: the restore rewrites
/// the pass's reports from that id back to the coid before planning (decision 0121,
/// `crates/vike-core/src/runtime/restore.rs`'s module doc). A venue joins this list only with that
/// override in its bridge AND a smoke that proves the round trip; the echo cross-pins below skip
/// exactly these names and pin the list.
pub const WIRE_ID_RESTORE_VENUES: &[&str] = &["hyperliquid"];

/// The per-venue registry. An unknown venue is [`RestoreState::Never`] — fail-CLOSED, because the
/// restore is a write and nobody has measured that venue's report. Every ROSTER venue is a NAMED
/// arm, even where its value equals the fallback.
#[must_use]
pub fn restore_row_for(venue: &str) -> RestoreRow {
    match venue {
        // `fetch_order_status_reports` reads `openOrders` for the mount's symbol and
        // `crates/bridges/binance/src/family/recon.rs`'s `parse_perp_open_orders` /
        // `parse_spot_open_orders` map `clientOrderId` straight to `client_order_id` (an empty
        // string becomes `None`, an externally placed order). The coid is echoed.
        "binance" => RestoreRow::enabled(concat!(
            "demo smoke green 2026-10-10 (crates/bridges/binance/tests/binance_restore_smoke.rs's ",
            "binance_restore_adopts_a_resting_order_and_the_engine_cancels_it, spot lane: placed -> ",
            "adopted -> re-registered -> engine cancel -> gone at venue -> folded Canceled; and ",
            "the perp lane, crates/bridges/binance/tests/binance_perp_restore_smoke.rs, green the ",
            "same day, broker-prefixed coid stripped on the report); ",
            "openOrders echoes clientOrderId ",
            "(crates/bridges/binance/src/family/recon.rs's parse_perp_open_orders)"
        )),
        // `/v5/order/realtime` rows carry `orderLinkId`
        // (`crates/bridges/bybit/src/recon_client.rs`'s `parse_orders`). ⚠ That endpoint reports
        // open AND recently closed orders (its `fetch_order_status_reports` doc says so), so the
        // restore must filter on status before adopting.
        "bybit" => RestoreRow::awaiting_smoke(concat!(
            "not yet enabled: its demo smoke could not run on 2026-10-10 (the demo api key had ",
            "expired, code=33004; only the owner can renew it); order/realtime echoes orderLinkId ",
            "(crates/bridges/bybit/src/recon_client.rs's parse_orders) and also lists recently ",
            "closed orders"
        )),
        // `orders-pending` rows carry `clOrdId`
        // (`crates/bridges/okx/src/recon_client.rs`'s `parse_orders_pending`, `client_order_id`).
        "okx" => RestoreRow::enabled(concat!(
            "demo smoke green 2026-10-10 (crates/bridges/okx/tests/okx_restore_smoke.rs's ",
            "okx_restore_adopts_a_resting_order_and_the_engine_cancels_it: placed -> adopted -> ",
            "re-registered -> engine cancel -> gone at venue -> folded Canceled); orders-pending ",
            "echoes clOrdId (crates/bridges/okx/src/recon_client.rs's parse_orders_pending)"
        )),
        // `get_open_orders_by_instrument` rows carry `label`, which IS our coid; an empty label is
        // `client_order_id: None` rather than a dropped row
        // (`crates/bridges/deribit/src/recon_client.rs`'s `parse_open_orders`).
        "deribit" => RestoreRow::enabled(concat!(
            "testnet smoke green 2026-10-10 (crates/bridges/deribit/tests/deribit_restore_smoke.rs's ",
            "deribit_restore_adopts_a_resting_order_and_the_engine_cancels_it: placed -> adopted -> ",
            "re-registered -> engine cancel -> gone at venue -> folded Canceled, the cancel finding ",
            "the order by label, crates/bridges/deribit/src/client.rs's cancel_order); open orders ",
            "echo label as the coid (crates/bridges/deribit/src/recon_client.rs's parse_open_orders)"
        )),
        // The coid IS echoed (`clientExtensions.id`, `parse_order_reports`), but oanda is one of
        // the venues the root `CLAUDE.md` keeps out of position-shaped writes until its report is
        // live-proven complete.
        "oanda" => RestoreRow::never(concat!(
            "echoes the coid (crates/bridges/oanda/src/recon_client.rs's parse_order_reports) but ",
            "is not live-proven; adoption is a write (root CLAUDE.md rollout rule)"
        )),
        // IG mints its own `dealReference`; its working orders report `client_order_id: None`.
        "ig" => RestoreRow::never(concat!(
            "working orders carry no coid ",
            "(crates/bridges/ig/src/recon_client.rs's parse_working_orders)"
        )),
        // The Orders table reports `client_order_id: None`.
        "fxcm" => RestoreRow::never(concat!(
            "the Orders table carries no coid ",
            "(crates/bridges/fxcm/src/recon_client.rs's parse_orders) and fxcm is not live-proven"
        )),
        // The stdio protocol has no order query: `fetch_order_status_reports` returns an empty
        // list by design, so "absent from the report" would cancel everything.
        "dukascopy" => RestoreRow::never(concat!(
            "no order query: the report is empty by design ",
            "(crates/bridges/dukascopy/src/recon_client.rs's DukascopyReconClient)"
        )),
        // A post-restart report maps an order to a coid only through the in-process registry
        // lookup, which a restart empties, so it reports `client_order_id: None`.
        "polymarket" => RestoreRow::never(concat!(
            "a post-restart report cannot be mapped back to our coid ",
            "(crates/bridges/polymarket/src/exec_plane/recon_client.rs's parse_order_reports)"
        )),
        // The coid IS echoed (`orderRef`), but ibkr is not live-proven complete (root `CLAUDE.md`
        // rollout rule) and adoption is a write.
        "ibkr" => RestoreRow::never(concat!(
            "echoes orderRef (crates/bridges/vike-ibkr/src/recon_client.rs's parse_order_reports) ",
            "but is not live-proven; adoption is a write (root CLAUDE.md rollout rule)"
        )),
        // The ORDER parser reads the coid (`clientOrderId`, falling back to `tradeData.label`) even
        // though `coid_budget_for("ctrader")` says `NotOnTheWire` — a pinned contradiction (module
        // doc). Kept off: not live-proven, and adoption is a write.
        "ctrader" => RestoreRow::never(concat!(
            "reads a coid on orders (crates/bridges/ctrader/src/recon_client.rs's parse_orders) ",
            "but is not live-proven; adoption is a write (root CLAUDE.md rollout rule)"
        )),
        // Same contradiction as ctrader: `parse_orders` reads `client_order_id` while
        // `coid_budget_for("alpaca")` says `NotOnTheWire`. Kept off for the same reason.
        "alpaca" => RestoreRow::never(concat!(
            "reads a coid on orders (crates/bridges/alpaca/src/recon_client.rs's parse_orders) ",
            "but is not live-proven; adoption is a write (root CLAUDE.md rollout rule)"
        )),
        // Aster is a Binance fork whose report echoes the coid, but every aster pass is an
        // authenticated MAINNET read (`crates/bridges/aster/CLAUDE.md`), so it never joins a
        // demo-proven set.
        "aster" => RestoreRow::never(concat!(
            "its reconcile passes are authenticated MAINNET reads ",
            "(crates/bridges/aster/src/recon_client.rs's AsterReconClient)"
        )),
        // The report echoes the cloid HASH, not our id
        // (`crates/bridges/hyperliquid/src/recon_client.rs`'s `parse_orders`), but the hash is a
        // pure function of the coid: the core asks the client for it
        // (`ExecutionClient::wire_id_for`, decision 0121) and rewrites the pass's reports before
        // planning. The one NAMED exception to the echo rule: `WIRE_ID_RESTORE_VENUES`.
        "hyperliquid" => RestoreRow::enabled(concat!(
            "testnet smoke green 2026-10-10 (crates/bridges/hyperliquid/tests/",
            "hyperliquid_restore_smoke.rs: placed -> 0x cloid reported -> translated -> adopted -> ",
            "re-registered -> engine cancel -> gone at venue -> folded Canceled); the report echoes ",
            "the keccak cloid, not our coid (crates/bridges/hyperliquid/src/recon_client.rs's ",
            "parse_orders), which the restore translates through ExecutionClient::wire_id_for ",
            "(crates/bridges/hyperliquid/src/exec.rs's HyperliquidExecutionClient)"
        )),
        // vike:new-venue:row // TODO(new-venue: {venue}): a NAMED row; Enabled only with a demo smoke and a coid echo
        // vike:new-venue:row "{venue}" => RestoreRow::never("scaffolded: nothing declared yet"),
        _ => RestoreRow::never("not a roster venue: nothing declares its open-orders report"),
    }
}

/// Does the restore run for `venue`? `false` for every venue not NAMED
/// [`RestoreState::Enabled`] and for any unknown string.
#[must_use]
pub fn restores_orders(venue: &str) -> bool {
    restore_row_for(venue).restores()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::venues::VENUES;
    use crate::venues::venue_coid_budget::{CoidWire, coid_budget_for};

    /// The first set's venues still waiting on a green demo smoke (the row says `awaiting_smoke`).
    const PLANNED: &[&str] = &["bybit"];

    /// The venues whose restore smoke is green: the row says `enabled` and the restore runs.
    const ENABLED: &[&str] = &["binance", "okx", "deribit", "hyperliquid"];

    /// The venues that stay off, with the reason class on their row.
    #[rustfmt::skip]
    const NEVER: &[&str] = &[
        "oanda", "ig", "fxcm", "dukascopy", "polymarket", "ibkr", "ctrader", "alpaca", "aster",
        // vike:new-venue:row "{venue}", // TODO(new-venue: {venue}): off until a demo smoke proves the round trip
    ];

    /// Completeness vs [`VENUES`]: every roster venue is in exactly one of the two lists and its
    /// row exists and says why; the lists name no extra venue.
    #[test]
    fn every_roster_venue_has_a_restore_row() {
        assert_eq!(
            PLANNED.len() + ENABLED.len() + NEVER.len(),
            VENUES.len(),
            "every roster venue classified exactly once, no extras"
        );
        for &v in VENUES {
            let lists = [PLANNED, ENABLED, NEVER].iter().filter(|l| l.contains(&v)).count();
            assert_eq!(lists, 1, "{v} must be classified exactly once");
            assert!(!restore_row_for(v).reason.is_empty(), "{v}: a row without a reason");
        }
        for &v in PLANNED.iter().chain(ENABLED).chain(NEVER) {
            assert!(VENUES.contains(&v), "{v} is not a roster venue");
        }
        let nope = restore_row_for("nope");
        assert_eq!(nope.state, RestoreState::Never);
        assert!(!restores_orders("nope"), "an unknown venue must fail CLOSED");
    }

    /// The whole table, verbatim: venue, state, and whether the restore runs. A venue is `Enabled`
    /// only once its restore smoke is green (binance, okx, deribit so far); bybit awaits its smoke.
    #[test]
    fn the_restore_matrix_is_pinned_verbatim() {
        use RestoreState::{AwaitingSmoke, Enabled, Never};
        #[rustfmt::skip]
        const MATRIX: &[(&str, RestoreState, bool)] = &[
            ("binance",     Enabled,       true),
            ("bybit",       AwaitingSmoke, false),
            ("okx",         Enabled,       true),
            ("deribit",     Enabled,       true),
            ("oanda",       Never,         false),
            ("ig",          Never,         false),
            ("fxcm",        Never,         false),
            ("dukascopy",   Never,         false),
            ("polymarket",  Never,         false),
            ("ibkr",        Never,         false),
            ("ctrader",     Never,         false),
            ("alpaca",      Never,         false),
            ("aster",       Never,         false),
            ("hyperliquid", Enabled,       true),
            // vike:new-venue:row ("{venue}", Never, false), // TODO(new-venue: {venue}): move with the row above
        ];
        assert_eq!(MATRIX.len(), VENUES.len(), "the pin names every roster venue once");
        for &(v, state, runs) in MATRIX {
            assert_eq!(restore_row_for(v).state, state, "{v}");
            assert_eq!(restores_orders(v), runs, "{v}");
        }
    }

    /// The planned venues are exactly the `AwaitingSmoke` rows, the enabled ones are exactly the
    /// `Enabled` rows, and the never-set carries neither state, so a flip cannot be made for the
    /// wrong venue by accident.
    #[test]
    fn only_the_planned_venues_await_a_smoke() {
        for &v in VENUES {
            let awaiting = restore_row_for(v).state == RestoreState::AwaitingSmoke;
            assert_eq!(awaiting, PLANNED.contains(&v), "{v}");
        }
        for &v in ENABLED {
            assert_eq!(restore_row_for(v).state, RestoreState::Enabled, "{v}");
            assert!(restores_orders(v), "{v} is in the enabled set");
        }
        for &v in NEVER {
            assert_eq!(restore_row_for(v).state, RestoreState::Never, "{v}");
            assert!(!restores_orders(v), "{v} is in the never-set");
        }
    }

    /// The venues a restore table would break the coid rule for, given any `enabled` predicate: a
    /// venue that restores must have a coid row that ECHOES the id back, unless it is a named
    /// [`WIRE_ID_RESTORE_VENUES`] exception. Takes the predicate so the check can be run against the
    /// real table AND against a table with everything switched on.
    fn echo_rule_violations(enabled: impl Fn(&str) -> bool) -> Vec<&'static str> {
        VENUES
            .iter()
            .copied()
            .filter(|&v| {
                enabled(v)
                    && !coid_budget_for(v).echoes_coid()
                    && !WIRE_ID_RESTORE_VENUES.contains(&v)
            })
            .collect()
    }

    /// THE cross-pin: every venue that restores echoes the coid per `coid_budget_for`, which is
    /// real code. A venue enabled here without the echo goes red here, not on a live order.
    #[test]
    fn an_enabled_venue_echoes_the_coid() {
        // Not vacuous: the enabled set is non-empty, so the loop below EXECUTES the echo rule.
        assert!(
            VENUES.iter().any(|&v| restores_orders(v)),
            "no venue restores: the pin is vacuous"
        );
        assert_eq!(echo_rule_violations(restores_orders), Vec::<&str>::new());
        for &v in VENUES {
            if restores_orders(v) && !WIRE_ID_RESTORE_VENUES.contains(&v) {
                assert!(
                    coid_budget_for(v).echoes_coid(),
                    "{v} restores but its coid is not echoed"
                );
            }
        }
    }

    /// The exception list is PINNED, not open: exactly the venues whose client answers
    /// `ExecutionClient::wire_id_for`, each really a venue the echo rule would otherwise refuse (an
    /// exception for an echoing venue is vacuous and hides a future change of its coid row) and each
    /// a roster venue that is planned or enabled. Growing it is a decision, and the bridge's own
    /// test (`crates/bridges/hyperliquid/src/exec_tests.rs`'s
    /// `wire_id_for_is_the_cloid_a_submit_puts_on_the_wire`) is where the override is proven.
    #[test]
    fn the_wire_id_exception_list_is_pinned() {
        assert_eq!(WIRE_ID_RESTORE_VENUES, &["hyperliquid"]);
        for &v in WIRE_ID_RESTORE_VENUES {
            assert!(VENUES.contains(&v), "{v} is not a roster venue");
            assert!(
                !coid_budget_for(v).echoes_coid(),
                "{v} echoes the coid, so it needs no wire-id exception"
            );
            assert!(
                PLANNED.contains(&v) || ENABLED.contains(&v),
                "{v} is excepted from the echo rule but is neither planned nor enabled"
            );
        }
    }

    /// The pin above checks only the rows as shipped, so prove the check can fail: with EVERY
    /// venue switched on, the violations are exactly the venues whose coid row is not echoed and
    /// that are not a named wire-id exception.
    #[test]
    fn the_echo_check_bites_when_a_venue_without_an_echo_is_switched_on() {
        let mut expected: Vec<&str> = VENUES
            .iter()
            .copied()
            .filter(|&v| !coid_budget_for(v).echoes_coid() && !WIRE_ID_RESTORE_VENUES.contains(&v))
            .collect();
        let mut got = echo_rule_violations(|_| true);
        expected.sort_unstable();
        got.sort_unstable();
        assert_eq!(got, expected);
        assert!(!got.is_empty(), "the roster has venues that do not echo the coid");
        assert!(got.contains(&"polymarket"), "polymarket reports no coid and has no wire id");
        assert!(
            !got.contains(&"hyperliquid"),
            "HL echoes a hash, but its wire id is derivable: the one named exception"
        );
    }

    /// The planned and enabled venues are only restorable because their coid rows echo (or, for the
    /// named exception, because the client names the wire id); if one of those rows moves, the plan
    /// is stale and this says so before the flip does.
    #[test]
    fn the_planned_venues_all_echo_the_coid() {
        for &v in PLANNED.iter().chain(ENABLED) {
            assert!(
                coid_budget_for(v).echoes_coid() || WIRE_ID_RESTORE_VENUES.contains(&v),
                "{v} is planned for the restore but its coid is not echoed"
            );
        }
    }

    /// A venue whose coid row is `NotOnTheWire` is never allowed, whatever its row says: the
    /// restore maps by coid and such a venue's reports cannot carry ours. (ctrader and alpaca are
    /// the two rows whose recon parsers read a coid despite this classification — pinned, module
    /// doc — and both are off.)
    #[test]
    fn a_venue_whose_coid_row_is_not_on_the_wire_is_never_enabled() {
        for &v in VENUES {
            if coid_budget_for(v) == CoidWire::NotOnTheWire && !WIRE_ID_RESTORE_VENUES.contains(&v)
            {
                assert!(!restores_orders(v), "{v}: NotOnTheWire cannot be restored by coid");
                assert!(
                    !PLANNED.contains(&v) && !ENABLED.contains(&v),
                    "{v}: NotOnTheWire cannot be a planned or enabled venue"
                );
            }
        }
    }
}
