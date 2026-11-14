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
//! `crate::venue_time_in_force` and `crate::venue_amend`: declare today's reality first,
//! flip one row per change behind a demo smoke (the playbook in `crates/vike-venues/CLAUDE.md`).
//!
//! # The two conditions a row needs before it may say `Enabled`
//!
//! 1. **The venue's open-orders report carries OUR order client id.** Adoption maps a reported
//!    order back to the engine's own record by order client id; a report that hashes the id, mints its own
//!    reference or has no id cannot be mapped, and an order adopted under a wrong key is worse than
//!    one left alone. That condition is `crate::venue_order_client_id_budget::OrderClientIdCarriage::echoes_order_client_id`,
//!    and the cross-pin `an_enabled_venue_echoes_the_order_client_id` EXECUTES it against this table so the
//!    two cannot drift. ONE named exception: a venue whose report carries a wire id that is a pure
//!    function of the order client id (Hyperliquid's `keccak` cloid) and whose client answers
//!    `ExecutionClient::wire_id_for` is listed in [`WIRE_ID_RESTORE_VENUES`]; the core translates
//!    its reports back to the order client id before planning (decision 0121), and a test pins the list.
//! 2. **A demo smoke proved the round trip** (place, restart, adopt, cancel). Nothing in the tree
//!    proves that for a venue until its row moves; a planned row therefore starts in
//!    [`RestoreState::AwaitingSmoke`], and flipping one is one row plus this file's pins (`git grep
//!    awaiting_smoke` lists them).
//!
//! Neither condition is sufficient alone, and a venue can pass the first and still stay off: aster
//! echoes the order client id but its passes are authenticated MAINNET reads, so its row says
//! [`RestoreState::Never`].
//!
//! # An echo is necessary, never sufficient
//!
//! `crate::venue_order_client_id_budget::order_client_id_budget_for` said `NotOnTheWire` for ctrader and alpaca while
//! both recon clients' ORDER parsers read an order client id; since order client id v2 their rows say what the venues
//! document (the id is echoed), and this table did not move: both stay `never`, each for its own
//! reason on its row, as aster does. `an_echoing_venue_stays_off_for_its_own_reason` pins that, and
//! with it `crates/vike-core/src/runtime/refusals.rs`'s `row_symbol_is_taught`, which needs both
//! terms and so still answers `false` for the three.
//!
//! An unrecognised venue string is never restored (fail-CLOSED). Every ROSTER venue is a NAMED arm
//! — `every_roster_venue_has_a_restore_row` enforces it.
//!
//! # The window that guards a restored tag, and the bound it puts on the first pass
//!
//! The table says WHICH venues restore; two constants after it say how long a restored order may
//! mute the strategy that owned it. [`RESTORED_TAG_DENY_WINDOW_MS`] is the core's refusal window
//! and [`MAX_RECONCILE_STARTUP_DELAY_MS`] is the longest first-pass delay the settings loader
//! accepts. They live here, beside the table that decides whether a restored order exists at all,
//! because `vike-config` (the loader that enforces the bound) and `vike-core` (the fold that
//! applies the window) must agree on them, and both already depend on this crate.

/// Where a venue stands on restoring a previous session's resting orders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreState {
    /// The restore runs on this venue's first successful reconcile pass.
    Enabled,
    /// Planned for the first set, but its demo smoke has not proven the round trip yet. Behaves as
    /// off; the row is flipped to [`RestoreState::Enabled`] by the change that carries the smoke.
    AwaitingSmoke,
    /// Not enabled and not planned: the reason on the row says why (a report that cannot be mapped
    /// back to our order client id, no order query, or a venue the rollout rule keeps off).
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

/// The venues whose open-orders report does NOT echo our order client id (`order_client_id_budget_for(v).echoes_order_client_id()`
/// is false) and which may still be planned or enabled, because their wire id is a pure function of
/// the order client id and their client names it through `ExecutionClient::wire_id_for`: the restore rewrites
/// the pass's reports from that id back to the order client id before planning (decision 0121,
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
        // `parse_spot_open_orders` map `clientOrderId` straight to `order_client_id` (an empty
        // string becomes `None`, an externally placed order). The order client id is echoed.
        "binance" => RestoreRow::enabled(concat!(
            "demo smoke green 2026-10-10 (crates/bridges/binance/tests/binance_smokes/binance_restore_smoke.rs's ",
            "binance_restore_adopts_a_resting_order_and_the_engine_cancels_it, spot lane: placed -> ",
            "adopted -> re-registered -> engine cancel -> gone at venue -> folded Canceled; and ",
            "the perp lane, crates/bridges/binance/tests/binance_smokes/binance_perp_restore_smoke.rs, green the ",
            "same day, broker-prefixed order client id stripped on the report); ",
            "openOrders echoes clientOrderId ",
            "(crates/bridges/binance/src/family/recon.rs's parse_perp_open_orders)"
        )),
        // `/v5/order/realtime` rows carry `orderLinkId`
        // (`crates/bridges/bybit/src/reconcile/mod.rs`'s `parse_orders`). ⚠ That endpoint reports
        // open AND recently closed orders (its `fetch_order_status_reports` doc says so), so the
        // restore must filter on status before adopting.
        "bybit" => RestoreRow::enabled(concat!(
            "demo smoke green 2026-10-24, twice (crates/bridges/bybit/tests/bybit_smokes/bybit_restore_smoke.rs's ",
            "bybit_restore_adopts_a_resting_order_and_the_engine_cancels_it on bybit#DEMO: placed -> ",
            "adopted -> re-registered -> engine cancel -> closed row not adopted -> folded Canceled); ",
            "order/realtime echoes orderLinkId (crates/bridges/bybit/src/reconcile/mod.rs's ",
            "parse_orders) and also lists recently closed orders, which the planner never adopts"
        )),
        // `orders-pending` rows carry `clOrdId`
        // (`crates/bridges/okx/src/reconcile/mod.rs`'s `parse_orders_pending`, `order_client_id`).
        "okx" => RestoreRow::enabled(concat!(
            "demo smoke green 2026-10-10 (crates/bridges/okx/tests/okx_smokes/okx_restore_smoke.rs's ",
            "okx_restore_adopts_a_resting_order_and_the_engine_cancels_it: placed -> adopted -> ",
            "re-registered -> engine cancel -> gone at venue -> folded Canceled); orders-pending ",
            "echoes clOrdId (crates/bridges/okx/src/reconcile/mod.rs's parse_orders_pending)"
        )),
        // `get_open_orders_by_instrument` rows carry `label`, which IS our order client id; an empty label is
        // `order_client_id: None` rather than a dropped row
        // (`crates/bridges/deribit/src/reconcile/mod.rs`'s `parse_open_orders`).
        "deribit" => RestoreRow::enabled(concat!(
            "testnet smoke green 2026-10-10 (crates/bridges/deribit/tests/deribit_smokes/deribit_restore_smoke.rs's ",
            "deribit_restore_adopts_a_resting_order_and_the_engine_cancels_it: placed -> adopted -> ",
            "re-registered -> engine cancel -> gone at venue -> folded Canceled, the cancel finding ",
            "the order by label, crates/bridges/deribit/src/execution/rest.rs's cancel_order); open orders ",
            "echo label as the order client id (crates/bridges/deribit/src/reconcile/mod.rs's parse_open_orders)"
        )),
        // The report carries the order client id (`clientExtensions.id`), venue id, canonical symbol and units, one
        // read per instrument the client was told; a cancel goes to `@{order_client_id}`, so no id map has to survive
        // the restart; `adopt_restored` teaches the fill's symbol.
        "oanda" => RestoreRow::enabled(concat!(
            "practice smoke green 2026-11-06 (crates/bridges/oanda/tests/oanda_smokes/oanda_restore_smoke.rs's ",
            "oanda_restore_adopts_a_resting_order_and_the_engine_cancels_it: placed -> adopted -> ",
            "re-registered -> engine cancel -> gone at venue -> folded Canceled); orders echo ",
            "clientExtensions.id (crates/bridges/oanda/src/reconcile/mod.rs's parse_order_reports) and ",
            "a cancel goes by it (crates/bridges/oanda/src/execution/mod.rs's run_on)"
        )),
        // IG mints its own `dealReference`; its working orders report `order_client_id: None` after a
        // restart. Our order-client-id-derived reference is not on the row (measured: `GET /workingorders` Version 2
        // rows carry none, `crates/bridges/ig/CLAUDE.md`), and a cancel needs the `dealId` a restart
        // empties (a gatewayhub restart alone is re-taught it: `crates/bridges/ig/src/execution/mod.rs`'s
        // `teach_deal_ids`). The core's pass now joins a row with no order client id to its restored order by the
        // `dealId` the ownership line learned (`crates/vike-execution/src/recon/restore.rs`'s
        // `name_by_venue_order_id`) and teaches the bridge that id, so what keeps the row off is a demo smoke of
        // the round trip (placed, both daemons restarted, adopted, engine cancel, gone at the venue).
        "ig" => RestoreRow::never(concat!(
            "working orders carry no order client id ",
            "(crates/bridges/ig/src/reconcile/mod.rs's parse_working_orders)"
        )),
        // The Orders table reports `order_client_id: None`, the shim stamps a constant `CustomID`
        // (`crates/bridges/fxcm/src/shim/fcshim.cpp`'s `RustShimEntry`), and a cancel needs the order,
        // account and offer ids, which the Orders table does not carry. A gatewayhub restart alone is re-taught
        // by the venue order id the tradehub's core holds (`crates/bridges/fxcm/src/execution/mod.rs`'s `bind_restored`),
        // which rebinds the listing and the fill routes, and the bridge rebuilds the cancel's ids from the
        // venue (`crates/bridges/fxcm/src/execution/mod.rs`'s `adopt_taught`: an exit's from the exits listing, an
        // entry's from the account and the Offers row of its symbol); a restart of both daemons would be joined
        // by the same id once this row opens (`crates/vike-execution/src/recon/restore.rs`'s
        // `name_by_venue_order_id`). What keeps it off: a demo run of the round trip on an account that is
        // inactive.
        "fxcm" => RestoreRow::never(concat!(
            "the Orders table carries no order client id: the listing names ours only for the ids the live process routes or ",
            "a gatewayhub restart's re-teaching rebinds by venue order id, none after a restart of both daemons ",
            "(crates/bridges/fxcm/src/reconcile/mod.rs's parse_orders), and fxcm is not live-proven"
        )),
        // The sidecar's `recon` answer DOES name a resting order by the order client id in its label and a new
        // sidecar takes it up (decisions 0235 and 0212, both still `proposed`), so a plain limit could
        // come back. What keeps the row off: a node's take-profit is a `limit` the venue stores as a price
        // with no order client id, a restarted gatewayhub's client has forgotten it (`ReconcileSource::protections`), and
        // the planner would call it gone after its quiet passes while it rests. It would take the owner's
        // word on 0235, a measured `engine.getOrders()` and the protections re-taught across that restart.
        "dukascopy" => RestoreRow::never(concat!(
            "a restored take-profit is absent from the report after a gatewayhub restart and would be ",
            "declared gone while it rests (crates/bridges/dukascopy/src/reconcile/mod.rs's order_reports)"
        )),
        // A post-restart report maps an order to an order client id only through the in-process registry
        // lookup, which a restart empties, so it reports `order_client_id: None`.
        "polymarket" => RestoreRow::never(concat!(
            "a post-restart report cannot be mapped back to our order client id ",
            "(crates/bridges/polymarket/src/reconcile/mod.rs's parse_order_reports)"
        )),
        // The socket report carries `orderRef`, the order id, the symbol the read pins and the filled
        // quantity; the exec connection learns its own open orders by the same `orderRef`, so a cancel
        // finds the id; what the host taught outlives the daily rebuild
        // (`crates/bridges/vike-ibkr/src/execution/mod.rs`'s `KEPT_TAUGHT`).
        "ibkr" => RestoreRow::enabled(concat!(
            "paper smoke green 2026-11-06 (crates/bridges/vike-ibkr/tests/ibkr_smokes/ibkr_restore_smoke.rs's ",
            "ibkr_restore_adopts_a_resting_order_and_the_engine_cancels_it: placed -> adopted -> ",
            "re-registered -> rebuilt -> engine cancel -> gone at venue -> folded Canceled); orders ",
            "echo orderRef (crates/bridges/vike-ibkr/src/reconcile/socket.rs's parse_socket_order_reports)"
        )),
        // The ORDER parser reads the order client id (`clientOrderId`, falling back to `tradeData.label`), which
        // `order_client_id_budget_for("ctrader")` records as echoed, and the connect seed teaches a cancel its
        // `orderId`, so a plain limit could come back.
        // Kept off: a reduce-only limit that spans hedged positions rests as legs `<order client id>L<n>` whose table
        // lives in the process, so after a restart no report names `<order client id>` and the planner calls it gone
        // while its legs rest. It would take the legs named across the restart and a smoke on an
        // account that takes orders (2026-11-06: "Trading account is not authorized").
        "ctrader" => RestoreRow::never(concat!(
            "reads an order client id on orders (crates/bridges/ctrader/src/reconcile/mod.rs's parse_orders) but ",
            "an exit's legs do not survive a restart (crates/bridges/ctrader/src/execution/exit_legs.rs)"
        )),
        // `parse_orders` reads Alpaca's `client_order_id`, which `order_client_id_budget_for("alpaca")` records as
        // echoed. Kept off: not live-proven, and adoption is a write.
        "alpaca" => RestoreRow::never(concat!(
            "reads an order client id on orders (crates/bridges/alpaca/src/reconcile/mod.rs's parse_orders) ",
            "but is not live-proven; adoption is a write (root CLAUDE.md rollout rule)"
        )),
        // Aster is a Binance fork whose report echoes the order client id, but every aster pass is an
        // authenticated MAINNET read (`crates/bridges/aster/CLAUDE.md`), so it never joins a
        // demo-proven set.
        "aster" => RestoreRow::never(concat!(
            "its reconcile passes are authenticated MAINNET reads ",
            "(crates/bridges/aster/src/reconcile.rs's ReconcileClientAster)"
        )),
        // The report echoes the cloid HASH, not our id
        // (`crates/bridges/hyperliquid/src/reconcile/mod.rs`'s `parse_orders`), but the hash is a
        // pure function of the order client id: the core asks the client for it
        // (`ExecutionClient::wire_id_for`, decision 0121) and rewrites the pass's reports before
        // planning. The one NAMED exception to the echo rule: `WIRE_ID_RESTORE_VENUES`.
        "hyperliquid" => RestoreRow::enabled(concat!(
            "testnet smoke green 2026-10-10 (crates/bridges/hyperliquid/tests/",
            "hyperliquid_restore_smoke.rs: placed -> 0x cloid reported -> translated -> adopted -> ",
            "re-registered -> engine cancel -> gone at venue -> folded Canceled); the report echoes ",
            "the keccak cloid, not our order client id (crates/bridges/hyperliquid/src/reconcile/mod.rs's ",
            "parse_orders), which the restore translates through ExecutionClient::wire_id_for ",
            "(crates/bridges/hyperliquid/src/execution/client.rs's with_venue_hooks)"
        )),
        // vike:new-venue:row // TODO(new-venue: {venue}): a NAMED row; Enabled only with a demo smoke and an order client id echo
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

/// How long, in event-clock milliseconds counted from the FIRST refused submit (not from boot), the
/// core keeps refusing a tagged submit over a tag a restored order still holds, before the refusal
/// FAILS OPEN to the old behaviour
/// (`crates/vike-core/src/runtime/strategy_drive/broker_drain.rs`'s `restored_tag_holds`).
///
/// The refusal covers the stretch between a restart and the first reconcile pass: the restored
/// order is back under its tag but not yet in the registry, so a resubmit would overwrite the tag
/// and orphan an order that still rests at the venue. It ends by itself when the pass spends the
/// restored entry (ADOPT, GONE or FILLED) or finds a plain-kind one unlisted (UNPROVEN: the entry
/// stays, see `restored_tag_holds`). This window is only the backstop for a pass that never
/// comes (reconcile off, a fetch that keeps failing), so that a mount is not silenced for ever.
///
/// ⚠ **It is also a deadline the first pass has to beat.** A pass that arrives after the window
/// finds the refusal already lifted, and the maker has by then put a second quote on the same
/// side, one no tag can address any more. The first pass runs after
/// `config.reconcile_startup_delay_ms`, so that row is bounded by
/// [`MAX_RECONCILE_STARTUP_DELAY_MS`], half of this window, and refused above it at the write and
/// at the load. The other half is the room the pass itself needs (its venues are fetched one after
/// another).
pub const RESTORED_TAG_DENY_WINDOW_MS: i64 = 120_000;

/// The reason of the `Denied` lifecycle event the core answers such a refused submit with
/// (`crates/vike-core/src/runtime/strategy_drive/broker_drain.rs`'s `deny_restored_tag`). Spelled once,
/// here beside the window, because the emulated route's law keys on it: its child slot
/// (`crates/vike-execution/src/emulated.rs`'s `OrderEmulationChildSlot`) reads it as "the child the saved state
/// predates is not judged yet" and awaits that child again, where any other refusal stops it.
pub const RESTORED_TAG_DENY_REASON: &str =
    "restart: a restored order still holds this tag until the first reconcile pass";

/// The longest `config.reconcile_startup_delay_ms` the settings loader accepts, in milliseconds:
/// HALF of [`RESTORED_TAG_DENY_WINDOW_MS`], so the first reconcile pass starts well inside the
/// window in which a restored order mutes a tagged resubmit
/// (`crates/vike-config/src/config.rs`'s `Config::apply` enforces it through `check_at_most`).
///
/// A literal, not an expression over the window, because the docs gate reads it as text
/// (`crates/vike-ops/tests/docs/docs_constants_gate/claim_table.rs`'s `CLAIMS` row for the knob);
/// `the_startup_delay_bound_is_half_the_refusal_window` pins the two together.
pub const MAX_RECONCILE_STARTUP_DELAY_MS: u32 = 60_000;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::venue_order_client_id_budget::{OrderClientIdCarriage, order_client_id_budget_for};
    use vike_model::VENUES;

    /// The venues still waiting on a green demo smoke (the row says `awaiting_smoke`): none today.
    const PLANNED: &[&str] = &[];

    /// The venues whose restore smoke is green: the row says `enabled` and the restore runs.
    const ENABLED: &[&str] =
        &["binance", "bybit", "okx", "deribit", "hyperliquid", "oanda", "ibkr"];

    /// The venues that stay off, with the reason class on their row.
    #[rustfmt::skip]
    const NEVER: &[&str] = &[
        "ig", "fxcm", "dukascopy", "polymarket", "ctrader", "alpaca", "aster",
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
    /// only once its restore smoke is green.
    #[test]
    fn the_restore_matrix_is_pinned_verbatim() {
        use RestoreState::{Enabled, Never};
        #[rustfmt::skip]
        const MATRIX: &[(&str, RestoreState, bool)] = &[
            ("binance",     Enabled,       true),
            ("bybit",       Enabled,       true),
            ("okx",         Enabled,       true),
            ("deribit",     Enabled,       true),
            ("oanda",       Enabled,       true),
            ("ig",          Never,         false),
            ("fxcm",        Never,         false),
            ("dukascopy",   Never,         false),
            ("polymarket",  Never,         false),
            ("ibkr",        Enabled,       true),
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

    /// The venues a restore table would break the order client id rule for, given any `enabled` predicate: a
    /// venue that restores must have an order client id row that ECHOES the id back, unless it is a named
    /// [`WIRE_ID_RESTORE_VENUES`] exception. Takes the predicate so the check can be run against the
    /// real table AND against a table with everything switched on.
    fn echo_rule_violations(enabled: impl Fn(&str) -> bool) -> Vec<&'static str> {
        VENUES
            .iter()
            .copied()
            .filter(|&v| {
                enabled(v)
                    && !order_client_id_budget_for(v).echoes_order_client_id()
                    && !WIRE_ID_RESTORE_VENUES.contains(&v)
            })
            .collect()
    }

    /// THE cross-pin: every venue that restores echoes the order client id per `order_client_id_budget_for`, which is
    /// real code. A venue enabled here without the echo goes red here, not on a live order.
    #[test]
    fn an_enabled_venue_echoes_the_order_client_id() {
        // Not vacuous: the enabled set is non-empty, so the loop below EXECUTES the echo rule.
        assert!(
            VENUES.iter().any(|&v| restores_orders(v)),
            "no venue restores: the pin is vacuous"
        );
        assert_eq!(echo_rule_violations(restores_orders), Vec::<&str>::new());
        for &v in VENUES {
            if restores_orders(v) && !WIRE_ID_RESTORE_VENUES.contains(&v) {
                assert!(
                    order_client_id_budget_for(v).echoes_order_client_id(),
                    "{v} restores but its order client id is not echoed"
                );
            }
        }
    }

    /// The exception list is PINNED, not open: exactly the venues whose client answers
    /// `ExecutionClient::wire_id_for`, each really a venue the echo rule would otherwise refuse (an
    /// exception for an echoing venue is vacuous and hides a future change of its order client id row) and each
    /// a roster venue that is planned or enabled. Growing it is a decision, and the bridge's own
    /// test (`crates/bridges/hyperliquid/src/execution/execution_tests.rs`'s
    /// `wire_id_for_is_the_cloid_a_submit_puts_on_the_wire`) is where the override is proven.
    #[test]
    fn the_wire_id_exception_list_is_pinned() {
        assert_eq!(WIRE_ID_RESTORE_VENUES, &["hyperliquid"]);
        for &v in WIRE_ID_RESTORE_VENUES {
            assert!(VENUES.contains(&v), "{v} is not a roster venue");
            assert!(
                !order_client_id_budget_for(v).echoes_order_client_id(),
                "{v} echoes the order client id, so it needs no wire-id exception"
            );
            assert!(
                PLANNED.contains(&v) || ENABLED.contains(&v),
                "{v} is excepted from the echo rule but is neither planned nor enabled"
            );
        }
    }

    /// The pin above checks only the rows as shipped, so prove the check can fail: with EVERY
    /// venue switched on, the violations are exactly the venues whose order client id row is not echoed and
    /// that are not a named wire-id exception.
    #[test]
    fn the_echo_check_bites_when_a_venue_without_an_echo_is_switched_on() {
        let mut expected: Vec<&str> = VENUES
            .iter()
            .copied()
            .filter(|&v| {
                !order_client_id_budget_for(v).echoes_order_client_id()
                    && !WIRE_ID_RESTORE_VENUES.contains(&v)
            })
            .collect();
        let mut got = echo_rule_violations(|_| true);
        expected.sort_unstable();
        got.sort_unstable();
        assert_eq!(got, expected);
        assert!(!got.is_empty(), "the roster has venues that do not echo the order client id");
        assert!(
            got.contains(&"polymarket"),
            "polymarket reports no order client id and has no wire id"
        );
        assert!(
            !got.contains(&"hyperliquid"),
            "HL echoes a hash, but its wire id is derivable: the one named exception"
        );
    }

    /// The planned and enabled venues are only restorable because their order client id rows echo (or, for the
    /// named exception, because the client names the wire id); if one of those rows moves, the plan
    /// is stale and this says so before the flip does.
    #[test]
    fn the_planned_venues_all_echo_the_order_client_id() {
        for &v in PLANNED.iter().chain(ENABLED) {
            assert!(
                order_client_id_budget_for(v).echoes_order_client_id()
                    || WIRE_ID_RESTORE_VENUES.contains(&v),
                "{v} is planned for the restore but its order client id is not echoed"
            );
        }
    }

    /// The echo does not open a row: ctrader, alpaca and aster echo the order client id and stay `never`,
    /// each for its own reason, so `row_symbol_is_taught` (restore open AND echoed) still answers
    /// `false` for all three, exactly as before ctrader's and alpaca's order client id rows said `Verbatim`.
    #[test]
    fn an_echoing_venue_stays_off_for_its_own_reason() {
        for v in ["ctrader", "alpaca", "aster"] {
            assert!(
                order_client_id_budget_for(v).echoes_order_client_id(),
                "{v} echoes the order client id"
            );
            assert_eq!(restore_row_for(v).state, RestoreState::Never, "{v}");
            assert!(!restores_orders(v), "{v}");
        }
        let echoing_but_off: Vec<&str> = VENUES
            .iter()
            .copied()
            .filter(|&v| {
                order_client_id_budget_for(v).echoes_order_client_id() && !restores_orders(v)
            })
            .collect();
        assert_eq!(echoing_but_off, ["ctrader", "alpaca", "aster"]);
    }

    /// A venue whose order client id row is `NotOnTheWire` is never allowed, whatever its row says: the
    /// restore maps by order client id and such a venue's reports cannot carry ours.
    #[test]
    fn a_venue_whose_order_client_id_row_is_not_on_the_wire_is_never_enabled() {
        for &v in VENUES {
            if order_client_id_budget_for(v) == OrderClientIdCarriage::NotOnTheWire
                && !WIRE_ID_RESTORE_VENUES.contains(&v)
            {
                assert!(
                    !restores_orders(v),
                    "{v}: NotOnTheWire cannot be restored by order client id"
                );
                assert!(
                    !PLANNED.contains(&v) && !ENABLED.contains(&v),
                    "{v}: NotOnTheWire cannot be a planned or enabled venue"
                );
            }
        }
    }

    /// The loader's upper bound on the first pass's delay is HALF the window in which a restored
    /// order mutes a tagged resubmit. The two are written as separate literals (the docs gate reads
    /// the bound as text), so this is what keeps them one fact: widening the window without the
    /// bound, or the bound without the window, goes red here instead of reopening a hole in which
    /// the window expires before the first pass and two quotes rest on one side.
    #[test]
    fn the_startup_delay_bound_is_half_the_refusal_window() {
        assert_eq!(
            i64::from(MAX_RECONCILE_STARTUP_DELAY_MS) * 2,
            RESTORED_TAG_DENY_WINDOW_MS,
            "the startup-delay bound must stay half of the refusal window"
        );
    }
}
