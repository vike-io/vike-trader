//! `command_lift` — the client-side lowering for the thin-client **Scope::Write** WRITE path: turn a live-core
//! [`vike_execution::Command`] into the thin-wire [`crate::wire::CommandWire`] the desktop's observer sends to a
//! remote headless `vike-tradehub` daemon's control server.
//!
//! [`wire_from_command`] is the exact INVERSE of the daemon's `lower_command`
//! (`crates/vike-tradehub/src/server/control.rs`, which lowers a received `CommandWire` back into a real
//! `Command`/`OrderIntent` at its edge). It is deliberately **PARTIAL**: the thin wire vocabulary
//! ([`crate::wire::CommandWire`]) is only the SESSION-relevant subset a remote GUI drives
//! (submit / cancel / modify / mass-cancel / flatten / close-all / trading-state, the TP/SL
//! bracket since the node-side bracket command, plus the strategy-level live-params re-tune since
//! split-plane B4), NOT the full internal `Command`/`OrderIntent` surface (batches, conditionals,
//! combos, margin, reconcile plumbing, shutdown). A `Command` with no wire form yields `None`; the
//! desktop's caller (`crates/vike-app-core/src/backend/tradehub_control.rs`'s `send_to_backend`) logs it and
//! latches a status-strip line saying it was not sent — a future PR either extends `CommandWire` or grays out
//! the UI control that produces it.
//!
//! It moved here from `vike-app-core`'s `tradehub_control` with the observe decode (D56, the owner's 2026-11-17
//! ruling), beside the wire it builds; the send, the routing gate and every status-strip word stayed with the
//! desktop.

/// Lower a live-core [`vike_execution::Command`] into the thin-wire [`crate::wire::CommandWire`]
/// a remote Scope::Write client sends — the client-side inverse of the daemon's `lower_command`.
///
/// PARTIAL (see the module doc): every mapped variant copies its fields into the standalone wire
/// mirror verbatim (a Submit copies every `OrderRequest` field the wire carries, its account in the
/// wire spelling; an
/// `UpdateParams` copies its target key and mount id and re-serializes the typed `StrategyParams`
/// into the core's own serde JSON — the shape the wire deliberately delegates to); everything with
/// no wire form — `OrderIntent::{SubmitBatch, CancelBatch, Confirm, ArmConditional,
/// DisarmConditional, Exercise}` and `Command::{SetMargin, ReconcileReports, Shutdown}` — returns
/// `None`.
/// (`Command::ConfirmReconcile` has one since the operator's `trade reconcile confirm`:
/// `CommandWire::ReconcileConfirm`.)
///
/// ⚠ An `UpdateParams` keeps its `mount_id` verbatim, and WHETHER to name one is the producer's call:
/// send it whenever the node advertises [`crate::proto::FEATURE_PARAMS_BY_MOUNT`]
/// (its status rows then carry the id to send), because two mounts can share a series and an
/// unaddressed update on one is refused by the core. Against a node WITHOUT the capability the
/// client write path refuses an addressed update locally (`ControlRejected::UnsupportedByHub`,
/// nothing sent) rather than let that node retune the first mount on the series.
///
/// ⚠ The gate covers ADDRESSED updates only. An UNADDRESSED update from a new client to an older
/// node (one without `strategy-params-mount`) is sent as before, and on a series two mounts share
/// that node still retunes the FIRST mount - it predates the core's refusal of an ambiguous update.
/// A producer that can name a mount should do so whenever the node advertises the capability, and
/// should not rely on the unaddressed shape against a node that does not.
pub fn wire_from_command(cmd: &vike_execution::Command) -> Option<crate::wire::CommandWire> {
    use crate::wire::OrderBracketWire;
    use crate::wire::{CommandWire, OrderRequestWire};
    use vike_execution::{Command, OrderIntent};

    match cmd {
        Command::Order(OrderIntent::Submit(req)) => Some(CommandWire::Submit(OrderRequestWire {
            order_client_id: req.order_client_id.clone(),
            venue: req.venue.clone(),
            symbol: req.symbol.clone(),
            side: req.side,
            qty: req.qty,
            order_type: req.order_type.as_str().to_owned(),
            price: req.price,
            trigger_price: req.trigger_price,
            reduce_only: req.reduce_only,
            // ⚠ The account rides across in the wire spelling (`Display`: `DEFAULT` for the unlabelled
            // book, as the `MountStrategy` arm below argues). Hard-coding `None` here dropped it, and
            // an account-less submit on a venue that runs two engines is refused as ambiguous. A node
            // older than account-scoped submits is refused client-side, never misrouted
            // (`crate::tradehub_control`'s `required_feature`).
            account: req.account.as_ref().map(|a| a.to_string()),
            // The lifetime rides across in the wire spelling too: `gtc`, the default, is the ABSENT field
            // (the frame from before the field), so an order that names none is byte-identical; any other
            // lifetime owes `order-tif` and a node without it is refused client-side, never run as a plain
            // GTC limit (`required_feature`). The one GUI widget that sets one is the Trade ticket's Post
            // only (`TimeInForce::PostOnly`, see `node_takes_post_only`); a strategy-built request may too.
            time_in_force: (req.time_in_force != vike_model::TimeInForce::Gtc)
                .then(|| req.time_in_force.as_word().to_owned()),
            gtd_expiry: req.gtd_expiry,
            // The display and the release ride across too, each absent when `None` (the frame from before), and
            // each owes its word, refused client-side against a node without it (`required_feature`).
            display_quantity: req.display_quantity,
            release: req.release.map(|r| crate::wire::OrderReleaseWire {
                at: r.at,
                price: r.price.map(|c| match c.side {
                    vike_model::OrderReleasePriceSide::Above => {
                        crate::wire::OrderReleasePriceWire::Above(c.level)
                    }
                    vike_model::OrderReleasePriceSide::Below => {
                        crate::wire::OrderReleasePriceWire::Below(c.level)
                    }
                }),
                // The condition's symbol only when it reads ANOTHER than the order's own (absent is the
                // order's), so a condition on its own symbol is the frame without the field.
                symbol: r.price.map(|c| c.symbol.to_string()).filter(|s| *s != req.symbol),
                // The venue its price is read on, only when the condition names one (absent is the order's own).
                venue: r.price.and_then(|c| c.venue).map(|v| v.to_string()),
            }),
            // A trailing order's trail and limit offset ride across too, each absent when `None`, and owe
            // `order-trailing` (`required_feature`). A `trail` on any other kind keeps its backtest-only meaning
            // and is NOT sent: the node refuses one there, and the frame stays the one from before.
            trail: req.trail.filter(|_| req.order_type.is_trailing()),
            limit_offset: req.limit_offset,
            // A pegged order's peg, absent when `None`, owes `order-peg` (`required_feature`).
            peg: req.peg.map(|p| crate::wire::OrderPegWire {
                anchor: p.anchor.as_word().to_string(),
                offset: p.offset,
            }),
            // The attributes, the percent trail and the adjustment (the wire carries the model's types), each
            // absent when `None`; each owes `order-attributes` (`required_feature`), and so does a release's venue
            // above. An empty attribute set is no attribute, so it never asks for the word. This arm copies; the
            // node's door judges.
            attributes: req.attributes.filter(|a| *a != vike_model::OrderAttributes::default()),
            trail_percent: req.trail_percent,
            adjust: req.adjust,
        })),
        // The TP/SL bracket: `OrderBracket` field for field, its account in the wire spelling like the
        // submit's above (`Display`: `DEFAULT` for the unlabelled book). A bracket naming none is
        // taken only on a venue whose one account is the default one; one that names an account owes
        // `FEATURE_BRACKET_ACCOUNT`, and a node without it is refused client-side, never misrouted
        // (`crate::tradehub_control`'s `required_feature`).
        //
        // ⚠ A non-finite `entry_price` serializes as `null`, which the node reads as a MARKET entry,
        // so the only guard is the sender's: the one producer of this intent in the GUI is
        // `crates/vike-order-entry/src/dispatch/planner.rs`'s `admit_bracket`, which validates
        // every leg first and refuses a non-finite price. This arm copies; it does not judge.
        Command::Order(OrderIntent::Bracket(spec)) => {
            Some(CommandWire::Bracket(OrderBracketWire {
                venue: spec.venue.clone(),
                symbol: spec.symbol.clone(),
                side: spec.side,
                qty: spec.qty,
                entry_price: spec.entry_price,
                stop_loss: spec.stop_loss,
                take_profit: spec.take_profit,
                account: spec.account.as_ref().map(|a| a.to_string()),
                // the stem, when the sender named one, owes `FEATURE_BRACKET_STEM` (`required_feature`)
                order_client_id: spec.order_client_id.clone(),
            }))
        }
        // The ORDER LIST: `OrderList` field for field, each leg's kind and lifetime in the wire's words (`gtc`, the
        // default, is the absent field) and the account in the wire spelling like the submit's. It owes `order-list`,
        // refused client-side against a node without it (`required_feature`). This arm copies; the node's door judges.
        Command::Order(OrderIntent::OrderList(spec)) => {
            use crate::wire::{OrderListLegWire, OrderListWire};
            Some(CommandWire::OrderList(OrderListWire {
                venue: spec.venue.clone(),
                symbol: spec.symbol.clone(),
                legs: spec
                    .legs
                    .iter()
                    .map(|leg| OrderListLegWire {
                        side: leg.side,
                        qty: leg.qty,
                        order_type: leg.order_type.as_str().to_owned(),
                        price: leg.price,
                        trigger_price: leg.trigger_price,
                        trail: leg.trail,
                        reduce_only: leg.reduce_only,
                        time_in_force: (leg.time_in_force != vike_model::TimeInForce::Gtc)
                            .then(|| leg.time_in_force.as_word().to_owned()),
                        parent: leg.parent,
                        contingency: leg.contingency,
                        linked: leg.linked.clone(),
                        order_client_id: leg.order_client_id.clone(),
                    })
                    .collect(),
                account: spec.account.as_ref().map(|a| a.to_string()),
            }))
        }
        // The COMBO: `OrderCombo` field for field (legs, signed ratios, leg limits, the signed net, its one client order
        // id), the account in the wire spelling like the submit's, and EVERY lifetime in the wire's word (`gtc`, the
        // default, is the absent field). It owes `combo`, refused client-side against a node without it
        // (`required_feature`). This arm copies; the node's door judges (it refuses a combo with no id of its own).
        Command::Order(OrderIntent::Combo(spec)) => {
            use crate::wire::OrderComboWire;
            Some(CommandWire::Combo(OrderComboWire {
                venue: spec.venue.clone(),
                side: spec.side,
                qty: spec.qty,
                legs: spec.legs.clone(),
                net_limit: spec.net_limit,
                account: spec.account.as_ref().map(|a| a.to_string()),
                time_in_force: (spec.time_in_force != vike_model::TimeInForce::Gtc)
                    .then(|| spec.time_in_force.as_word().to_owned()),
                order_client_id: spec.order_client_id.clone(),
            }))
        }
        Command::Order(OrderIntent::Cancel(order_client_id)) => {
            Some(CommandWire::Cancel(order_client_id.clone()))
        }
        // The trigger price rides across, absent when `None`; a modify naming one owes `order-modify-trigger`,
        // refused client-side against a node without it (`required_feature`), never sent with the trigger dropped.
        Command::Order(OrderIntent::Modify {
            order_client_id,
            new_qty,
            new_price,
            new_trigger_price,
        }) => Some(CommandWire::Modify {
            order_client_id: order_client_id.clone(),
            new_qty: *new_qty,
            new_price: *new_price,
            new_trigger_price: *new_trigger_price,
        }),
        // The three risk-REDUCING verbs carry their account across the lift, in the wire spelling
        // (`Display` — `DEFAULT` for the unlabelled book, as the `MountStrategy` arm below argues).
        // ⚠ Dropping it here would change the verb on the far side: the node takes an account-less reduce
        // only for a venue of one account (decision 0254) and refuses it anywhere else, so a lift to `None`
        // turns "cancel ALT's book" into a refusal. A node that does not honour the field refuses nothing —
        // the client refuses the send against it (`crate::tradehub_control`'s `required_feature`).
        Command::Order(OrderIntent::MassCancel { venue, symbol, account }) => {
            Some(CommandWire::MassCancel {
                venue: venue.clone(),
                symbol: symbol.clone(),
                account: account.as_ref().map(|a| a.to_string()),
            })
        }
        Command::Order(OrderIntent::Flatten { venue, symbol, account }) => {
            Some(CommandWire::Flatten {
                venue: venue.clone(),
                symbol: symbol.clone(),
                account: account.as_ref().map(|a| a.to_string()),
            })
        }
        Command::Order(OrderIntent::MarketExit { venue, account }) => {
            Some(CommandWire::MarketExit {
                venue: venue.clone(),
                account: account.as_ref().map(|a| a.to_string()),
            })
        }
        Command::SetTradingState(ts) => Some(CommandWire::SetTradingState(*ts)),
        // The strategy-level live-params re-tune (split-plane B4): the target key copies verbatim;
        // the typed `StrategyParams` re-serializes into the core's OWN serde JSON, which is exactly
        // what the wire variant carries (delegated, not mirrored — see its doc) and what the
        // daemon's `lower_command` deserializes back. `to_value` on these serde-derive params
        // structs cannot fail in practice; `.ok()?` keeps the function total rather than panicking
        // a GUI thread on a hypothetical unserializable future variant.
        Command::UpdateParams(u) => Some(CommandWire::UpdateParams {
            venue: u.venue.clone(),
            symbol: u.symbol.clone(),
            interval: u.interval.clone(),
            mount_id: u.mount_id.clone(),
            params: serde_json::to_value(&u.params).ok()?,
        }),
        // The runtime mount verbs (split-plane B5): the spec is fully serde, so the lift is a
        // field-for-field copy — the wire variant mirrors `vike_execution::MountSpec` exactly, except
        // that `params` crosses as JSON (the wire variant keeps it a JSON object): the
        // `vike_model::params::Params` table's own serde JSON, the object the daemon's
        // `lower_command` reads back. `to_value` on a table of named values cannot fail in
        // practice; `.ok()?` keeps the function total, as the `UpdateParams` arm above does.
        Command::MountStrategy(spec) => Some(CommandWire::MountStrategy {
            venue: spec.venue.clone(),
            // ⚠ **`Display`, not `text()`** — and the difference is a whole wire state. `text()`
            // answers `None` for the DEFAULT account, which would flatten "named the unlabelled
            // account" into "named nothing"; `Display` renders it `DEFAULT`, the spelling
            // `parse_wire_account` reads back on the far side. The two are different rows of the
            // routing table at `N >= 2`, so collapsing them here would lose the operator's choice
            // between the lift and the frame.
            account: spec.account.as_ref().map(|a| a.to_string()),
            symbol: spec.symbol.clone(),
            interval: spec.interval.clone(),
            controller_id: spec.controller_id.clone(),
            file: spec.file.clone(),
            name: spec.name.clone(),
            rhai: spec.rhai.clone(),
            params: serde_json::to_value(&spec.params).ok()?,
        }),
        Command::UnmountStrategy { controller_id } => {
            Some(CommandWire::UnmountStrategy { controller_id: controller_id.clone() })
        }
        Command::ConfirmReconcile(id) => Some(CommandWire::ReconcileConfirm { id: *id }),
        // No thin-wire form yet (see the module doc) — the caller logs these and reports them
        // as not sent:
        //   OrderIntent::{SubmitBatch, CancelBatch, Confirm, ArmConditional,
        //                 DisarmConditional, Exercise}
        //   Command::{SetMargin, ReconcileReports, Shutdown}
        // Exercise keeps no wire form: every venue refuses one (the capability gate's row says why).
        _ => None,
    }
}

/// Whether a TP/SL bracket has a wire form — DERIVED from [`wire_from_command`] itself (it lifts a
/// sample bracket and asks), so the Trade window's ticket enables its TP/SL toggle the day a wire
/// form lands and nothing here needs flipping (Ruling R8 of the Trade window plan).
///
/// The answer is a property of the BUILD, so it is computed once and cached: the Trade window reads
/// it every frame, and the lift allocates two strings per call.
///
/// ⚠ It says what THIS client can lift, never what the connected node can take: against a node that
/// predates [`crate::proto::FEATURE_BRACKET`] the client refuses the send, and the desktop's
/// `crates/vike-app-core/src/backend/tradehub_control.rs`'s `send_to_backend` latches the status-strip line that
/// says so.
pub fn bracket_has_wire_form() -> bool {
    static LIFTS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *LIFTS.get_or_init(|| {
        use vike_execution::{Command, OrderIntent};
        wire_from_command(&Command::Order(OrderIntent::Bracket(Box::new(
            vike_model::OrderBracket {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                side: 1,
                qty: 1.0,
                entry_price: None,
                stop_loss: 1.0,
                take_profit: 2.0,
                account: None,
                order_client_id: None,
            },
        ))))
        .is_some()
    })
}

/// Whether the connected node carries a POST-ONLY order, for the Trade ticket's Post only control:
/// `takes` is the node's own gate ([`crate::TradehubControlHandle::takes`], the check
/// `try_command` applies before the queue), asked of the very command the ticket would send, lifted by
/// [`wire_from_command`]. The feature word is therefore spelled in ONE place
/// (`crate::tradehub_control`'s `required_feature`), and a lift that dropped the lifetime
/// would make this `true` against every node, which `a_sample_post_only_order_carries_its_lifetime_key`
/// pins; a node too old to take the key makes it `false`, and the ticket greys the control with that
/// reason instead of letting the click be refused after the fact.
///
/// ⚠ It says what the CONNECTED NODE takes. Whether the VENUE has a post-only order is
/// `vike_venues::VenueCapabilities::supports_post_only`, and the widget reads both (`ticket::post_only_block`).
pub fn node_takes_post_only(takes: impl FnOnce(&crate::wire::CommandWire) -> bool) -> bool {
    use vike_execution::{Command, OrderIntent};
    // The sample is a property of the BUILD (the lift), so it is lifted once and read every frame: the
    // Trade window asks per frame, and the lift allocates a handful of strings per call.
    static SAMPLE: std::sync::OnceLock<Option<crate::wire::CommandWire>> =
        std::sync::OnceLock::new();
    SAMPLE
        .get_or_init(|| {
            let sample = vike_model::OrderRequest {
                order_client_id: "post-only-sample".into(),
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                side: 1,
                qty: 1.0,
                order_type: vike_model::OrderType::Limit,
                price: Some(1.0),
                time_in_force: vike_model::TimeInForce::PostOnly,
                ..Default::default()
            };
            wire_from_command(&Command::Order(OrderIntent::Submit(Box::new(sample))))
        })
        .as_ref()
        .is_some_and(takes)
}

/// Whether the connected node takes a TP/SL bracket that names its group STEM (decision 0270's 4.1), for the
/// desktop's `crates/vike-order-entry/src/dispatch/mod.rs`'s `OrderEntryDispatchInputs::bracket_stem`:
/// [`node_takes_post_only`]'s pattern, `takes` asked of a lifted sample stem bracket, so the feature word stays
/// spelled in `required_feature` alone. A node too old for the key makes it `false`, and the planner sends the
/// bracket with no stem, as before, instead of having the client refuse it whole.
pub fn node_takes_bracket_stem(takes: impl FnOnce(&crate::wire::CommandWire) -> bool) -> bool {
    use vike_execution::{Command, OrderIntent};
    static SAMPLE: std::sync::OnceLock<Option<crate::wire::CommandWire>> =
        std::sync::OnceLock::new();
    SAMPLE
        .get_or_init(|| {
            wire_from_command(&Command::Order(OrderIntent::Bracket(Box::new(
                vike_model::OrderBracket {
                    venue: "binance".into(),
                    symbol: "BTCUSDT.P".into(),
                    side: 1,
                    qty: 1.0,
                    entry_price: None,
                    stop_loss: 1.0,
                    take_profit: 2.0,
                    account: None,
                    order_client_id: Some("stem-sample".into()),
                },
            ))))
        })
        .as_ref()
        .is_some_and(takes)
}

#[path = "command_lift_tests.rs"]
#[cfg(test)]
mod command_lift_tests;
