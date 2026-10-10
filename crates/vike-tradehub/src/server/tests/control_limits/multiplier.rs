//! The contract multiplier: the ceiling is sized with the addressed engine's multiplier.
use super::*;

// ---------------------------------------------------------------------------------------------
// THE CONTRACT MULTIPLIER — the ceiling sized `|qty| · |price|` and nothing else
// ---------------------------------------------------------------------------------------------
//
// The desktop's order preview and the core's `RiskGate` size with `vike_model::order_notional`,
// which multiplies by the instrument's contract multiplier; this edge — the only enforcing site of
// `policy.max_notional_per_order` — did not, so for an instrument whose multiplier is not 1 the
// ceiling was weaker here than in the GUI that sent the order. The rows below publish a roster
// (the node's engine blocks) and watch the SAME ceiling move with it.

/// [`submit`]'s limit order naming an ACCOUNT of the venue — the three wire states
/// (`None`, `DEFAULT`, a label) are what picks WHICH engine's multiplier applies.
fn submit_on(account: Option<&str>, qty: f64, price: f64) -> WireCommand {
    WireCommand::Submit(WireOrderRequest {
        client_order_id: "c1".into(),
        venue: "hyperliquid".into(),
        symbol: "BTC".into(),
        side: 1,
        qty,
        order_type: "limit".into(),
        price: Some(price),
        trigger_price: None,
        reduce_only: false,
        account: account.map(str::to_string),
    })
}

/// **THE TABLE.** One ceiling (1000), many rosters: a `Submit` is refused exactly when
/// `|qty| · |price| · multiplier` is above it, the multiplier being the one of the engine the
/// command ADDRESSES for the symbol it NAMES — and the read-only dry-run says the same words the
/// executing gate does, row for row.
///
/// The `at the cap` rows sit on products that are exact in binary floating point (`10 · 100`,
/// `1 · 10 · 100`), because the cap is `>` and an equality row is only a witness where the
/// arithmetic does not round.
///
/// ⚠ **THE FLOOR.** A multiplier below 1, and one that is zero, negative or not a finite number,
/// is counted as 1.0 — the node's ceiling is never weaker than `|qty| · |price|` and a bad value
/// in venue data cannot loosen it. The rows that used to pin "a multiplier below 1 sizes the order
/// SMALLER" (the `0.01` ones) now pin the opposite, and the `0`, negative, NaN and infinite rows
/// each sit where the UNfloored arithmetic gives the other verdict (`0` sizes everything at
/// nothing; `|-0.5|` halves; NaN is never above the cap; infinity is always above it).
///
/// ⚠ KILL PROOF 1: remove the multiplier from `notional_reason` (size `(qty, price, 1.0)` whatever
/// the roster says) and every `multiplier 100` row that expects a refusal, plus the `ALT` row,
/// flips to accepted. KILL PROOF 2: remove the floor (`floored_multiplier` returns `m`) and the
/// `0.01`, `0`, `-0.5` and NaN rows that expect a refusal flip to accepted, the infinite row that
/// expects an accept flips to refused.
#[test]
fn the_ceiling_counts_the_contract_multiplier_of_the_addressed_engine() {
    // (label, published roster, the account the wire names, qty, price, refused?)
    type Row = (&'static str, Vec<vike_exec::VenueBlock>, Option<&'static str>, f64, f64, bool);
    let hl = |m: f64| vec![block("hyperliquid", "BTC", m)];
    let two_books =
        || vec![block("hyperliquid", "BTC", 1.0), block("hyperliquid#ALT", "BTC", 100.0)];
    // A block whose GRID lists nothing and whose engine-wide DEFAULT is `m` — the other place a
    // published multiplier comes from (`VenueBlock::multiplier_of`'s fallback).
    let default_only = |m: f64| {
        vec![vike_exec::VenueBlock {
            venue: "hyperliquid".into(),
            route_key: "hyperliquid".into(),
            symbol: "BTC".into(),
            multiplier_default: m,
            ..Default::default()
        }]
    };
    let rows: Vec<Row> = vec![
        // No roster, or a multiplier of 1: today's arithmetic, bit for bit.
        ("no roster: exactly at the cap", vec![], None, 10.0, 100.0, false),
        ("no roster: just over", vec![], None, 10.0, 100.01, true),
        ("multiplier 1: exactly at the cap", hl(1.0), None, 10.0, 100.0, false),
        ("multiplier 1: just over", hl(1.0), None, 10.0, 100.01, true),
        // A multiplier above 1 makes the node refuse SOONER.
        ("multiplier 100: exactly at the cap (1 x 10 x 100)", hl(100.0), None, 1.0, 10.0, false),
        ("multiplier 100: just over (1 x 10.01 x 100)", hl(100.0), None, 1.0, 10.01, true),
        (
            "multiplier 100: 1 x 20 is far under the cap unmultiplied; 2000 is over",
            hl(100.0),
            None,
            1.0,
            20.0,
            true,
        ),
        ("multiplier 100: a negative qty is a magnitude", hl(100.0), None, -1.0, 20.0, true),
        ("multiplier 100: a negative price is a magnitude", hl(100.0), None, 1.0, -20.0, true),
        // A multiplier below 1 is FLOORED to 1: the node is stricter than the desktop and the core,
        // which size a sub-1 contract smaller, and never weaker than the unmultiplied arithmetic.
        (
            "multiplier 0.01 floors to 1: 1000 x 99 = 99000 is over (0.01 would say 990, under)",
            hl(0.01),
            None,
            1000.0,
            99.0,
            true,
        ),
        (
            "multiplier 0.01 floors to 1: 1000 x 50 = 50000 is over (0.01 would say 500, under)",
            hl(0.01),
            None,
            1000.0,
            50.0,
            true,
        ),
        (
            "multiplier 0.01 floors to 1: exactly at the cap (10 x 100)",
            hl(0.01),
            None,
            10.0,
            100.0,
            false,
        ),
        (
            "multiplier 0.01 floors to 1: just over (10 x 100.01)",
            hl(0.01),
            None,
            10.0,
            100.01,
            true,
        ),
        (
            "a sub-1 engine-wide DEFAULT floors too (no grid row at all)",
            default_only(0.5),
            None,
            10.0,
            100.01,
            true,
        ),
        // A multiplier that is not a finite positive number is bad venue data: read as 1.
        (
            "multiplier 0 floors to 1: 10 x 100.01 is over (0 would size it at nothing)",
            hl(0.0),
            None,
            10.0,
            100.01,
            true,
        ),
        ("multiplier 0 floors to 1: exactly at the cap", hl(0.0), None, 10.0, 100.0, false),
        (
            "multiplier -0.5 floors to 1: 10 x 100.01 is over (|-0.5| would say 500, under)",
            hl(-0.5),
            None,
            10.0,
            100.01,
            true,
        ),
        (
            "multiplier NaN floors to 1: 10 x 100.01 is over (a NaN notional is never above the cap)",
            hl(f64::NAN),
            None,
            10.0,
            100.01,
            true,
        ),
        ("multiplier NaN floors to 1: exactly at the cap", hl(f64::NAN), None, 10.0, 100.0, false),
        (
            "multiplier +inf floors to 1: exactly at the cap (inf would always be over)",
            hl(f64::INFINITY),
            None,
            10.0,
            100.0,
            false,
        ),
        (
            "multiplier -inf floors to 1: exactly at the cap",
            hl(f64::NEG_INFINITY),
            None,
            10.0,
            100.0,
            false,
        ),
        // WHICH engine's multiplier: the one the command addresses, never a neighbour's.
        (
            "account ALT named: ALT's engine, multiplier 100",
            two_books(),
            Some("ALT"),
            1.0,
            20.0,
            true,
        ),
        ("no account named: the bare-venue engine, not ALT's", two_books(), None, 1.0, 20.0, false),
        (
            "DEFAULT named: the bare-venue engine, not ALT's",
            two_books(),
            Some("DEFAULT"),
            1.0,
            20.0,
            false,
        ),
        ("an account no engine carries sizes at 1", hl(100.0), Some("NOSUCH"), 1.0, 20.0, false),
        (
            "another venue's engine lends no multiplier",
            vec![block("deribit", "BTC", 100.0)],
            None,
            1.0,
            20.0,
            false,
        ),
        (
            "a symbol the engine's grid does not list sizes at 1",
            vec![block("hyperliquid", "ETH", 100.0)],
            None,
            1.0,
            20.0,
            false,
        ),
    ];
    for (label, roster, account, qty, price, refused) in rows {
        let cmd = submit_on(account, qty, price);
        let mut l = limits(Some(1_000.0), 1e9);
        let vetted = l.vet(&cmd, &roster, &[]);
        assert_eq!(vetted.is_some(), refused, "{label}: `vet` answered {vetted:?}");
        let previewed = l.preview_vet(&cmd, &roster, &[]);
        assert_eq!(
            previewed, vetted,
            "{label}: the dry-run must give the executing gate's verdict, word for word"
        );
    }
}

/// The refusal says WHY the notional is not `qty x price` only when there is a multiplier — so every
/// sentence that existed before the node counted one reads byte-identically (tests, runbooks and
/// the Telegram channel key off those words). A multiplier the floor raised to 1 is no multiplier
/// at all as far as the sentence goes.
#[test]
fn the_refusal_names_the_multiplier_only_when_there_is_one() {
    let l = limits(Some(1_000.0), 1e9);
    let plain =
        "order notional 2000.00 exceeds the node's policy ceiling max_notional_per_order 1000.00";
    assert_eq!(l.preview_vet(&submit(5.0, Some(400.0)), &[], &[]).as_deref(), Some(plain));
    assert_eq!(
        l.preview_vet(&submit(5.0, Some(400.0)), &[block("hyperliquid", "BTC", 1.0)], &[])
            .as_deref(),
        Some(plain),
        "a multiplier of exactly 1 adds nothing to the sentence"
    );
    assert_eq!(
        l.preview_vet(&submit(5.0, Some(400.0)), &[block("hyperliquid", "BTC", 0.01)], &[])
            .as_deref(),
        Some(plain),
        "a multiplier the floor raised to 1 adds nothing either, and is not named as 0.01"
    );
    assert_eq!(
        l.preview_vet(&submit(1.0, Some(20.0)), &[block("hyperliquid", "BTC", 100.0)], &[])
            .as_deref(),
        Some(
            "order notional 2000.00 exceeds the node's policy ceiling max_notional_per_order \
             1000.00 (the notional counts this instrument's contract multiplier 100)"
        )
    );
}

/// A bracket is three orders of one size on ONE engine, so every priced leg is sized with that
/// engine's multiplier — and the refusal still names the leg and now says why the number is what
/// it is. A sub-1 engine is floored to 1 on every leg, like a `Submit`.
///
/// ⚠ KILL PROOF: remove the floor and the sub-1 bracket below is sized at 0.01 on every leg
/// (1200 becomes 12) and passes.
#[test]
fn a_bracket_is_sized_with_its_engines_multiplier_on_every_priced_leg() {
    let l = limits(Some(1_000.0), 1e9);
    let roster = [block("hyperliquid", "BTC", 100.0)];
    // Legs 1 x {5, 4, 6} x 100 = 500, 400, 600 — all under.
    assert!(l.preview_vet(&bracket(1, 1.0, Some(5.0), 4.0, 6.0), &roster, &[]).is_none());
    // The take-profit alone is over: 1 x 12 x 100 = 1200.
    let msg = l
        .preview_vet(&bracket(1, 1.0, Some(5.0), 4.0, 12.0), &roster, &[])
        .expect("a take-profit leg of 1200 is over a 1000 ceiling");
    assert!(
        msg.contains("the bracket's take-profit leg")
            && msg.contains("1200.00")
            && msg.contains("contract multiplier 100"),
        "{msg}"
    );
    // Unmultiplied — no roster — the very same bracket passes: the multiplier is the difference.
    assert!(l.preview_vet(&bracket(1, 1.0, Some(5.0), 4.0, 12.0), &[], &[]).is_none());
    // A sub-1 engine is FLOORED to 1, not sized smaller: 100 x {5, 4, 12} x 1 = 500, 400, 1200 —
    // the take-profit is over (0.01 would say 12), and the sentence names no multiplier, because
    // the one counted is 1.
    let sub_one = [block("hyperliquid", "BTC", 0.01)];
    let msg = l
        .preview_vet(&bracket(1, 100.0, Some(5.0), 4.0, 12.0), &sub_one, &[])
        .expect("a sub-1 engine is floored to 1, so the 1200 take-profit leg is over the ceiling");
    assert!(
        msg.contains("the bracket's take-profit leg")
            && msg.contains("1200.00")
            && !msg.contains("contract multiplier"),
        "{msg}"
    );
}
