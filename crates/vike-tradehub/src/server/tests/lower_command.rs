//! The B4 lowering edge in isolation: the wire `UpdateParams` payload IS the core's own
//! `StrategyParams` serde schema, and an undecodable payload is a REFUSAL, never a silent
//! drop. (The end-to-end proof — the lowered command reaching a mounted strategy through the
//! real server — is `tests/control_roundtrip.rs`'s
//! `update_params_over_the_wire_retunes_the_mounted_strategy`.)

use super::control::{command_kind, lower_command};
use super::refusal::bracket_refusal;
use vike_exec::{Command, OrderIntent};
use vike_model::StrategyParams;
use vike_tradehub_client::wire::WireCommand;

/// A fully-populated `SpreadMakerParams` payload built from the REAL core type — what proves
/// "the wire schema IS the core schema" rather than a lookalike.
fn spread_maker_params(qty: f64, half_spread: f64) -> StrategyParams {
    StrategyParams::SpreadMaker(vike_model::SpreadMakerParams {
        qty,
        half_spread,
        target_inventory: 0.0,
        max_inventory: 1.0,
        skew: 0.0,
        fill_window_ms: 0,
        net_fill_threshold: 0.0,
        suppress_cooldown_ms: 0,
        style: vike_model::QuoteStyle::Mid,
        depth_levels: 1,
        tick_size: 0.0,
        filter_own: false,
        avellaneda_stoikov: None,
        refresh_tolerance: None,
        ladder: None,
        reward: None,
        toxicity: None,
    })
}

#[test]
fn update_params_lowers_into_the_real_core_command() {
    let wire = WireCommand::UpdateParams {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        // The core's own externally-tagged form — proven against the REAL type below.
        params: serde_json::to_value(spread_maker_params(2.0, 1.0)).unwrap(),
    };
    let (cmd, coid) = lower_command(wire).expect("a decodable payload lowers");
    assert_eq!(coid, "", "account-wide-verb convention: nothing order-scoped to echo");
    let Command::UpdateParams(u) = cmd else {
        panic!("must lower to Command::UpdateParams, got a different command");
    };
    assert_eq!(
        (u.venue.as_str(), u.symbol.as_str(), u.interval.as_str()),
        ("binance", "BTCUSDT", "1m")
    );
    let StrategyParams::SpreadMaker(p) = u.params else {
        panic!("the typed variant survives the wire round-trip");
    };
    assert_eq!(p.qty.to_bits(), 2.0_f64.to_bits(), "the payload's knobs land verbatim");
    assert_eq!(p.half_spread.to_bits(), 1.0_f64.to_bits());
}

#[test]
fn an_undecodable_params_payload_is_refused_not_dropped() {
    let wire = WireCommand::UpdateParams {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        params: serde_json::json!({"NoSuchStrategyParams": {"qty": 1.0}}),
    };
    let err = lower_command(wire).expect_err("an unknown variant must refuse");
    assert!(err.contains("update_params"), "names the verb: {err}");
}

#[test]
fn update_params_has_its_own_audit_kind() {
    let wire = WireCommand::UpdateParams {
        venue: "b".into(),
        symbol: "s".into(),
        interval: "1m".into(),
        params: serde_json::json!({}),
    };
    assert_eq!(command_kind(&wire), "update_params");
}

/// **THE WIRE-TO-CORE HOP (this task).** A wire `Submit` naming an account must reach the
/// core `OrderRequest` carrying it. Before this arm read the field, the `OrderRequest`
/// literal's `..Default::default()` silently dropped it and every order routed to whichever
/// engine happened to be first, regardless of what the client asked for — routing is still
/// blind to it after this task (Task 4's job), but the value now survives the hop for that
/// task to consume.
mod submit_account_tests {
    use super::*;
    use vike_tradehub_client::wire::WireOrderRequest;

    /// Every field `client_order_id`'s non-empty gate and `OrderRequest`'s construction need,
    /// with `account` the one axis under test.
    fn submit(account: Option<&str>) -> WireCommand {
        WireCommand::Submit(WireOrderRequest {
            client_order_id: "c-1".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type: "limit".into(),
            price: Some(1.0),
            trigger_price: None,
            reduce_only: false,
            account: account.map(str::to_string),
        })
    }

    fn submitted_order(wire: WireCommand) -> Result<vike_model::OrderRequest, String> {
        let (cmd, _coid) = lower_command(wire)?;
        let Command::Order(OrderIntent::Submit(order)) = cmd else {
            panic!("must lower to Command::Order(OrderIntent::Submit), got a different command");
        };
        Ok(*order)
    }

    #[test]
    fn a_submit_naming_an_account_carries_it_into_the_core_order() {
        let order = submitted_order(submit(Some("ALT"))).expect("a valid label lowers");
        assert_eq!(
            order.account,
            Some(vike_model::accounts::account_keys::AccountLabel::Named("ALT".to_string())),
            "the account named on the wire must reach the core order"
        );
    }

    /// The wire's reserved spelling for "the unlabelled account, deliberately" —
    /// `parse_wire_account`'s own contract, admitted here exactly as `MountStrategy`'s arm
    /// admits it, where the `policy.accounts.<venue>.<LABEL>` rows refuse it.
    #[test]
    fn a_submit_naming_default_on_the_wire_carries_the_default_account() {
        let order = submitted_order(submit(Some("DEFAULT"))).expect("DEFAULT lowers");
        assert_eq!(order.account, Some(vike_model::accounts::account_keys::AccountLabel::Default));
    }

    /// Absence is unchanged: a client naming no account — every pre-existing caller — still
    /// lowers to `None`, byte-identical to before this task.
    #[test]
    fn a_submit_naming_no_account_lowers_to_none() {
        let order = submitted_order(submit(None)).expect("an account-less submit lowers");
        assert_eq!(order.account, None);
    }

    /// ⚠ The failure this task exists to prevent: `None` routes to the venue's DEFAULT book,
    /// so swallowing a malformed label into `None` (`.ok()`, `.unwrap_or_default()`, …) would
    /// silently trade the wrong account for an operator's typo. It must be REFUSED instead —
    /// the same shape `MountStrategy`'s arm already gives for the same parser's error.
    #[test]
    fn a_submit_naming_an_invalid_account_label_is_refused_not_dropped() {
        let err = submitted_order(submit(Some("bad label"))).expect_err("must refuse");
        assert!(err.contains("account"), "names the field: {err}");
    }
}

/// **THE WIRE-TO-CORE HOP for the three risk-REDUCING verbs** — the arm that used to
/// destructure them as `{ venue, symbol, .. }` and drop the account on the floor, so a
/// `market-exit binance ALT` reached the core as `market-exit binance` and fanned over every
/// account of the exchange. Same parser as the submit arm above (`parse_wire_account`, the ONE
/// reading of `DEFAULT`), same refusal of a malformed label rather than a silent `None`.
mod reduce_account_tests {
    use super::*;
    use vike_model::accounts::account_keys::AccountLabel;

    fn lowered_account(wire: WireCommand) -> Result<Option<AccountLabel>, String> {
        let (cmd, coid) = lower_command(wire)?;
        assert!(coid.is_empty(), "an account-wide verb echoes no coid");
        match cmd {
            Command::Order(OrderIntent::MassCancel { account, .. })
            | Command::Order(OrderIntent::Flatten { account, .. })
            | Command::Order(OrderIntent::MarketExit { account, .. }) => Ok(account),
            _ => panic!("a reducing verb must lower to its own reducing intent"),
        }
    }

    fn reducers(account: Option<&str>) -> Vec<WireCommand> {
        let account = account.map(str::to_string);
        vec![
            WireCommand::MassCancel {
                venue: Some("binance".into()),
                symbol: None,
                account: account.clone(),
            },
            WireCommand::Flatten {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                account: account.clone(),
            },
            WireCommand::MarketExit { venue: Some("binance".into()), account },
        ]
    }

    #[test]
    fn a_reducing_verb_naming_an_account_carries_it_into_the_core_intent() {
        for wire in reducers(Some("ALT")) {
            let label = format!("{wire:?}");
            assert_eq!(
                lowered_account(wire),
                Ok(Some(AccountLabel::Named("ALT".into()))),
                "{label}: the account named on the wire must reach the core"
            );
        }
    }

    #[test]
    fn a_reducing_verb_naming_default_carries_the_default_account() {
        for wire in reducers(Some("DEFAULT")) {
            let label = format!("{wire:?}");
            assert_eq!(lowered_account(wire), Ok(Some(AccountLabel::Default)), "{label}");
        }
    }

    /// Absence is unchanged — and it is the panic button's shape, so it is asserted for the
    /// unscoped exit too: `None` reaches the core as `None`, and the core fans out.
    #[test]
    fn a_reducing_verb_naming_no_account_lowers_to_none() {
        let mut wires = reducers(None);
        wires.push(WireCommand::MarketExit { venue: None, account: None });
        wires.push(WireCommand::MassCancel { venue: None, symbol: None, account: None });
        for wire in wires {
            let label = format!("{wire:?}");
            assert_eq!(lowered_account(wire), Ok(None), "{label}");
        }
    }

    #[test]
    fn a_reducing_verb_naming_an_invalid_account_label_is_refused_not_dropped() {
        for wire in reducers(Some("bad label")) {
            let label = format!("{wire:?}");
            let err = lowered_account(wire).expect_err("must refuse");
            assert!(err.contains("account"), "{label}: names the field: {err}");
        }
    }
}

/// **THE TP/SL BRACKET at the lowering edge** — the move into the core's own
/// `OrderIntent::Bracket`, and the checks that must happen BEFORE the `Ack` because the core
/// examines a bracket's exits only at RELEASE, after the entry has filled.
mod bracket_tests {
    use super::*;
    use vike_tradehub_client::wire::WireBracketSpec;

    /// A well-formed LONG limit bracket on a binance PERP symbol — every check below varies one
    /// field of it, so a refusal can only be that field's.
    fn spec() -> WireBracketSpec {
        WireBracketSpec {
            venue: "binance".into(),
            symbol: "BTCUSDT.P".into(),
            side: 1,
            qty: 2.0,
            entry_price: Some(100.0),
            stop_loss: 90.0,
            take_profit: 110.0,
        }
    }

    fn lowered(spec: WireBracketSpec) -> Result<(Command, String), String> {
        lower_command(WireCommand::Bracket(spec))
    }

    /// The bracket lowers into the core's own `OrderIntent::Bracket`, field for field. It echoes NO
    /// coid: the runtime mints all three after this returns (`vike_model::build_bracket`'s doc).
    #[test]
    fn a_bracket_lowers_field_for_field_and_echoes_no_coid() {
        let wire = WireCommand::Bracket(WireBracketSpec {
            venue: "binance".into(),
            symbol: "BTCUSDT.P".into(),
            side: -1,
            qty: 2.0,
            entry_price: Some(100.0),
            stop_loss: 110.0,
            take_profit: 90.0,
        });
        let (cmd, coid) = lower_command(wire).expect("a bracket lowers");
        assert_eq!(coid, "", "the runtime mints the ids after the Ack, so there is none to echo");
        let Command::Order(OrderIntent::Bracket(spec)) = cmd else {
            panic!("must lower to OrderIntent::Bracket");
        };
        assert_eq!(
            *spec,
            vike_model::BracketSpec {
                venue: "binance".into(),
                symbol: "BTCUSDT.P".into(),
                side: -1,
                qty: 2.0,
                entry_price: Some(100.0),
                stop_loss: 110.0,
                take_profit: 90.0,
            }
        );
    }

    #[test]
    fn a_bracket_has_its_own_audit_kind() {
        let wire = WireCommand::Bracket(WireBracketSpec {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            entry_price: None,
            stop_loss: 1.0,
            take_profit: 2.0,
        });
        assert_eq!(command_kind(&wire), "bracket");
    }

    /// ⚠ **`side` is +1 or -1 and nothing else.** The adapters read anything `<= 0` as a SELL, and
    /// `vike_model::build_bracket` gives the exits `-side`, so a `0` would make all three legs sells.
    #[test]
    fn a_bracket_with_side_zero_is_refused() {
        for side in [0, 2, -2] {
            let err = lowered(WireBracketSpec { side, ..spec() }).expect_err("must refuse");
            assert!(err.contains("`side`"), "side {side}: names the field: {err}");
        }
    }

    /// ⚠ The node's notional cap cannot see NaN (`NaN > max` is false) and has no answer at all
    /// without a `max_notional_per_order` row, so the finite check is THIS edge's.
    #[test]
    fn a_bracket_with_a_non_positive_or_non_finite_qty_is_refused() {
        for qty in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let err = lowered(WireBracketSpec { qty, ..spec() }).expect_err("must refuse");
            assert!(err.contains("`qty`"), "qty {qty}: names the field: {err}");
        }
    }

    /// A priced entry must be a finite price above zero. `None` is a MARKET entry and is admitted.
    #[test]
    fn a_bracket_with_a_bad_entry_price_is_refused() {
        for px in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let err = lowered(WireBracketSpec { entry_price: Some(px), ..spec() })
                .expect_err("must refuse");
            assert!(err.contains("`entry_price`"), "entry {px}: names the field: {err}");
        }
        assert!(lowered(WireBracketSpec { entry_price: None, ..spec() }).is_ok(), "a market entry");
    }

    /// ⚠ **A bad EXIT is the failure this check exists for.** The core holds both exits and first
    /// sends them when the entry FILLS, and a covered reduce bypasses the risk gate's collar and
    /// floors, so a NaN or non-positive exit would be rejected only after the position was open —
    /// leaving it unprotected. A plain `Submit` carrying the same value fails with no position.
    #[test]
    fn a_bracket_with_a_non_finite_or_non_positive_exit_is_refused() {
        for px in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let err =
                lowered(WireBracketSpec { stop_loss: px, ..spec() }).expect_err("must refuse");
            assert!(err.contains("`stop_loss`"), "stop {px}: names the field: {err}");
            let err =
                lowered(WireBracketSpec { take_profit: px, ..spec() }).expect_err("must refuse");
            assert!(err.contains("`take_profit`"), "take profit {px}: names the field: {err}");
        }
    }

    /// ⚠ **An INVERTED long is refused when the entry is priced**: a buy needs
    /// `stop_loss < entry_price < take_profit`, strictly. A stop at or above the entry, or a
    /// take-profit at or below it, would fire the moment the entry fills.
    ///
    /// ⚠ The converse is NOT promised. The check compares against the LIMIT price, not the fill: a
    /// marketable buy limit fills below its price and can leave a passing stop ABOVE the fill
    /// (`check_bracket`'s declared residual). Passing here is a statement about the frame only.
    #[test]
    fn an_inverted_long_bracket_is_refused() {
        for (sl, tp) in [(110.0, 120.0), (100.0, 110.0), (90.0, 95.0), (90.0, 100.0)] {
            let err = lowered(WireBracketSpec { stop_loss: sl, take_profit: tp, ..spec() })
                .expect_err("must refuse");
            assert!(err.contains("inverted"), "sl {sl} tp {tp}: {err}");
        }
    }

    /// ...and the mirror for a short: `take_profit < entry_price < stop_loss`.
    #[test]
    fn an_inverted_short_bracket_is_refused() {
        let short = WireBracketSpec { side: -1, stop_loss: 110.0, take_profit: 90.0, ..spec() };
        assert!(lowered(short.clone()).is_ok(), "the well-formed short");
        for (sl, tp) in [(90.0, 80.0), (100.0, 90.0), (110.0, 105.0), (110.0, 100.0)] {
            let err = lowered(WireBracketSpec { stop_loss: sl, take_profit: tp, ..short.clone() })
                .expect_err("must refuse");
            assert!(err.contains("inverted"), "sl {sl} tp {tp}: {err}");
        }
    }

    /// ⚠ **A MARKET entry's exits are checked against EACH OTHER**: there is no entry price to
    /// order them around, but a buy whose stop-loss is not below its take-profit (or a sell whose
    /// is not above it) has one exit on the wrong side of ANY fill, so it is refused with no false
    /// positive.
    ///
    /// ⚠ **DECLARED LIMIT, pinned so it cannot change silently: nothing checks a market entry's
    /// exits against where the market IS.** A well-ordered pair below the market is admitted —
    /// this edge holds no book.
    #[test]
    fn a_market_entry_bracket_is_checked_for_its_exits_order_only() {
        let market = WireBracketSpec { entry_price: None, ..spec() };
        assert!(lowered(market.clone()).is_ok(), "a well-ordered market long: sl 90 < tp 110");
        for (sl, tp) in [(110.0, 90.0), (100.0, 100.0)] {
            let err = lowered(WireBracketSpec { stop_loss: sl, take_profit: tp, ..market.clone() })
                .expect_err("a market long needs stop_loss < take_profit");
            assert!(err.contains("inverted") && err.contains("market entry"), "{sl}/{tp}: {err}");
        }
        let short = WireBracketSpec { side: -1, stop_loss: 110.0, take_profit: 90.0, ..market };
        assert!(lowered(short.clone()).is_ok(), "a well-ordered market short: tp 90 < sl 110");
        for (sl, tp) in [(90.0, 110.0), (100.0, 100.0)] {
            let err = lowered(WireBracketSpec { stop_loss: sl, take_profit: tp, ..short.clone() })
                .expect_err("a market short needs take_profit < stop_loss");
            assert!(err.contains("inverted") && err.contains("market entry"), "{sl}/{tp}: {err}");
        }
        // The pinned limit: exits well-ordered between themselves but nowhere near any plausible
        // market are admitted, because this edge cannot know where the market is.
        assert!(
            lowered(WireBracketSpec { stop_loss: 1.0, take_profit: 2.0, ..spec_market() }).is_ok()
        );
    }

    /// [`spec`] with a MARKET entry.
    fn spec_market() -> WireBracketSpec {
        WireBracketSpec { entry_price: None, ..spec() }
    }

    /// The ONE engine a bracket on `venue` reaches, as the node publishes it: the block keyed by
    /// the bare venue id (the default account — the only shape `account_refusal` admits a bracket
    /// to), mounted on `symbol`.
    fn engine(venue: &str, symbol: &str) -> vike_core::VenueBlock {
        vike_core::VenueBlock {
            venue: venue.into(),
            route_key: venue.into(),
            symbol: symbol.into(),
            ..Default::default()
        }
    }

    /// [`spec`]'s binance PERP engine — the roster every value check below is judged against, so
    /// a refusal can only be the field's.
    fn perp_roster() -> Vec<vike_core::VenueBlock> {
        vec![engine("binance", "BTCUSDT.P")]
    }

    /// `b` judged as the node judges it past the account gate, against a one-engine roster.
    fn judged(b: WireBracketSpec, roster: &[vike_core::VenueBlock]) -> Option<String> {
        bracket_refusal(&WireCommand::Bracket(b), roster)
    }

    /// ⚠ **The dry run and the command share ONE bracket verdict.** `bracket_refusal` is the call
    /// `accept_command` makes at its step 1d AND the fourth step of both preview surfaces, so
    /// "Preview agrees with Command" is a property of there being one function. This table pins
    /// what that function refuses:
    ///
    /// * the VALUE shapes, with the reason `lower_command`'s own copy of the value check gives — so
    ///   a caller that lowers a frame without the gate is refused in the same words;
    /// * the ENGINE shapes, which `lower_command` cannot see (it holds no roster) and therefore
    ///   LOWERS: the refusal exists only because `accept_command` runs this same function before
    ///   it lowers. Each names what the engine trades.
    ///
    /// The wire-level twin, sending each shape as a Command AND a Preview to one live node and
    /// requiring the same sentence back, is `tests/daemon/venue_routing.rs`'s
    /// `the_dry_run_reports_the_same_bracket_refusals`.
    #[test]
    fn the_preview_and_the_command_share_one_bracket_verdict() {
        let value_shapes = [
            WireBracketSpec { side: 0, ..spec() },
            WireBracketSpec { qty: f64::NAN, ..spec() },
            WireBracketSpec { entry_price: Some(0.0), ..spec() },
            WireBracketSpec { stop_loss: f64::INFINITY, ..spec() },
            WireBracketSpec { stop_loss: 105.0, ..spec() },
            WireBracketSpec { side: -1, ..spec() },
            WireBracketSpec { stop_loss: 110.0, take_profit: 90.0, ..spec_market() },
        ];
        for b in value_shapes {
            let cmd = WireCommand::Bracket(b);
            let verdict = bracket_refusal(&cmd, &perp_roster());
            assert!(verdict.is_some(), "{cmd:?} must refuse");
            assert_eq!(verdict, lower_command(cmd.clone()).err(), "{cmd:?}: the same reason");
        }
        // (bracket, the one engine it reaches, what the refusal must name)
        let engine_shapes = [
            // ⚠ The I-1 scenario: the old refusal TOLD the operator to use `.P`, and a spot-mounted
            // engine then signed the `.P` bracket on the spot lane.
            (spec(), engine("binance", "BTCUSDT"), "`BTCUSDT`"),
            (
                WireBracketSpec { symbol: "BTCUSDT".into(), ..spec() },
                engine("binance", "BTCUSDT"),
                "spot lane",
            ),
            (
                WireBracketSpec { venue: "aster".into(), ..spec() },
                engine("aster", "BTCUSDT"),
                "spot lane",
            ),
            (
                WireBracketSpec { symbol: "ETHUSDT.P".into(), ..spec() },
                engine("binance", "BTCUSDT.P"),
                "`BTCUSDT.P`",
            ),
            (
                WireBracketSpec { venue: "okx".into(), symbol: "BTC-USDT".into(), ..spec() },
                engine("okx", "BTC-USDT-SWAP"),
                "`BTC-USDT-SWAP`",
            ),
        ];
        for (b, eng, names) in engine_shapes {
            let cmd = WireCommand::Bracket(b);
            let verdict = bracket_refusal(&cmd, std::slice::from_ref(&eng)).unwrap_or_else(|| {
                panic!("{cmd:?} on an engine trading `{}` must refuse", eng.symbol)
            });
            assert!(verdict.contains(names), "names what the engine trades: {verdict}");
            assert!(
                lower_command(cmd).is_ok(),
                "the FRAME is well-formed: only the engine refuses it"
            );
        }
        let ok = WireCommand::Bracket(spec());
        assert_eq!(bracket_refusal(&ok, &perp_roster()), None);
        assert!(lower_command(ok).is_ok());
        assert_eq!(
            bracket_refusal(&WireCommand::Cancel("c-1".into()), &[]),
            None,
            "not a bracket, whatever the roster"
        );
    }

    /// A lower-case `.p` is the SPOT lane, exactly as the adapter reads it
    /// (`vike_catalog::split_perp_at` strips `.P` only): an engine MOUNTED on `BTCUSDT.p` is a
    /// spot-lane engine and holds no bracket. And a bracket typed `BTCUSDT.p` against a perp engine
    /// is refused naming the engine's own `BTCUSDT.P` — the refusal suggests nothing it built
    /// itself, so it can never print `BTCUSDT.p.P`.
    #[test]
    fn a_lower_case_perp_suffix_is_the_spot_lane_and_the_refusal_names_the_real_one() {
        let err = judged(spec(), &[engine("binance", "BTCUSDT.p")]).expect("a `.p` engine is spot");
        assert!(err.contains("spot lane"), "{err}");
        let err = judged(WireBracketSpec { symbol: "BTCUSDT.p".into(), ..spec() }, &perp_roster())
            .expect("`.p` is not a symbol the perp engine trades");
        assert!(err.contains("`BTCUSDT.P`"), "names the engine's perp symbol: {err}");
        assert!(!err.contains(".p.P"), "{err}");
    }

    /// ⚠ **On binance and aster a bracket goes only to an engine mounted on the PERP lane — and it
    /// is the ENGINE's lane, never the frame's.** Their `vike_model::caps_for` rows list `stop` as
    /// the spot+perp UNION, while the spot order builder sends `type=STOP` with no stop price: the
    /// core preflight passes the stop-loss leg, the entry fills, and the venue rejects the stop at
    /// release — leaving the take-profit and an UNPROTECTED position the operator believes is
    /// protected. The adapter picks its lane ONCE, from the symbol its engine was mounted on, and
    /// signs every order on that symbol, so a `.P` FRAME against a spot-mounted engine is still a
    /// spot order: this check reads the engine's published block (`bracket_engine_refusal`). This
    /// test used to read "a `.P` symbol picks the perp lane", which was the defect.
    ///
    /// ⚠ A PAPER mount cannot show this: the paper client fills stops on either lane, which is why
    /// the refusal lives at the edge, in front of every mount, rather than in a caps column this cut
    /// does not add.
    #[test]
    fn a_bracket_to_a_binance_or_aster_spot_engine_is_refused_whatever_symbol_it_names() {
        for venue in ["binance", "aster"] {
            let spot = [engine(venue, "BTCUSDT")];
            for symbol in ["BTCUSDT.P", "BTCUSDT"] {
                let err = judged(
                    WireBracketSpec { venue: venue.into(), symbol: symbol.into(), ..spec() },
                    &spot,
                )
                .unwrap_or_else(|| panic!("{venue}/{symbol} on a spot engine must refuse"));
                assert!(err.contains("spot lane"), "{venue}/{symbol}: names the lane: {err}");
                assert!(err.contains("stop-loss"), "{venue}/{symbol}: what it cannot hold: {err}");
                assert!(
                    err.contains("trades `BTCUSDT`"),
                    "{venue}/{symbol}: what it trades: {err}"
                );
                assert!(
                    !err.contains("`BTCUSDT.P`"),
                    "{venue}/{symbol}: no perp symbol is suggested — this engine trades none: {err}"
                );
            }
            let perp = [engine(venue, "BTCUSDT.P")];
            assert_eq!(
                judged(WireBracketSpec { venue: venue.into(), ..spec() }, &perp),
                None,
                "{venue}: a perp engine holds the bracket"
            );
        }
        // A venue with no spot lane to confuse is judged on its symbol alone: bybit's exec is
        // linear-perp only, and hyperliquid's perps are bare coins.
        for (venue, symbol) in [("bybit", "BTCUSDT"), ("hyperliquid", "BTC")] {
            assert_eq!(
                judged(
                    WireBracketSpec { venue: venue.into(), symbol: symbol.into(), ..spec() },
                    &[engine(venue, symbol)]
                ),
                None,
                "{venue}/{symbol}"
            );
        }
    }

    /// ⚠ **A bracket goes to the ONE engine of its venue, so a symbol that engine does not trade is
    /// refused, naming what it does** — the node-side twin of the desktop's `order_dispatch::tradable`.
    /// Without it the adapter places the bracket on its engine's own instrument: okx's client is
    /// bound to one SWAP instrument, so a spot-named bracket would be placed on the SWAP. Every
    /// symbol the engine publishes counts (`VenueBlock::trades` reads the extra symbols too), and the
    /// engine's own symbol is the only perp `BASE.P` a refusal ever suggests.
    #[test]
    fn a_bracket_on_a_symbol_its_engine_does_not_trade_is_refused_naming_the_engines_symbols() {
        for (b, eng, named) in [
            (
                WireBracketSpec { symbol: "ETHUSDT.P".into(), ..spec() },
                engine("binance", "BTCUSDT.P"),
                "`BTCUSDT.P`",
            ),
            (
                WireBracketSpec { symbol: "BTCUSDT".into(), ..spec() },
                engine("binance", "BTCUSDT.P"),
                "`BTCUSDT.P`",
            ),
            (
                WireBracketSpec { venue: "okx".into(), symbol: "BTC-USDT".into(), ..spec() },
                engine("okx", "BTC-USDT-SWAP"),
                "`BTC-USDT-SWAP`",
            ),
            (
                WireBracketSpec { venue: "bybit".into(), symbol: "ETHUSDT".into(), ..spec() },
                engine("bybit", "BTCUSDT"),
                "`BTCUSDT`",
            ),
        ] {
            let asked = format!("`{}`", b.symbol);
            let err = judged(b, &[eng]).expect("a symbol the engine does not trade must refuse");
            assert!(err.contains(named), "names the engine's symbol: {err}");
            assert!(err.contains(&asked), "...and the one the bracket asked for: {err}");
        }
        // An extra symbol the engine publishes is traded.
        let mut wide = engine("binance", "BTCUSDT.P");
        wide.extra_symbols = vec!["ETHUSDT.P".into()];
        assert_eq!(judged(WireBracketSpec { symbol: "ETHUSDT.P".into(), ..spec() }, &[wide]), None);
    }

    /// ⚠ **No engine to judge is a refusal, never a pass.** `accept_command`'s account gate refuses
    /// an empty roster and a labelled-only venue first, in its own words; this pins that the engine
    /// half, called by any OTHER caller with an empty or stale roster, or handed a block that does
    /// not say what it trades, refuses rather than admitting a bracket nobody checked.
    #[test]
    fn a_bracket_with_no_published_engine_to_judge_it_is_refused() {
        let other_venue = [engine("bybit", "BTCUSDT")];
        let labelled = [vike_core::VenueBlock {
            route_key: "binance#ALT".into(),
            ..engine("binance", "BTCUSDT.P")
        }];
        let silent = [engine("binance", "")];
        for roster in [&[][..], &other_venue[..], &labelled[..], &silent[..]] {
            let err = judged(spec(), roster).unwrap_or_else(|| panic!("{roster:?} must refuse"));
            assert!(err.contains("cannot tell"), "{err}");
        }
    }
}
