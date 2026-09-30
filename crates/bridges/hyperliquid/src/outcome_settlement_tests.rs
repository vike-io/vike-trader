use super::*;

const WALLET: &str = "0x1234567890abcdef1234567890abcdef12345678";

/// The docs' own `outcomeMeta` sample (two independent integrator references reproduce this
/// exact shape). Note there is NO settlement field anywhere on it — that is the point: a settled
/// outcome is REMOVED from this response, and its fraction comes from `settledOutcome`.
fn meta_body() -> String {
    r#"{
          "outcomes": [
            {
              "outcome": 9,
              "name": "Who will win the HL 100 meter dash?",
              "description": "This race is yet to be scheduled.",
              "sideSpecs": [{ "name": "Hypurr" }, { "name": "Usain Bolt" }]
            },
            {
              "outcome": 3151,
              "name": "Recurring",
              "description": "class:priceBinary|underlying:HYPE|expiry:20260404-1145|targetPrice:38|period:15m",
              "sideSpecs": [{ "name": "Yes" }, { "name": "No" }]
            }
          ],
          "questions": [
            {
              "question": 1,
              "name": "What will Hypurr eat the most of in Feb 2026?",
              "description": "Hypurr has committed to weighing and recording daily food intake.",
              "fallbackOutcome": 13,
              "namedOutcomes": [10, 11, 12],
              "settledNamedOutcomes": [11]
            }
          ]
        }"#
        .to_string()
}

/// `spotClearinghouseState` with an ordinary USDC row plus BOTH legs of outcome 3151
/// (`+31510` = side 0, `+31511` = side 1) and one leg of the UNSETTLED outcome 9 (`+90`).
fn balances_body() -> String {
    r#"{"balances":[
            {"coin":"USDC","token":0,"hold":"0.0","total":"14.62","entryNtl":"0.0"},
            {"coin":"+31510","token":900,"hold":"0.0","total":"25.0","entryNtl":"12.5"},
            {"coin":"+31511","token":901,"hold":"0.0","total":"10.0","entryNtl":"4.0"},
            {"coin":"+90","token":50,"hold":"0.0","total":"7.0","entryNtl":"3.5"}
        ]}"#
    .to_string()
}

/// The docs' own `settledOutcome` response sample, re-targeted at `outcome` and `fraction`.
/// `settleFraction` is a decimal STRING exactly as documented.
fn settled_body(outcome: u32, fraction: &str) -> String {
    format!(
        r#"{{
              "spec": {{
                "outcome": {outcome},
                "name": "Recurring",
                "description": "class:priceBinary|underlying:BTC|expiry:20260526-0600|targetPrice:77363|period:1d",
                "sideSpecs": [{{ "name": "Yes" }}, {{ "name": "No" }}],
                "quoteToken": "USDC"
              }},
              "settleFraction": "{fraction}",
              "details": "price:76876.9"
            }}"#
    )
}

/// `outcomeMeta` after outcome 3151 settled: the venue has REMOVED it, leaving only the live
/// outcome 9. This is the shape the candidate filter is built against.
fn live_meta_without_3151() -> OutcomeMeta {
    parse_outcome_meta(
        r#"{"outcomes":[{"outcome":9,"sideSpecs":[{"name":"Hypurr"},{"name":"Usain Bolt"}]}]}"#,
    )
    .unwrap()
}

/// The settled record for outcome 3151 at `fraction`.
fn settled_3151(fraction: &str) -> Vec<SettledOutcome> {
    vec![parse_settled_outcome(&settled_body(3151, fraction)).unwrap().unwrap()]
}

// --- encoding ------------------------------------------------------------------------------

#[test]
fn encoding_matches_the_documented_worked_example() {
    // docs: outcome 1, side 0 -> encoding 10 -> "#10" / "+10" / 100000010.
    assert_eq!(encoding(1, 0), 10);
    assert_eq!(spot_coin(1, 0), "#10");
    assert_eq!(token_name(1, 0), "+10");
    assert_eq!(asset_id(1, 0), 100_000_010);
    // side 1 of the same outcome.
    assert_eq!(encoding(1, 1), 11);
    assert_eq!(token_name(1, 1), "+11");
}

#[test]
fn decode_token_name_inverts_the_encoding() {
    for (outcome, side) in [(1u32, 0u32), (1, 1), (3151, 0), (3151, 1), (0, 0)] {
        assert_eq!(decode_token_name(&token_name(outcome, side)), Some((outcome, side)));
    }
    // Ordinary spot coins are not outcome tokens.
    assert_eq!(decode_token_name("USDC"), None);
    assert_eq!(decode_token_name("PURR"), None);
    // The "#" spot-coin form is NOT the token name form.
    assert_eq!(decode_token_name("#10"), None);
    assert_eq!(decode_token_name("+notanumber"), None);
}

// --- parsing -------------------------------------------------------------------------------

#[test]
fn parse_outcome_meta_reads_the_documented_shape() {
    let meta = parse_outcome_meta(&meta_body()).expect("valid json");
    assert_eq!(meta.outcomes.len(), 2);

    let dash = &meta.outcomes[0];
    assert_eq!(dash.outcome, 9);
    assert_eq!(dash.name, "Who will win the HL 100 meter dash?");
    assert_eq!(dash.sides, vec!["Hypurr".to_string(), "Usain Bolt".to_string()]);
    assert!(dash.is_binary());
    assert_eq!(dash.quote_token, None, "outcomeMeta rows carry no quoteToken");

    let recurring = &meta.outcomes[1];
    assert_eq!(recurring.outcome, 3151);
    assert!(recurring.description.starts_with("class:priceBinary|underlying:HYPE"));
    assert_eq!(recurring.sides, vec!["Yes".to_string(), "No".to_string()]);

    let q = &meta.questions[0];
    assert_eq!(q.question, 1);
    assert_eq!(q.fallback_outcome, Some(13));
    assert_eq!(q.named_outcomes, vec![10, 11, 12]);
    assert_eq!(q.settled_named_outcomes, vec![11]);
    assert_eq!(meta.settled_by_question(), HashSet::from([11]));
}

#[test]
fn parse_settled_outcome_reads_the_documented_response() {
    let s = parse_settled_outcome(&settled_body(95, "0.0")).unwrap().expect("settled");
    assert_eq!(s.spec.outcome, 95);
    assert_eq!(s.spec.sides, vec!["Yes".to_string(), "No".to_string()]);
    assert_eq!(s.spec.quote_token, Some("USDC".to_string()));
    assert_eq!(s.settle_fraction, 0.0, "documented as a decimal STRING");
    assert_eq!(s.details, "price:76876.9");
}

#[test]
fn settle_fraction_is_read_from_either_a_string_or_a_bare_number() {
    // Documented as a decimal string; a bare number is tolerated too.
    let spec = r#""spec":{"outcome":1,"sideSpecs":[{"name":"Yes"},{"name":"No"}]}"#;
    let s =
        parse_settled_outcome(&format!(r#"{{{spec},"settleFraction":"0.25"}}"#)).unwrap().unwrap();
    assert_eq!(s.settle_fraction, 0.25);
    let n =
        parse_settled_outcome(&format!(r#"{{{spec},"settleFraction":0.25}}"#)).unwrap().unwrap();
    assert_eq!(n.settle_fraction, 0.25);
}

#[test]
fn parse_settled_outcome_fails_closed_without_a_fraction_or_spec() {
    // An unsettled/unknown outcome: a well-formed body carrying no fraction is NOT settled.
    let no_fraction = r#"{"spec":{"outcome":1,"sideSpecs":[{"name":"Yes"},{"name":"No"}]}}"#;
    assert_eq!(parse_settled_outcome(no_fraction).unwrap(), None);
    assert_eq!(parse_settled_outcome(r#"{"settleFraction":"1.0"}"#).unwrap(), None);
    assert_eq!(parse_settled_outcome("{}").unwrap(), None);
    assert_eq!(parse_settled_outcome("null").unwrap(), None);
    // Only a non-JSON body is an error.
    assert!(parse_settled_outcome("not json").is_err());
}

// --- candidate selection -------------------------------------------------------------------

#[test]
fn candidates_are_held_outcomes_the_live_meta_no_longer_lists() {
    let bals = parse_spot_balances(&balances_body()).unwrap();

    // While BOTH outcomes are still live, nothing is worth querying.
    let live_all = parse_outcome_meta(&meta_body()).unwrap();
    assert!(settlement_candidates(&live_all, &bals).is_empty());

    // Once 3151 is removed from outcomeMeta it becomes the one candidate — deduped across its
    // two held legs. Outcome 9 is still live, and USDC is not an outcome token.
    assert_eq!(settlement_candidates(&live_meta_without_3151(), &bals), vec![3151]);
}

#[test]
fn a_flat_outcome_token_is_never_a_candidate() {
    let bals = parse_spot_balances(r#"{"balances":[{"coin":"+31510","hold":"0","total":"0.0"}]}"#)
        .unwrap();
    assert!(settlement_candidates(&live_meta_without_3151(), &bals).is_empty());
}

#[test]
fn parse_outcome_meta_degrades_on_missing_sections_and_errors_only_on_bad_json() {
    assert_eq!(parse_outcome_meta("{}").unwrap(), OutcomeMeta::default());
    // A row without an `outcome` id is unidentifiable ⇒ skipped, not an error.
    let m = parse_outcome_meta(r#"{"outcomes":[{"name":"nameless"},{"outcome":4}]}"#).unwrap();
    assert_eq!(m.outcomes.len(), 1);
    assert_eq!(m.outcomes[0].outcome, 4);
    assert!(parse_outcome_meta("not json").is_err());
}

#[test]
fn parse_spot_balances_reads_coin_total_hold() {
    let bals = parse_spot_balances(&balances_body()).expect("valid json");
    assert_eq!(bals.len(), 4);
    assert_eq!(bals[0].coin, "USDC");
    assert_eq!(bals[0].total, 14.62);
    assert_eq!(bals[0].outcome_side(), None);
    assert_eq!(bals[1].outcome_side(), Some((3151, 0)));
    assert_eq!(bals[1].total, 25.0);
    assert_eq!(bals[2].outcome_side(), Some((3151, 1)));
    assert_eq!(bals[3].outcome_side(), Some((9, 0)));
    assert!(parse_spot_balances("nope").is_err());
    assert!(parse_spot_balances("{}").unwrap().is_empty());
}

// --- payout --------------------------------------------------------------------------------

#[test]
fn payout_follows_the_hip4_settle_fraction_rule() {
    // "binary yes": settleFraction = 1 ⇒ side 0 pays 1, side 1 pays 0.
    assert_eq!(payout_for_side(0, 1.0), Some(1.0));
    assert_eq!(payout_for_side(1, 1.0), Some(0.0));
    // "binary no": settleFraction = 0 ⇒ the mirror.
    assert_eq!(payout_for_side(0, 0.0), Some(0.0));
    assert_eq!(payout_for_side(1, 0.0), Some(1.0));
    // A fractional settlement splits the quote unit.
    assert_eq!(payout_for_side(0, 0.25), Some(0.25));
    assert_eq!(payout_for_side(1, 0.25), Some(0.75));
    // Beyond the binary side pair the scalar determines nothing.
    assert_eq!(payout_for_side(2, 1.0), None);
}

// --- the derivation core -------------------------------------------------------------------

#[test]
fn derives_both_settled_legs_and_skips_the_unsettled_outcome() {
    let bals = parse_spot_balances(&balances_body()).unwrap();
    let fills = derive_outcome_settlements(&settled_3151("1.0"), &bals, WALLET);

    // Outcome 9 is unsettled (no fraction) and USDC is not an outcome token ⇒ only 3151's two
    // legs settle, ordered by (outcome, side).
    assert_eq!(fills.len(), 2);
    assert_eq!((fills[0].outcome, fills[0].side), (3151, 0));
    assert_eq!(fills[0].coin, "#31510");
    assert_eq!(fills[0].side_name, "Yes");
    assert_eq!(fills[0].qty, 25.0);
    assert_eq!(fills[0].payout, 1.0, "settleFraction 1.0 ⇒ the Yes leg won");
    assert_eq!((fills[1].outcome, fills[1].side), (3151, 1));
    assert_eq!(fills[1].side_name, "No");
    assert_eq!(fills[1].qty, 10.0);
    assert_eq!(fills[1].payout, 0.0, "the No leg is worthless");
}

#[test]
fn no_settled_record_derives_nothing() {
    // The oracle returned nothing for any held outcome ⇒ nothing settles, however much is held.
    let bals = parse_spot_balances(&balances_body()).unwrap();
    assert!(derive_outcome_settlements(&[], &bals, WALLET).is_empty());
}

#[test]
fn a_zero_balance_leg_is_not_settled() {
    // Partial holding: only the losing leg is held, the winning leg is flat.
    let bals = parse_spot_balances(
        r#"{"balances":[
                {"coin":"+31510","hold":"0.0","total":"0.0"},
                {"coin":"+31511","hold":"0.0","total":"10.0"}
            ]}"#,
    )
    .unwrap();
    let fills = derive_outcome_settlements(&settled_3151("1.0"), &bals, WALLET);
    assert_eq!(fills.len(), 1, "the flat leg has nothing to close");
    assert_eq!(fills[0].side, 1);
    assert_eq!(fills[0].payout, 0.0);
}

#[test]
fn a_held_token_with_no_settled_record_is_skipped() {
    let bals = parse_spot_balances(r#"{"balances":[{"coin":"+77770","hold":"0","total":"5.0"}]}"#)
        .unwrap();
    assert!(derive_outcome_settlements(&settled_3151("1.0"), &bals, WALLET).is_empty());
}

#[test]
fn a_non_binary_settled_outcome_is_never_settled_from_one_scalar() {
    let settled = vec![
        parse_settled_outcome(
            r#"{"spec":{"outcome":5,"sideSpecs":[{"name":"A"},{"name":"B"},{"name":"C"}]},
                    "settleFraction":"1.0"}"#,
        )
        .unwrap()
        .unwrap(),
    ];
    assert!(!settled[0].spec.is_binary());
    let bals =
        parse_spot_balances(r#"{"balances":[{"coin":"+50","hold":"0","total":"3.0"}]}"#).unwrap();
    assert!(derive_outcome_settlements(&settled, &bals, WALLET).is_empty());
}

#[test]
fn settled_named_outcomes_alone_never_settles_a_position() {
    // The question reports outcome 11 settled, but only `settledOutcome` carries a fraction —
    // so this signal is corroborating and can never, on its own, emit a fill.
    let meta = parse_outcome_meta(
        r#"{"outcomes":[],
                "questions":[{"question":1,"namedOutcomes":[11],"settledNamedOutcomes":[11]}]}"#,
    )
    .unwrap();
    assert_eq!(meta.settled_by_question(), HashSet::from([11]));
    let bals =
        parse_spot_balances(r#"{"balances":[{"coin":"+110","hold":"0","total":"9.0"}]}"#).unwrap();
    assert!(
        derive_outcome_settlements(&[], &bals, WALLET).is_empty(),
        "a fractionless settled signal is corroborating only"
    );
}

// --- fill id determinism -------------------------------------------------------------------

#[test]
fn settlement_trade_id_is_deterministic_and_wallet_case_insensitive() {
    let a = settlement_trade_id(3151, 0, WALLET);
    let b = settlement_trade_id(3151, 0, WALLET);
    assert_eq!(a, b, "same inputs ⇒ same id, every process and restart");
    assert_eq!(
        a,
        settlement_trade_id(3151, 0, &WALLET.to_ascii_uppercase()),
        "a checksummed address and its lowercase form are ONE identity"
    );
    assert!(a.starts_with("hlsettle:3151:0:"), "id is prefixed + readable: {a}");
    assert_eq!(a.len(), "hlsettle:3151:0:".len() + 16, "16 hex digits of FNV-1a-64");
}

#[test]
fn settlement_trade_id_separates_sides_outcomes_and_wallets() {
    let base = settlement_trade_id(3151, 0, WALLET);
    assert_ne!(base, settlement_trade_id(3151, 1, WALLET), "sides differ");
    assert_ne!(base, settlement_trade_id(3152, 0, WALLET), "outcomes differ");
    assert_ne!(
        base,
        settlement_trade_id(3151, 0, "0x00000000000000000000000000000000deadbeef"),
        "wallets differ"
    );
}

#[test]
fn derived_fills_carry_the_deterministic_ids() {
    let bals = parse_spot_balances(&balances_body()).unwrap();
    let a = derive_outcome_settlements(&settled_3151("1.0"), &bals, WALLET);
    let b = derive_outcome_settlements(&settled_3151("1.0"), &bals, WALLET);
    assert_eq!(a, b, "the pure core is deterministic over one snapshot");
    assert_eq!(a[0].trade_id, settlement_trade_id(3151, 0, WALLET));
}

// --- the fill event ------------------------------------------------------------------------

#[test]
fn settlement_fill_event_is_the_inverse_of_the_local_position() {
    let bals = parse_spot_balances(&balances_body()).unwrap();
    let fills = derive_outcome_settlements(&settled_3151("1.0"), &bals, WALLET);

    // A long 25 winning leg closes with a SELL 25 @ 1.0.
    let ev = settlement_fill_event(&fills[0], 25.0, 1_700_000_000_000);
    assert_eq!(ev.side, -1);
    assert_eq!(ev.last_qty, 25.0);
    assert_eq!(ev.last_px, 1.0);
    assert_eq!(ev.symbol.as_str(), "#31510", "the # spot coin is the vike symbol");
    assert_eq!(ev.venue.as_str(), VENUE);
    assert_eq!(ev.trade_id, fills[0].trade_id);
    assert_eq!(ev.commission, 0.0);
    assert_eq!(
        ev.liquidity_side,
        LiquiditySide::Unknown,
        "a settlement is neither maker nor taker"
    );
    assert!(ev.mark_price.is_none(), "a settlement never writes the price board");
    assert_eq!(ev.ts, 1_700_000_000_000);

    // A SHORT position closes with a BUY of the same size.
    let short = settlement_fill_event(&fills[1], -10.0, 1);
    assert_eq!(short.side, 1);
    assert_eq!(short.last_qty, 10.0);
    assert_eq!(short.last_px, 0.0);
}

// --- ledger --------------------------------------------------------------------------------

#[test]
fn ledger_marks_are_idempotent_and_survive_reopen() {
    let dir = std::env::temp_dir().join(format!("hl_outcome_ledger_{}", std::process::id()));
    let path = dir.join("settled.txt");
    let _ = std::fs::remove_file(&path);

    let ledger = SettlementLedger::open(path.clone());
    assert!(!ledger.contains(3151, 0, WALLET));
    ledger.mark(3151, 0, WALLET);
    ledger.mark(3151, 0, WALLET); // idempotent
    assert!(ledger.contains(3151, 0, WALLET));
    assert!(!ledger.contains(3151, 1, WALLET), "the sibling leg is a distinct key");
    drop(ledger);

    let reopened = SettlementLedger::open(path.clone());
    assert!(reopened.contains(3151, 0, WALLET), "the mark survives a restart");
    // One line per key, not two.
    assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

// --- the tick ------------------------------------------------------------------------------

/// `outcomeMeta` as the venue serves it once outcome 3151 has settled: 3151 is REMOVED, only
/// the live outcome 9 remains. This is the tick-level twin of [`live_meta_without_3151`].
fn live_meta_body() -> String {
    r#"{"outcomes":[{"outcome":9,"name":"Who will win the HL 100 meter dash?",
            "sideSpecs":[{"name":"Hypurr"},{"name":"Usain Bolt"}]}]}"#
        .to_string()
}

/// Scripted stub: canned bodies + a canned settlement oracle + an optional local-book map, no
/// network. By default the oracle reports outcome 3151 settled at `1.0` (the Yes leg won) and
/// every other outcome unsettled.
struct StubDeps {
    meta: String,
    balances: String,
    settled: Vec<(u32, String)>,
    local: Option<Vec<(String, f64)>>,
    fail_meta: bool,
    fail_settled: bool,
}
impl StubDeps {
    fn new(meta: String, balances: String) -> Self {
        StubDeps {
            meta,
            balances,
            settled: vec![(3151, "1.0".to_string())],
            local: None,
            fail_meta: false,
            fail_settled: false,
        }
    }
    fn with_local(mut self, local: Vec<(String, f64)>) -> Self {
        self.local = Some(local);
        self
    }
    /// No outcome is settled — the oracle answers every query with "not settled".
    fn with_nothing_settled(mut self) -> Self {
        self.settled.clear();
        self
    }
    /// Every `settledOutcome` query errors.
    fn failing_settled(mut self) -> Self {
        self.fail_settled = true;
        self
    }
}
impl OutcomeDeps for StubDeps {
    fn fetch_outcome_meta(&self) -> Result<OutcomeMeta, String> {
        if self.fail_meta {
            return Err("boom".to_string());
        }
        parse_outcome_meta(&self.meta)
    }
    fn fetch_spot_balances(&self, _wallet: &str) -> Result<Vec<SpotBalance>, String> {
        parse_spot_balances(&self.balances)
    }
    fn fetch_settled_outcome(&self, outcome: u32) -> Result<Option<SettledOutcome>, String> {
        if self.fail_settled {
            return Err("settledOutcome boom".to_string());
        }
        match self.settled.iter().find(|(o, _)| *o == outcome) {
            Some((o, fraction)) => parse_settled_outcome(&settled_body(*o, fraction)),
            None => Ok(None),
        }
    }
    fn local_position(&self, coin: &str) -> Option<f64> {
        self.local
            .as_ref()
            .map(|m| m.iter().find(|(c, _)| c == coin).map(|(_, q)| *q).unwrap_or(0.0))
    }
}

/// Collecting sink that always accepts.
fn sink(out: &mut Vec<Event>) -> impl FnMut(Event) -> bool + '_ {
    move |e| {
        out.push(e);
        true
    }
}

fn tmp_ledger(tag: &str) -> (PathBuf, SettlementLedger) {
    let dir = std::env::temp_dir().join(format!("hl_outcome_{}_{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("settled.txt");
    (dir, SettlementLedger::open(path))
}

#[test]
fn settle_once_emits_one_fill_per_settled_leg() {
    let (dir, ledger) = tmp_ledger("emit");
    let deps = StubDeps::new(live_meta_body(), balances_body());
    let mut events = Vec::new();

    let report = settle_once(&deps, WALLET, &ledger, &mut sink(&mut events));

    assert_eq!(report.settled, vec!["#31510".to_string(), "#31511".to_string()]);
    assert!(report.failed.is_empty());
    assert_eq!(events.len(), 2);
    match &events[0] {
        Event::Fill(f) => {
            assert_eq!(f.symbol.as_str(), "#31510");
            assert_eq!(f.last_px, 1.0);
            assert_eq!(f.last_qty, 25.0);
            assert_eq!(f.side, -1);
        }
        other => panic!("expected a bare Event::Fill, got {other:?}"),
    }
    // No OrderFilled/OrderPartiallyFilled wraps are emitted (module doc).
    assert!(events.iter().all(|e| matches!(e, Event::Fill(_))));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn settle_once_is_idempotent_across_ticks() {
    let (dir, ledger) = tmp_ledger("idem");
    let deps = StubDeps::new(live_meta_body(), balances_body());
    let mut events = Vec::new();

    let first = settle_once(&deps, WALLET, &ledger, &mut sink(&mut events));
    assert_eq!(first.settled.len(), 2);
    // The very same snapshot on the next tick settles NOTHING more.
    let second = settle_once(&deps, WALLET, &ledger, &mut sink(&mut events));
    assert!(second.settled.is_empty());
    assert_eq!(events.len(), 2, "a rerun emits no duplicate fill");

    // And a fresh ledger reopened from the same file still suppresses them (restart guard).
    let path = dir.join("settled.txt");
    let reopened = SettlementLedger::open(path);
    let third = settle_once(&deps, WALLET, &reopened, &mut sink(&mut events));
    assert!(third.settled.is_empty());
    assert_eq!(events.len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_send_leaves_the_key_unmarked_for_the_next_tick() {
    let (dir, ledger) = tmp_ledger("failsend");
    let deps = StubDeps::new(live_meta_body(), balances_body());

    // Lane gone: every emit rejects.
    let mut rejected = 0usize;
    let report = settle_once(&deps, WALLET, &ledger, &mut |_e| {
        rejected += 1;
        false
    });
    assert_eq!(report.failed, vec!["#31510".to_string(), "#31511".to_string()]);
    assert_eq!(rejected, 2);
    assert!(!ledger.contains(3151, 0, WALLET), "a failed send must not mark");

    // Lane back: the same tick's work is retried and settles.
    let mut events = Vec::new();
    let retry = settle_once(&deps, WALLET, &ledger, &mut sink(&mut events));
    assert_eq!(retry.settled.len(), 2);
    assert_eq!(events.len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_locally_flat_position_is_skipped_not_settled() {
    let (dir, ledger) = tmp_ledger("flat");
    // Local book knows nothing about either leg ⇒ both report 0.0.
    let deps = StubDeps::new(live_meta_body(), balances_body()).with_local(vec![]);
    let mut events = Vec::new();

    let report = settle_once(&deps, WALLET, &ledger, &mut sink(&mut events));
    assert!(events.is_empty(), "nothing to close ⇒ no fabricated fill");
    assert_eq!(report.skipped_flat, vec!["#31510".to_string(), "#31511".to_string()]);
    assert!(ledger.contains(3151, 0, WALLET), "marked so it is not re-derived every tick");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_local_book_override_sizes_the_fill_not_the_venue_balance() {
    let (dir, ledger) = tmp_ledger("override");
    // Venue balance is 25 (some transferred in); the local book only ever traded 4.
    let deps = StubDeps::new(live_meta_body(), balances_body())
        .with_local(vec![("#31510".to_string(), 4.0)]);
    let mut events = Vec::new();

    settle_once(&deps, WALLET, &ledger, &mut sink(&mut events));
    assert_eq!(events.len(), 1, "the other leg is locally flat");
    match &events[0] {
        Event::Fill(f) => {
            assert_eq!(f.last_qty, 4.0, "closes the LOCAL size, not the venue balance")
        }
        other => panic!("expected Event::Fill, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_fetch_aborts_the_tick_without_settling_anything() {
    let (dir, ledger) = tmp_ledger("fetchfail");
    let mut deps = StubDeps::new(live_meta_body(), balances_body());
    deps.fail_meta = true;
    let mut events = Vec::new();

    let report = settle_once(&deps, WALLET, &ledger, &mut sink(&mut events));
    assert_eq!(report, OutcomeTickReport::default());
    assert!(events.is_empty(), "a partial snapshot must never settle");
    assert!(!ledger.contains(3151, 0, WALLET));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_outcome_the_oracle_reports_unsettled_never_settles() {
    // Held, and gone from outcomeMeta — but `settledOutcome` carries no fraction, so nothing is
    // acted on. This is the fail-closed direction the whole design rests on.
    let (dir, ledger) = tmp_ledger("notsettled");
    let deps = StubDeps::new(live_meta_body(), balances_body()).with_nothing_settled();
    let mut events = Vec::new();

    let report = settle_once(&deps, WALLET, &ledger, &mut sink(&mut events));
    assert_eq!(report, OutcomeTickReport::default());
    assert!(events.is_empty(), "no fraction ⇒ no fill ⇒ nothing fabricated");
    assert!(!ledger.contains(3151, 0, WALLET));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_settled_outcome_query_is_skipped_and_retried_not_fatal() {
    let (dir, ledger) = tmp_ledger("oraclefail");
    let deps = StubDeps::new(live_meta_body(), balances_body()).failing_settled();
    let mut events = Vec::new();

    let report = settle_once(&deps, WALLET, &ledger, &mut sink(&mut events));
    assert_eq!(report.unresolved, vec![3151], "the outcome is reported, not settled");
    assert!(report.settled.is_empty());
    assert!(events.is_empty());
    assert!(!ledger.contains(3151, 0, WALLET), "an unresolved outcome must not mark");

    // The oracle recovers: the next tick settles normally.
    let ok = StubDeps::new(live_meta_body(), balances_body());
    let retry = settle_once(&ok, WALLET, &ledger, &mut sink(&mut events));
    assert_eq!(retry.settled.len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_outcome_still_live_in_meta_is_never_queried_or_settled() {
    // outcome 9 is held (`+90`) and still live ⇒ it is not even a candidate.
    let (dir, ledger) = tmp_ledger("stilllive");
    let deps = StubDeps::new(meta_body(), balances_body());
    let mut events = Vec::new();

    let report = settle_once(&deps, WALLET, &ledger, &mut sink(&mut events));
    assert_eq!(report, OutcomeTickReport::default(), "everything held is still live");
    assert!(events.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

// --- the opt-in gate -----------------------------------------------------------------------

#[test]
fn spawn_returns_none_without_a_wallet() {
    // Wallet is checked BEFORE the env gate, so this holds regardless of VIKE_HL_OUTCOME.
    let (tx, _rx) = vike_exec::event_channel(16);
    let handle = OutcomePoller::spawn(
        HyperliquidTransport::new(crate::config::Network::Testnet),
        "   ".to_string(),
        std::env::temp_dir().join("unused_hl_outcome.txt"),
        DEFAULT_POLL_INTERVAL,
        tx,
    );
    assert!(handle.is_none(), "no wallet ⇒ no thread");
}

#[test]
fn the_env_gate_is_the_exact_string_one() {
    // Read-only assertion about the CURRENT process env: this test never mutates it (setting
    // env vars is process-global and would race the rest of the suite). The gate's contract is
    // simply that it mirrors the exact-"1" read.
    let expected = std::env::var("VIKE_HL_OUTCOME").as_deref() == Ok("1");
    assert_eq!(hl_outcome_enabled(), expected);
}
