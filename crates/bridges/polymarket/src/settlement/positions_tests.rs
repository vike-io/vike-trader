use super::*;
use std::collections::HashMap;

/// A last page of `GET /v2/positions` around `rows`.
fn page(rows: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "data": rows,
        "pagination": {"limit": 1000, "offset": 0, "has_more": false, "next_cursor": null},
    })
}

/// The positions of a last page around `rows`.
fn held(rows: serde_json::Value) -> Vec<Position> {
    parse_positions(&page(rows)).expect("a page").positions
}

fn canned() -> serde_json::Value {
    serde_json::json!([
        {"condition_id":"0x01","token_id":"111","current_size":100.0,"redeemable":true,"negative_risk":false,"outcome_index":0,"title":"BTC>100k","current_price":1.0},
        {"condition_id":"0x02","token_id":"222","current_size":50.0,"redeemable":false,"negative_risk":false,"outcome_index":1,"title":"still trading","current_price":0.4},
        {"condition_id":"0x03","token_id":"333","current_size":10.0,"redeemable":true,"negative_risk":true,"outcome_index":0,"title":"neg-risk win","current_price":1.0},
        {"condition_id":"0x04","token_id":"444","current_size":0.0,"redeemable":true,"negative_risk":false,"outcome_index":0,"title":"zero size","current_price":1.0}
    ])
}

/// A `WinnerSource::Chain` backing table — the offline stand-in for `ChainOracle`. A conditionId
/// absent from the table yields `None`, i.e. the RPC-blip shape.
fn chain_table(rows: &[(&str, u128, &[u128])]) -> HashMap<String, ChainResolution> {
    rows.iter()
        .map(|(cid, den, numerators_value)| {
            (
                (*cid).to_string(),
                ChainResolution {
                    condition_id: (*cid).to_string(),
                    denominator: *den,
                    numerators: numerators_value.to_vec(),
                },
            )
        })
        .collect()
}

#[test]
fn parse_reads_all_positions() {
    let ps = held(canned());
    assert_eq!(ps.len(), 4);
    assert_eq!(ps[0].condition_id, "0x01");
    assert!(ps[0].redeemable && ps[0].size == 100.0 && !ps[0].neg_risk);
    assert!(ps[2].neg_risk);
    assert_eq!(ps[0].current_price, Some(1.0));
    assert_eq!(ps[0].outcome_index, Some(0));
    assert_eq!(ps[1].outcome_index, Some(1));
}

#[test]
fn redeemable_includes_binary_and_neg_risk_winners() {
    let ps = held(canned());
    let r = redeemable(&ps, |_| false, &WinnerSource::CurrentPriceOnly);
    // Includes BOTH the binary winner (0x01) and the neg-risk winner (0x03); still excludes
    // the unresolved row (0x02, redeemable=false) and the zero-size one (0x04).
    let cids: Vec<_> = r.iter().map(|p| p.condition_id.as_str()).collect();
    assert!(cids.contains(&"0x01") && cids.contains(&"0x03"), "both winners included");
    assert!(!cids.contains(&"0x02") && !cids.contains(&"0x04"));
    assert_eq!(r.len(), 2);
}

#[test]
fn redeemable_excludes_already_redeemed() {
    let ps = held(canned());
    let r = redeemable(&ps, |cid| cid == "0x01", &WinnerSource::CurrentPriceOnly);
    assert!(r.iter().all(|p| p.condition_id != "0x01"));
}

/// A row naming no token is skipped; a row naming a token with no condition is still a holding to the
/// parser (the reconcile client reports it) and only settlement drops it ([`PositionsClient::list`]).
#[test]
fn parse_skips_a_row_naming_no_token() {
    let ps = held(serde_json::json!([
        {"token_id":"111","current_size":100.0,"redeemable":true}, // missing condition_id
        {"condition_id":"0x05"}, // missing token_id
        {"condition_id":"0x06","token_id":"666","current_size":5.0,"redeemable":true,"negative_risk":false,"outcome_index":0,"title":"ok"},
        "not an object",
    ]));
    assert_eq!(ps.len(), 2);
    assert_eq!((ps[0].asset.as_str(), ps[0].condition_id.as_str()), ("111", ""));
    assert_eq!(ps[1].condition_id, "0x06");
    assert_eq!(ps[1].current_price, None, "an absent current_price is None, not a guessed 0.0");
    assert_eq!(ps[1].average_price, None, "an absent avg_price is None, not a guessed 0.0");
}

/// A body that is no page is an error and never an empty book: an empty book reads as a wallet that
/// sold everything, which `prune_flat` acts on. The old parser answered `[]` to every one of these.
#[test]
fn a_body_that_is_no_page_is_an_error_not_an_empty_book() {
    let rows = serde_json::json!([]);
    for bad in [
        serde_json::json!({"error": "Data API v1 has been retired"}),
        serde_json::json!({"data": null, "pagination": {"has_more": false}}),
        serde_json::json!({"data": {"proxy_wallet": "0x1", "value": 10.0}, "pagination": {}}),
        serde_json::json!([]),
        serde_json::json!({"data": rows}),
        serde_json::json!({"data": rows, "pagination": null}),
        serde_json::json!({"data": rows, "pagination": {"has_more": true, "next_cursor": null}}),
        serde_json::json!({"data": rows, "pagination": {"has_more": true, "next_cursor": ""}}),
    ] {
        assert!(parse_positions(&bad).is_err(), "read as a page: {bad}");
    }
}

/// An empty list IS an answer when the page says so (measured on a never-used address).
#[test]
fn an_empty_wallet_is_an_empty_page() {
    let body = serde_json::json!({
        "data": [], "pagination": {"limit": 1000, "offset": 0, "has_more": false, "next_cursor": null}
    });
    let got = parse_positions(&body).unwrap();
    assert!(got.positions.is_empty() && got.next_cursor.is_none());
}

/// The holding is `current_size`: a row whose holding cannot be read refuses the WHOLE read, naming the
/// token. Reading it as zero reports a held position flat; skipping it reports a false orphan.
#[test]
fn an_unreadable_holding_refuses_the_read_and_is_never_a_zero() {
    for bad in [
        serde_json::json!(null),
        serde_json::json!("abc"),
        serde_json::json!("NaN"),
        serde_json::json!("inf"),
        serde_json::json!(-1.0),
        serde_json::json!(true),
        serde_json::json!({}),
    ] {
        let body = page(serde_json::json!([
            {"condition_id":"0x1","token_id":"good","current_size":3.0,"total_size":9.0},
            {"condition_id":"0x2","token_id":"tok-77","current_size":bad,"total_size":9.0},
        ]));
        let why = parse_positions(&body).unwrap_err();
        assert!(why.contains("tok-77") && why.contains("current_size"), "{why}");
    }
    // And an ABSENT one, with the lifetime total right there to be misread.
    let body =
        page(serde_json::json!([{"condition_id":"0x1","token_id":"tok-77","total_size":9.0}]));
    assert!(parse_positions(&body).unwrap_err().contains("tok-77"));
}

/// `total_size` is LIFETIME bought shares (it equals the holding on 3 of a real wallet's 4 rows, which
/// is why reading it could look right), and a quoted holding reads as a number.
#[test]
fn the_holding_is_current_size_never_the_lifetime_total() {
    let ps = held(serde_json::json!([
        {"condition_id":"0x1","token_id":"a","current_size":3.0,"total_size":9.0},
        {"condition_id":"0x2","token_id":"b","current_size":"2.5","total_size":9.0},
    ]));
    assert_eq!((ps[0].size, ps[1].size), (3.0, 2.5));
}

/// `outcome_index: 999` means the venue could not label the outcome: no slot, so no verdict, under
/// either winner source (`current_price` alone must not license a redeem addressed to slot 999).
#[test]
fn the_unlabeled_outcome_sentinel_is_no_slot() {
    let ps = held(serde_json::json!([
        {"condition_id":"0x1","token_id":"a","current_size":3.0,"redeemable":true,"outcome_index":999,"current_price":1.0},
    ]));
    assert_eq!(ps[0].outcome_index, None);
    assert_eq!(payout_of(&ps[0], &WinnerSource::CurrentPriceOnly), Payout::Unknown);
}

// --- the walk, over a loopback double ---------------------------------------------------------

/// The public sample address, a plain `0x` address the read accepts.
const WALLET: &str = "0x0000000000000000000000000000000000000001";

const ANCHOR: &str = "user=0x0000000000000000000000000000000000000001&filter_amount=0\
                      &include_archived=true&sort_by=TOKENS";

fn walk_server(
    handler: impl Fn(&crate::execution::test_support::Recorded) -> crate::execution::test_support::Reply
    + Send
    + 'static,
) -> crate::execution::test_support::FakeServer {
    crate::execution::test_support::declare_direct_egress();
    crate::execution::test_support::FakeServer::start(handler)
}

fn one_row_page(token: &str, cursor: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "data": [{"condition_id": "0xc", "token_id": token, "current_size": 1.0}],
        "pagination": {"limit": 1, "offset": 0, "has_more": cursor.is_some(), "next_cursor": cursor},
    })
}

/// Every page, in order: the first asks the most rows the route serves, and each later one re-sends the
/// anchor and filters beside an encoded cursor (the cursor does not carry them all, and a bare cursor
/// is a 400), with no `limit`, which only the first page takes.
#[test]
fn the_walk_follows_the_cursor_to_its_end_and_resends_the_filters() {
    use crate::execution::test_support::Reply;
    let server = walk_server(|r| {
        Reply::json(
            200,
            match r.query().split("cursor=").nth(1) {
                None => one_row_page("1", Some("a+b/c=")),
                Some("a%2Bb%2Fc%3D") => one_row_page("2", Some("second")),
                Some("second") => one_row_page("3", None),
                Some(other) => panic!("unexpected cursor {other}"),
            },
        )
    });
    let got = read_whole_wallet(&server.base, WALLET, Duration::from_secs(5)).unwrap();
    let tokens: Vec<_> = got.iter().map(|p| p.asset.as_str()).collect();
    assert_eq!(tokens, ["1", "2", "3"]);
    let seen = server.seen();
    assert_eq!(seen.len(), 3);
    assert!(seen.iter().all(|r| r.method == "GET" && r.path() == "/v2/positions"), "{seen:?}");
    assert_eq!(seen[0].query(), format!("{ANCHOR}&limit=1000"));
    assert_eq!(seen[1].query(), format!("{ANCHOR}&cursor=a%2Bb%2Fc%3D"));
    assert_eq!(seen[2].query(), format!("{ANCHOR}&cursor=second"));
}

/// All or nothing: a page that fails mid-walk fails the read, so a wallet never reads as holding only
/// its first page.
#[test]
fn a_page_failing_mid_walk_fails_the_whole_read() {
    use crate::execution::test_support::Reply;
    let server = walk_server(|r| {
        if r.query().contains("cursor=") {
            Reply::text(503, "<html>secret-from-a-proxy</html>")
        } else {
            Reply::json(200, one_row_page("1", Some("next")))
        }
    });
    let why = read_whole_wallet(&server.base, WALLET, Duration::from_secs(5)).unwrap_err();
    assert!(why.contains("could not be read: HTTP 503"), "{why}");
    assert!(!why.contains("secret-from-a-proxy"), "the status head only: {why}");
}

/// A cursor that never ends is a refusal, not a hung pass and not a short list.
#[test]
fn a_walk_that_does_not_end_is_refused() {
    use crate::execution::test_support::Reply;
    let server = walk_server(|_| Reply::json(200, one_row_page("1", Some("again"))));
    let why = read_whole_wallet(&server.base, WALLET, Duration::from_secs(5)).unwrap_err();
    assert!(why.contains("did not end"), "{why}");
    assert_eq!(server.seen().len(), MAX_WALK_PAGES, "bounded");
}

/// Settlement's discovery: the walk, minus a row naming no condition (nothing can be redeemed without
/// one). A retired path is a refusal here too: the old `get_json` + `parse_positions` read the 410 body
/// as an EMPTY wallet, which would have evicted every watched token.
#[test]
fn settlement_discovery_refuses_a_retired_path_and_drops_a_row_with_no_condition() {
    use crate::execution::test_support::Reply;
    let retired = walk_server(|_| Reply::text(410, r#"{"error":"Data API v1 has been retired"}"#));
    let why = PositionsClient::list(&retired.base, WALLET).unwrap_err();
    assert!(why.contains("retired") && why.contains("/v2/positions"), "{why}");

    let server = walk_server(|_| {
        Reply::json(
            200,
            page(serde_json::json!([
                {"token_id":"no-condition","current_size":1.0},
                {"condition_id":"0xc","token_id":"kept","current_size":1.0},
            ])),
        )
    });
    let got = PositionsClient::list(&server.base, WALLET).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].asset, "kept");
}

/// A REAL `/v2/positions` body, unedited: the public sample address, read through the Dublin tunnel on
/// 2026-11-07. Row 1's `current_size` (10) and row 0's (5000) equal the CTF's `balanceOf / 1e6`.
const V2_SAMPLE: &str =
    include_str!("../../tests/data/data_api_v2_positions_sample_2026-11-07.json");

/// Every field settlement reads, under its v2 name, from the real body.
#[test]
fn the_real_v2_body_reads_every_field_under_its_new_name() {
    let body: serde_json::Value = serde_json::from_str(V2_SAMPLE).expect("the fixture is JSON");
    let ps = parse_positions(&body).unwrap().positions;
    assert_eq!(ps.len(), 2, "both held tokens, never an empty book: {ps:?}");
    let (loser, winner) = (&ps[0], &ps[1]);
    assert_eq!(
        winner.condition_id,
        "0x87822b3d4ccba1835d698354b01c050b55f20c5f34b14908bff4ea5a6d58af4c"
    );
    assert_eq!(
        winner.asset,
        "29677413448356631423250052573262008356913370083986817030510370064664558041792"
    );
    assert_eq!(winner.size, 10.0, "current_size, not the lifetime total_size (0 on this row)");
    assert_eq!(winner.outcome_index, Some(0));
    assert_eq!(winner.current_price, Some(1.0));
    assert!(winner.neg_risk && winner.redeemable);
    assert_eq!(loser.size, 5000.0);
    assert_eq!(loser.outcome_index, Some(1));
    assert_eq!(loser.current_price, Some(0.0), "a stated zero, not an absent price");
    assert!(loser.neg_risk && loser.redeemable, "redeemable is a RESOLUTION flag, losers included");
}

// =============================================================================================
// `outcomeIndex` — THE LAST ROUTE BY WHICH A LOSER COULD REACH THE RELAYER.
//
// Pre-fix: `.and_then(|v| v.as_u64()).unwrap_or(0) as u32`. Every shape below yielded `0` — a
// silent claim to hold slot 0 — and on a condition the chain resolved `[1, 0]` that reads
// `payout_for_index(0) == 1.0` ⇒ Winner ⇒ a relayer redeem for a WORTHLESS position plus a
// permanent at-most-once ledger tombstone, and a `neg_risk_amounts` array pointed at the wrong
// slot. Every test here holds slot **1** of exactly that resolution: the truth is LOSER, and no
// unreadable shape may ever say Winner.
// =============================================================================================
mod outcome_index_hardening {
    use super::*;

    /// The condition every test below is held against: resolved `[1, 0]` over denominator 1, so
    /// slot 0 pays 1.0 and slot 1 — the one actually held — pays 0.
    const COND: &str = "0xdead";

    /// One `/v2/positions` row for `COND` with `outcome_index` set to `index` VERBATIM (whatever JSON
    /// shape the caller passes), `current_price: 1.0` (a stale/degraded indexer mark, so the
    /// `CurrentPriceOnly` arm has something to wrongly believe) and `negative_risk: true` (so the
    /// mis-slotting of `neg_risk_amounts` is in scope too).
    fn row(index: serde_json::Value) -> Vec<Position> {
        held(serde_json::json!([{
            "condition_id": COND, "token_id": "tok", "current_size": 10.0, "redeemable": true,
            "negative_risk": true, "outcome_index": index, "title": "held on slot 1",
            "current_price": 1.0
        }]))
    }

    /// The same row with `outcome_index` OMITTED entirely — the partial/degraded page.
    fn row_without_the_field() -> Vec<Position> {
        held(serde_json::json!([{
            "condition_id": COND, "token_id": "tok", "current_size": 10.0, "redeemable": true,
            "negative_risk": true, "title": "held on slot 1", "current_price": 1.0
        }]))
    }

    fn resolved_one_zero() -> HashMap<String, ChainResolution> {
        chain_table(&[(COND, 1, &[1, 0])])
    }

    /// **The premise, stated as an assertion**: on this resolution slot 0 pays and slot 1 does
    /// not. Every `unwrap_or(0)` below would therefore have converted the held LOSER into a
    /// winner — this is the exact arithmetic that made the defect expensive.
    #[test]
    fn slot_zero_pays_and_slot_one_does_not_on_this_resolution() {
        let table = resolved_one_zero();
        let r = table.get(COND).unwrap();
        assert_eq!(r.payout_for_index(0), Some(1.0), "the slot a `0` fallback would claim");
        assert_eq!(r.payout_for_index(1), Some(0.0), "the slot actually held — worthless");
    }

    /// An absent field is `None`, never a guessed `0` — the `current_price` discipline, applied to
    /// the strictly more load-bearing field.
    #[test]
    fn an_absent_outcome_index_is_none_not_slot_zero() {
        let ps = row_without_the_field();
        assert_eq!(ps.len(), 1, "the ROW is kept — only the slot is unknown");
        assert_eq!(ps[0].outcome_index, None);
    }

    /// A quoted decimal is read EXACTLY, per this venue's documented number-or-string wire
    /// (`recon_client::number_val` on these very rows). Reading `"1"` as `1` is the difference
    /// between correctly calling the held leg a loser and stalling on it forever.
    #[test]
    fn a_quoted_numeric_outcome_index_is_read_exactly() {
        assert_eq!(row(serde_json::json!("1"))[0].outcome_index, Some(1));
        assert_eq!(row(serde_json::json!("0"))[0].outcome_index, Some(0));
        assert_eq!(row(serde_json::json!(" 2 "))[0].outcome_index, Some(2), "whitespace trimmed");
        assert_eq!(row(serde_json::json!(1))[0].outcome_index, Some(1), "plain number still works");
    }

    /// Everything that is not an exact non-negative integer is `None`. Note `1.5` and `-1`:
    /// tolerating the STRING encoding must not become tolerating a rounded or signed one.
    #[test]
    fn a_non_integer_outcome_index_is_none() {
        for bad in [
            serde_json::json!("abc"),
            serde_json::json!(""),
            serde_json::json!("1abc"),
            serde_json::json!("1.5"),
            serde_json::json!(1.5),
            serde_json::json!(-1),
            serde_json::json!("-1"),
            serde_json::json!(null),
            serde_json::json!(true),
            serde_json::json!({}),
            serde_json::json!([1]),
        ] {
            assert_eq!(row(bad.clone())[0].outcome_index, None, "garbage `{bad}` became a slot");
        }
    }

    /// **The truncation bug, pinned.** `as u32` WRAPPED: `4294967296` (2³²) truncated to exactly
    /// `0` — the paying slot — so a huge value was not merely out of range, it was a forged
    /// claim to hold the winner. `u32::try_from` refuses it.
    #[test]
    fn an_out_of_u32_range_outcome_index_is_none_not_a_wrapped_zero() {
        // Multiples of 2³² wrapped to EXACTLY slot 0 — the paying slot. The premise is asserted
        // so this test states the harm, not just the fix.
        for huge in [4_294_967_296u64, 8_589_934_592] {
            assert_eq!(huge as u32, 0, "premise: the pre-fix cast wrapped this to slot 0");
            assert_eq!(row(serde_json::json!(huge))[0].outcome_index, None);
            assert_eq!(row(serde_json::json!(huge.to_string()))[0].outcome_index, None);
        }
        // Other out-of-range values wrapped to some arbitrary in-range slot instead (`u64::MAX`
        // → 4294967295). Harmless on today's 2-slot binaries, but still a fabricated index.
        for huge in [u64::MAX, u64::from(u32::MAX) + 1, 4_294_967_297] {
            assert_eq!(row(serde_json::json!(huge))[0].outcome_index, None);
            assert_eq!(row(serde_json::json!(huge.to_string()))[0].outcome_index, None);
        }
    }

    /// **THE REAL-MONEY PIN.** For every unreadable shape, on the `[1, 0]` resolution whose slot
    /// 1 is genuinely held: the verdict is `Unknown` under BOTH winner sources, and `redeemable`
    /// selects NOTHING. Not `Winner`, and — equally important — not `Loser` either: a
    /// fail-closed `Unknown` retries next tick, whereas asserting `Loser` on an unreadable slot
    /// would be the same guess in the other direction.
    #[test]
    fn no_unreadable_outcome_index_ever_produces_a_winner() {
        let table = resolved_one_zero();
        let lookup = |cid: &str| table.get(cid).cloned();
        let unreadable = [
            serde_json::json!(null),
            serde_json::json!("abc"),
            serde_json::json!(1.5),
            serde_json::json!(-1),
            serde_json::json!(true),
            serde_json::json!(4_294_967_296u64),
            serde_json::json!(u64::MAX),
        ];
        for bad in unreadable {
            for ps in [row(bad.clone()), row_without_the_field()] {
                for src in [WinnerSource::Chain(&lookup), WinnerSource::CurrentPriceOnly] {
                    assert_eq!(
                        payout_of(&ps[0], &src),
                        Payout::Unknown,
                        "an unreadable outcomeIndex (`{bad}`) must fail closed, not guess"
                    );
                    assert!(
                        redeemable(&ps, |_| false, &src).is_empty(),
                        "an unreadable outcomeIndex (`{bad}`) reached the relayer"
                    );
                }
            }
        }
    }

    /// The control: the SAME row with a readable slot behaves exactly as before — slot 1 is a
    /// `Loser` on the chain, and slot 0 (the one the old fallback invented) is a `Winner`. So
    /// the hardening removed the guess without dulling the verdict.
    #[test]
    fn a_readable_outcome_index_still_decides_normally() {
        let table = resolved_one_zero();
        let lookup = |cid: &str| table.get(cid).cloned();
        let held_loser = row(serde_json::json!(1));
        assert_eq!(payout_of(&held_loser[0], &WinnerSource::Chain(&lookup)), Payout::Loser);
        assert!(redeemable(&held_loser, |_| false, &WinnerSource::Chain(&lookup)).is_empty());

        let held_winner = row(serde_json::json!(0));
        assert_eq!(payout_of(&held_winner[0], &WinnerSource::Chain(&lookup)), Payout::Winner);
        assert_eq!(redeemable(&held_winner, |_| false, &WinnerSource::Chain(&lookup)).len(), 1);
    }

    /// An unreadable slot must NOT evict the position from settlement tracking — the reason
    /// `parse_positions` keeps the row instead of skipping it like a missing `token_id`.
    /// `resolved_candidates` still sees it (so a caller can say "resolved but unclassifiable"),
    /// and `crate::settlement::resolve`'s `prune_flat` still sees a held `size > 0` row.
    #[test]
    fn an_unreadable_slot_keeps_the_row_visible_as_a_held_position() {
        let ps = row_without_the_field();
        assert_eq!(resolved_candidates(&ps, |_| false).len(), 1, "still a resolved candidate");
        assert!(ps[0].size > 0.0, "still reads as held, so prune_flat cannot evict it");
    }
}

// =============================================================================================
// THE LIVE WALLET FIXTURE — the reproducible four-position table this module's doc pins.
//
// This repo's own mainnet funder, read 2026-07-23 and again 2026-08-02.
// ALL FOUR rows report `redeemable: true`; exactly ONE of them pays. Everything below exists so
// the loser-selection bug cannot come back.
// =============================================================================================
mod live_wallet_fixture {
    use super::*;

    // The four real conditionIds, right-padded to a full 32 bytes (only the leading bytes were
    // recorded in the investigation; the padding is inert — nothing here parses the value).
    const SOL: &str = "0x13bf6efb00000000000000000000000000000000000000000000000000000000";
    const DOGE_JUN11: &str = "0xbf33623900000000000000000000000000000000000000000000000000000000";
    const DOGE_JUN9: &str = "0x5a71ff8800000000000000000000000000000000000000000000000000000000";
    const BNB: &str = "0xf361b0aa00000000000000000000000000000000000000000000000000000000";

    /// The rows as the data-api returns them, IN DATA-API ORDER. That order is load-bearing for the
    /// D2 regression guard below: the single winner is LAST.
    fn live_positions_json() -> serde_json::Value {
        serde_json::json!([
            {"condition_id":SOL,"token_id":"1001","current_size":29.11,"redeemable":true,"negative_risk":false,"outcome_index":1,"title":"Solana Up or Down - June 9","current_price":0.0},
            {"condition_id":DOGE_JUN11,"token_id":"1002","current_size":10.0,"redeemable":true,"negative_risk":false,"outcome_index":0,"title":"Doge Up or Down - June 11","current_price":0.0},
            {"condition_id":DOGE_JUN9,"token_id":"1003","current_size":10.0,"redeemable":true,"negative_risk":false,"outcome_index":0,"title":"Doge Up or Down - June 9","current_price":0.0},
            {"condition_id":BNB,"token_id":"1004","current_size":5.0,"redeemable":true,"negative_risk":false,"outcome_index":1,"title":"BNB Up or Down - June 12","current_price":1.0}
        ])
    }

    /// The chain's answer for the same four conditions: `payoutNumerators` over denominator 1.
    fn live_chain() -> HashMap<String, ChainResolution> {
        chain_table(&[
            (SOL, 1, &[1, 0]),        // held index 1 -> 0  LOSER
            (DOGE_JUN11, 1, &[0, 1]), // held index 0 -> 0  LOSER
            (DOGE_JUN9, 1, &[0, 1]),  // held index 0 -> 0  LOSER
            (BNB, 1, &[0, 1]),        // held index 1 -> 1  WINNER
        ])
    }

    fn chain_source(
        t: &HashMap<String, ChainResolution>,
    ) -> impl Fn(&str) -> Option<ChainResolution> + '_ {
        move |cid: &str| t.get(cid).cloned()
    }

    /// **The defect, pinned.** The `redeemable` flag alone selects ALL FOUR — this is exactly
    /// what the old filter returned, and what would have produced four relayer calls, three
    /// paying nothing.
    #[test]
    fn the_redeemable_flag_alone_selects_all_four_including_three_losers() {
        let ps = held(live_positions_json());
        assert_eq!(ps.len(), 4);
        assert!(ps.iter().all(|p| p.redeemable), "every row reports redeemable: true");
        assert_eq!(
            resolved_candidates(&ps, |_| false).len(),
            4,
            "the flag is a RESOLUTION flag: it cannot tell the winner from the losers"
        );
    }

    /// The fix: the on-chain source keeps exactly the BNB winner.
    #[test]
    fn the_chain_source_selects_only_the_one_real_winner() {
        let ps = held(live_positions_json());
        let table = live_chain();
        let lookup = chain_source(&table);
        let r = redeemable(&ps, |_| false, &WinnerSource::Chain(&lookup));
        assert_eq!(r.len(), 1, "one winner, not four");
        assert_eq!(r[0].condition_id, BNB);
        assert_eq!(r[0].size, 5.0, "the $5 BNB position");
    }

    /// **The D2 regression guard.** In data-api order the winner is LAST, so
    /// `candidates.iter().find(|p| !p.neg_risk)` over the FLAG-only set picks the Solana LOSER.
    /// Over the winner-filtered set the same `find` can only ever pick a winner.
    #[test]
    fn first_binary_of_the_flag_set_is_a_loser_but_of_the_winner_set_is_the_winner() {
        let ps = held(live_positions_json());
        let flag_only = resolved_candidates(&ps, |_| false);
        assert_eq!(
            flag_only.iter().find(|p| !p.neg_risk).map(|p| p.condition_id.as_str()),
            Some(SOL),
            "the pre-fix smoke selected the Solana loser and redeemed $0"
        );

        let table = live_chain();
        let lookup = chain_source(&table);
        let winners = redeemable(&ps, |_| false, &WinnerSource::Chain(&lookup));
        assert_eq!(
            winners.iter().find(|p| !p.neg_risk).map(|p| p.condition_id.as_str()),
            Some(BNB),
            "the winner-filtered set has no loser to pick"
        );
    }

    /// `curPrice` agreed with the chain on 4/4 of this sample — the measurement that makes the
    /// opt-out defensible, pinned so a future divergence in the fixture is visible.
    #[test]
    fn current_price_agrees_with_the_chain_on_all_four_rows() {
        let ps = held(live_positions_json());
        let table = live_chain();
        let lookup = chain_source(&table);
        for p in &ps {
            assert_eq!(
                payout_of(p, &WinnerSource::Chain(&lookup)),
                payout_of(p, &WinnerSource::CurrentPriceOnly),
                "sources disagree on {} ({})",
                p.title,
                p.condition_id
            );
        }
        let chain_set = redeemable(&ps, |_| false, &WinnerSource::Chain(&lookup));
        let price_set = redeemable(&ps, |_| false, &WinnerSource::CurrentPriceOnly);
        assert_eq!(chain_set, price_set);
    }

    /// Agreement is NOT a licence to trust `curPrice`: when the two disagree the chain wins,
    /// because the chain is what the redeem contract pays out of. A stale indexer marking the
    /// Solana loser at 1.0 must not make it redeemable.
    #[test]
    fn when_current_price_disagrees_with_the_chain_the_chain_decides() {
        let mut ps = held(live_positions_json());
        ps[0].current_price = Some(1.0); // stale indexer: the Solana LOSER now marks 1.0
        let table = live_chain();
        let lookup = chain_source(&table);

        let chain_set = redeemable(&ps, |_| false, &WinnerSource::Chain(&lookup));
        assert_eq!(chain_set.len(), 1);
        assert_eq!(chain_set[0].condition_id, BNB, "the chain still says only BNB pays");

        let price_set = redeemable(&ps, |_| false, &WinnerSource::CurrentPriceOnly);
        assert_eq!(price_set.len(), 2, "the opt-out believes the stale mark — its stated cost");
    }

    /// An RPC blip is `Unknown`, and `Unknown` redeems NOTHING. The next tick asks again, so
    /// this is a delay, never a forfeit.
    #[test]
    fn an_unreachable_chain_redeems_nothing() {
        let ps = held(live_positions_json());
        let down = |_: &str| None; // every lookup fails
        let r = redeemable(&ps, |_| false, &WinnerSource::Chain(&down));
        assert!(r.is_empty(), "fail closed: no winner source, no relayer call");
        for p in &ps {
            assert_eq!(payout_of(p, &WinnerSource::Chain(&down)), Payout::Unknown);
        }
    }

    /// A condition the chain says has NOT resolved (denominator 0) while the data-api flags it
    /// redeemable is a contradiction, not a winner — `Unknown`, skip.
    #[test]
    fn a_chain_unresolved_condition_is_unknown_not_a_winner() {
        let ps = held(live_positions_json());
        let table = chain_table(&[(BNB, 0, &[])]);
        let lookup = chain_source(&table);
        assert_eq!(
            payout_of(&ps[3], &WinnerSource::Chain(&lookup)),
            Payout::Unknown,
            "denominator 0 means the CTF has not been told the outcome yet"
        );
        assert!(redeemable(&ps, |_| false, &WinnerSource::Chain(&lookup)).is_empty());
    }

    /// An outcome index that is READABLE but outside the payout vector cannot be priced —
    /// `Unknown`, not a guessed 0. Distinct from an UNREADABLE index (`None`, see
    /// `outcome_index_hardening`): this one fails at `payout_for_index`, that one before the
    /// source is consulted at all.
    #[test]
    fn an_out_of_range_outcome_index_is_unknown() {
        let mut ps = held(live_positions_json());
        ps[3].outcome_index = Some(7);
        let table = live_chain();
        let lookup = chain_source(&table);
        assert_eq!(payout_of(&ps[3], &WinnerSource::Chain(&lookup)), Payout::Unknown);
    }

    /// An absent `curPrice` (a degraded/partial page) is `Unknown`, NOT a loser — the whole
    /// reason the field is `Option<f64>`.
    #[test]
    fn an_absent_current_price_is_unknown_not_a_loser() {
        let mut ps = held(live_positions_json());
        ps[3].current_price = None;
        assert_eq!(payout_of(&ps[3], &WinnerSource::CurrentPriceOnly), Payout::Unknown);
        assert!(redeemable(&ps, |_| false, &WinnerSource::CurrentPriceOnly).is_empty());
    }

    /// A SPLIT resolution (`[1,1]` over 2) pays 0.5 to BOTH legs — a payout no boolean flag can
    /// express, and both legs are genuine winners.
    #[test]
    fn a_split_resolution_makes_both_legs_winners() {
        let ps = held(serde_json::json!([
            {"condition_id":"0xsplit","token_id":"1","current_size":10.0,"redeemable":true,"negative_risk":false,"outcome_index":0,"title":"split","current_price":0.5},
            {"condition_id":"0xsplit","token_id":"2","current_size":10.0,"redeemable":true,"negative_risk":false,"outcome_index":1,"title":"split","current_price":0.5}
        ]));
        let table = chain_table(&[("0xsplit", 2, &[1, 1])]);
        let lookup = chain_source(&table);
        let r = redeemable(&ps, |_| false, &WinnerSource::Chain(&lookup));
        assert_eq!(r.len(), 2, "0.5 is a payout, so both legs pay");
    }
}
