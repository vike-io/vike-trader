use super::*;
use std::collections::HashMap;

fn canned() -> serde_json::Value {
    serde_json::json!([
        {"conditionId":"0x01","asset":"111","size":100.0,"redeemable":true,"negativeRisk":false,"outcomeIndex":0,"title":"BTC>100k","curPrice":1.0},
        {"conditionId":"0x02","asset":"222","size":50.0,"redeemable":false,"negativeRisk":false,"outcomeIndex":1,"title":"still trading","curPrice":0.4},
        {"conditionId":"0x03","asset":"333","size":10.0,"redeemable":true,"negativeRisk":true,"outcomeIndex":0,"title":"neg-risk win","curPrice":1.0},
        {"conditionId":"0x04","asset":"444","size":0.0,"redeemable":true,"negativeRisk":false,"outcomeIndex":0,"title":"zero size","curPrice":1.0}
    ])
}

/// A `WinnerSource::Chain` backing table — the offline stand-in for `ChainOracle`. A conditionId
/// absent from the table yields `None`, i.e. the RPC-blip shape.
fn chain_table(rows: &[(&str, u128, &[u128])]) -> HashMap<String, ChainResolution> {
    rows.iter()
        .map(|(cid, den, nums)| {
            (
                (*cid).to_string(),
                ChainResolution {
                    condition_id: (*cid).to_string(),
                    denominator: *den,
                    numerators: nums.to_vec(),
                },
            )
        })
        .collect()
}

#[test]
fn parse_reads_all_positions() {
    let ps = parse_positions(&canned());
    assert_eq!(ps.len(), 4);
    assert_eq!(ps[0].condition_id, "0x01");
    assert!(ps[0].redeemable && ps[0].size == 100.0 && !ps[0].neg_risk);
    assert!(ps[2].neg_risk);
    assert_eq!(ps[0].cur_price, Some(1.0));
    assert_eq!(ps[0].outcome_index, Some(0));
    assert_eq!(ps[1].outcome_index, Some(1));
}

#[test]
fn redeemable_includes_binary_and_neg_risk_winners() {
    let ps = parse_positions(&canned());
    let r = redeemable(&ps, |_| false, &WinnerSource::CurPriceOnly);
    // Includes BOTH the binary winner (0x01) and the neg-risk winner (0x03); still excludes
    // the unresolved row (0x02, redeemable=false) and the zero-size one (0x04).
    let cids: Vec<_> = r.iter().map(|p| p.condition_id.as_str()).collect();
    assert!(cids.contains(&"0x01") && cids.contains(&"0x03"), "both winners included");
    assert!(!cids.contains(&"0x02") && !cids.contains(&"0x04"));
    assert_eq!(r.len(), 2);
}

#[test]
fn redeemable_excludes_already_redeemed() {
    let ps = parse_positions(&canned());
    let r = redeemable(&ps, |cid| cid == "0x01", &WinnerSource::CurPriceOnly);
    assert!(r.iter().all(|p| p.condition_id != "0x01"));
}

#[test]
fn parse_skips_malformed_entries() {
    let json = serde_json::json!([
        {"asset":"111","size":100.0,"redeemable":true}, // missing conditionId
        {"conditionId":"0x05"}, // missing asset
        {"conditionId":"0x06","asset":"666","size":5.0,"redeemable":true,"negativeRisk":false,"outcomeIndex":0,"title":"ok"},
        "not an object",
    ]);
    let ps = parse_positions(&json);
    assert_eq!(ps.len(), 1);
    assert_eq!(ps[0].condition_id, "0x06");
    assert_eq!(ps[0].cur_price, None, "an absent curPrice is None, not a guessed 0.0");
}

#[test]
fn parse_non_array_returns_empty() {
    let ps = parse_positions(&serde_json::json!({"error": "bad user"}));
    assert!(ps.is_empty());
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

    /// One `/positions` row for `COND` with `outcomeIndex` set to `idx` VERBATIM (whatever JSON
    /// shape the caller passes), `curPrice: 1.0` (a stale/degraded indexer mark, so the
    /// `CurPriceOnly` arm has something to wrongly believe) and `negativeRisk: true` (so the
    /// mis-slotting of `neg_risk_amounts` is in scope too).
    fn row(idx: serde_json::Value) -> Vec<Position> {
        parse_positions(&serde_json::json!([{
            "conditionId": COND, "asset": "tok", "size": 10.0, "redeemable": true,
            "negativeRisk": true, "outcomeIndex": idx, "title": "held on slot 1", "curPrice": 1.0
        }]))
    }

    /// The same row with `outcomeIndex` OMITTED entirely — the partial/degraded page.
    fn row_without_the_field() -> Vec<Position> {
        parse_positions(&serde_json::json!([{
            "conditionId": COND, "asset": "tok", "size": 10.0, "redeemable": true,
            "negativeRisk": true, "title": "held on slot 1", "curPrice": 1.0
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

    /// An absent field is `None`, never a guessed `0` — the `cur_price` discipline, applied to
    /// the strictly more load-bearing field.
    #[test]
    fn an_absent_outcome_index_is_none_not_slot_zero() {
        let ps = row_without_the_field();
        assert_eq!(ps.len(), 1, "the ROW is kept — only the slot is unknown");
        assert_eq!(ps[0].outcome_index, None);
    }

    /// A quoted decimal is read EXACTLY, per this venue's documented number-or-string wire
    /// (`recon_client::num_val` on these very rows). Reading `"1"` as `1` is the difference
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
                for src in [WinnerSource::Chain(&lookup), WinnerSource::CurPriceOnly] {
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
    /// `parse_positions` keeps the row instead of skipping it like a missing `conditionId`.
    /// `resolved_candidates` still sees it (so a caller can say "resolved but unclassifiable"),
    /// and `crate::exec_plane::settlement::resolve`'s `prune_flat` still sees a held `size > 0` row.
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
// Funder 0x107C01D04Fd68557ACd52E89dD01972b22803aD5, read 2026-07-23 and again 2026-08-02.
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

    /// The `/positions` payload as the data-api returns it, IN DATA-API ORDER. That order is
    /// load-bearing for the D2 regression guard below: the single winner is LAST.
    fn live_positions_json() -> serde_json::Value {
        serde_json::json!([
            {"conditionId":SOL,"asset":"1001","size":29.11,"redeemable":true,"negativeRisk":false,"outcomeIndex":1,"title":"Solana Up or Down - June 9","curPrice":0.0},
            {"conditionId":DOGE_JUN11,"asset":"1002","size":10.0,"redeemable":true,"negativeRisk":false,"outcomeIndex":0,"title":"Doge Up or Down - June 11","curPrice":0.0},
            {"conditionId":DOGE_JUN9,"asset":"1003","size":10.0,"redeemable":true,"negativeRisk":false,"outcomeIndex":0,"title":"Doge Up or Down - June 9","curPrice":0.0},
            {"conditionId":BNB,"asset":"1004","size":5.0,"redeemable":true,"negativeRisk":false,"outcomeIndex":1,"title":"BNB Up or Down - June 12","curPrice":1.0}
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
        let ps = parse_positions(&live_positions_json());
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
        let ps = parse_positions(&live_positions_json());
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
        let ps = parse_positions(&live_positions_json());
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
    fn cur_price_agrees_with_the_chain_on_all_four_rows() {
        let ps = parse_positions(&live_positions_json());
        let table = live_chain();
        let lookup = chain_source(&table);
        for p in &ps {
            assert_eq!(
                payout_of(p, &WinnerSource::Chain(&lookup)),
                payout_of(p, &WinnerSource::CurPriceOnly),
                "sources disagree on {} ({})",
                p.title,
                p.condition_id
            );
        }
        let chain_set = redeemable(&ps, |_| false, &WinnerSource::Chain(&lookup));
        let price_set = redeemable(&ps, |_| false, &WinnerSource::CurPriceOnly);
        assert_eq!(chain_set, price_set);
    }

    /// Agreement is NOT a licence to trust `curPrice`: when the two disagree the chain wins,
    /// because the chain is what the redeem contract pays out of. A stale indexer marking the
    /// Solana loser at 1.0 must not make it redeemable.
    #[test]
    fn when_cur_price_disagrees_with_the_chain_the_chain_decides() {
        let mut ps = parse_positions(&live_positions_json());
        ps[0].cur_price = Some(1.0); // stale indexer: the Solana LOSER now marks 1.0
        let table = live_chain();
        let lookup = chain_source(&table);

        let chain_set = redeemable(&ps, |_| false, &WinnerSource::Chain(&lookup));
        assert_eq!(chain_set.len(), 1);
        assert_eq!(chain_set[0].condition_id, BNB, "the chain still says only BNB pays");

        let price_set = redeemable(&ps, |_| false, &WinnerSource::CurPriceOnly);
        assert_eq!(price_set.len(), 2, "the opt-out believes the stale mark — its stated cost");
    }

    /// An RPC blip is `Unknown`, and `Unknown` redeems NOTHING. The next tick asks again, so
    /// this is a delay, never a forfeit.
    #[test]
    fn an_unreachable_chain_redeems_nothing() {
        let ps = parse_positions(&live_positions_json());
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
        let ps = parse_positions(&live_positions_json());
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
        let mut ps = parse_positions(&live_positions_json());
        ps[3].outcome_index = Some(7);
        let table = live_chain();
        let lookup = chain_source(&table);
        assert_eq!(payout_of(&ps[3], &WinnerSource::Chain(&lookup)), Payout::Unknown);
    }

    /// An absent `curPrice` (a degraded/partial page) is `Unknown`, NOT a loser — the whole
    /// reason the field is `Option<f64>`.
    #[test]
    fn an_absent_cur_price_is_unknown_not_a_loser() {
        let mut ps = parse_positions(&live_positions_json());
        ps[3].cur_price = None;
        assert_eq!(payout_of(&ps[3], &WinnerSource::CurPriceOnly), Payout::Unknown);
        assert!(redeemable(&ps, |_| false, &WinnerSource::CurPriceOnly).is_empty());
    }

    /// A SPLIT resolution (`[1,1]` over 2) pays 0.5 to BOTH legs — a payout no boolean flag can
    /// express, and both legs are genuine winners.
    #[test]
    fn a_split_resolution_makes_both_legs_winners() {
        let ps = parse_positions(&serde_json::json!([
            {"conditionId":"0xsplit","asset":"1","size":10.0,"redeemable":true,"negativeRisk":false,"outcomeIndex":0,"title":"split","curPrice":0.5},
            {"conditionId":"0xsplit","asset":"2","size":10.0,"redeemable":true,"negativeRisk":false,"outcomeIndex":1,"title":"split","curPrice":0.5}
        ]));
        let table = chain_table(&[("0xsplit", 2, &[1, 1])]);
        let lookup = chain_source(&table);
        let r = redeemable(&ps, |_| false, &WinnerSource::Chain(&lookup));
        assert_eq!(r.len(), 2, "0.5 is a payout, so both legs pay");
    }
}
