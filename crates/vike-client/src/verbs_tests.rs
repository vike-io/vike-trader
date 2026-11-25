use super::*;

// NOTE: no test here sets the VIKE_MAX_ORDER_* env vars (env writes race across the parallel
// test harness); the cap-less path is what these pin, same as the mcp/trade preview tests.

fn submit_command(qty: f64, price: Option<f64>) -> CommandWire {
    CommandWire::Submit(OrderRequestWire {
        order_client_id: String::new(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty,
        order_type: if price.is_some() { "limit".into() } else { "market".into() },
        price,
        trigger_price: None,
        reduce_only: false,
        account: None,
        time_in_force: None,
        gtd_expiry: None,
        display_quantity: None,
        release: None,
        trail: None,
        limit_offset: None,
        peg: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
    })
}

/// The guardrail over the ordinary instrument (`Linear`, 1.0). What another value does and where a
/// value comes from is `crate::addressed`'s suite.
fn check(cmd: &CommandWire, capabilities: GuardrailCapabilities) -> Guardrail {
    guardrail_check(cmd, capabilities, Ok(ContractValue::default()), false)
}

// ---- the `old → new` a settings write previews (`SettingChange`) ------------------------------

/// A node's `SettingsShow` answer carrying the given `(key, value, origin)` rows.
fn show(rows: &[(&str, &str, &str)]) -> SettingsShowWire {
    SettingsShowWire {
        settings_dir: Some("/srv/vike-<unit>/settings".into()),
        rows: rows
            .iter()
            .map(|(key, value, origin)| vike_tradehub_client::wire::SettingsShowRowWire {
                section: key.split('.').next().unwrap_or_default().to_string(),
                key: (*key).to_string(),
                value: (*value).to_string(),
                origin: (*origin).to_string(),
                read_by: "yes".into(),
            })
            .collect(),
        refusal: None,
    }
}

/// **Rows beside a refusal are not what the node holds.** A store it could not read, or whose seal is
/// broken, leaves every key at its compiled-in default and `origin: default`, a ceiling reading as
/// absent, so the OLD side of `old → new` is unread and says why, however plausible the row.
#[test]
fn a_nodes_refusal_makes_the_old_value_unread() {
    let mut node = show(&[("policy.x", "", "default")]);
    let why = "the settings database could not be read (database is locked)";
    node.refusal = Some(why.to_string());
    let change = SettingChange::from_show("policy.x", "250", Ok(&node));
    assert_eq!(change.old, OldSetting::Unread(why.to_string()));
    let json = change.to_json();
    assert_eq!(json["old"], serde_json::Value::Null);
    assert_eq!(json["old_origin"], serde_json::Value::Null);
    assert_eq!(json["old_unread"], why);
    assert!(change.line().contains(why), "{}", change.line());
}

/// **The OLD value is the NODE's row for exactly this key**, and the line is `key: old → new` —
/// the pair 0086 point 7 names as what guards a live limit now that no key is retyped.
#[test]
fn a_setting_change_reads_the_old_value_off_the_nodes_row_for_that_key() {
    let node =
        show(&[("config.tradehub_addr", "0.0.0.0:17879", "db"), ("policy.x", "100.0", "db")]);
    let change = SettingChange::from_show("policy.x", "250", Ok(&node));
    assert_eq!(
        change.old,
        OldSetting::Read { value: "100.0".into(), origin: "db".into() },
        "the row for THIS key, not the first row"
    );
    assert_eq!(change.line(), "policy.x: 100.0 → 250");
    let json = change.to_json();
    assert_eq!(json["old"], json!("100.0"));
    assert_eq!(json["old_origin"], json!("db"));
    assert_eq!(json["old_unread"], Value::Null, "a value was read, so there is no reason to give");
    assert_eq!(json["new"], json!("250"));
    assert_eq!(json["line"], json!("policy.x: 100.0 → 250"));
    assert_eq!(change.env_shadow(), None, "a row-set value follows the row");
}

/// An UNSET key renders as `(unset)` rather than as an empty slot a reader would take for a typo,
/// and a key the node does not carry says so rather than showing an old value it does not have.
#[test]
fn an_unset_or_unknown_key_says_what_it_is_rather_than_showing_nothing() {
    let node = show(&[("policy.x", "", "default")]);
    assert_eq!(
        SettingChange::from_show("policy.x", "5", Ok(&node)).line(),
        "policy.x: (unset) → 5"
    );

    let unknown = SettingChange::from_show("policy.nope", "5", Ok(&node));
    assert_eq!(unknown.old, OldSetting::NoSuchKey);
    assert!(unknown.line().contains("no such key"), "{}", unknown.line());
    assert_eq!(unknown.to_json()["old"], Value::Null, "no row, so no old value — never `\"\"`");
    assert!(unknown.to_json()["old_unread"].is_string(), "…and the payload says why");
}

/// **A node that could not be asked yields NO old value, and every rendering says so.** A
/// fabricated one — an empty string, a default — is the one field an agent would relay to the
/// owner as fact.
#[test]
fn an_unread_old_value_is_null_and_names_why() {
    let change = SettingChange::from_show("policy.x", "250", Err("the node is down".into()));
    assert_eq!(change.old, OldSetting::Unread("the node is down".into()));
    assert_eq!(change.line(), "policy.x: (current value not read: the node is down) → 250");
    let json = change.to_json();
    assert_eq!(json["old"], Value::Null);
    assert_eq!(json["old_origin"], Value::Null);
    assert_eq!(json["old_unread"], json!("the node is down"));
}

/// **An environment variable outranks a settings row** (0086's first declared residual), so a key
/// whose current value one sets will NOT follow the row being written. The change names the
/// variable so the surface can say so; a row-set or default value names none.
#[test]
fn a_value_an_environment_variable_sets_is_named_as_shadowing_the_row() {
    let node = show(&[("config.tradehub_addr", "0.0.0.0:17879", "env:VIKE_TRADEHUB_ADDR")]);
    let change = SettingChange::from_show("config.tradehub_addr", "127.0.0.1:17879", Ok(&node));
    assert_eq!(change.env_shadow(), Some("env:VIKE_TRADEHUB_ADDR"));
    assert_eq!(change.to_json()["old_origin"], json!("env:VIKE_TRADEHUB_ADDR"));
    let default = show(&[("config.tradehub_addr", "127.0.0.1:17879", "default")]);
    let change = SettingChange::from_show("config.tradehub_addr", "x", Ok(&default));
    assert_eq!(change.env_shadow(), None);
}

// ---- the order client id mint (the node REFUSES a remote submit with an empty one) -------------------

/// **THE defect.** A `Submit` built by either surface leaves `order_client_id` empty; the mint
/// must fill it with a NON-EMPTY, venue-valid id — otherwise the node answers "remote submit
/// requires a pre-minted order_client_id" and nothing is ever placed.
#[test]
fn an_empty_submit_order_client_id_is_minted_and_is_venue_valid() {
    let mut minter = order_client_id_minter(OrderClientIdSource::Cli);
    let filled =
        fill_order_client_id(submit_command(1.0, Some(100.0)), &mut minter, None, any_node);
    let CommandWire::Submit(o) = filled else { panic!("still a Submit") };
    assert!(
        !o.order_client_id.is_empty(),
        "an empty order client id is exactly what the node refuses"
    );
    assert!(
        vike_model::is_valid_crypto_order_client_id(&o.order_client_id),
        "the minted id must survive the node, the core and the venue edge: {:?}",
        o.order_client_id
    );
    assert_eq!(o.order_client_id.len(), 21, "a plain full-shape v2 id: {:?}", o.order_client_id);
}

/// Successive submits in one session get DIFFERENT ids. The node refuses a repeated id (it used to
/// book nothing the second time, in silence), so an order surface cannot reuse one.
#[test]
fn every_minted_order_client_id_in_a_session_is_distinct() {
    let mut minter = order_client_id_minter(OrderClientIdSource::Cli);
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..500 {
        let CommandWire::Submit(o) =
            fill_order_client_id(submit_command(1.0, Some(100.0)), &mut minter, None, any_node)
        else {
            panic!("still a Submit")
        };
        assert!(
            seen.insert(o.order_client_id.clone()),
            "duplicate order client id {:?}",
            o.order_client_id
        );
    }
}

/// Two sessions (two `vike-cli trade` processes, or one restarted while an order still rests)
/// must not mint the same id — the session's base is what buys that.
#[test]
fn two_sessions_do_not_share_an_order_client_id_prefix() {
    let (mut a, mut b) = (
        order_client_id_minter(OrderClientIdSource::Cli),
        order_client_id_minter(OrderClientIdSource::Cli),
    );
    let first = |m: &mut OrderClientIdMinter| submit_id(m, None, submit_command(1.0, Some(100.0)));
    assert_ne!(first(&mut a), first(&mut b), "two sessions minted the same first order client id");
}

/// An EXPLICIT id is never overwritten (the `--order-client-id` / `order_client_id` override path), and no
/// non-`Submit` verb is touched by the mint.
#[test]
fn the_mint_never_overwrites_an_explicit_id_or_touches_another_verb() {
    let mut minter = order_client_id_minter(OrderClientIdSource::Cli);
    let mut pinned = submit_command(1.0, Some(100.0));
    if let CommandWire::Submit(o) = &mut pinned {
        o.order_client_id = "operatorPinned7".into();
    }
    assert_eq!(fill_order_client_id(pinned.clone(), &mut minter, None, any_node), pinned);

    for cmd in [
        CommandWire::Cancel("c-1".into()),
        CommandWire::MarketExit { venue: None, account: None },
        CommandWire::MassCancel { venue: None, symbol: None, account: None },
    ] {
        assert_eq!(fill_order_client_id(cmd.clone(), &mut minter, None, any_node), cmd);
    }
}

// ---- the v2 order client id: what the client mints and how it reads one back (decision 0270) -------------------

/// A node that takes every command, a stem bracket included.
fn any_node(_: &CommandWire) -> bool {
    true
}

/// A node from before `bracket-stem`: it takes every command but a bracket naming a stem.
fn stemless_node(cmd: &CommandWire) -> bool {
    !matches!(cmd, CommandWire::Bracket(OrderBracketWire { order_client_id: Some(_), .. }))
}

/// The id `minter` gives `cmd` (a submit) against `frame`.
fn submit_id(
    minter: &mut OrderClientIdMinter,
    frame: Option<&CoreSnapshotWire>,
    cmd: CommandWire,
) -> String {
    let CommandWire::Submit(o) = fill_order_client_id(cmd, minter, frame, any_node) else {
        panic!("still a Submit")
    };
    o.order_client_id
}

/// `submit_command`'s order on `venue#account` (a bare `venue` for the default account), `symbol`, `side`.
fn order_on(key: &str, symbol: &str, side: i32, reduce_only: bool) -> CommandWire {
    let CommandWire::Submit(o) = submit_command(1.0, Some(100.0)) else { unreachable!("a submit") };
    let (venue, account) = key.split_once('#').map_or((key, None), |(v, a)| (v, Some(a)));
    CommandWire::Submit(OrderRequestWire {
        venue: venue.into(),
        account: account.map(str::to_string),
        symbol: symbol.into(),
        side,
        reduce_only,
        ..o
    })
}

/// A frame of a Binance account (#7, compact shape `U` = 12, `R` up to 9) and a Bybit one (#4, the full shape), each
/// trading `BTCUSDT`, the Bybit one long 2. Both named, so no lane twin moves an order.
fn two_venue_frame() -> CoreSnapshotWire {
    let mut frame = frame_of(&["binance#ALT", "bybit#MAIN"]);
    let compact = OrderClientIdShape { account: false, u_width: 12, max_gen: 9, tag: false };
    let full = OrderClientIdShape { account: true, u_width: 12, max_gen: 99, tag: false };
    for (block, code, shape) in [(0, "07", compact), (1, "04", full)] {
        let block = &mut frame.venues[block];
        block.account_code = Some(code.into());
        block.order_client_id_shape = Some(shape);
        block.symbols = vec!["BTCUSDT".into(), "BTCUSDT.P".into()];
    }
    // bybit's long is on its linear perpetual (decision 0290's `BTCUSDT.P`; the bare `BTCUSDT` is the spot pair).
    frame.venues[1].positions = vec![serde_json::from_value(json!({
        "venue": "bybit", "symbol": "BTCUSDT.P", "position_side": "BOTH", "size": 2.0, "average_price": 100.0,
        "unrealized": 0.0, "leverage": 0.0, "liquidation_price": 0.0,
    }))
    .expect("a position")];
    frame
}

/// The parts of a v2 id, or a panic naming it.
fn v2(id: &str) -> vike_model::orders::client_id::OrderClientIdParts<'_> {
    match parse_order_client_id(id) {
        Some(OrderClientIdReading::V2(p)) => p,
        other => panic!("{id:?} is no v2 id: {other:?}"),
    }
}

/// **Each surface's ids say which surface made them**: the source letter its minter was built with, no strategy, and
/// with no frame the full shape with account `00` and the intent of a flat book.
#[test]
fn each_surface_mints_its_own_source_letter() {
    for (source, letter) in [
        (OrderClientIdSource::Cli, "C"),
        (OrderClientIdSource::Mcp, "M"),
        (OrderClientIdSource::Telegram, "T"),
    ] {
        let mut minter = order_client_id_minter(source);
        let id = submit_id(&mut minter, None, submit_command(1.0, Some(100.0)));
        assert!(id.starts_with(&format!("{letter}000000")), "{id}");
        let p = v2(&id);
        assert_eq!(p.src, source);
        assert_eq!(
            (p.strat, p.account),
            (OrderClientIdStrategyCode::NONE, Some(OrderClientIdAccountCode::NONE)),
            "{id}"
        );
        assert_eq!(p.intent, OrderClientIdIntent::OpenLong, "a buy on a flat book: {id}");
        assert_eq!((p.role, p.generation), (None, 0), "{id}");
    }
}

/// **The shape and the account are the TARGET block's**: one minter, one sequence, a Binance account's compact id
/// (no account code) beside a Bybit account's full one carrying `04`, every id distinct. A block that says nothing
/// mints the full shape with `00`.
#[test]
fn a_binance_target_gets_the_compact_shape_and_a_bybit_one_the_full_shape() {
    let frame = two_venue_frame();
    let mut minter = order_client_id_minter(OrderClientIdSource::Cli);
    let binance =
        submit_id(&mut minter, Some(&frame), order_on("binance#ALT", "BTCUSDT.P", 1, false));
    let bybit = submit_id(&mut minter, Some(&frame), order_on("bybit#MAIN", "BTCUSDT", 1, false));
    assert_eq!((binance.len(), v2(&binance).account), (19, None), "compact: {binance}");
    assert_eq!(bybit.len(), 21, "{bybit}");
    assert_eq!(v2(&bybit).account, OrderClientIdAccountCode::parse("04"), "{bybit}");
    assert_ne!(v2(&binance).unique, v2(&bybit).unique, "one sequence, two numbers");

    let mut silent = frame.clone();
    silent.venues[1].account_code = None;
    silent.venues[1].order_client_id_shape = None;
    let unsaid = submit_id(&mut minter, Some(&silent), order_on("bybit#MAIN", "BTCUSDT", 1, false));
    assert_eq!(
        (unsaid.len(), v2(&unsaid).account),
        (21, Some(OrderClientIdAccountCode::NONE)),
        "{unsaid}"
    );
}

/// **A shape the parser could not read back is no reason to end the client**: it mints the full shape, as for a
/// block that says none.
#[test]
fn a_shape_no_parser_reads_mints_the_full_shape() {
    let mut frame = two_venue_frame();
    for bad in [
        OrderClientIdShape { account: true, u_width: 11, max_gen: 99, tag: false },
        OrderClientIdShape { account: false, u_width: 13, max_gen: 9, tag: false },
        OrderClientIdShape { account: false, u_width: 12, max_gen: 100, tag: false },
    ] {
        frame.venues[0].order_client_id_shape = Some(bad);
        let mut minter = order_client_id_minter(OrderClientIdSource::Cli);
        let id =
            submit_id(&mut minter, Some(&frame), order_on("binance#ALT", "BTCUSDT.P", 1, false));
        assert_eq!(
            (id.len(), v2(&id).account),
            (21, OrderClientIdAccountCode::parse("07")),
            "{bad:?}: {id}"
        );
    }
}

/// **The intent is the frame's**: against Bybit's long, a plain sell closes it and may open a short (`FL`), a
/// reduce-only sell only closes (`CL`), a buy adds (`OL`); against the flat Binance account a sell opens short (`OS`)
/// and a reduce-only buy closes a short (`CS`).
#[test]
fn the_intent_reads_the_position_the_frame_shows() {
    let frame = two_venue_frame();
    let mut minter = order_client_id_minter(OrderClientIdSource::Mcp);
    for (key, symbol, side, reduce_only, intent) in [
        ("bybit#MAIN", "BTCUSDT.P", -1, false, OrderClientIdIntent::PlainCloseLong),
        ("bybit#MAIN", "btcusdt.p", -1, true, OrderClientIdIntent::CloseLong),
        ("bybit#MAIN", "BTCUSDT.P", 1, false, OrderClientIdIntent::OpenLong),
        ("binance#ALT", "BTCUSDT.P", -1, false, OrderClientIdIntent::OpenShort),
        ("binance#ALT", "BTCUSDT.P", 1, true, OrderClientIdIntent::CloseShort),
    ] {
        let id = submit_id(&mut minter, Some(&frame), order_on(key, symbol, side, reduce_only));
        assert_eq!(
            v2(&id).intent,
            intent,
            "{key} {symbol} side {side} reduce-only {reduce_only}: {id}"
        );
    }
}

/// **On spot a sell only closes a long (`CL`), never `OS` or `FL`** (the owner's ruling): a plain sell of binance's
/// spot `BTCUSDT` against a long, with the frame or with none, and each leg of a spot OCO sell, are `CL`, while a sell
/// of the flat perp `BTCUSDT.P` opens a short (`OS`). A polymarket share, on a venue that books only `Cash`, reads
/// the same way; a spot buy is `OL`.
#[test]
fn a_spot_sell_closes_a_long_and_a_flat_perp_sell_opens_a_short() {
    let mut frame = frame_of(&["binance#SPOT", "binance#ALT"]);
    frame.venues[0].symbols = vec!["BTCUSDT".into()];
    frame.venues[1].symbols = vec!["BTCUSDT.P".into()];
    frame.venues[0].positions = vec![serde_json::from_value(json!({
        "venue": "binance", "symbol": "BTCUSDT", "position_side": "BOTH", "size": 2.0, "average_price": 100.0,
        "unrealized": 0.0, "leverage": 0.0, "liquidation_price": 0.0,
    }))
    .expect("a position")];
    let mut minter = order_client_id_minter(OrderClientIdSource::Cli);
    for (key, symbol, side, seen, intent) in [
        ("binance#SPOT", "BTCUSDT", -1, Some(&frame), OrderClientIdIntent::CloseLong),
        ("binance#SPOT", "BTCUSDT", -1, None, OrderClientIdIntent::CloseLong),
        ("binance#SPOT", "BTCUSDT", 1, Some(&frame), OrderClientIdIntent::OpenLong),
        ("binance#ALT", "BTCUSDT.P", -1, Some(&frame), OrderClientIdIntent::OpenShort),
        ("polymarket", "0xabc", -1, None, OrderClientIdIntent::CloseLong),
    ] {
        let id = submit_id(&mut minter, seen, order_on(key, symbol, side, false));
        assert_eq!(v2(&id).intent, intent, "{key} {symbol} side {side}: {id}");
        assert!(id.ends_with(intent.code()), "{id}");
    }
    let head = OrderListHead {
        venue: "binance".into(),
        account: Some("SPOT".into()),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 1.0,
    };
    let oco = oco_list(head, 110.0, Some(90.0), None, None, false).expect("an OCO");
    let CommandWire::OrderList(list) =
        fill_order_client_id(CommandWire::OrderList(oco), &mut minter, Some(&frame), any_node)
    else {
        panic!("an order list stays one")
    };
    for leg in &list.legs {
        let id = leg.order_client_id.as_deref().expect("every leg");
        assert_eq!(v2(id).intent, OrderClientIdIntent::CloseLong, "a spot list's sell: {id}");
    }
}

/// **A list shares ONE stem**: the ladder's legs are the stem plus each one's intent and role (entry `OLEN`, the stop
/// `CLSL`, the targets `CLTP1` and `CLTP2`), so the group costs one sequence number; a leg the caller named keeps its
/// id and the rest still share a stem.
#[test]
fn an_order_list_shares_one_stem() {
    let frame = two_venue_frame();
    let head = OrderListHead {
        venue: "bybit".into(),
        account: Some("MAIN".into()),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 3.0,
    };
    let ladder =
        ladder_list(head, None, Some(90.0), None, &[(110.0, 1.0), (120.0, 2.0)]).expect("a ladder");
    let mut minter = order_client_id_minter(OrderClientIdSource::Cli);
    let list = |minter: &mut OrderClientIdMinter, list: OrderListWire| match fill_order_client_id(
        CommandWire::OrderList(list),
        minter,
        Some(&frame),
        any_node,
    ) {
        CommandWire::OrderList(list) => list
            .legs
            .into_iter()
            .map(|leg| leg.order_client_id.expect("every leg"))
            .collect::<Vec<_>>(),
        other => panic!("an order list stays one: {other:?}"),
    };
    let ids = list(&mut minter, ladder.clone());
    let stem = v2(&ids[0]).stem.to_string();
    let tails: Vec<&str> = ids.iter().map(|id| &id[stem.len()..]).collect();
    assert_eq!(tails, ["OLEN", "CLSL", "CLTP1", "CLTP2"], "{ids:?}");
    assert!(ids.iter().all(|id| v2(id).stem == stem), "one stem: {ids:?}");
    let next = submit_id(&mut minter, Some(&frame), order_on("bybit#MAIN", "BTCUSDT", 1, false));
    assert_ne!(v2(&next).unique, v2(&ids[0]).unique, "the next order takes the next number");

    let mut part = ladder;
    part.legs[1].order_client_id = Some("mystop1".into());
    let ids = list(&mut minter, part);
    assert_eq!(ids[1], "mystop1", "a named leg keeps its id");
    let stem = v2(&ids[0]).stem;
    assert!(ids[2].starts_with(stem) && ids[3].starts_with(stem), "{ids:?}");
}

/// **A bracket gets its stem only from a node that takes one, and keeps it on a resend**: the stem is a v2 id
/// through its unique part, on the target account's shape; filling the filled bracket again changes nothing, so a
/// resend names the same group. A node from before `bracket-stem` gets the bracket it always got, with no stem.
#[test]
fn a_bracket_stem_is_minted_for_a_node_that_takes_it_and_kept_on_a_resend() {
    let frame = two_venue_frame();
    let bracket = CommandWire::Bracket(OrderBracketWire {
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        entry_price: None,
        stop_loss: 90.0,
        take_profit: 110.0,
        account: Some("MAIN".into()),
        order_client_id: None,
    });
    let mut minter = order_client_id_minter(OrderClientIdSource::Cli);
    let old = fill_order_client_id(bracket.clone(), &mut minter, Some(&frame), stemless_node);
    assert_eq!(old, bracket, "an old node gets the old bracket");

    let sent = fill_order_client_id(bracket, &mut minter, Some(&frame), any_node);
    let CommandWire::Bracket(spec) = &sent else { panic!("a bracket stays one") };
    let stem = spec.order_client_id.as_deref().expect("a stem");
    assert!(vike_model::orders::client_id::OrderClientIdStem::given(stem).is_some(), "{stem}");
    assert!(stem.starts_with("C000004"), "the CLI, no strategy, account #4: {stem}");
    assert_eq!(stem.len(), 19, "src + strategy + account + U: {stem}");
    let resent = fill_order_client_id(sent.clone(), &mut minter, Some(&frame), any_node);
    assert_eq!(resent, sent, "a resend names the same stem");
    assert_eq!(bracket_stem_refusal(stem), None, "the stem a send showed is what its retry pins");
}

/// **A pinned bracket stem is judged at parse**: outside the charset it is the submit's refusal, and inside it but no
/// group stem (a whole leg id, a typed word) the stem's own, which the core would otherwise give only after the node's
/// answer.
#[test]
fn a_pinned_bracket_stem_outside_the_charset_or_the_stem_shape_is_refused() {
    use crate::refusals::OrderGateRefusalId;
    let refused = |stem: &str| bracket_stem_refusal(stem).map(|refusal| refusal.id());
    assert_eq!(refused("C0000-bad"), Some(OrderGateRefusalId::OrderClientIdCharset));
    for stem in ["ABC", "S00A307k3x9q2m7d1p4OL"] {
        assert_eq!(refused(stem), Some(OrderGateRefusalId::BracketStemShape), "{stem}");
    }
    assert_eq!(refused("S00A307k3x9q2m7d1p4"), None, "a v2 stem passes");
}

/// **Decode names what the node knows**: the account label behind the id's account code, the strategy behind its
/// strategy code, the other legs of its group resting now (a filled one is not), each code and the words; without a
/// frame the codes and the words alone. An id minted before v2 and one no form of ours fits each say what they are.
#[test]
fn decode_takes_its_names_from_the_frame() {
    let id = "S00A30Bq7w2e9r4t1y6CSSLR2";
    let mut frame = frame_of(&["bybit#MAIN", "okx"]);
    frame.venues[0].account_code = Some("0B".into());
    frame.venues[1].account_code = Some("0C".into());
    let order = |order_client_id: &str,
                 status: &str|
     -> vike_tradehub_client::wire::OrderViewWire {
        serde_json::from_value(json!({
            "order_client_id": order_client_id, "venue": "bybit", "account": "MAIN", "symbol": "BTCUSDT", "side": -1,
            "qty": 1.0, "order_type": "limit", "price": 110.0, "trigger_price": null, "status": status,
            "venue_order_id": null, "filled_qty": 0.0, "average_fill_price": 0.0,
        }))
        .expect("an order row")
    };
    frame.orders = vec![
        order("S00A30Bq7w2e9r4t1y6OLEN", "FILLED"),
        order("S00A30Bq7w2e9r4t1y6CSTP1", "ACCEPTED"),
        order("S00A30Bq7w2e9r4t1y7CSTP1", "ACCEPTED"),
        order(id, "ACCEPTED"),
    ];
    let mount: MountViewWire = serde_json::from_value(json!({
        "strategy": "spread_maker", "params": "", "live": false, "mount_id": "bybit__BTCUSDT__1m",
        "order_client_id_code": "00A3",
    }))
    .expect("a mount row");

    let named = decode_order_client_id(id, Some(&frame), std::slice::from_ref(&mount));
    assert_eq!(named["form"], "v2");
    assert_eq!(named["source"], "S");
    assert_eq!(
        (&named["strategy"], &named["strategy_name"]),
        (&json!("00A3"), &json!("spread_maker"))
    );
    assert_eq!(named["mount_id"], "bybit__BTCUSDT__1m");
    assert_eq!((&named["account"], &named["account_number"]), (&json!("0B"), &json!(11)));
    assert_eq!(
        (&named["account_label"], &named["route_key"]),
        (&json!("MAIN"), &json!("bybit#MAIN"))
    );
    assert_eq!((&named["intent"], &named["role"]), (&json!("CS"), &json!("SL")));
    assert_eq!(
        (&named["group"], &named["replacement"]),
        (&json!("S00A30Bq7w2e9r4t1y6"), &json!(2))
    );
    assert_eq!(named["unique"], "q7w2e9r4t1y6");
    assert_eq!(
        named["siblings"],
        json!(["S00A30Bq7w2e9r4t1y6CSTP1"]),
        "resting legs of the group only"
    );
    let words = named["words"].as_str().expect("words");
    for said in [
        "strategy (auto)",
        "strategy 00A3 spread_maker",
        "account #11 MAIN",
        "stop of group q7w2e9r4t1y6",
        "replacement 2",
    ] {
        assert!(words.contains(said), "{said:?} not in {words}");
    }

    let bare = decode_order_client_id(id, None, &[]);
    assert_eq!((&bare["strategy_name"], &bare["account_label"]), (&Value::Null, &Value::Null));
    assert_eq!(bare["siblings"], json!([]));
    assert!(bare["words"].as_str().expect("words").contains("account #11 ·"), "{}", bare["words"]);

    let default = decode_order_client_id("C00000Cq7w2e9r4t1y6OL", Some(&frame), &[]);
    assert_eq!(default["account_label"], "(no label)", "the default account's block has none");
    assert_eq!(default["role"], Value::Null);
    assert_eq!(decode_order_client_id("deadbeef17", Some(&frame), &[])["form"], "v1");
    assert_eq!(decode_order_client_id("westVdeadbeef17", None, &[])["origin"], "west");
    assert_eq!(decode_order_client_id("EXT-1", Some(&frame), &[])["form"], "other");
}

/// **An OLDER node's Debug-spelled status reads as the desktop reads it** (the shared
/// `vike_tradehub_client::observe_decode::order_status_of`): its `Accepted` leg is a resting sibling and its
/// `Filled` one is not. Before the shared reading both were unknown words, so the group listed no sibling.
#[test]
fn decode_reads_an_older_nodes_status_words_as_the_desktop_does() {
    let id = "S00A30Bq7w2e9r4t1y6CSSLR2";
    let mut frame = frame_of(&["bybit#MAIN"]);
    let order = |order_client_id: &str,
                 status: &str|
     -> vike_tradehub_client::wire::OrderViewWire {
        serde_json::from_value(json!({
            "order_client_id": order_client_id, "venue": "bybit", "account": "MAIN", "symbol": "BTCUSDT", "side": -1,
            "qty": 1.0, "order_type": "limit", "price": 110.0, "trigger_price": null, "status": status,
            "venue_order_id": null, "filled_qty": 0.0, "average_fill_price": 0.0,
        }))
        .expect("an order row")
    };
    frame.orders = vec![
        order("S00A30Bq7w2e9r4t1y6OLEN", "Filled"),
        order("S00A30Bq7w2e9r4t1y6CSTP1", "Accepted"),
        order(id, "Accepted"),
    ];
    assert_eq!(
        decode_order_client_id(id, Some(&frame), &[])["siblings"],
        json!(["S00A30Bq7w2e9r4t1y6CSTP1"])
    );
}

#[test]
fn guardrail_json_and_line_render_the_same_check() {
    // No caps at all: a priced submit is within limits, notional = qty * price.
    let g = check(&submit_command(0.5, Some(100.0)), GuardrailCapabilities::default());
    let j = g.to_json();
    assert_eq!(j["qty"], 0.5);
    assert_eq!(j["notional"], 50.0);
    assert_eq!(j["within_limits"], true);
    let line = g.line();
    assert!(line.contains("qty=0.5") && line.contains("notional=50"), "{line}");
    assert!(line.contains("within limits"), "{line}");
}

/// The POLICY ceiling reaching the advisory check — the "value flows" half of Phase 5's
/// contract at this surface. PURE now, so it needs no process-env mutation to test (which is
/// unsound from parallel test threads and is why this was never covered before).
#[test]
fn the_policy_ceiling_is_what_the_notional_check_uses() {
    let capabilities = GuardrailCapabilities { max_qty: None, max_notional: Some(40.0) };
    let g = check(&submit_command(0.5, Some(100.0)), capabilities); // notional 50 > 40
    assert_eq!(g.to_json()["max_notional"], 40.0);
    assert_eq!(g.to_json()["within_limits"], false);
    assert!(g.line().contains("OVER LIMIT"), "{}", g.line());
    // The caps are this box's: `preferences.max_order_qty` has no enforcing twin, and this box's
    // ceiling may sit below the node's, so the line promises no rejection.
    assert!(!g.line().contains("will reject"), "{}", g.line());
    assert!(g.line().contains("this box's own caps"), "{}", g.line());

    // …and no ceiling (no `policy.max_notional_per_order` row) is today's behaviour: nothing is
    // over the limit.
    let g = check(&submit_command(0.5, Some(100.0)), GuardrailCapabilities::default());
    assert_eq!(g.to_json()["max_notional"], Value::Null);
    assert_eq!(g.to_json()["within_limits"], true);
}

// -- the preview's number rendering -----------------------------------------------------------

/// **The reported line, and the reason this renderer exists.**
///
/// `0.4 * 3` is exactly `1.2000000000000002` as an `f64`, and `to_string` prints all of it. The
/// guardrail line therefore read
/// `guardrail: qty=3 notional=1.2000000000000002 max_qty=- max_notional=42.5` for an order the
/// operator typed as three at forty cents.
#[test]
fn the_preview_renders_a_typed_number_the_way_it_was_typed() {
    let capabilities = GuardrailCapabilities { max_qty: None, max_notional: Some(42.5) };
    let line = check(&submit_command(3.0, Some(0.4)), capabilities).line();
    assert_eq!(
        line, "guardrail: qty=3 notional=1.2 max_qty=- max_notional=42.5 → within limits",
        "the float tail must not reach the preview"
    );
    // The raw value really is the ugly one — this is a RENDERING fix, not a computation change.
    assert_eq!((0.4f64 * 3.0).to_string(), "1.2000000000000002");
}

/// **A notional may never be shortened into looking like it clears the ceiling it exceeds.**
///
/// The display drops noise; it does not get to soften a verdict. Without the floor rule this
/// line would read `notional=42.5 max_notional=42.5 → OVER LIMIT`, i.e. a verdict its own two
/// numbers appear to contradict — precisely the "output that communicates the opposite of the
/// truth" this whole change is about.
#[test]
fn a_notional_is_never_rendered_below_the_ceiling_it_exceeds() {
    // The next representable f64 above the cap — computed, not written as a literal, so the
    // "one ulp over" intent is in the code rather than in a digit count a reader has to verify
    // (and so clippy's `excessive_precision` has nothing to object to).
    let over = f64::from_bits(42.5_f64.to_bits() + 1);
    assert!(over > 42.5, "the premise: this f64 is genuinely over");
    assert_eq!(format_number(over), "42.5", "…and shortening alone would hide that");
    assert_eq!(fmt_number_over(over, Some(42.5)), over.to_string(), "so the exact value is shown");

    // A value genuinely UNDER the cap is rendered normally — the rule fires only when hiding
    // the difference would contradict the verdict.
    assert_eq!(fmt_number_over(1.2000000000000002, Some(42.5)), "1.2");
    // …and with no cap there is nothing to understate against.
    assert_eq!(fmt_number_over(over, None), "42.5");
}

/// The rendering is DISPLAY ONLY: `within_limits` is computed from the raw `f64`s, so no
/// tolerance in the renderer can change a verdict.
#[test]
fn rendering_never_moves_the_verdict() {
    let capabilities = GuardrailCapabilities { max_qty: None, max_notional: Some(42.5) };
    // qty is one ulp above 1.0, so qty * price lands just over the ceiling — and the check must
    // say so, however the line renders it.
    let qty = f64::from_bits(1.0_f64.to_bits() + 1);
    let g = check(&submit_command(qty, Some(42.5)), capabilities);
    assert_eq!(g.to_json()["within_limits"], false, "the raw comparison decides");
    assert!(g.line().contains("OVER LIMIT"), "{}", g.line());
    // The JSON keeps exact numbers — a machine reader must never get a formatted string.
    assert!(g.to_json()["notional"].is_number(), "{}", g.to_json());
}

// -- the order client id charset, on the verbs that do not mint one --------------------------------------

/// `cancel`/`modify` name an id that must ALREADY exist; `submit` mints one. So the charset is
/// stated on all three and enforced on one. See [`order_client_id_charset_warning`] for the argument.
#[test]
fn an_order_client_id_no_minter_could_have_produced_warns_on_cancel_and_modify() {
    for cmd in [
        CommandWire::Cancel("MYPINNED-001".into()),
        CommandWire::Modify {
            order_client_id: "!!!bad***order client id".into(),
            new_qty: Some(2.0),
            new_price: None,
            new_trigger_price: None,
        },
    ] {
        let w = order_client_id_charset_warning(&cmd)
            .unwrap_or_else(|| panic!("an unmintable order client id must warn: {cmd:?}"));
        assert!(w.contains(ORDER_CLIENT_ID_CHARSET), "the warning must state the charset: {w}");
        assert!(
            w.contains("sending it anyway"),
            "it must be clear the command is still sent — cancel stays fire-and-forget: {w}"
        );
    }
}

/// A conforming id is silent, and a `Submit` never warns at all: by the time one reaches the
/// preview its order client id has been through `parse_submit`'s refusal or `fill_order_client_id`'s mint.
#[test]
fn a_conforming_order_client_id_and_every_submit_are_silent() {
    assert_eq!(order_client_id_charset_warning(&CommandWire::Cancel("baa0ec7d00".into())), None);
    assert_eq!(
        order_client_id_charset_warning(&CommandWire::Modify {
            order_client_id: "baa0ec7d00".into(),
            new_qty: None,
            new_price: Some(1.0),
            new_trigger_price: None,
        }),
        None
    );
    assert_eq!(order_client_id_charset_warning(&submit_command(1.0, Some(1.0))), None);
    // …and neither do the verbs that name no order at all.
    assert_eq!(
        order_client_id_charset_warning(&CommandWire::MassCancel {
            venue: None,
            symbol: None,
            account: None
        }),
        None
    );
}

/// The warning and the refusal must describe the SAME charset, or the two surfaces teach
/// different rules again — which is the defect, one level up.
#[test]
fn the_warning_agrees_with_the_validator_it_describes() {
    for id in ["MYPINNED-001", "!!!bad***order client id", "", &"a".repeat(33)] {
        assert!(!vike_model::is_valid_crypto_order_client_id(id), "premise: {id:?} is invalid");
        assert!(
            order_client_id_charset_warning(&CommandWire::Cancel(id.into())).is_some(),
            "{id:?}"
        );
    }
    for id in ["baa0ec7d00", "A", &"z".repeat(32)] {
        assert!(vike_model::is_valid_crypto_order_client_id(id), "premise: {id:?} is valid");
        assert!(
            order_client_id_charset_warning(&CommandWire::Cancel(id.into())).is_none(),
            "{id:?}"
        );
    }
}

#[test]
fn guardrail_market_order_has_no_notional_to_check() {
    // Even WITH a ceiling: a market order on a linear contract carries no price, so nothing can be
    // sized client-side — the node is the one that will know. It says so, and it used to read
    // `within_limits: true`.
    let capabilities = GuardrailCapabilities { max_qty: None, max_notional: Some(1.0) };
    let g = check(&submit_command(2.0, None), capabilities);
    assert_eq!(g.to_json()["notional"], Value::Null);
    assert_eq!(g.to_json()["within_limits"], Value::Null, "not checked, never within limits");
    assert!(g.to_json()["unpriced"].is_string(), "{}", g.to_json());
    assert!(g.line().contains("notional=-"), "{}", g.line());
    assert!(g.line().contains("not checked"), "{}", g.line());
    assert!(!g.line().contains("within limits"), "{}", g.line());
    // The qty cap needs no price, and can still say no.
    let capped = GuardrailCapabilities { max_qty: Some(1.0), max_notional: None };
    assert_eq!(check(&submit_command(2.0, None), capped).to_json()["within_limits"], false);
    // A priced answer carries no `unpriced` key at all.
    assert!(
        check(&submit_command(2.0, Some(1.0)), capabilities).to_json().get("unpriced").is_none()
    );
}

/// **An order list's largest leg is the one the door values largest**, `ContractValue::order_notional`, not
/// `qty × price`: on an inverse contract a leg's notional is its qty whatever its price. 1,000 dollars at
/// 50,000 and 1,500 at a 30,000 trigger, against a 1,200 ceiling: `qty × price` picked the first leg and read
/// "within limits" while the node refuses the second.
#[test]
fn an_order_lists_largest_leg_is_the_doors_notional_not_quantity_times_price() {
    let list: OrderListWire = serde_json::from_value(json!({
        "venue": "deribit",
        "symbol": "BTC-PERPETUAL",
        "legs": [
            { "side": 1, "qty": 1_000.0, "order_type": "limit", "price": 50_000.0 },
            { "side": -1, "qty": 1_500.0, "order_type": "stop", "trigger_price": 30_000.0 },
        ],
    }))
    .expect("an order list");
    let cmd = CommandWire::OrderList(list);
    let capabilities = GuardrailCapabilities { max_qty: None, max_notional: Some(1_200.0) };
    let g = guardrail_check(&cmd, capabilities, Ok(ContractValue::inverse(1.0)), false);
    assert_eq!(g.to_json()["notional"], 1_500.0, "{}", g.to_json());
    assert_eq!(g.to_json()["within_limits"], false, "{}", g.to_json());
    // a linear contract still picks by qty × price: the first leg, 50,000,000
    assert_eq!(check(&cmd, capabilities).to_json()["notional"], 50_000_000.0);
}

/// A value that could not be read leaves even a priced order NOT CHECKED, and the reason is on the line.
#[test]
fn an_unknown_contract_value_is_not_checked_and_says_why() {
    let capabilities = GuardrailCapabilities { max_qty: None, max_notional: Some(1.0) };
    let g = guardrail_check(
        &submit_command(2.0, Some(100.0)),
        capabilities,
        Err("no node to ask".to_string()),
        false,
    );
    assert_eq!(g.to_json()["notional"], Value::Null);
    assert_eq!(g.to_json()["within_limits"], Value::Null);
    assert_eq!(g.to_json()["unpriced"], "no node to ask");
    assert!(g.line().contains("not checked: no node to ask"), "{}", g.line());
}

/// A `Modify` naming a new qty states a size: it used to read `no order size to check` and
/// `within_limits: true`, the shape the node's own door calls walking past its ceiling.
#[test]
fn a_modify_naming_a_quantity_states_a_size_and_one_naming_only_a_price_does_not() {
    let modify = |new_qty, new_price| CommandWire::Modify {
        order_client_id: "c-1".into(),
        new_qty,
        new_price,
        new_trigger_price: None,
    };
    let capabilities = GuardrailCapabilities { max_qty: None, max_notional: Some(100.0) };
    assert_eq!(check(&modify(Some(2.0), Some(100.0)), capabilities).to_json()["notional"], 200.0);
    assert_eq!(
        check(&modify(Some(2.0), Some(100.0)), capabilities).to_json()["within_limits"],
        false
    );
    assert_eq!(
        check(&modify(Some(2.0), None), capabilities).to_json()["within_limits"],
        Value::Null
    );
    assert_eq!(check(&modify(None, Some(100.0)), capabilities), Guardrail::NoSize);
    // A trigger and no price is sized at the trigger, as the node's door sizes it; a price still wins.
    let trigger = |price| CommandWire::Modify {
        order_client_id: "c-1".into(),
        new_qty: Some(2.0),
        new_price: price,
        new_trigger_price: Some(30.0),
    };
    assert_eq!(check(&trigger(None), capabilities).to_json()["notional"], 60.0);
    assert_eq!(check(&trigger(Some(100.0)), capabilities).to_json()["notional"], 200.0);
}

/// A stop or take-profit names a TRIGGER and no price, and is sized at the trigger — the reference
/// price the node's door and the core's `RiskGate` give it. It used to print `notional=-` and
/// `within limits` for an order the node refuses. A price of its own (a limit, a stop-limit) still
/// wins over the trigger, and an order with neither (a market order) is unchanged.
#[test]
fn guardrail_sizes_a_stop_or_take_profit_at_its_trigger() {
    let capabilities = GuardrailCapabilities { max_qty: None, max_notional: Some(1_000.0) };
    let with = |order_type: &str, price: Option<f64>, trigger: Option<f64>| {
        CommandWire::Submit(OrderRequestWire {
            trigger_price: trigger,
            ..submit_request(10.0, order_type, price)
        })
    };
    for kind in ["stop", "take_profit"] {
        let g = check(&with(kind, None, Some(100_000.0)), capabilities);
        assert_eq!(g.to_json()["notional"], 1_000_000.0, "{kind}");
        assert_eq!(g.to_json()["within_limits"], false, "{kind}");
        assert!(g.line().contains("OVER LIMIT"), "{kind}: {}", g.line());
        // …and a trigger inside the ceiling is within it.
        let g = check(&with(kind, None, Some(50.0)), capabilities);
        assert_eq!(g.to_json()["notional"], 500.0, "{kind}");
        assert_eq!(g.to_json()["within_limits"], true, "{kind}");
    }
    // A stop-limit is sized at its limit price, not its trigger.
    let g = check(&with("stop_limit", Some(50.0), Some(100_000.0)), capabilities);
    assert_eq!(g.to_json()["notional"], 500.0);
    assert_eq!(g.to_json()["within_limits"], true);
    // A limit order is exactly what it was, and a market order is not priced.
    let g = check(&with("limit", Some(200.0), None), capabilities);
    assert_eq!(g.to_json()["notional"], 2_000.0);
    assert_eq!(g.to_json()["within_limits"], false);
    let g = check(&with("market", None, None), capabilities);
    assert_eq!(g.to_json()["notional"], Value::Null);
    assert_eq!(g.to_json()["within_limits"], Value::Null);
}

/// **A MARKET order that names a price is judged as the node judges it** (`RiskGate::check_inner`'s `own_price`, the
/// door's sizing): the venue fills it at the market, so the number prices nothing. `qty 10, price 1` read notional
/// 10 and `within limits` here while the node judged it at the mark. A trigger on one still sizes it, as the gate
/// reads it (`fixtures/r5/risk.json` case 133), and the price is not what sizes it then either.
#[test]
fn guardrail_judges_a_market_order_naming_a_price_as_the_node_does() {
    let capabilities = GuardrailCapabilities { max_qty: None, max_notional: Some(100.0) };
    for word in ["market", " MARKET "] {
        let g = check(&CommandWire::Submit(submit_request(10.0, word, Some(1.0))), capabilities);
        assert_eq!(g.to_json()["notional"], Value::Null, "{word:?}: not 10");
        assert_eq!(g.to_json()["within_limits"], Value::Null, "{word:?}: not `within limits`");
        assert!(g.line().contains("not checked"), "{word:?}: {}", g.line());
    }
    let stated =
        OrderRequestWire { trigger_price: Some(50.0), ..submit_request(10.0, "market", Some(1.0)) };
    let g = check(&CommandWire::Submit(stated), capabilities);
    assert_eq!(g.to_json()["notional"], 500.0, "at the trigger, not the price's 10");
    assert_eq!(g.to_json()["within_limits"], false);
}

/// `OrderRequestWire::limit_price` reads the word itself (the wire crate links no `vike-model`), and the node reads it
/// through `OrderType::parse`: a word the two read differently would size one order at two prices.
#[test]
fn the_wire_market_word_reads_as_the_nodes_parser_reads_it() {
    let words = ["market", "MARKET", " Market ", "\tmarket\n", "limit", "stop", "stop_limit"];
    for word in words.into_iter().chain(["take_profit", "", "mkt", "market_order"]) {
        let is_market = OrderType::parse(word) == Ok(OrderType::Market);
        let req = submit_request(1.0, word, Some(7.0));
        assert_eq!(req.limit_price(), (!is_market).then_some(7.0), "{word:?}");
    }
}

#[test]
fn guardrail_non_submit_verbs_have_no_size_to_check() {
    let g = check(&CommandWire::Cancel("c-1".into()), GuardrailCapabilities::default());
    assert_eq!(g, Guardrail::NoSize);
    assert_eq!(g.to_json()["within_limits"], true);
    assert_eq!(g.line(), "guardrail: no order size to check for this verb");
}

/// A bracket is ONE qty over three priced legs, and the node's door caps each priced leg as a
/// `Submit` at that price (`ControlLimits`, the bracket arm), so the advisory line sizes it at its
/// LARGEST leg: a market entry has no price of its own but its exits do, and it used to print
/// `no order size to check` for a bracket with a size. They can only say it is OVER: the door sizes that entry at the
/// price it holds, so exits that fit are NOT CHECKED, and they used to read `within limits` of what the door refuses.
#[test]
fn guardrail_sizes_a_bracket_by_its_quantity_and_its_largest_priced_leg() {
    let bracket = |entry_price: Option<f64>| {
        CommandWire::Bracket(OrderBracketWire {
            order_client_id: None,
            venue: "ig".into(),
            symbol: "EURUSD".into(),
            side: 1,
            qty: 2.0,
            entry_price,
            stop_loss: 90.0,
            take_profit: 120.0,
            account: None,
        })
    };
    let capabilities = GuardrailCapabilities { max_qty: Some(5.0), max_notional: Some(200.0) };
    // A market entry: the take-profit (120 x 2 = 240) is the largest priced leg, over the ceiling.
    let g = check(&bracket(None), capabilities);
    assert_eq!(g.to_json()["notional"], 240.0);
    assert_eq!(g.to_json()["within_limits"], false);
    assert!(g.line().contains("OVER LIMIT"), "{}", g.line());
    // A limit entry above every exit is the largest leg itself (130 x 2 = 260).
    assert_eq!(check(&bracket(Some(130.0)), capabilities).to_json()["notional"], 260.0);
    // Inside the ceiling a priced entry is within it; a market entry's exits say nothing of the entry. The qty cap
    // judges the one qty.
    let roomy = GuardrailCapabilities { max_qty: Some(5.0), max_notional: Some(500.0) };
    assert_eq!(check(&bracket(Some(130.0)), roomy).to_json()["within_limits"], true);
    let unchecked = check(&bracket(None), roomy);
    assert_eq!(unchecked.to_json()["within_limits"], Value::Null);
    assert_eq!(unchecked.to_json()["notional"], Value::Null);
    assert!(unchecked.line().contains("not checked: its market entry"), "{}", unchecked.line());
    let tight = GuardrailCapabilities { max_qty: Some(1.0), max_notional: Some(500.0) };
    assert_eq!(check(&bracket(None), tight).to_json()["within_limits"], false);
}

// -- the unsizeable-order refusal: the shared function ------------------------------------------
//
// The REPL, the one-shot verb and the tool agreeing on the SAME classes is one table driving all
// three real parsers, in `vike-cli`'s `cmd::trade` tests (it needs the private `order` module, which
// this file cannot see). What can only be said HERE: the non-finite rows over a hand-built request
// (JSON cannot carry them, so the tool has no such leg) and the `limit`-with-no-price row (the two
// command-line grammars cannot spell it). The tool's own rows are `crates/vike-mcp/src/tool_verbs_tests.rs`.

fn submit_request(qty: f64, order_type: &str, price: Option<f64>) -> OrderRequestWire {
    OrderRequestWire {
        order_client_id: String::new(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty,
        order_type: order_type.into(),
        price,
        trigger_price: None,
        reduce_only: false,
        account: None,
        time_in_force: None,
        gtd_expiry: None,
        display_quantity: None,
        release: None,
        trail: None,
        limit_offset: None,
        peg: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
    }
}

/// Every class the function refuses, with the reason it gives — the NON-FINITE rows included, which
/// no JSON argument can reach.
#[test]
fn the_shared_refusal_names_each_unsizeable_class() {
    for (class, req, reason) in [
        ("qty zero", submit_request(0.0, "market", None), "qty must be > 0"),
        ("qty negative", submit_request(-1.0, "market", None), "qty must be > 0"),
        (
            "qty NaN",
            submit_request(f64::NAN, "market", None),
            "qty must be a finite number, got NaN",
        ),
        (
            "qty +inf",
            submit_request(f64::INFINITY, "market", None),
            "qty must be a finite number, got inf",
        ),
        (
            "qty -inf",
            submit_request(f64::NEG_INFINITY, "market", None),
            "qty must be a finite number, got -inf",
        ),
        (
            "price NaN",
            submit_request(1.0, "limit", Some(f64::NAN)),
            "price must be a finite number, got NaN",
        ),
        (
            "price +inf",
            submit_request(1.0, "limit", Some(f64::INFINITY)),
            "price must be a finite number, got inf",
        ),
        (
            "price -inf",
            submit_request(1.0, "limit", Some(f64::NEG_INFINITY)),
            "price must be a finite number, got -inf",
        ),
        ("limit, no price", submit_request(1.0, "limit", None), "a limit order needs a price"),
        ("LIMIT, no price", submit_request(1.0, "LIMIT", None), "a limit order needs a price"),
        ("market carrying a price", submit_request(1.0, "market", Some(100.0)), MARKET_WITH_PRICE),
        ("MARKET carrying a price", submit_request(1.0, " MARKET ", Some(1.0)), MARKET_WITH_PRICE),
        ("market at price zero", submit_request(1.0, "market", Some(0.0)), MARKET_WITH_PRICE),
        (
            "market carrying a trigger",
            OrderRequestWire { trigger_price: Some(1.0), ..submit_request(10.0, "market", None) },
            MARKET_WITH_TRIGGER,
        ),
        (
            "MARKET carrying a trigger",
            OrderRequestWire { trigger_price: Some(1.0), ..submit_request(10.0, " MARKET ", None) },
            MARKET_WITH_TRIGGER,
        ),
        (
            "market carrying both says the price first",
            OrderRequestWire {
                trigger_price: Some(1.0),
                ..submit_request(10.0, "market", Some(1.0))
            },
            MARKET_WITH_PRICE,
        ),
        (
            "market_to_limit carrying a trigger",
            OrderRequestWire {
                trigger_price: Some(1.0),
                ..submit_request(10.0, "market_to_limit", None)
            },
            "a market_to_limit order takes no trigger price, it fills at the market first: drop \
             the trigger price",
        ),
        (
            "trigger NaN",
            OrderRequestWire { trigger_price: Some(f64::NAN), ..submit_request(1.0, "stop", None) },
            "trigger price must be a finite number, got NaN",
        ),
        (
            "trigger +inf",
            OrderRequestWire {
                trigger_price: Some(f64::INFINITY),
                ..submit_request(1.0, "take_profit", None)
            },
            "trigger price must be a finite number, got inf",
        ),
    ] {
        assert_eq!(unsizeable_submit_refusal(&req).as_deref(), Some(reason), "{class}");
    }
}

/// The refusal of a market order that states a price: the price is what the guardrail would size it
/// by, and the venue fills at the market.
const MARKET_WITH_PRICE: &str =
    "a market order takes no price, it fills at the market: drop the price, or use a limit order";

/// The same for a trigger: the guardrail and the node's door size it at the trigger, and a venue that
/// ignores one fills at the market.
const MARKET_WITH_TRIGGER: &str = "a market order takes no trigger price, it fills at the market: \
                                   drop the trigger price, or use a stop or take-profit order";

/// Every shape below is something the node judges, so refusing it here would be a new refusal the
/// node never made.
#[test]
fn the_shared_refusal_leaves_every_sizeable_order_alone() {
    let trigger = |order_type: &str| OrderRequestWire {
        trigger_price: Some(90.0),
        ..submit_request(1.0, order_type, None)
    };
    let priced = |order_type: &str| OrderRequestWire {
        trigger_price: Some(90.0),
        ..submit_request(1.0, order_type, Some(100.0))
    };
    for (class, req) in [
        ("market, no price", submit_request(1.0, "market", None)),
        ("limit with a price", submit_request(1.0, "limit", Some(100.0))),
        ("stop with a price", priced("stop")),
        ("stop_limit with a price", priced("stop_limit")),
        ("take_profit with a price", priced("take_profit")),
        // Zero and negative PRICES are the node's call — see `unsizeable_submit_refusal`'s doc.
        ("limit at price zero", submit_request(1.0, "limit", Some(0.0))),
        ("limit at a negative price", submit_request(1.0, "limit", Some(-5.0))),
        ("stop, trigger only", trigger("stop")),
        ("take_profit, trigger only", trigger("take_profit")),
        ("a market_to_limit, no price", submit_request(1.0, "market_to_limit", None)),
        ("the smallest positive qty", submit_request(f64::MIN_POSITIVE, "market", None)),
        ("an enormous but finite qty", submit_request(1e300, "limit", Some(1e300))),
    ] {
        assert_eq!(unsizeable_submit_refusal(&req), None, "{class}");
    }
}

/// The ack wait is spelled in seconds without ever rounding a sub-second wait to `0s`: the default
/// two seconds reads `2s` exactly as the constant it replaced did, the floor reads `0.5s`, and a
/// finer wait keeps its milliseconds with the trailing zeros trimmed.
#[test]
fn the_ack_wait_is_spelled_in_seconds_and_never_rounds_to_zero() {
    let ms = |n: u64| wait_seconds_text(Duration::from_millis(n));
    assert_eq!(ms(2_000), "2s", "the default's text is what the constant always printed");
    assert_eq!(ms(500), "0.5s", "the floor must not read `0s`");
    assert_eq!(ms(2_250), "2.25s");
    assert_eq!(ms(1_001), "1.001s");
    assert_eq!(ms(30_000), "30s");
    assert_eq!(ms(10_100), "10.1s");
}

/// A frame publishing `keys`, a venue's route keys, with no symbol or balance the rule could read.
fn frame_of(keys: &[&str]) -> CoreSnapshotWire {
    let mut frame = CoreSnapshotWire::empty();
    frame.sequence = 1;
    frame.venues = keys
        .iter()
        .map(|key| {
            let (venue, account) = key.split_once('#').map_or((*key, None), |(v, a)| (v, Some(a)));
            let mut block: vike_tradehub_client::wire::VenueBlockWire =
                serde_json::from_value(json!({
                    "venue": venue, "balance": 0.0, "realized_pnl": 0.0, "fees_paid": 0.0,
                    "funding_paid": 0.0, "equity": 0.0, "unrealized": 0.0, "missing_prices": 0,
                    "margin_used": 0.0, "free_buying_power": 0.0, "trading_state": "Active", "positions": [],
                }))
                .expect("a block");
            block.account = account.map(str::to_string);
            block.route_key = (*key).to_string();
            block
        })
        .collect();
    frame
}

/// Every verb the one-account rule covers, naming `venue` and `account`.
fn venue_scoped_commands(venue: &str, account: Option<&str>) -> Vec<CommandWire> {
    let named = account.map(str::to_string);
    let venue = venue.to_string();
    let CommandWire::Submit(order) = submit_command(1.0, None) else { unreachable!("a submit") };
    vec![
        CommandWire::Submit(OrderRequestWire {
            venue: venue.clone(),
            account: named.clone(),
            ..order
        }),
        CommandWire::Bracket(OrderBracketWire {
            order_client_id: None,
            venue: venue.clone(),
            symbol: "BTC".into(),
            side: 1,
            qty: 1.0,
            entry_price: None,
            stop_loss: 90.0,
            take_profit: 110.0,
            account: named.clone(),
        }),
        CommandWire::Flatten { venue: venue.clone(), symbol: "BTC".into(), account: named.clone() },
        CommandWire::MassCancel {
            venue: Some(venue.clone()),
            symbol: None,
            account: named.clone(),
        },
        CommandWire::MarketExit { venue: Some(venue), account: named },
    ]
}

/// **The early refusal counts accounts over the CI box's four shapes** (decision 0254): one account, one account
/// with its lane twin, a default and a labelled account, two labelled accounts. Every verb, and the refusal
/// names the books to choose from.
#[test]
fn a_bare_venue_is_refused_early_only_where_the_frame_shows_several_accounts() {
    let shapes: [(&str, &[&str], &[&str]); 4] = [
        ("deribit", &["deribit"], &[]),
        ("binance", &["binance", "binance#PERP"], &[]),
        (
            "hyperliquid",
            &["hyperliquid", "hyperliquid#LIVE"],
            &["hyperliquid/DEFAULT", "hyperliquid/LIVE"],
        ),
        ("bybit", &["bybit#DEMO", "bybit#LIVE"], &["bybit/DEMO", "bybit/LIVE"]),
    ];
    for (venue, keys, books) in shapes {
        let frame = frame_of(keys);
        for cmd in venue_scoped_commands(venue, None) {
            match bare_venue_refusal(&cmd, &frame) {
                None => assert!(books.is_empty(), "{venue}: {cmd:?} went through"),
                Some(why) => {
                    assert!(!books.is_empty(), "{venue} has one account: {why}");
                    assert!(why.contains("REFUSED"), "{why}");
                    for book in books {
                        assert!(why.contains(book), "{book} missing from: {why}");
                    }
                }
            }
        }
        // Naming a book is never this check's business, on any shape.
        for cmd in venue_scoped_commands(venue, Some("LIVE")) {
            assert_eq!(bare_venue_refusal(&cmd, &frame), None, "{venue}: {cmd:?}");
        }
    }
}

/// **The panic button and a frame that shows nothing go on to the node**: the button names no venue, and an
/// empty frame is no evidence of how many accounts run, which the door decides.
#[test]
fn the_panic_button_and_an_empty_frame_are_not_refused_early() {
    let two = frame_of(&["bybit#DEMO", "bybit#LIVE"]);
    for cmd in [
        CommandWire::MarketExit { venue: None, account: None },
        CommandWire::MassCancel { venue: None, symbol: None, account: None },
    ] {
        assert_eq!(bare_venue_refusal(&cmd, &two), None, "{cmd:?}");
    }
    for cmd in venue_scoped_commands("bybit", None) {
        assert_eq!(bare_venue_refusal(&cmd, &CoreSnapshotWire::empty()), None, "{cmd:?}");
    }
}

/// **A NOT RUNNING account is still an account**, with or without its last block on the frame: the same words must
/// not reach the other account because one stopped. Another venue's accounts are not counted.
#[test]
fn a_stopped_account_counts_and_another_venues_accounts_do_not() {
    let stopped: vike_tradehub_client::wire::ArmingNotRunningViewWire =
        serde_json::from_value(json!({
            "venue": "bybit", "account": "LIVE", "route_key": "bybit#LIVE", "reason": "stopped",
        }))
        .expect("a row");
    for keys in [&["bybit#DEMO", "bybit#LIVE", "deribit"][..], &["bybit#DEMO", "deribit"]] {
        let mut frame = frame_of(keys);
        frame.not_running = vec![stopped.clone()];
        for cmd in venue_scoped_commands("bybit", None) {
            let why = bare_venue_refusal(&cmd, &frame).expect("two accounts of bybit");
            assert!(why.contains("bybit/DEMO") && why.contains("bybit/LIVE"), "{why}");
        }
        for cmd in venue_scoped_commands("deribit", None) {
            assert_eq!(bare_venue_refusal(&cmd, &frame), None, "{cmd:?}");
        }
    }
}

/// A twin is one account's second lane only beside its parent; alone, or under another venue's name, it is an
/// account like any other and is kept.
#[test]
fn only_the_venues_own_twin_beside_its_parent_folds_away() {
    let of = |venue, keys: &[&'static str]| accounts_of_venue(venue, keys);
    assert_eq!(of("binance", &["binance", "binance#PERP"]), ["binance"]);
    assert_eq!(of("binance", &["binance#PERP"]), ["binance#PERP"]);
    assert_eq!(of("binance", &["binance", "binance#ALT"]), ["binance", "binance#ALT"]);
    assert_eq!(of("bybit", &["bybit", "bybit#PERP"]), ["bybit", "bybit#PERP"]);
}

/// The price-condition word: `above:` / `below:` and a price above zero, any case; anything else is refused naming
/// the two forms.
#[test]
fn the_price_condition_word_reads_above_and_below_and_nothing_else() {
    assert_eq!(
        release_price_from_word("above:100"),
        Ok((OrderReleasePriceWire::Above(100.0), None))
    );
    assert_eq!(
        release_price_from_word(" BELOW : 2.5 "),
        Ok((OrderReleasePriceWire::Below(2.5), None))
    );
    // another instrument's price, named between the side and the level, in the node's spelling
    assert_eq!(
        release_price_from_word("above:ETHUSDT:3500"),
        Ok((OrderReleasePriceWire::Above(3500.0), Some("ETHUSDT".to_string())))
    );
    assert_eq!(
        release_price_from_word("below: BTC-USDT-SWAP :90000"),
        Ok((OrderReleasePriceWire::Below(90000.0), Some("BTC-USDT-SWAP".to_string())))
    );
    for bad in ["100", "over:100", "above", "above:", "above:x", "above::100", "above:ETH:x"] {
        let e = release_price_from_word(bad).expect_err(bad);
        assert!(e.contains("above:<price> or below:<price>"), "{bad}: {e}");
    }
    for bad in ["above:0", "below:-1", "above:inf", "above:nan"] {
        let e = release_price_from_word(bad).expect_err(bad);
        assert!(e.contains("is not a price above zero"), "{bad}: {e}");
    }
}

/// **A price condition's venue** (decision 0267's 2.13): a roster id exactly, beside a condition that names its
/// symbol, and it comes back as typed; anything else is refused in the model's words or the reader's own.
#[test]
fn a_price_venue_is_a_roster_id_beside_a_condition_that_names_its_symbol() {
    assert_eq!(release_venue_from_word("binance", Some("BTCUSDT")), Ok("binance".to_string()));
    for (venue, symbol, needle) in [
        (
            "Binance",
            Some("BTCUSDT"),
            "the price venue `Binance` is not a venue this platform knows",
        ),
        ("nasdaq", Some("AAPL"), "the price venue `nasdaq` is not a venue"),
        ("", Some("BTCUSDT"), "is not a venue this platform knows"),
        ("binance", None, "a price read on `binance` names its symbol, e.g. above:BTCUSDT:100000"),
    ] {
        let e = release_venue_from_word(venue, symbol).expect_err(venue);
        assert!(e.contains(needle), "{venue:?} {symbol:?}: {e}");
    }
}

/// **A stop's adjustment from the operator's terms**: each target in any spelling, its stop required, a
/// stop-limit's limit required, a trailing one's distance carried as given, and a term of another target
/// refused by name; whether the numbers hold is the model's rule, applied after.
#[test]
fn a_stop_adjustment_is_read_from_its_terms_and_a_stray_term_is_refused() {
    let read = |to: &str, limit: Option<f64>, trail: Option<f64>, pct: Option<f64>| {
        stop_adjust_from_terms(110.0, to, Some(100.0), limit, trail, pct)
    };
    assert_eq!(
        read("stop", None, None, None),
        Ok(OrderStopAdjust { at: 110.0, to: OrderStopAdjustTo::Stop { stop: 100.0 } })
    );
    for word in ["stop-limit", "stop_limit", " Stop-Limit "] {
        assert_eq!(
            read(word, Some(99.0), None, None).map(|a| a.to),
            Ok(OrderStopAdjustTo::StopLimit { stop: 100.0, limit: 99.0 }),
            "{word:?}"
        );
    }
    assert_eq!(
        read("TRAILING", None, None, Some(2.0)).map(|a| a.to),
        Ok(OrderStopAdjustTo::Trailing { stop: 100.0, trail: None, trail_percent: Some(2.0) })
    );
    let refused = |r: Result<OrderStopAdjust, String>| r.expect_err("refused");
    let no_stop = stop_adjust_from_terms(110.0, "stop", None, None, None, None);
    assert!(refused(no_stop).contains("an adjustment to `stop` needs its target's stop price"));
    for (to, limit, trail, pct, needle) in [
        ("stop", Some(99.0), None, None, "an adjustment to `stop` takes no limit price"),
        ("stop", None, Some(2.0), None, "an adjustment to `stop` takes no trail"),
        ("stop-limit", None, None, Some(1.0), "an adjustment to `stop-limit` takes no trail"),
        ("stop-limit", None, None, None, "an adjustment to `stop-limit` needs its limit price"),
        (
            "trailing",
            Some(99.0),
            Some(2.0),
            None,
            "an adjustment to `trailing` takes no limit price",
        ),
        (
            "market",
            None,
            None,
            None,
            "`market` is not an adjustment target: write stop, stop-limit",
        ),
    ] {
        let e = refused(read(to, limit, trail, pct));
        assert!(e.contains(needle), "{to}: {e}");
    }
}

/// **Every leg of an order list carries an id before the preview**, as a submit does: the mint fills each leg that
/// names none and never overwrites one the caller named; the caller's ids go on one per leg, in leg order, or not at
/// all.
#[test]
fn every_order_list_leg_is_given_an_id_the_way_a_submit_is() {
    let head = OrderListHead {
        venue: "sim".into(),
        account: None,
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 3.0,
    };
    let ladder =
        ladder_list(head, None, Some(90.0), None, &[(110.0, 1.0), (120.0, 2.0)]).expect("a ladder");
    assert!(ladder.legs.iter().all(|leg| leg.order_client_id.is_none()), "the builder names none");
    let mut minter = order_client_id_minter(OrderClientIdSource::Cli);
    let CommandWire::OrderList(minted) =
        fill_order_client_id(CommandWire::OrderList(ladder.clone()), &mut minter, None, any_node)
    else {
        panic!("an order list stays one")
    };
    let ids: Vec<&str> = minted.legs.iter().filter_map(|l| l.order_client_id.as_deref()).collect();
    assert_eq!(ids.len(), 4, "every leg");
    assert!(ids.iter().all(|id| vike_model::is_valid_crypto_order_client_id(id)), "{ids:?}");
    assert!(ids.windows(2).all(|w| w[0] != w[1]), "each its own: {ids:?}");

    let named: Vec<String> = ["e1", "s1", "t1", "t2"].map(String::from).to_vec();
    let pinned = with_leg_order_client_ids(ladder.clone(), &named).expect("one per leg");
    let CommandWire::OrderList(kept) =
        fill_order_client_id(CommandWire::OrderList(pinned), &mut minter, None, any_node)
    else {
        panic!("an order list stays one")
    };
    let kept: Vec<&str> = kept.legs.iter().filter_map(|l| l.order_client_id.as_deref()).collect();
    assert_eq!(kept, ["e1", "s1", "t1", "t2"], "a named id is never overwritten");
    assert!(
        with_leg_order_client_ids(ladder.clone(), &named[..3])
            .unwrap_err()
            .contains("3 order client ids")
    );
    let blank = ["e1", " ", "t1", "t2"].map(String::from);
    assert!(with_leg_order_client_ids(ladder, &blank).unwrap_err().contains("leg 1"));
}

/// **A trailing leg in each list shape** (plan P4 #15): `trail` makes the protective leg a `trailing` order carrying
/// its trail and no trigger and no price, in the place and with the links the stop had; it is the stop role of the
/// group's ids. A leg named two ways, or none, is refused by its typed id.
#[test]
fn each_list_shape_takes_a_trailing_protective_leg() {
    let head = |side| OrderListHead {
        venue: "sim".into(),
        account: None,
        symbol: "BTCUSDT".into(),
        side,
        qty: 3.0,
    };
    fn trailing(leg: &OrderListLegWire) -> (&str, Option<f64>, Option<f64>, Option<f64>, bool) {
        (leg.order_type.as_str(), leg.trail, leg.trigger_price, leg.price, leg.reduce_only)
    }
    let oco = oco_list(head(-1), 110.0, None, None, Some(5.0), true).expect("an exit oco");
    assert_eq!(trailing(&oco.legs[1]), ("trailing", Some(5.0), None, None, true));
    assert_eq!(
        (oco.legs[1].contingency, oco.legs[1].linked.clone()),
        (oco.legs[0].contingency, vec![0])
    );
    let oto = oto_list(head(1), Some(100.0), None, None, Some(5.0)).expect("an oto");
    assert_eq!(trailing(&oto.legs[1]), ("trailing", Some(5.0), None, None, true));
    assert_eq!((oto.legs[1].side, oto.legs[1].parent), (-1, Some(0)));
    let stopped = ladder_list(head(1), None, Some(90.0), None, &[(110.0, 1.0)]).expect("a ladder");
    let ladder = ladder_list(head(1), None, None, Some(5.0), &[(110.0, 1.0)]).expect("a ladder");
    assert_eq!(trailing(&ladder.legs[1]), ("trailing", Some(5.0), None, None, true));
    assert_eq!(
        (ladder.legs[1].qty, ladder.legs[1].parent, ladder.legs[1].linked.clone()),
        (stopped.legs[1].qty, stopped.legs[1].parent, stopped.legs[1].linked.clone()),
        "the trailing stop keeps the stop's size and links"
    );
    let mut minter = order_client_id_minter(OrderClientIdSource::Cli);
    let CommandWire::OrderList(minted) =
        fill_order_client_id(CommandWire::OrderList(ladder), &mut minter, None, any_node)
    else {
        panic!("an order list stays one")
    };
    let stop_id = minted.legs[1].order_client_id.as_deref().expect("minted");
    assert!(stop_id.ends_with("SL"), "the trailing leg is the group's stop: {stop_id}");

    let refused = |list: Result<OrderListWire, String>, id: OrderListRefusal| {
        assert_eq!(list.expect_err("refused"), id.rendered());
    };
    refused(
        oco_list(head(-1), 110.0, Some(90.0), None, Some(5.0), true),
        OrderListRefusal::OcoStopLeg,
    );
    refused(
        oco_list(head(-1), 110.0, None, Some(89.0), Some(5.0), true),
        OrderListRefusal::OcoStopLeg,
    );
    refused(oco_list(head(-1), 110.0, None, None, None, true), OrderListRefusal::OcoStopLeg);
    refused(oto_list(head(1), None, Some(90.0), None, Some(5.0)), OrderListRefusal::OtoOneExit);
    refused(oto_list(head(1), None, None, None, None), OrderListRefusal::OtoOneExit);
    refused(
        ladder_list(head(1), None, Some(90.0), Some(5.0), &[(110.0, 1.0)]),
        OrderListRefusal::LadderStopLeg,
    );
    refused(
        ladder_list(head(1), None, None, None, &[(110.0, 1.0)]),
        OrderListRefusal::LadderStopLeg,
    );
    assert!(
        oco_list(head(-1), 110.0, None, None, Some(f64::NAN), true).is_err(),
        "a trail is a number above zero"
    );
}

/// **A DISCRETIONARY limit is sized at its worst price** (decision 0267: L + d for a buy, L − d for a sell), as the
/// node's pre-trade gate judges it, so the advisory guardrail never calls within limits an order the gate refuses.
#[test]
fn the_guardrail_sizes_a_discretionary_limit_at_its_worst_price() {
    use vike_model::OrderAttributes;
    let discretion =
        Some(OrderAttributes { discretionary_amount: Some(10.0), ..Default::default() });
    let capabilities = GuardrailCapabilities { max_qty: None, max_notional: Some(1_005.0) };
    let CommandWire::Submit(buy) = submit_command(10.0, Some(100.0)) else { unreachable!() };
    let notional = |req: OrderRequestWire| match check(&CommandWire::Submit(req), capabilities) {
        Guardrail::Order { notional, within_limits, .. } => (notional, within_limits),
        other => panic!("{other:?}"),
    };
    assert_eq!(notional(buy.clone()), (Some(1_000.0), Some(true)), "a plain limit at L");
    let discretionary = OrderRequestWire { attributes: discretion, ..buy.clone() };
    assert_eq!(notional(discretionary.clone()), (Some(1_100.0), Some(false)), "a buy at L + d");
    let sell = OrderRequestWire { side: -1, ..discretionary };
    assert_eq!(notional(sell), (Some(900.0), Some(true)), "a sell at L − d");
}

/// **The combo builder says what the door would**: a valid net combo and a valid leg-limit combo build the wire
/// spec field for field; a mix, a missing leg limit, a non-finite net (it would go out as `null`, a market combo) and
/// one leg are refused in the model's words. The combo is a write verb whose ONE id is minted before the preview when
/// its caller named none (a named one is never overwritten), and the guardrail never says it is within limits: its
/// legs are sized by the node.
#[test]
fn the_combo_builder_refuses_what_the_door_refuses_and_mints_one_id() {
    use vike_model::TimeInForce;
    let leg = |symbol: &str, ratio, limit| ComboLegArg { symbol: symbol.into(), ratio, limit };
    let build = |legs: Vec<ComboLegArg>, net| {
        combo_of("deribit".into(), Some("ALT".into()), 1, 2.0, legs, net, TimeInForce::Gtc)
    };
    let net =
        build(vec![leg("C1", 1, None), leg("C2", -1, None)], Some(-0.03)).expect("a credit net");
    assert_eq!(net.net_limit, Some(-0.03));
    assert_eq!(net.account.as_deref(), Some("ALT"));
    assert_eq!((net.legs[1].symbol.as_str(), net.legs[1].ratio), ("C2", -1));
    let by_legs = build(vec![leg("C1", 1, Some(0.05)), leg("C2", -1, Some(0.02))], None)
        .expect("priced by its legs");
    assert_eq!(by_legs.legs[0].limit, Some(0.05));
    for (legs, net, said) in [
        (vec![leg("C1", 1, Some(0.05)), leg("C2", -1, Some(0.02))], Some(0.03), "never both"),
        (vec![leg("C1", 1, Some(0.05)), leg("C2", -1, None)], None, "leg 1 states no limit"),
        (vec![leg("C1", 1, None), leg("C2", -1, None)], Some(f64::NAN), "net_limit must be finite"),
        (vec![leg("C1", 1, None)], None, "needs 2..=N legs"),
    ] {
        let why = build(legs, net).expect_err(said);
        assert!(why.contains(said), "{said:?} not in {why}");
    }
    // The lifetime, as the door judges it: `gtc` is the absent field, a word rides on a priced combo, a `gtd` has no
    // expiry to carry and a market combo takes no limit's lifetime.
    assert_eq!(net.time_in_force, None, "gtc goes out as the absent field");
    let two = || vec![leg("C1", 1, None), leg("C2", -1, None)];
    let with = |net, tif| combo_of("deribit".into(), None, 1, 2.0, two(), net, tif);
    let ioc = with(Some(0.01), TimeInForce::Ioc).expect("a priced ioc combo");
    assert_eq!(ioc.time_in_force.as_deref(), Some("ioc"));
    for (net, tif, said) in [
        (Some(0.01), TimeInForce::Gtd, "`gtd` needs an expiry, and a combo carries none"),
        (None, TimeInForce::Fok, "belongs to a limit order"),
        (
            Some(0.01),
            TimeInForce::PostOnly,
            "a combo cannot be post-only: no book models a maker-only combo",
        ),
    ] {
        let why = with(net, tif).expect_err(said);
        assert!(why.contains(said), "{said:?} not in {why}");
    }
    let verb = Verb::Combo(net);
    assert!(verb.is_write());
    let cmd = verb.to_wire_command().expect("a write verb has a frame");
    let mut minter = order_client_id_minter(OrderClientIdSource::Cli);
    let CommandWire::Combo(minted) = fill_order_client_id(cmd.clone(), &mut minter, None, any_node)
    else {
        panic!("a combo stays one")
    };
    let id = minted.order_client_id.clone().expect("one id is minted for the combo");
    assert!(vike_model::is_valid_crypto_order_client_id(&id), "{id}");
    let named =
        CommandWire::Combo(OrderComboWire { order_client_id: Some("mine1".into()), ..minted });
    assert_eq!(
        fill_order_client_id(named.clone(), &mut minter, None, any_node),
        named,
        "a named id is kept"
    );
    let Guardrail::Order { qty, within_limits, .. } =
        guardrail_check(&cmd, GuardrailCapabilities::default(), Err("not known".into()), false)
    else {
        panic!("a combo states a size");
    };
    assert_eq!((qty, within_limits), (2.0, None), "its largest leg, never `within limits`");
}

/// `submit_command`'s limit with `edit` applied: the request [`submit_rules_refusal`] judges.
fn submit_with(edit: impl FnOnce(&mut OrderRequestWire)) -> OrderRequestWire {
    let CommandWire::Submit(mut o) = submit_command(1.0, Some(100.0)) else { unreachable!() };
    edit(&mut o);
    o
}

/// **The one rule over a built submit, both doors' (#11b row 2).** Each rule refuses under its own id, a sound order
/// passes, and an order wrong twice is refused by the rule that bites first, in [`SubmitRuleRefusal`]'s order: the
/// CLI judged its lifetime and its trail at the flag, before the prices, and MCP after them, until this.
#[test]
fn a_built_submit_is_refused_by_the_first_rule_it_breaks_under_that_rules_id() {
    use crate::refusals::SubmitRuleRefusalId as Id;
    use vike_tradehub_client::wire::OrderReleaseWire;
    assert_eq!(submit_rules_refusal(&submit_with(|_| {})), None, "a plain limit");
    let id = |edit: fn(&mut OrderRequestWire)| {
        submit_rules_refusal(&submit_with(edit)).map(|refusal| refusal.id())
    };
    assert_eq!(id(|o| o.trail = Some(5.0)), Some(Id::TrailOffKind));
    assert_eq!(id(|o| o.trail_percent = Some(1.0)), Some(Id::Terms));
    assert_eq!(id(|o| o.display_quantity = Some(1.0)), Some(Id::Display), "not below a qty of 1");
    assert_eq!(
        id(|o| {
            o.order_type = "market".into();
            o.price = None;
            o.attributes =
                Some(vike_model::OrderAttributes { all_or_none: true, ..Default::default() });
        }),
        Some(Id::Attributes)
    );
    assert_eq!(
        id(|o| {
            o.adjust =
                Some(OrderStopAdjust { at: 110.0, to: OrderStopAdjustTo::Stop { stop: 100.0 } });
        }),
        Some(Id::StopAdjust),
        "an adjustment rides a stop"
    );
    assert_eq!(id(|o| o.time_in_force = Some("gtd".into())), Some(Id::Lifetime));
    assert_eq!(
        id(|o| {
            o.release =
                Some(OrderReleaseWire { at: 1_000, price: None, symbol: None, venue: None });
        }),
        Some(Id::Release)
    );
    // wrong twice: a market order with a trail and a lifetime is refused for its trail, as at the node's door
    let twice = submit_rules_refusal(&submit_with(|o| {
        o.order_type = "market".into();
        o.price = None;
        o.trail = Some(5.0);
        o.time_in_force = Some("ioc".into());
    }))
    .expect("refused");
    assert_eq!(
        twice.rendered(),
        "error [order.submit.trail_off_kind]: a trail belongs to a trailing, trailing_limit or \
         trailing_take_profit order; this market order states 5"
    );
}

/// **A peg anchor is read in any case, both doors' (#11b row 2).** The CLI lower-cased `--peg MID` and MCP refused
/// `"MID"`; both now read it as `mid`, and the word goes out as the model's, the one the node's door reads exactly.
#[test]
fn a_peg_anchor_is_read_in_any_case_and_goes_out_as_the_models_word() {
    for word in ["mid", "MID", " Mid "] {
        assert_eq!(
            peg_of_word(word, -0.05),
            Ok(OrderPegWire { anchor: "mid".into(), offset: -0.05 }),
            "{word:?}"
        );
    }
    let refusal = peg_of_word("near", 0.0).expect_err("no anchor");
    assert_eq!(
        refusal.rendered(),
        "error [order.submit.peg_anchor]: \"near\" is not a peg anchor: expected one of best, mid, \
         market, primary (any case)"
    );
}

/// **A modify that changes nothing is refused by the one builder (#11b row 14)**; any one change is a modify.
#[test]
fn a_modify_that_changes_nothing_is_refused_and_any_change_is_a_modify() {
    assert_eq!(modify_of("c-1".into(), None, None, None), Err(ModifyRefusal::NothingToChange));
    assert_eq!(
        modify_of("c-1".into(), None, None, Some(95.0)),
        Ok(Verb::Modify {
            order_client_id: "c-1".into(),
            new_qty: None,
            new_price: None,
            new_trigger_price: Some(95.0),
        })
    );
}
