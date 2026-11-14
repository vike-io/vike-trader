//! Property tests of the v2 order client id (decision 0270): every shape, code and width minted
//! from its parts parses back to them, no string reads as two shapes, every id minted before v2
//! reads as v1, and the instance tag reads back off a v2 id exactly when the engine stamped it.
//!
//! A counterexample's seed is committed BESIDE this file as `order_client_id_property_tests.proptest-regressions`.

use proptest::prelude::*;
use vike_model::is_valid_crypto_order_client_id;
use vike_model::orders::client_id::origin::{OrderClientIdOrigin, origin_of_order_client_id};
use vike_model::orders::client_id::{
    FULL_U_WIDTH, MAX_ROLE_NUMBER, OrderClientIdAccountCode, OrderClientIdGenerator,
    OrderClientIdIntent, OrderClientIdLabels, OrderClientIdLegRole, OrderClientIdPlan,
    OrderClientIdReading, OrderClientIdShape, OrderClientIdSource, OrderClientIdStrategyCode,
    U_WIDTHS, order_client_id_readings, parse_order_client_id, write_base36,
};

fn shape() -> impl Strategy<Value = OrderClientIdShape> {
    (any::<bool>(), 0..U_WIDTHS.len(), prop::sample::select(vec![0u8, 9, 99]), any::<bool>())
        .prop_map(|(account, w, max_gen, tag)| OrderClientIdShape {
            account,
            u_width: if account { FULL_U_WIDTH } else { U_WIDTHS[w] },
            max_gen,
            tag,
        })
}

fn role() -> impl Strategy<Value = Option<OrderClientIdLegRole>> {
    prop_oneof![
        Just(None),
        Just(Some(OrderClientIdLegRole::Entry)),
        Just(Some(OrderClientIdLegRole::Stop)),
        (1..=MAX_ROLE_NUMBER).prop_map(|n| Some(OrderClientIdLegRole::Target(n))),
        (1..=MAX_ROLE_NUMBER).prop_map(|n| Some(OrderClientIdLegRole::Member(n))),
    ]
}

proptest! {
    /// ROUND TRIP over all shapes, codes and widths, with the tag read back by both `parse_order_client_id`
    /// and `origin_of_order_client_id`, and exactly one shape reading the id.
    #[test]
    fn a_minted_v2_id_parses_back_to_its_parts(
        tag in proptest::option::of("[a-z0-9]{1,4}"),
        shape in shape(),
        strat_id in 0..=OrderClientIdStrategyCode::MAX_ID,
        account_id in 0i64..1_400,
        src in 0..OrderClientIdSource::ALL.len(),
        intent in 0..OrderClientIdIntent::ALL.len(),
        role in role(),
        generation in 0u8..=99,
        base in 0u64..36u64.pow(12),
        seq in 0u64..(1 << 40),
    ) {
        let origin = tag.as_deref().map(|t| OrderClientIdOrigin::parse(t).unwrap());
        let mut u12 = [0u8; 12];
        let session = format!(
            "{}W{}",
            tag.as_deref().unwrap_or(""),
            std::str::from_utf8(write_base36(base, 12, &mut u12)).unwrap()
        );
        let mut order_client_ids = OrderClientIdGenerator::resume(session, seq);
        prop_assert!(order_client_ids.is_v2());
        prop_assert_eq!(order_client_ids.origin(), origin.as_ref().map(OrderClientIdOrigin::as_str));
        let account = OrderClientIdAccountCode::of_id(account_id);
        let plan = OrderClientIdPlan::new(shape, account);
        let order = OrderClientIdLabels {
            src: OrderClientIdSource::ALL[src],
            strat: OrderClientIdStrategyCode::new(strat_id).unwrap(),
            intent: OrderClientIdIntent::ALL[intent],
            role,
            generation: generation.min(shape.max_gen),
        };
        let id = plan.mint(&mut order_client_ids, order);
        prop_assert!(is_valid_crypto_order_client_id(&id), "{}", id);
        prop_assert_eq!(order_client_id_readings(&id), 1, "{}", id);
        let shown = if shape.tag { tag.as_deref() } else { None };
        prop_assert_eq!(origin_of_order_client_id(&id), shown, "{}", id);
        let Some(OrderClientIdReading::V2(p)) = parse_order_client_id(&id) else {
            return Err(TestCaseError::fail(format!("{id} did not parse v2")));
        };
        let mut u = [0u8; 12];
        let want_u = std::str::from_utf8(write_base36(base + seq, shape.u_width as usize, &mut u)).unwrap();
        prop_assert_eq!(p.origin, shown);
        prop_assert_eq!(p.src, order.src);
        prop_assert_eq!(p.strat, order.strat);
        prop_assert_eq!(p.account, shape.account.then_some(account));
        prop_assert_eq!(p.unique, want_u);
        prop_assert_eq!(p.intent, order.intent);
        prop_assert_eq!(p.role, order.role);
        prop_assert_eq!(p.generation, order.generation);
        prop_assert!(id.starts_with(p.chain) && p.chain.starts_with(p.stem), "{}", id);
        prop_assert_eq!(order_client_ids.state().1, seq + 1, "one id, one sequence number");
    }

    /// NO STRING READS AS TWO SHAPES, over strings built from the grammar's own pieces (the
    /// all-digit account code that a compact `U` could swallow included) and over any alphanumeric
    /// string.
    #[test]
    fn no_string_parses_as_two_shapes(
        grammar in "([a-z0-9]{1,4}W)?[GCTMSE][0-9A-Z]{4}([0-9]{2}|[0-9A-Z]{2})?[0-9a-z]{5,16}(OL|OS|CL|CS|FL|FS)(EN|SL|TP[1-9A-Z]|M[1-9A-Z])?(R[1-9][0-9]?)?",
        any_id in "[A-Za-z0-9]{0,32}",
    ) {
        prop_assert!(order_client_id_readings(&grammar) <= 1, "{}", grammar);
        prop_assert!(order_client_id_readings(&any_id) <= 1, "{}", any_id);
    }

    /// EVERY v1 ID PARSES V1: the core's and the clients' ids, tagged or not, and the desktop's
    /// prefixed ones, with their instance tag read by `origin_of_order_client_id` as before.
    #[test]
    fn every_v1_id_parses_v1(
        hex in "[0-9a-f]{8}",
        tag in proptest::option::of("[a-z0-9]{1,4}"),
        prefix in prop::sample::select(vec!["", "ui", "dom", "poly", "opt"]),
        seq in 0u64..1_000_000_000,
    ) {
        let origin = tag.as_deref().map(|t| OrderClientIdOrigin::parse(t).unwrap());
        let mut order_client_ids = OrderClientIdGenerator::with_origin(origin.as_ref(), Some(hex.as_str()));
        let (session, _) = order_client_ids.state();
        order_client_ids = OrderClientIdGenerator::resume(session, seq);
        prop_assert!(!order_client_ids.is_v2());
        let id = format!("{prefix}{}", order_client_ids.generate());
        prop_assert_eq!(parse_order_client_id(&id), Some(OrderClientIdReading::V1 { origin: origin_of_order_client_id(&id) }), "{}", id);
        if prefix.is_empty() {
            prop_assert_eq!(origin_of_order_client_id(&id), tag.as_deref(), "{}", id);
        }
    }
}
