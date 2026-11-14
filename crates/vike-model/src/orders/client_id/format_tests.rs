use super::*;
use crate::orders::order::{ContingencyType, LadderSpec, LadderTarget, TimeInForce, build_ladder};

fn strat(code: &str) -> OrderClientIdStrategyCode {
    OrderClientIdStrategyCode::parse(code).unwrap()
}

/// A v2 generator whose next id's `U` (at width 12) is `u`: the session base is `u` at seq 0.
fn at(tag: &str, u: &str) -> OrderClientIdGenerator {
    let g = OrderClientIdGenerator::resume(format!("{tag}W{u}"), 0);
    assert!(g.is_v2(), "{tag}W{u}");
    g
}

fn compact(u_width: u8, max_gen: u8) -> OrderClientIdShape {
    OrderClientIdShape { account: false, u_width, max_gen, tag: true }
}

fn order(
    src: OrderClientIdSource,
    code: &str,
    intent: OrderClientIdIntent,
    role: Option<OrderClientIdLegRole>,
    generation: u8,
) -> OrderClientIdLabels {
    OrderClientIdLabels { src, strat: strat(code), intent, role, generation }
}

fn role_code(role: Option<OrderClientIdLegRole>) -> String {
    let d = |n: u8| UPPER36[n as usize] as char;
    match role {
        None => String::new(),
        Some(OrderClientIdLegRole::Entry) => "EN".into(),
        Some(OrderClientIdLegRole::Stop) => "SL".into(),
        Some(OrderClientIdLegRole::Target(n)) => format!("TP{}", d(n)),
        Some(OrderClientIdLegRole::Member(n)) => format!("M{}", d(n)),
    }
}

/// The `format!` reference the hand-rolled mint is pinned against.
fn reference(
    tag: &str,
    o: OrderClientIdLabels,
    account: Option<OrderClientIdAccountCode>,
    u: &str,
) -> String {
    let r = if o.generation == 0 { String::new() } else { format!("R{}", o.generation) };
    format!(
        "{tag}{}{}{}{u}{}{}{r}",
        o.src.letter() as char,
        o.strat,
        account.map_or(String::new(), |a| a.to_string()),
        o.intent.code(),
        role_code(o.role),
    )
}

/// (id, tag, src, strat, account id (None = compact), U width, session U, intent, role, R)
type Example = (
    &'static str,
    &'static str,
    OrderClientIdSource,
    &'static str,
    Option<i64>,
    u8,
    &'static str,
    OrderClientIdIntent,
    Option<OrderClientIdLegRole>,
    u8,
);

/// Every worked example of decision 0270 (its sections 0, 3.3 and 3.4), minted from its parts and
/// parsed back to them.
#[test]
fn every_worked_example_is_minted_from_its_parts_and_parsed_back() {
    use OrderClientIdIntent::*;
    use OrderClientIdSource::*;
    let examples: &[Example] = &[
        (
            "S00A307k3x9q2m7d1p4CSSLR2",
            "",
            Strategy,
            "00A3",
            Some(7),
            12,
            "k3x9q2m7d1p4",
            CloseShort,
            Some(OrderClientIdLegRole::Stop),
            2,
        ),
        (
            "S00A3k3x9q2m7d1p4CSSLR2",
            "",
            Strategy,
            "00A3",
            None,
            12,
            "k3x9q2m7d1p4",
            CloseShort,
            Some(OrderClientIdLegRole::Stop),
            2,
        ),
        (
            "C00000Bq7w2e9r4t1y6FLSL",
            "",
            Cli,
            "0000",
            Some(11),
            12,
            "q7w2e9r4t1y6",
            PlainCloseLong,
            Some(OrderClientIdLegRole::Stop),
            0,
        ),
        (
            "S00A408c9v8b7n6m5l3CLSLR1",
            "",
            Strategy,
            "00A4",
            Some(8),
            12,
            "c9v8b7n6m5l3",
            CloseLong,
            Some(OrderClientIdLegRole::Stop),
            1,
        ),
        (
            "M000004k3x9q2m7d1p4CLTP1",
            "",
            Mcp,
            "0000",
            Some(4),
            12,
            "k3x9q2m7d1p4",
            CloseLong,
            Some(OrderClientIdLegRole::Target(1)),
            0,
        ),
        (
            "G00000Dm4n8b2v6c1x0OLEN",
            "",
            Gui,
            "0000",
            Some(13),
            12,
            "m4n8b2v6c1x0",
            OpenLong,
            Some(OrderClientIdLegRole::Entry),
            0,
        ),
        (
            "S01Z90Fz8y7x6w5v4u3CSTP1R3",
            "",
            Strategy,
            "01Z9",
            Some(15),
            12,
            "z8y7x6w5v4u3",
            CloseShort,
            Some(OrderClientIdLegRole::Target(1)),
            3,
        ),
        (
            "T00001Ah5g4f3d2s1a9FSM2",
            "",
            Telegram,
            "0000",
            Some(46),
            12,
            "h5g4f3d2s1a9",
            PlainCloseShort,
            Some(OrderClientIdLegRole::Member(2)),
            0,
        ),
        (
            "S00C10Hp0o9i8u7y6t5CLSL",
            "",
            Strategy,
            "00C1",
            Some(17),
            12,
            "p0o9i8u7y6t5",
            CloseLong,
            Some(OrderClientIdLegRole::Stop),
            0,
        ),
        (
            "S00C20Jr5e4w3q2a1z8OLEN",
            "",
            Strategy,
            "00C2",
            Some(19),
            12,
            "r5e4w3q2a1z8",
            OpenLong,
            Some(OrderClientIdLegRole::Entry),
            0,
        ),
        ("C00000Kd1e2f3a4b5c6OL", "", Cli, "0000", Some(20), 12, "d1e2f3a4b5c6", OpenLong, None, 0),
        (
            "S00A30Mj2k3l4m5n6o7OLEN",
            "",
            Strategy,
            "00A3",
            Some(22),
            12,
            "j2k3l4m5n6o7",
            OpenLong,
            Some(OrderClientIdLegRole::Entry),
            0,
        ),
        (
            "S00A307k3x9q2m7d1p4CSTP9R99",
            "",
            Strategy,
            "00A3",
            Some(7),
            12,
            "k3x9q2m7d1p4",
            CloseShort,
            Some(OrderClientIdLegRole::Target(9)),
            99,
        ),
        (
            "wxyzWS00A307k3x9q2m7d1p4CSTP9R99",
            "wxyz",
            Strategy,
            "00A3",
            Some(7),
            12,
            "k3x9q2m7d1p4",
            CloseShort,
            Some(OrderClientIdLegRole::Target(9)),
            99,
        ),
        // 3.4's longest broker code: U = 10, no `R`. U is the LOW ten digits of base + seq.
        (
            "S00A3k3x9q2m7d1CSTP9",
            "",
            Strategy,
            "00A3",
            None,
            10,
            "00k3x9q2m7d1",
            CloseShort,
            Some(OrderClientIdLegRole::Target(9)),
            0,
        ),
    ];
    for &(id, tag, src, code, account, width, session_u, intent, role, generation) in examples {
        let shape = match account {
            Some(_) => OrderClientIdShape::FULL,
            None => compact(width, if width == 12 { 99 } else { 0 }),
        };
        let plan = OrderClientIdPlan::new(
            shape,
            account.map_or(OrderClientIdAccountCode::NONE, OrderClientIdAccountCode::of_id),
        );
        let o = order(src, code, intent, role, generation);
        let minted = plan.mint(&mut at(tag, session_u), o);
        assert_eq!(minted, id);
        assert!(is_valid_crypto_order_client_id(&minted), "{id}");
        let Some(OrderClientIdReading::V2(p)) = parse_order_client_id(id) else {
            panic!("{id} did not parse v2")
        };
        assert_eq!(p.origin, (!tag.is_empty()).then_some(tag), "{id}");
        assert_eq!(
            (p.src, p.strat, p.intent, p.role, p.generation),
            (src, strat(code), intent, role, generation),
            "{id}"
        );
        assert_eq!(p.account, account.map(OrderClientIdAccountCode::of_id), "{id}");
        assert_eq!(p.unique.len(), width as usize, "{id}");
        assert!(session_u.ends_with(p.unique), "{id}");
    }
    // The two maxima: 27 untagged, 32 (exactly `is_valid_crypto_order_client_id`'s and OKX's ceiling) tagged.
    assert_eq!(examples[12].0.len(), 27);
    assert_eq!(examples[13].0.len(), 32);
    assert_eq!(examples[14].0.len(), 20);
}

/// The 3.4 fit's shapes, each at its worst case (target 35, the highest replacement), have the
/// lengths the fit was computed with, and the fixed-width `U` continues across a restart.
#[test]
fn every_binance_shape_mints_its_worst_case_and_resumes_its_unique_part() {
    for (width, max_gen, len) in
        [(12, 99, 25), (12, 9, 24), (11, 9, 23), (11, 0, 21), (10, 0, 20), (7, 0, 17)]
    {
        for tag in ["", "wxyz"] {
            let plan =
                OrderClientIdPlan::new(compact(width, max_gen), OrderClientIdAccountCode::NONE);
            let worst = order(
                OrderClientIdSource::Strategy,
                "ZZZZ",
                OrderClientIdIntent::CloseShort,
                Some(OrderClientIdLegRole::Target(35)),
                max_gen,
            );
            let mut g = OrderClientIdGenerator::resume(format!("{tag}Wzzzzzzzzzzzz"), 12_345);
            let first = plan.mint(&mut g, worst);
            let tag_cost = if tag.is_empty() { 0 } else { tag.len() + 1 };
            assert_eq!(first.len(), len + tag_cost, "{first}");
            assert_eq!(
                parse_order_client_id(&first).map(|p| matches!(p, OrderClientIdReading::V2(_))),
                Some(true),
                "{first}"
            );
            let (session, seq) = g.state();
            let next = plan.mint(&mut g, worst);
            assert_eq!(plan.mint(&mut OrderClientIdGenerator::resume(session, seq), worst), next);
            assert_ne!(first, next);
        }
    }
}

/// THE neutral-plan pin: on a v1 session every plan and every label mints exactly what
/// `generate` minted, and a stem mints each leg whole, one sequence number per leg.
#[test]
fn a_v1_session_mints_exactly_the_ids_generate_mints_through_every_plan() {
    let shapes =
        [OrderClientIdShape::FULL, compact(12, 99), compact(11, 9), compact(10, 0), compact(7, 0)];
    for session in ["deadbeef", "westVdeadbeef", "ui", "a1b2c3d4"] {
        let mut old = OrderClientIdGenerator::resume(session.to_string(), 0);
        let mut new = OrderClientIdGenerator::resume(session.to_string(), 0);
        assert!(!new.is_v2());
        for (i, shape) in shapes.iter().cycle().take(200).enumerate() {
            let plan = OrderClientIdPlan::new(*shape, OrderClientIdAccountCode::of_id(i as i64));
            let o = order(
                OrderClientIdSource::ALL[i % 6],
                "00A3",
                OrderClientIdIntent::ALL[i % 6],
                Some(OrderClientIdLegRole::Stop),
                3,
            );
            assert_eq!(plan.mint(&mut new, o), old.generate());
            let stem = plan.generate_stem(
                &mut new,
                OrderClientIdSource::Strategy,
                OrderClientIdStrategyCode::NONE,
            );
            assert_eq!(stem.as_str(), None);
            assert_eq!(
                stem.leg(&mut new, OrderClientIdIntent::OpenLong, OrderClientIdLegRole::Entry),
                old.generate()
            );
            assert_eq!(
                stem.leg(&mut new, OrderClientIdIntent::CloseLong, OrderClientIdLegRole::Stop),
                old.generate()
            );
        }
        assert_eq!(new.state(), old.state());
    }
    let mut neutral = OrderClientIdGenerator::new(Some("deadbeef"));
    assert_eq!(
        OrderClientIdPlan::NEUTRAL.mint(&mut neutral, OrderClientIdLabels::NEUTRAL),
        "deadbeef0"
    );
}

/// The mint functions are byte-identical to a `format!` reference, across shapes, tags, labels and
/// a wide sequence range.
#[test]
fn the_mint_is_byte_identical_to_a_format_reference() {
    let shapes =
        [OrderClientIdShape::FULL, compact(12, 99), compact(11, 9), compact(10, 0), compact(7, 0)];
    let roles = [
        None,
        Some(OrderClientIdLegRole::Entry),
        Some(OrderClientIdLegRole::Stop),
        Some(OrderClientIdLegRole::Target(1)),
        Some(OrderClientIdLegRole::Target(35)),
        Some(OrderClientIdLegRole::Member(27)),
    ];
    for tag in ["", "a", "wxyz"] {
        for shape in shapes {
            for no_tag in [false, true] {
                let shape = OrderClientIdShape { tag: !no_tag, ..shape };
                let account = OrderClientIdAccountCode::of_id(1_295);
                let plan = OrderClientIdPlan::new(shape, account);
                let base = 36u64.pow(12) - 5_000; // the 12-digit U wraps inside the range below
                let mut g = OrderClientIdGenerator::resume(
                    v2_session(OrderClientIdOrigin::parse(tag).ok().as_ref(), base),
                    0,
                );
                for seq in 0..10_000u64 {
                    let o = order(
                        OrderClientIdSource::ALL[(seq % 6) as usize],
                        "Z0A9",
                        OrderClientIdIntent::ALL[(seq % 6) as usize],
                        roles[(seq % 6) as usize],
                        (seq % (u64::from(shape.max_gen) + 1)) as u8,
                    );
                    let mut u = [0u8; 12];
                    let u = std::str::from_utf8(write_base36(
                        base + seq,
                        shape.u_width as usize,
                        &mut u,
                    ))
                    .unwrap()
                    .to_string();
                    let shown_tag =
                        if tag.is_empty() || no_tag { String::new() } else { format!("{tag}W") };
                    let want = reference(&shown_tag, o, shape.account.then_some(account), &u);
                    assert_eq!(plan.mint(&mut g, o), want, "seq {seq}");
                }
            }
        }
    }
}

#[test]
fn write_base36_renders_the_low_digits_zero_padded() {
    let mut b = [0u8; 12];
    assert_eq!(write_base36(0, 12, &mut b), b"000000000000");
    assert_eq!(write_base36(35, 3, &mut b), b"00z");
    assert_eq!(write_base36(36, 2, &mut b), b"10");
    assert_eq!(write_base36(36u64.pow(12) - 1, 12, &mut b), b"zzzzzzzzzzzz");
    assert_eq!(write_base36(36u64.pow(12), 12, &mut b), b"000000000000", "mod 36^12");
    assert_eq!(write_base36(7, 0, &mut b), b"");
    for n in [1u64, 35, 36, 1_000_000, 4_738_381_338_321_616_895, u64::MAX] {
        let mut s = String::new();
        let mut m = n;
        while m > 0 {
            s.insert(0, LOWER36[(m % 36) as usize] as char);
            m /= 36;
        }
        let low12 = format!("{s:0>12}");
        let low12 = &low12[low12.len() - 12..];
        assert_eq!(std::str::from_utf8(write_base36(n, 12, &mut b)).unwrap(), low12, "{n}");
    }
}

/// Two of our ids of one shape never differ only by case: `U` is lower-case, every label upper-case.
#[test]
fn no_two_ids_of_one_generator_differ_only_by_case() {
    let shapes =
        [OrderClientIdShape::FULL, compact(12, 99), compact(11, 9), compact(10, 0), compact(7, 0)];
    let mut g = OrderClientIdGenerator::resume("apWzzzzzzzzzzzz".into(), 0);
    let mut ids = std::collections::HashSet::new();
    let mut folded = std::collections::HashSet::new();
    for i in 0..20_000usize {
        let plan = OrderClientIdPlan::new(
            shapes[i % shapes.len()],
            OrderClientIdAccountCode::of_id((i % 1_300) as i64),
        );
        let o = order(
            OrderClientIdSource::ALL[i % 6],
            "00A3",
            OrderClientIdIntent::ALL[(i / 6) % 6],
            None,
            0,
        );
        let id = plan.mint(&mut g, o);
        folded.insert(id.to_ascii_lowercase());
        assert!(ids.insert(id));
    }
    assert_eq!(folded.len(), ids.len());
}

/// The intent rule over its whole domain: spot or margin, reduce-only or not, buy or sell, and a
/// short, flat or long position.
#[test]
fn the_intent_matrix_is_exhaustive() {
    // (cash, reduce_only, side) -> the codes at position -1, 0, +1
    let table: [(bool, bool, i32, [&str; 3]); 8] = [
        (true, false, 1, ["OL", "OL", "OL"]),
        (true, true, 1, ["OL", "OL", "OL"]),
        (true, false, -1, ["CL", "CL", "CL"]),
        (true, true, -1, ["CL", "CL", "CL"]),
        (false, true, 1, ["CS", "CS", "CS"]),
        (false, true, -1, ["CL", "CL", "CL"]),
        (false, false, 1, ["FS", "OL", "OL"]),
        (false, false, -1, ["OS", "OS", "FL"]),
    ];
    for (cash, reduce_only, side, codes) in table {
        for (position, code) in [-1.0, 0.0, 1.0].into_iter().zip(codes) {
            let intent = OrderClientIdIntent::of(side, reduce_only, cash, position);
            assert_eq!(
                intent.code(),
                code,
                "cash {cash} reduce-only {reduce_only} side {side} at {position}"
            );
            assert_eq!(OrderClientIdIntent::from_code(code.as_bytes()), Some(intent));
            if cash {
                assert!(matches!(
                    intent,
                    OrderClientIdIntent::OpenLong | OrderClientIdIntent::CloseLong
                ));
            }
        }
    }
}

fn leg(order_type: OrderType, reduce_only: bool, parent: Option<usize>) -> OrderListLeg {
    OrderListLeg {
        side: -1,
        qty: 1.0,
        order_type,
        price: Some(1.0),
        trigger_price: None,
        trail: None,
        reduce_only,
        time_in_force: TimeInForce::default(),
        parent,
        contingency: None,
        linked: Vec::new(),
        order_client_id: None,
    }
}

#[test]
fn leg_roles_of_a_bracket_a_ladder_an_oco_and_an_ouo() {
    use OrderClientIdLegRole::*;
    let bracket = [
        leg(OrderType::Limit, false, None),
        leg(OrderType::Stop, true, Some(0)),
        leg(OrderType::Limit, true, Some(0)),
    ];
    assert_eq!(OrderClientIdLegRole::of(&bracket).unwrap(), OrderClientIdLegRole::BRACKET);

    let ladder = build_ladder(&LadderSpec {
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 3.0,
        entry_price: None,
        stop_loss: 90.0,
        targets: vec![LadderTarget { price: 110.0, qty: 1.0 }; 3],
        account: None,
    })
    .unwrap();
    assert_eq!(
        OrderClientIdLegRole::of(&ladder.legs).unwrap(),
        [Entry, Stop, Target(1), Target(2), Target(3)]
    );

    // An OCO exit pair, and an OCO of two entries.
    let oco = |a, b, reduce_only| {
        let mut legs = vec![leg(a, reduce_only, None), leg(b, reduce_only, None)];
        legs[0].contingency = Some(ContingencyType::Oco);
        legs[1].contingency = Some(ContingencyType::Oco);
        legs
    };
    assert_eq!(
        OrderClientIdLegRole::of(&oco(OrderType::Stop, OrderType::Limit, true)).unwrap(),
        [Stop, Target(1)]
    );
    assert_eq!(
        OrderClientIdLegRole::of(&oco(OrderType::Limit, OrderType::Limit, false)).unwrap(),
        [Member(1), Member(2)]
    );

    // An OUO of two take-profits and a second stop: the stop's role is the first stop's alone.
    let ouo = [
        leg(OrderType::TakeProfit, true, None),
        leg(OrderType::TrailingTakeProfit, true, None),
        leg(OrderType::Stop, true, None),
        leg(OrderType::StopLimit, true, None),
    ];
    assert_eq!(OrderClientIdLegRole::of(&ouo).unwrap(), [Target(1), Target(2), Stop, Member(1)]);

    // An OTO chain: only the first parent is the entry.
    let chain = [
        leg(OrderType::Limit, false, None),
        leg(OrderType::Limit, false, Some(0)),
        leg(OrderType::Market, false, Some(1)),
    ];
    assert_eq!(OrderClientIdLegRole::of(&chain).unwrap(), [Entry, Member(1), Member(2)]);

    // 35 targets of one role fit, a 36th falls back to plain ids.
    let many = |n| (0..n).map(|_| leg(OrderType::Limit, true, None)).collect::<Vec<_>>();
    assert_eq!(OrderClientIdLegRole::of(&many(35)).unwrap().last(), Some(&Target(35)));
    assert_eq!(OrderClientIdLegRole::of(&many(36)), None);
}

#[test]
fn a_group_costs_one_sequence_number_and_its_legs_share_the_stem() {
    let plan = OrderClientIdPlan::new(OrderClientIdShape::FULL, OrderClientIdAccountCode::of_id(7));
    let mut g = at("", "k3x9q2m7d1p4");
    let stem = plan.generate_stem(&mut g, OrderClientIdSource::Strategy, strat("00A3"));
    assert_eq!(stem.as_str(), Some("S00A307k3x9q2m7d1p4"));
    let legs: Vec<String> = [
        (OrderClientIdIntent::OpenLong, OrderClientIdLegRole::Entry),
        (OrderClientIdIntent::CloseLong, OrderClientIdLegRole::Stop),
        (OrderClientIdIntent::CloseLong, OrderClientIdLegRole::Target(1)),
    ]
    .into_iter()
    .map(|(intent, role)| stem.leg(&mut g, intent, role))
    .collect();
    assert_eq!(
        legs,
        ["S00A307k3x9q2m7d1p4OLEN", "S00A307k3x9q2m7d1p4CLSL", "S00A307k3x9q2m7d1p4CLTP1"]
    );
    for leg in &legs {
        let Some(OrderClientIdReading::V2(p)) = parse_order_client_id(leg) else { panic!("{leg}") };
        assert_eq!(p.stem, "S00A307k3x9q2m7d1p4");
    }
    // The next id is one sequence number on: U ...p4 then ...p5.
    let next = plan.mint(&mut g, OrderClientIdLabels::NEUTRAL);
    assert_eq!(next, "E000007k3x9q2m7d1p5OL");

    // A stem a client sent is taken exactly, and nothing else is.
    assert_eq!(OrderClientIdStem::given("S00A307k3x9q2m7d1p4"), Some(stem.clone()));
    assert_eq!(
        OrderClientIdStem::given("wxyzWS00A3k3x9q2m7d1p4").map(|s| s.as_str().map(str::len)),
        Some(Some(22))
    );
    for bad in ["S00A307k3x9q2m7d1p4OL", "S00A307k3x9q2m7d1p", "deadbeef17", "", "EXT-1"] {
        assert_eq!(OrderClientIdStem::given(bad), None, "{bad}");
    }
}

#[test]
fn next_generation_walks_a_chain_to_its_shape_cap() {
    let first = "wxyzWS00A307k3x9q2m7d1p4CSTP9";
    let mut id = first.to_string();
    for n in 1..=99u8 {
        id = next_generation(&id, 99).unwrap();
        let Some(OrderClientIdReading::V2(p)) = parse_order_client_id(&id) else { panic!("{id}") };
        assert_eq!((p.generation, p.chain), (n, first));
        assert!(is_valid_crypto_order_client_id(&id));
    }
    assert_eq!(id, "wxyzWS00A307k3x9q2m7d1p4CSTP9R99");
    assert_eq!(next_generation(&id, 99), None, "past R99");

    let r9 =
        (0..9).fold("S00A3k3x9q2m7d1p4CSSL".to_string(), |id, _| next_generation(&id, 9).unwrap());
    assert_eq!(r9, "S00A3k3x9q2m7d1p4CSSLR9");
    assert_eq!(next_generation(&r9, 9), None, "a tight engine's R9 cap");
    assert_eq!(next_generation("S00A3k3x9q2m7d1CSTP9", 0), None, "a no-R shape");
    assert_eq!(
        next_generation("S00A307k3x9q2m7d1p4OL", 99).as_deref(),
        Some("S00A307k3x9q2m7d1p4OLR1")
    );
    for not_v2 in ["deadbeef17", "westVdeadbeef17", "EXT-binance-7", ""] {
        assert_eq!(next_generation(not_v2, 99), None, "{not_v2}");
    }
}

#[test]
fn ids_minted_before_v2_parse_v1_and_foreign_strings_parse_as_nothing() {
    assert_eq!(
        parse_order_client_id("deadbeef17"),
        Some(OrderClientIdReading::V1 { origin: None })
    );
    assert_eq!(
        parse_order_client_id("westVdeadbeef17"),
        Some(OrderClientIdReading::V1 { origin: Some("west") })
    );
    for desktop in ["uideadbeef17", "domdeadbeef0", "polydeadbeef3", "optdeadbeef9"] {
        assert_eq!(
            parse_order_client_id(desktop),
            Some(OrderClientIdReading::V1 { origin: None }),
            "{desktop}"
        );
    }
    for foreign in [
        "EXT-binance-771",
        "0x9f3c1a2b4d5e6f708192a3b4c5d6e7f8",
        "vkrp1760000000000-1",
        "my hedge",
        "",
        "ui-3-7",
        "XYZ123",
        "S00A307k3x9q2m7d1p4CSSLR0",
        "S00A307k3x9q2m7d1p4CSSLR100",
        "S00A307k3x9q2m7d1p4XX",
        "S00A307k3x9q2m7d1p4CSM0",
        // A retired draft letter is no source: the same id under `B` (now `S`) reads as nothing.
        "B00A307k3x9q2m7d1p4CSSL",
    ] {
        assert_eq!(parse_order_client_id(foreign), None, "{foreign}");
    }
}

/// An arm or refusal id (`{session}a{n}`, `{session}r{n}`) never reads as a v2 id, under a v1 or a
/// v2 session, tagged or not.
#[test]
fn arm_and_refusal_ids_never_parse_v2() {
    for session in
        ["deadbeef", "westVdeadbeef", "Wk3x9q2m7d1p4", "westWk3x9q2m7d1p4", "aWzzzzzzzzzzzz"]
    {
        for n in 0..2_000u64 {
            for kind in ['a', 'r'] {
                let id = format!("{session}{kind}{n}");
                assert!(
                    !matches!(parse_order_client_id(&id), Some(OrderClientIdReading::V2(_))),
                    "{id}"
                );
            }
        }
    }
}

/// The two shapes a full id with an all-digit account and a compact id two digits wider could be
/// confused for: one reading each, which [`FULL_U_WIDTH`] guarantees.
#[test]
fn a_digit_account_code_never_makes_a_full_id_read_as_compact() {
    assert_eq!(order_client_id_readings("S00A307k3x9q2m7d1p4OL"), 1);
    let Some(OrderClientIdReading::V2(full)) = parse_order_client_id("S00A307k3x9q2m7d1p4OL")
    else {
        panic!()
    };
    assert_eq!(
        (full.account, full.unique),
        (Some(OrderClientIdAccountCode::of_id(7)), "k3x9q2m7d1p4")
    );
    // The same bytes as a full id with a 10-wide U: read as compact-12 alone, never both.
    assert_eq!(order_client_id_readings("S00A307k3x9q2m7d1OL"), 1);
    let Some(OrderClientIdReading::V2(p)) = parse_order_client_id("S00A307k3x9q2m7d1OL") else {
        panic!()
    };
    assert_eq!((p.account, p.unique), (None, "07k3x9q2m7d1"));
}

#[test]
#[should_panic(expected = "U width")]
fn a_plan_refuses_a_full_shape_at_a_width_the_parser_does_not_read() {
    OrderClientIdPlan::new(
        OrderClientIdShape { u_width: 10, ..OrderClientIdShape::FULL },
        OrderClientIdAccountCode::NONE,
    );
}

#[test]
fn is_mintable_is_exactly_what_a_plan_takes() {
    assert!(OrderClientIdShape::FULL.is_mintable());
    for w in U_WIDTHS {
        assert!(compact(w, 0).is_mintable(), "compact {w}");
    }
    assert!(!OrderClientIdShape { u_width: 10, ..OrderClientIdShape::FULL }.is_mintable());
    assert!(!compact(13, 0).is_mintable());
    assert!(!compact(9, 0).is_mintable());
    assert!(
        !OrderClientIdShape { max_gen: MAX_GENERATION + 1, ..OrderClientIdShape::FULL }
            .is_mintable()
    );
}

#[test]
#[should_panic(expected = "U width")]
fn a_plan_refuses_a_compact_shape_wider_than_the_full_one() {
    OrderClientIdPlan::new(compact(13, 0), OrderClientIdAccountCode::NONE);
}

#[test]
fn the_codes_cover_their_ranges_and_refuse_beyond() {
    assert_eq!(OrderClientIdStrategyCode::new(0), Some(OrderClientIdStrategyCode::NONE));
    assert_eq!(OrderClientIdStrategyCode::new(363).unwrap().as_str(), "00A3");
    assert_eq!(
        OrderClientIdStrategyCode::new(OrderClientIdStrategyCode::MAX_ID).unwrap().as_str(),
        "ZZZZ"
    );
    assert_eq!(OrderClientIdStrategyCode::new(OrderClientIdStrategyCode::MAX_ID + 1), None);
    assert_eq!(OrderClientIdStrategyCode::new(-1), None);
    for id in [0, 1, 35, 36, 363, 1_679_615] {
        assert_eq!(OrderClientIdStrategyCode::new(id).unwrap().id(), id);
    }
    assert_eq!(OrderClientIdStrategyCode::parse("00a3"), None, "upper-case only");
    assert_eq!(OrderClientIdAccountCode::of_id(7).as_str(), "07");
    assert_eq!(OrderClientIdAccountCode::of_id(46).as_str(), "1A");
    assert_eq!(OrderClientIdAccountCode::of_id(OrderClientIdAccountCode::MAX_ID).as_str(), "ZZ");
    for none in [0, -3, OrderClientIdAccountCode::MAX_ID + 1] {
        assert_eq!(OrderClientIdAccountCode::of_id(none), OrderClientIdAccountCode::NONE, "{none}");
    }
    assert_eq!(OrderClientIdAccountCode::of_id(1_295).number(), Some(1_295));
    assert_eq!(OrderClientIdAccountCode::NONE.number(), None);
    assert_eq!(serde_json::to_string(&strat("00A3")).unwrap(), "\"00A3\"");
    assert_eq!(
        serde_json::from_str::<OrderClientIdAccountCode>("\"0B\"").unwrap(),
        OrderClientIdAccountCode::of_id(11)
    );
    assert!(serde_json::from_str::<OrderClientIdAccountCode>("\"0b\"").is_err());
}

#[test]
fn a_fresh_v2_session_is_self_describing_and_rolls_at_its_point() {
    let plain = OrderClientIdGenerator::new_v2(None);
    let (session, seq) = plain.state();
    assert_eq!((session.len(), session.as_bytes()[0], seq), (13, b'W', 0));
    assert!(plain.is_v2() && plain.origin().is_none());
    let (base, tag_len) = decode_v2_session(&session).unwrap();
    assert!(base < V2_BASE_LIMIT && tag_len == 0);

    let west = OrderClientIdOrigin::parse("west").unwrap();
    let tagged = OrderClientIdGenerator::new_v2(Some(&west));
    assert!(tagged.state().0.starts_with("westW") && tagged.origin() == Some("west"));
    assert!(OrderClientIdGenerator::resume(tagged.state().0, 9).is_v2(), "the journal round trip");

    // A v1 session is always due (the one-time switch); a v2 one at its roll point.
    let point = order_client_id_roll_point([OrderClientIdShape::FULL]);
    assert_eq!(point, 1 << 39);
    assert_eq!(
        order_client_id_roll_point([OrderClientIdShape::FULL, compact(7, 0)]),
        36u64.pow(7) / 2
    );
    assert_eq!(order_client_id_roll_point(Vec::new()), 1 << 39);
    assert!(OrderClientIdGenerator::new(Some("deadbeef")).due_to_roll(point));
    assert!(!plain.due_to_roll(point));
    assert!(OrderClientIdGenerator::resume(session.clone(), point).due_to_roll(point));
    assert!(!OrderClientIdGenerator::resume(session, point - 1).due_to_roll(point));

    // Not v2: a mark too deep, an upper-case tag, a body of the wrong width or case.
    for v1 in
        ["abcdeWk3x9q2m7d1p4", "AWk3x9q2m7d1p4", "Wk3x9q2m7d1p", "Wk3x9q2m7d1p45", "WK3X9Q2M7D1P4"]
    {
        assert_eq!(decode_v2_session(v1), None, "{v1}");
    }
}

#[test]
fn the_words_name_every_source_and_intent() {
    let words = |id: &str| describe_order_client_id(&parse_order_client_id(id).unwrap());
    let mut w = words("C00000Bq7w2e9r4t1y6FLSL");
    w.account_label = Some("PERP".into());
    assert_eq!(
        w.to_string(),
        "CLI (by hand) · no strategy · account #11 PERP · closes long with a plain order (may open \
         a short) · stop of group q7w2e9r4t1y6"
    );
    assert_eq!(
        words("wxyzWS00A307k3x9q2m7d1p4CSTP9R99").to_string(),
        "strategy (auto) · strategy 00A3 · account #7 · closes short (cannot open) · target 9 of \
         group k3x9q2m7d1p4 · replacement 99 · instance wxyz"
    );
    let mut named = words("S00A3k3x9q2m7d1p4OL");
    named.strategy_name = Some("sma_cross".into());
    assert_eq!(named.to_string(), "strategy (auto) · strategy 00A3 sma_cross · opens or adds long");
    assert_eq!(
        words("E000000k3x9q2m7d1p4OL").to_string(),
        "engine (the core itself) · no strategy · no account · opens or adds long"
    );
    assert_eq!(words("deadbeef17").to_string(), "minted before v2");
    assert_eq!(words("westVdeadbeef17").to_string(), "minted before v2 · instance west");
    for src in OrderClientIdSource::ALL {
        for intent in OrderClientIdIntent::ALL {
            let id = format!("{}000004k3x9q2m7d1p4{}", src.letter() as char, intent.code());
            let text = words(&id).to_string();
            assert!(
                text.starts_with(src.words()) && text.ends_with(intent.words()),
                "{id}: {text}"
            );
        }
    }
}
