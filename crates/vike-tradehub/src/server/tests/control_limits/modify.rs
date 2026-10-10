//! A `Modify` is sized with the multiplier of the open order it names.
use super::*;

// ---------------------------------------------------------------------------------------------
// A `Modify` IS SIZED WITH THE MULTIPLIER OF THE ORDER IT NAMES
// ---------------------------------------------------------------------------------------------
//
// A `Modify` names a client order id and nothing else, so the edge reads which instrument that id
// rests on off the published OPEN order. This used to be a pinned residual — "a modify is sized
// unmultiplied because it names no instrument" — which let a `Submit` that passed small be
// modified UP on a multiplier engine and checked at 1.0.

/// **THE TABLE for a `Modify`.** One ceiling (1000), many snapshots: a priced modify is refused
/// exactly when `|new qty| · |new price| · multiplier` is above it, the multiplier being the one of
/// the engine that holds the OPEN order the coid names, for that order's symbol — and an order the
/// published snapshot does not hold sizes at 1.0, which is what every row here did before the edge
/// could resolve one. The read-only dry-run says the same words the executing gate does.
///
/// ⚠ KILL PROOF 1: put the `1.0` back in the `Modify` arm of `notional_reason` (size
/// `(qty, price, 1.0)` whatever the snapshot says) and every row that expects a refusal flips to
/// accepted — `resolved, multiplier 100`, `the ALT order`, `PARTIALLY_FILLED`, `SUBMITTED`. KILL
/// PROOF 2: match the coid on terminal orders too (drop the `is_terminal` test in
/// `order_multiplier`) and the `FILLED` row flips to refused. KILL PROOF 3: look the block up by
/// bare venue instead of by the order's route key and the `ALT` rows swap their verdicts.
#[test]
fn a_modify_is_sized_with_the_multiplier_of_the_order_it_names() {
    use vike_exec::OrderStatus;
    // (label, published roster, published orders, the coid the modify names, qty, price, refused?)
    type Row = (
        &'static str,
        Vec<vike_exec::VenueBlock>,
        Vec<vike_exec::OrderView>,
        &'static str,
        f64,
        f64,
        bool,
    );
    let hl = |m: f64| vec![block("hyperliquid", "BTC", m)];
    let two_books =
        || vec![block("hyperliquid", "BTC", 1.0), block("hyperliquid#ALT", "BTC", 100.0)];
    let on_hl = |coid: &str, status: OrderStatus| {
        vec![published_order(coid, "hyperliquid", None, "BTC", status)]
    };
    let rows: Vec<Row> = vec![
        // The case this change exists for: 1 x 20 is 20 unmultiplied and 2000 on the engine the
        // order rests on.
        (
            "resolved, multiplier 100: 1 x 20 x 100 = 2000 is over (1.0 would say 20, under)",
            hl(100.0),
            on_hl("c1", OrderStatus::Accepted),
            "c1",
            1.0,
            20.0,
            true,
        ),
        (
            "resolved, multiplier 100: exactly at the cap (1 x 10 x 100)",
            hl(100.0),
            on_hl("c1", OrderStatus::Accepted),
            "c1",
            1.0,
            10.0,
            false,
        ),
        (
            "resolved, multiplier 100: a negative qty is a magnitude",
            hl(100.0),
            on_hl("c1", OrderStatus::Accepted),
            "c1",
            -1.0,
            20.0,
            true,
        ),
        // A multiplier of 1 is today's arithmetic, bit for bit.
        (
            "resolved, multiplier 1: exactly at the cap (10 x 100)",
            hl(1.0),
            on_hl("c1", OrderStatus::Accepted),
            "c1",
            10.0,
            100.0,
            false,
        ),
        (
            "resolved, multiplier 1: just over (10 x 100.01)",
            hl(1.0),
            on_hl("c1", OrderStatus::Accepted),
            "c1",
            10.0,
            100.01,
            true,
        ),
        // Every OPEN status resolves — a modify arriving before the order is accepted is sized with
        // the instrument it will rest on.
        (
            "resolved while PARTIALLY_FILLED",
            hl(100.0),
            on_hl("c1", OrderStatus::PartiallyFilled),
            "c1",
            1.0,
            20.0,
            true,
        ),
        (
            "resolved while SUBMITTED, before the venue accepted it",
            hl(100.0),
            on_hl("c1", OrderStatus::Submitted),
            "c1",
            1.0,
            20.0,
            true,
        ),
        // What the snapshot does not hold sizes at 1.0 — exactly today's behaviour.
        (
            "no published order: 1.0, the coid may be too new for the last publish",
            hl(100.0),
            vec![],
            "c1",
            1.0,
            20.0,
            false,
        ),
        (
            "a different coid is published: this one is unknown, 1.0",
            hl(100.0),
            on_hl("other", OrderStatus::Accepted),
            "c1",
            1.0,
            20.0,
            false,
        ),
        (
            "a TERMINAL order (FILLED) of that coid lends nothing: 1.0",
            hl(100.0),
            on_hl("c1", OrderStatus::Filled),
            "c1",
            1.0,
            20.0,
            false,
        ),
        (
            "a TERMINAL order (CANCELED) of that coid lends nothing: 1.0",
            hl(100.0),
            on_hl("c1", OrderStatus::Canceled),
            "c1",
            1.0,
            20.0,
            false,
        ),
        (
            "a TERMINAL order (REJECTED) lends nothing: 10 x 100 stays exactly at the cap",
            hl(100.0),
            on_hl("c1", OrderStatus::Rejected),
            "c1",
            10.0,
            100.0,
            false,
        ),
        // WHICH engine's multiplier: the one that holds the order, never a neighbour's.
        (
            "another venue's block lends nothing: the order rests on hyperliquid, deribit is 100",
            vec![block("deribit", "BTC", 100.0), block("hyperliquid", "BTC", 1.0)],
            on_hl("c1", OrderStatus::Accepted),
            "c1",
            1.0,
            20.0,
            false,
        ),
        (
            "the order rests on deribit: deribit's multiplier 100 applies, hyperliquid's 1 does not",
            vec![block("hyperliquid", "BTC", 1.0), block("deribit", "BTC", 100.0)],
            vec![published_order("c1", "deribit", None, "BTC", OrderStatus::Accepted)],
            "c1",
            1.0,
            20.0,
            true,
        ),
        (
            "the order rests on a venue with NO block: 1.0",
            hl(100.0),
            vec![published_order("c1", "deribit", None, "BTC", OrderStatus::Accepted)],
            "c1",
            1.0,
            20.0,
            false,
        ),
        (
            "the ALT order: ALT's engine, multiplier 100",
            two_books(),
            vec![published_order("c1", "hyperliquid", Some("ALT"), "BTC", OrderStatus::Accepted)],
            "c1",
            1.0,
            20.0,
            true,
        ),
        (
            "the DEFAULT-account order: the bare-venue engine, not ALT's",
            two_books(),
            on_hl("c1", OrderStatus::Accepted),
            "c1",
            1.0,
            20.0,
            false,
        ),
        (
            "an account no block carries sizes at 1",
            hl(100.0),
            vec![published_order(
                "c1",
                "hyperliquid",
                Some("NOSUCH"),
                "BTC",
                OrderStatus::Accepted,
            )],
            "c1",
            1.0,
            20.0,
            false,
        ),
        (
            "a symbol the engine's grid does not list sizes at 1",
            vec![block("hyperliquid", "ETH", 100.0)],
            on_hl("c1", OrderStatus::Accepted),
            "c1",
            1.0,
            20.0,
            false,
        ),
        // The floor applies to a resolved Modify exactly as to a Submit.
        (
            "resolved, multiplier 0.01 floors to 1: 10 x 100.01 is over (0.01 would say 10, under)",
            hl(0.01),
            on_hl("c1", OrderStatus::Accepted),
            "c1",
            10.0,
            100.01,
            true,
        ),
        (
            "resolved, multiplier 0.01 floors to 1: exactly at the cap",
            hl(0.01),
            on_hl("c1", OrderStatus::Accepted),
            "c1",
            10.0,
            100.0,
            false,
        ),
        (
            "resolved, multiplier NaN floors to 1: 10 x 100.01 is over",
            hl(f64::NAN),
            on_hl("c1", OrderStatus::Accepted),
            "c1",
            10.0,
            100.01,
            true,
        ),
        (
            "resolved, multiplier 0 floors to 1: 10 x 100.01 is over",
            hl(0.0),
            on_hl("c1", OrderStatus::Accepted),
            "c1",
            10.0,
            100.01,
            true,
        ),
    ];
    for (label, roster, orders, coid, qty, price, refused) in rows {
        let cmd = modify_to(coid, qty, price);
        let mut l = limits(Some(1_000.0), 1e9);
        let vetted = l.vet(&cmd, &roster, &orders);
        assert_eq!(vetted.is_some(), refused, "{label}: `vet` answered {vetted:?}");
        let previewed = l.preview_vet(&cmd, &roster, &orders);
        assert_eq!(
            previewed, vetted,
            "{label}: the dry-run must give the executing gate's verdict, word for word"
        );
    }
}

/// A resolved `Modify`'s refusal names the multiplier in the same words a `Submit`'s does, and a
/// `Modify` that resolves to 1 reads byte-identically to the sentence before the edge could resolve
/// one.
#[test]
fn a_modify_refusal_names_the_multiplier_only_when_there_is_one() {
    use vike_exec::OrderStatus;
    let l = limits(Some(1_000.0), 1e9);
    let open =
        |coid: &str| vec![published_order(coid, "hyperliquid", None, "BTC", OrderStatus::Accepted)];
    assert_eq!(
        l.preview_vet(
            &modify_to("c1", 1.0, 20.0),
            &[block("hyperliquid", "BTC", 100.0)],
            &open("c1")
        )
        .as_deref(),
        Some(
            "order notional 2000.00 exceeds the node's policy ceiling max_notional_per_order \
             1000.00 (the notional counts this instrument's contract multiplier 100)"
        )
    );
    let plain =
        "order notional 2000.00 exceeds the node's policy ceiling max_notional_per_order 1000.00";
    assert_eq!(
        l.preview_vet(
            &modify_to("c1", 5.0, 400.0),
            &[block("hyperliquid", "BTC", 1.0)],
            &open("c1")
        )
        .as_deref(),
        Some(plain),
        "a multiplier of exactly 1 adds nothing to the sentence"
    );
    assert_eq!(
        l.preview_vet(&modify_to("c1", 5.0, 400.0), &[], &[]).as_deref(),
        Some(plain),
        "an unknown order reads exactly as a Modify always did"
    );
}

/// **Resolving the order can only make the node stricter or equal.** Over a grid of sizes and every
/// multiplier shape the roster can carry (above 1, exactly 1, below 1, zero, negative, NaN,
/// infinite), a `Modify` the node refuses with NO order known is still refused once the order IS
/// known — the floor is what makes that true for the values that are not above 1.
///
/// ⚠ KILL PROOF: remove the floor and the sub-1, zero, negative and NaN multipliers refuse LESS
/// than the unknown-order baseline, which is the failure this loop exists to catch.
#[test]
fn resolving_the_order_never_makes_a_modify_weaker_than_leaving_it_unknown() {
    use vike_exec::OrderStatus;
    let l = limits(Some(1_000.0), 1e9);
    let orders = [published_order("c1", "hyperliquid", None, "BTC", OrderStatus::Accepted)];
    let multipliers =
        [100.0, 2.0, 1.0, 0.5, 0.01, 0.0, -0.5, -100.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY];
    let sizes = [(1.0, 20.0), (10.0, 100.0), (10.0, 100.01), (1000.0, 99.0), (-5.0, 400.0)];
    for m in multipliers {
        let roster = [block("hyperliquid", "BTC", m)];
        for (qty, price) in sizes {
            let cmd = modify_to("c1", qty, price);
            let baseline = l.preview_vet(&cmd, &roster, &[]);
            let resolved = l.preview_vet(&cmd, &roster, &orders);
            // `baseline` sizes at 1.0 (nothing known); `resolved` may only refuse MORE.
            assert!(
                baseline.is_none() || resolved.is_some(),
                "multiplier {m}, {qty} x {price}: refused while the order was unknown ({baseline:?}) \
                 but passes once it is known"
            );
        }
    }
}

/// **A non-finite size or price is REFUSED, not compared.** `NaN > max` is false, so a NaN notional
/// used to read as "within the ceiling"; `inf · 0` is NaN too, so the check is on the FACTORS. The
/// wire cannot carry one in (JSON has no NaN) and the Telegram parser refuses one, so this is the
/// guard for a caller that builds a `WireCommand` in process.
///
/// ⚠ KILL PROOF: delete the `is_finite` check in `notional_reason` and the NaN rows flip to
/// accepted (`NaN > max` is false) and the infinite ones to the old `order notional inf exceeds`
/// sentence, which this test's wording assertion also fails.
#[test]
fn a_non_finite_size_or_price_is_refused_not_compared() {
    let l = limits(Some(1_000.0), 1e9);
    let bad = [
        (f64::NAN, 50.0),
        (10.0, f64::NAN),
        (f64::NAN, f64::NAN),
        (f64::INFINITY, 0.0),
        (f64::INFINITY, 50.0),
        (10.0, f64::INFINITY),
        (f64::NEG_INFINITY, 50.0),
    ];
    for (qty, price) in bad {
        let modify = WireCommand::Modify {
            client_order_id: "c1".into(),
            new_qty: Some(qty),
            new_price: Some(price),
        };
        for cmd in [submit(qty, Some(price)), modify] {
            let label = format!("{cmd:?}");
            let msg = l
                .preview_vet(&cmd, &[], &[])
                .unwrap_or_else(|| panic!("{label} has no finite notional and must be refused"));
            assert!(
                msg.contains("no finite notional") && msg.contains("max_notional_per_order"),
                "{label}: the refusal must say the ceiling could not be evaluated: {msg}"
            );
        }
    }
    // Scoped to a ceiling that exists: with none there is nothing to evaluate and the core
    // `RiskGate` is the floor, exactly as `no_size_cap_without_a_policy_ceiling` says.
    let uncapped = limits(None, 1e9);
    assert!(uncapped.preview_vet(&submit(f64::NAN, Some(50.0)), &[], &[]).is_none());
    // …and a finite order is untouched.
    assert!(l.preview_vet(&submit(10.0, Some(50.0)), &[], &[]).is_none());
}

/// **The roster reaches the ceiling THROUGH `accept_command`**, which is the production call the
/// unit rows above bypass: the gate is handed the `blocks` the surface published, not an empty
/// slice. The refusal happens at step 1, before anything is lowered, so the paper core exists only
/// to supply a real `CommandSink`.
///
/// ⚠ KILL PROOF: have `accept_command` call `limits.vet(&cmd, &[], orders)` and this fails — the
/// order is accepted at multiplier 1.0 and the `expect_err` never sees a refusal.
#[test]
fn accept_command_hands_the_published_roster_to_the_ceiling() {
    let mount = vike_mount::build_paper_maker_core(&vike_mount::MakerMountConfig::outcome_token(
        "polymarket",
        "NOTIONAL_MULTIPLIER_TOKEN",
        Some(3_000_000_000),
    ));
    let sink = mount.handle.command_sink();
    let mut l = limits(Some(1_000.0), 1e9);
    let roster = [block("hyperliquid", "BTC", 100.0)];
    let cmd = submit(1.0, Some(20.0));

    let err = super::control::accept_command(
        cmd.clone(),
        None,
        &mut l,
        &sink,
        None,
        &[],
        &[],
        &roster,
        &[],
        None,
        None,
    )
    .expect_err("1 x 20 x 100 = 2000 is over a 1000 ceiling");
    let msg = err.message();
    assert!(
        msg.contains("2000.00") && msg.contains("contract multiplier 100"),
        "the refusal must come from the roster's multiplier: {msg}"
    );
    assert!(!err.is_fatal(), "a ceiling refusal never closes the surface");
    // The same command with no roster is NOT refused at the edge — the roster is the difference.
    assert!(l.preview_vet(&cmd, &[], &[]).is_none());
}

/// **The order a `Modify` names reaches the ceiling THROUGH `accept_command`** — the production call
/// the table above bypasses. `submit small, modify up` on a multiplier-100 engine: the modify's
/// 1 x 20 is 20 unmultiplied and 2000 on the engine the published order rests on. The refusal
/// happens at step 1, before anything is lowered, so the paper core exists only to supply a real
/// `CommandSink` — and a refused modify must not reach it.
///
/// ⚠ KILL PROOF: have `accept_command` hand `limits.vet` an empty `orders` (or have the `Modify`
/// arm size at 1.0) and the modify is accepted at multiplier 1.0 and the `expect_err` never sees a
/// refusal. The twin assertions pin the unknown-order case: the SAME frame with no published order
/// is not refused at the edge.
#[test]
fn accept_command_hands_the_order_a_modify_names_to_the_ceiling() {
    let mount = vike_mount::build_paper_maker_core(&vike_mount::MakerMountConfig::outcome_token(
        "polymarket",
        "NOTIONAL_MODIFY_MULTIPLIER_TOKEN",
        Some(3_000_000_000),
    ));
    let sink = mount.handle.command_sink();
    let mut l = limits(Some(1_000.0), 1e9);
    let roster = [block("hyperliquid", "BTC", 100.0)];
    let orders =
        [published_order("c1", "hyperliquid", None, "BTC", vike_exec::OrderStatus::Accepted)];
    let cmd = modify_to("c1", 1.0, 20.0);

    let err = super::control::accept_command(
        cmd.clone(),
        None,
        &mut l,
        &sink,
        None,
        &[],
        &[],
        &roster,
        &orders,
        None,
        None,
    )
    .expect_err("1 x 20 x 100 = 2000 is over a 1000 ceiling");
    let msg = err.message();
    assert!(
        msg.contains("2000.00") && msg.contains("contract multiplier 100"),
        "the refusal must come from the named order's multiplier: {msg}"
    );
    assert!(!err.is_fatal(), "a ceiling refusal never closes the surface");
    // The same frame against a snapshot that does not hold the order is sized as it always was.
    assert!(l.preview_vet(&cmd, &roster, &[]).is_none(), "unknown order: 1.0");
    assert!(l.preview_vet(&cmd, &roster, &orders).is_some(), "known order: 100");
}
