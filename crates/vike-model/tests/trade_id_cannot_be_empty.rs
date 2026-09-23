//! The `TradeId` contract: an empty fill identifier is IMPOSSIBLE TO CONSTRUCT.
//!
//! `trade_id` is the dedup key for the whole money lane — `ExecutionEngine::on_event` guards
//! `Account::apply_fill` with it, and `vike_exec::recon::diff` decides whether a venue-reported fill
//! is already booked by looking it up in `seen_trade_ids`. Both guards used to be written
//! `if !fill.trade_id.is_empty()`, so an EMPTY id did not dedup badly — it skipped dedup ENTIRELY
//! and the fill was applied unconditionally. On a live daemon that re-booked commission and realized
//! PnL. Five venue mappers reached the field through `unwrap_or_default()`, which on a string is `""`.
//!
//! These tests pin the type-level half of the cure. The behavioural half (each venue's chosen
//! drop/synthesize policy) is pinned in that venue's own crate.
//!
//! ⚠ The most important property here is NOT tested by any assertion and cannot be: that `TradeId`
//! implements no [`Default`], which is what makes `unwrap_or_default()` a COMPILE error at every
//! producing site. `default_is_not_implemented_and_that_is_the_whole_point` documents it and
//! `trybuild`-free proof is left to the compiler: adding `#[derive(Default)]` to `TradeId` would make
//! the five reverted mapper sites compile again, which is precisely the regression to prevent.

use vike_model::events::{FillEvent, LiquiditySide, PositionSide, TradeId};

#[test]
fn new_refuses_the_empty_string() {
    assert!(TradeId::new("").is_err(), "an empty trade_id must not be constructible");
    assert!(TradeId::new(String::new()).is_err());
    assert!(TradeId::new(String::from("")).is_err());
}

#[test]
fn new_accepts_any_non_empty_id() {
    assert_eq!(TradeId::new("t1").unwrap().as_str(), "t1");
    // A single space is not empty. Refusing it would be a DIFFERENT rule (a trimming rule), and
    // inventing one would silently discard a venue id that is legitimately whitespace-ish.
    assert_eq!(TradeId::new(" ").unwrap().as_str(), " ");
    assert_eq!(TradeId::new("0").unwrap().as_str(), "0");
    // 77-char polymarket-style token ids and long composite ids are ordinary values.
    let long = "a".repeat(200);
    assert_eq!(TradeId::new(&long).unwrap().as_str(), long);
}

#[test]
fn prefixed_is_non_empty_even_when_the_rest_renders_empty() {
    // The whole point of `prefixed`: a static non-empty prefix makes the result non-empty BY
    // CONSTRUCTION, so a minting site needs no `Result` and no `unwrap`.
    assert_eq!(TradeId::prefixed("EXT-ORD-", "").as_str(), "EXT-ORD-");
    assert_eq!(TradeId::prefixed("paper-", 7).as_str(), "paper-7");
    assert_eq!(
        TradeId::prefixed("EXT-ORD-", format_args!("{}-{}", "binance", "v9")).as_str(),
        "EXT-ORD-binance-v9"
    );
}

#[test]
#[should_panic(expected = "non-empty static prefix")]
fn prefixed_rejects_an_empty_prefix() {
    // Guards the one way `prefixed` could produce an empty id. `prefix` is `&'static str`, so this
    // is a source-level bug any test touching the call site would hit — never a venue-data path.
    let _ = TradeId::prefixed("", "");
}

#[test]
#[should_panic(expected = "empty trade_id literal")]
fn the_literal_conversion_rejects_an_empty_literal() {
    // `impl From<&'static str>` exists so the tree's ~150 literal construction sites keep compiling.
    // It is the one constructor that can panic, and it is reachable ONLY from a source literal: a
    // borrowed venue JSON string is never `&'static`, so wire data cannot arrive here. This pins that
    // even the literal door is shut on `""` — `crates/bridges/ctrader/src/conn.rs`'s
    // `fsm_terminal_fill` used to be exactly this, a hardcoded `trade_id: "".into()`.
    let _: TradeId = "".into();
}

#[test]
fn default_is_not_implemented_and_that_is_the_whole_point() {
    // Documentation-as-test. There is no assertion that a trait is ABSENT, so what this pins is the
    // consequence: every constructor below reaches a value, and none of them is a zero value.
    //
    // If `TradeId` ever gained `Default`, `unwrap_or_default()` would compile again at every venue
    // mapper — which is how all five original defects were written:
    //     trade_id: s("id").unwrap_or_default()          // oanda exec + stream
    //     trade_id: deal_id.unwrap_or_default()          // ig
    //     trade_id: s("trade_id").unwrap_or_default()    // fxcm
    //     trade_id: f.get("id")…unwrap_or("")            // alpaca
    // and each yields the empty string, which skipped dedup and double-booked on replay.
    let from_wire = TradeId::new("venue-exec-1").unwrap();
    let minted = TradeId::prefixed("EXT-", 1);
    let literal: TradeId = "t1".into();
    for id in [from_wire, minted, literal] {
        assert!(!id.as_str().is_empty(), "no constructor may yield an empty id");
    }
}

// --- serde: the wire form is unchanged, and the empty case is refused on the way IN --------------

fn fill_with(trade_id: TradeId) -> FillEvent {
    FillEvent {
        trade_id,
        client_order_id: "c1".into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: 1.0,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: "".into(),
        liquidity_side: LiquiditySide::Unknown,
        ts: 1,
        mark_price: None,
        position_side: PositionSide::Both,
    }
}

#[test]
fn serializes_transparently_so_journals_and_fixtures_are_byte_identical() {
    // The newtype must be INVISIBLE on the wire: `#[serde(transparent)]` means a `TradeId` field
    // serializes as the bare string it always did. This is what keeps every committed parity fixture,
    // every exec_db journal row and every Parquet column identical across this change.
    let json = serde_json::to_string(&TradeId::new("t1").unwrap()).unwrap();
    assert_eq!(json, r#""t1""#, "a TradeId must serialize as a bare JSON string, not as an object");

    let fill = serde_json::to_value(fill_with(TradeId::new("258544119").unwrap())).unwrap();
    assert_eq!(fill["trade_id"], serde_json::json!("258544119"));
}

#[test]
fn round_trips_through_serde() {
    let fill = fill_with(TradeId::new("t-round").unwrap());
    let back: FillEvent = serde_json::from_str(&serde_json::to_string(&fill).unwrap()).unwrap();
    assert_eq!(back, fill);
}

#[test]
fn deserialize_refuses_an_empty_id_rather_than_admitting_it() {
    // The door serde would otherwise leave open: without a custom `Deserialize`, a hostile venue
    // payload or a legacy journal row spelling `"trade_id": ""` would rebuild exactly the value the
    // type exists to forbid, and the guard would be skipped again.
    //
    // ⚠ CONSEQUENCE, deliberate and documented on the impl: a pre-existing journal/Parquet row whose
    // `trade_id` is `""` now fails to deserialize. That is the intended direction — such a row is the
    // un-dedupable garbage being eliminated — but it means every persisted READ path must degrade PER
    // ROW (skip + warn) instead of failing a whole batch.
    let err = serde_json::from_str::<TradeId>(r#""""#).unwrap_err().to_string();
    assert!(err.contains("empty"), "the refusal should say why, got: {err}");

    let mut v = serde_json::to_value(fill_with(TradeId::new("t1").unwrap())).unwrap();
    v["trade_id"] = serde_json::json!("");
    assert!(
        serde_json::from_value::<FillEvent>(v).is_err(),
        "a FillEvent carrying an empty trade_id must not deserialize"
    );
}

#[test]
fn a_missing_trade_id_field_does_not_default_into_existence() {
    // `FillEvent.trade_id` carries no `#[serde(default)]`, and could not usefully carry one now that
    // the type has no `Default`. An absent field is a hard decode error rather than a silent `""`.
    let mut v = serde_json::to_value(fill_with(TradeId::new("t1").unwrap())).unwrap();
    v.as_object_mut().unwrap().remove("trade_id");
    assert!(serde_json::from_value::<FillEvent>(v).is_err());
}

// --- the properties the dedup sets rely on ------------------------------------------------------

#[test]
fn is_usable_as_a_hash_set_key_by_str() {
    // `ExecutionEngine::seen_trade_ids` is a `HashSet<String>` looked up by `&str`, and
    // `recon::diff` does `local.seen_trade_ids.contains(f.trade_id.as_str())`. Pin that the newtype
    // still hashes/compares as its string so those lookups cannot silently stop matching.
    use std::collections::HashSet;
    let mut seen: HashSet<String> = HashSet::new();
    let id = TradeId::new("execid-42").unwrap();
    seen.insert(id.to_string());
    assert!(seen.contains(id.as_str()));
    assert_eq!(id, "execid-42");
    assert_eq!(id.as_str(), "execid-42");
}

#[test]
fn is_not_interned_the_interning_contract_is_preserved() {
    // `events.rs`'s interning contract says `trade_id` must NOT be a `Ustr`: it is minted per fill,
    // unbounded, and the `ustr` global table never frees — Nautilus shipped and reverted this exact
    // leak. `TradeId` wraps the same `CompactString` the bare field did, so the property is
    // structural. Pinned here as a SIZE check: a `Ustr` is a single 8-byte pointer into the global
    // table, so a `TradeId` being strictly larger proves it is not one.
    assert!(
        std::mem::size_of::<TradeId>() > std::mem::size_of::<ustr::Ustr>(),
        "TradeId must be an inline/heap string, never an interned Ustr — see the interning \
         contract in vike-model's events.rs module doc"
    );
    // ...and that two equal ids are independent values rather than one shared handle.
    let a = TradeId::new("same").unwrap();
    let b = TradeId::new("same").unwrap();
    assert_eq!(a, b);
    assert_ne!(a.as_str().as_ptr(), b.as_str().as_ptr(), "must not be a shared interned pointer");
}
