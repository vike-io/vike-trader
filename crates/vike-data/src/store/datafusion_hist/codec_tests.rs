use super::*;

#[test]
fn quotes_from_batch_defaults_missing_local_ts() {
    // A batch built with the PRE-plan schema (no local_ts column) must decode with
    // local_ts = 0 — the additive-column contract for already-recorded data.
    let schema = Arc::new(Schema::new(vec![
        Field::new("ts", DataType::Int64, false),
        Field::new("bid", DataType::Float64, false),
        Field::new("ask", DataType::Float64, false),
        Field::new("bid_size", DataType::Float64, false),
        Field::new("ask_size", DataType::Float64, false),
    ]));
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from(vec![5i64])),
            Arc::new(Float64Array::from(vec![1.0])),
            Arc::new(Float64Array::from(vec![1.1])),
            Arc::new(Float64Array::from(vec![2.0])),
            Arc::new(Float64Array::from(vec![3.0])),
        ],
    )
    .unwrap();
    let rows = quotes_from_batch(&batch, "S").unwrap();
    assert_eq!(rows[0].local_ts, 0);
}

#[test]
fn exec_fills_from_batch_defaults_missing_mark_price() {
    // A batch built with the PRE-mark_price schema (no mark_price column at all — the exact
    // shape of an exec_fill part written before this field existed) must decode with
    // mark_price = None rather than erroring — the additive-column contract, mirroring
    // `quotes_from_batch_defaults_missing_local_ts` above for `local_ts`.
    let schema = Arc::new(Schema::new(vec![
        Field::new("ts", DataType::Int64, false),
        Field::new("trade_id", DataType::Utf8, false),
        Field::new("client_order_id", DataType::Utf8, false),
        Field::new("venue", DataType::Utf8, false),
        Field::new("symbol", DataType::Utf8, false),
        Field::new("side", DataType::Int64, false),
        Field::new("qty", DataType::Float64, false),
        Field::new("px", DataType::Float64, false),
        Field::new("commission", DataType::Float64, false),
    ]));
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from(vec![5i64])),
            Arc::new(StringArray::from(vec!["t1"])),
            Arc::new(StringArray::from(vec!["c1"])),
            Arc::new(StringArray::from(vec!["binance"])),
            Arc::new(StringArray::from(vec!["BTCUSDT"])),
            Arc::new(Int64Array::from(vec![1i64])),
            Arc::new(Float64Array::from(vec![0.5])),
            Arc::new(Float64Array::from(vec![65_000.0])),
            Arc::new(Float64Array::from(vec![0.13])),
        ],
    )
    .unwrap();
    let rows = exec_fills_from_batch(&batch).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].mark_price, None);
    // this same oldest-shape batch also predates liquidity_side/commission_asset — both must
    // default to "" rather than erroring.
    assert_eq!(rows[0].liquidity_side, "");
    assert_eq!(rows[0].commission_asset, "");
}

#[test]
fn exec_fills_from_batch_defaults_missing_liquidity_and_commission() {
    // A batch built with the schema as it stood right after mark_price shipped (#374) — i.e. it
    // HAS mark_price but predates liquidity_side/commission_asset — must decode those two with
    // "" rather than erroring. This is the realistic upgrade path: parts already on disk carry
    // mark_price but not the two fields added in this change.
    let schema = Arc::new(Schema::new(vec![
        Field::new("ts", DataType::Int64, false),
        Field::new("trade_id", DataType::Utf8, false),
        Field::new("client_order_id", DataType::Utf8, false),
        Field::new("venue", DataType::Utf8, false),
        Field::new("symbol", DataType::Utf8, false),
        Field::new("side", DataType::Int64, false),
        Field::new("qty", DataType::Float64, false),
        Field::new("px", DataType::Float64, false),
        Field::new("commission", DataType::Float64, false),
        Field::new("mark_price", DataType::Float64, true),
    ]));
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from(vec![5i64])),
            Arc::new(StringArray::from(vec!["t1"])),
            Arc::new(StringArray::from(vec!["c1"])),
            Arc::new(StringArray::from(vec!["binance"])),
            Arc::new(StringArray::from(vec!["BTCUSDT"])),
            Arc::new(Int64Array::from(vec![1i64])),
            Arc::new(Float64Array::from(vec![0.5])),
            Arc::new(Float64Array::from(vec![65_000.0])),
            Arc::new(Float64Array::from(vec![0.13])),
            Arc::new(Float64Array::from(vec![65_001.0])),
        ],
    )
    .unwrap();
    let rows = exec_fills_from_batch(&batch).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].mark_price, Some(65_001.0)); // still decodes fine
    assert_eq!(rows[0].liquidity_side, "");
    assert_eq!(rows[0].commission_asset, "");
}

#[test]
fn filter_codec_round_trips_all_fields() {
    use vike_model::SymbolProperties;
    let rows = vec![
        (
            1_000i64,
            SymbolProperties {
                tick_size: 0.01,
                step_size: 0.001,
                min_qty: 0.001,
                max_qty: 9000.0,
                min_notional: 5.0,
                // a deribit-shaped contract size: the additive column must survive round-trip
                contract_size: 10.0,
                tick_scheme: None,
                taker_hold_ms: 0,
                asset_class: None,
            },
        ),
        (
            2_000i64,
            SymbolProperties {
                tick_size: 0.5,
                step_size: 1.0,
                min_qty: 1.0,
                max_qty: 0.0,
                min_notional: 0.0,
                // …and the absent case round-trips as absent (written NULL, decoded 0.0)
                contract_size: 0.0,
                tick_scheme: None,
                taker_hold_ms: 0,
                asset_class: None,
            },
        ),
    ];
    let idxs: Vec<usize> = (0..rows.len()).collect();
    let cols = properties_columns(&rows, &idxs);
    let batch = RecordBatch::try_new(properties_schema(), cols).unwrap();
    let got = properties_from_batch(&batch).unwrap();
    assert_eq!(got, rows);
    assert_eq!(
        properties_schema().metadata().get("vike.schema.properties").map(String::as_str), // (was vike.schema.properties)
        Some("1")
    );
}

/// THE schema-compat pin for the PIT properties series: a part written BEFORE `contract_size`
/// existed has no such column at all. It must still decode (to the absent `0.0`), exactly like
/// exec_fill's `mark_price` — this is why the column is `f64_add` (nullable, appended LAST) and
/// why the schema version stays "1". A non-additive column here would break every recorded
/// properties part on disk.
#[test]
fn properties_codec_decodes_parts_written_before_contract_size() {
    use vike_model::SymbolProperties;
    // rebuild the OLD schema/batch: every column except the appended `contract_size`
    let old_schema = Arc::new(Schema::new(vec![
        Field::new("ts", DataType::Int64, false),
        Field::new("tick_size", DataType::Float64, false),
        Field::new("step_size", DataType::Float64, false),
        Field::new("min_qty", DataType::Float64, false),
        Field::new("max_qty", DataType::Float64, false),
        Field::new("min_notional", DataType::Float64, false),
    ]));
    let batch = RecordBatch::try_new(
        old_schema,
        vec![
            Arc::new(Int64Array::from(vec![1_000i64])),
            Arc::new(Float64Array::from(vec![0.01])),
            Arc::new(Float64Array::from(vec![0.001])),
            Arc::new(Float64Array::from(vec![0.001])),
            Arc::new(Float64Array::from(vec![9000.0])),
            Arc::new(Float64Array::from(vec![5.0])),
        ],
    )
    .unwrap();
    let got = properties_from_batch(&batch).expect("a pre-contract_size part must still decode");
    assert_eq!(
        got,
        vec![(
            1_000i64,
            SymbolProperties {
                tick_size: 0.01,
                step_size: 0.001,
                min_qty: 0.001,
                max_qty: 9000.0,
                min_notional: 5.0,
                contract_size: 0.0,
                tick_scheme: None,
                taker_hold_ms: 0,
                asset_class: None,
            },
        )]
    );
    assert_eq!(got[0].1.multiplier(), 1.0, "an old part is inert, never a 0.0 multiplier");
}

/// The `taker_hold_ms` twin of the pin above, and the reason the column exists at all: a
/// `SymbolProperties` field with NO column is silently dropped on a store round-trip. Both
/// live Polymarket values survive — `itode` → 250 (crypto up/down) and `seconds_delay` → 3000
/// (sports game markets) — and absent stays absent.
#[test]
fn properties_codec_round_trips_the_venue_taker_hold() {
    use vike_model::SymbolProperties;
    let rows: Vec<(i64, SymbolProperties)> = [0u32, 250, 3000]
        .iter()
        .enumerate()
        .map(|(i, &hold)| {
            (
                1_000i64 + i as i64,
                SymbolProperties { tick_size: 0.01, taker_hold_ms: hold, ..Default::default() },
            )
        })
        .collect();
    let idxs: Vec<usize> = (0..rows.len()).collect();
    let batch =
        RecordBatch::try_new(properties_schema(), properties_columns(&rows, &idxs)).unwrap();
    // the ABSENT hold is written as a SQL NULL, not a 0 — "the venue never told us" stays
    // distinguishable in the tape from an explicit no-hold.
    let col = batch.column_by_name("taker_hold_ms").expect("the column must exist");
    assert!(col.is_null(0), "hold 0 is written NULL");
    assert!(!col.is_null(1) && !col.is_null(2));
    assert_eq!(properties_from_batch(&batch).unwrap(), rows);
}

/// A part written BEFORE `taker_hold_ms` existed — i.e. EVERY properties part on every store
/// today — has no such column. It must still decode, to the absent `0` (= "this venue declares
/// no hold"), which is why the column is additive+nullable and appended LAST and why the
/// schema version stays "1".
#[test]
fn properties_codec_decodes_parts_written_before_taker_hold_ms() {
    use vike_model::SymbolProperties;
    // the PREVIOUS schema: everything through the `contract_size` column, nothing after it
    let old_schema = Arc::new(Schema::new(vec![
        Field::new("ts", DataType::Int64, false),
        Field::new("tick_size", DataType::Float64, false),
        Field::new("step_size", DataType::Float64, false),
        Field::new("min_qty", DataType::Float64, false),
        Field::new("max_qty", DataType::Float64, false),
        Field::new("min_notional", DataType::Float64, false),
        Field::new("contract_size", DataType::Float64, true),
    ]));
    let batch = RecordBatch::try_new(
        old_schema,
        vec![
            Arc::new(Int64Array::from(vec![1_000i64])),
            Arc::new(Float64Array::from(vec![0.01])),
            Arc::new(Float64Array::from(vec![0.001])),
            Arc::new(Float64Array::from(vec![0.001])),
            Arc::new(Float64Array::from(vec![9000.0])),
            Arc::new(Float64Array::from(vec![5.0])),
            Arc::new(Float64Array::from(vec![10.0])),
        ],
    )
    .unwrap();
    let got = properties_from_batch(&batch).expect("a pre-taker_hold_ms part must still decode");
    assert_eq!(
        got,
        vec![(
            1_000i64,
            SymbolProperties {
                tick_size: 0.01,
                step_size: 0.001,
                min_qty: 0.001,
                max_qty: 9000.0,
                min_notional: 5.0,
                contract_size: 10.0,
                tick_scheme: None,
                taker_hold_ms: 0,
                asset_class: None,
            },
        )]
    );
}

/// The `tick_scheme` twin of `properties_codec_round_trips_the_venue_taker_hold`, and the whole
/// point of this change: a `SymbolProperties` field with NO codec column is silently dropped on a
/// store round-trip. A scheme-BEARING row (a real Deribit option grid) survives encode→decode
/// with its scheme intact and still resolves by price; a scheme-LESS row stays `None` — written
/// as a SQL NULL, distinguishable in the tape from a present value.
#[test]
fn properties_codec_round_trips_the_tick_scheme() {
    use vike_model::{SymbolProperties, TickScheme, TickTier};
    // the real Deribit BTC-option grid: base 0.0001, 0.0005 above 0.005.
    let scheme = TickScheme::new(0.0001, &[TickTier { above_price: 0.005, tick_size: 0.0005 }])
        .expect("valid deribit grid");
    let rows: Vec<(i64, SymbolProperties)> = vec![
        // scheme-LESS — the state every venue but a Deribit option is in
        (1_000, SymbolProperties { tick_size: 0.01, ..Default::default() }),
        // scheme-BEARING — base_tick equals tick_size (the scalar non-price consumers still read)
        (
            2_000,
            SymbolProperties { tick_size: 0.0001, tick_scheme: Some(scheme), ..Default::default() },
        ),
    ];
    let idxs: Vec<usize> = (0..rows.len()).collect();
    let batch =
        RecordBatch::try_new(properties_schema(), properties_columns(&rows, &idxs)).unwrap();
    // the ABSENT scheme is written as a SQL NULL; the present one as a JSON string.
    let col = batch.column_by_name("tick_scheme").expect("the column must exist");
    assert!(col.is_null(0), "a None scheme is written NULL");
    assert!(!col.is_null(1), "a Some scheme is written as a JSON string");
    // and the whole grid rides back through intact.
    let got = properties_from_batch(&batch).unwrap();
    assert_eq!(got, rows);
    assert_eq!(got[1].1.tick_scheme, Some(scheme), "the scheme survives the round-trip");
    // the recovered scheme still resolves by price (not flattened to a scalar tick).
    assert_eq!(got[1].1.effective_tick(0.05), 0.0005);
    assert_eq!(got[1].1.effective_tick(0.004), 0.0001);
}

/// A part written BEFORE `tick_scheme` existed — i.e. EVERY properties part on every store today
/// — has no such column. It must still decode, to the absent `None` (a flat grid), which is why
/// the column is additive+nullable, appended LAST, and the schema version stays "1". Mirrors the
/// contract_size / taker_hold_ms pins above.
#[test]
fn properties_codec_decodes_parts_written_before_tick_scheme() {
    use vike_model::SymbolProperties;
    // the PREVIOUS schema: everything through `taker_hold_ms`, nothing after it.
    let old_schema = Arc::new(Schema::new(vec![
        Field::new("ts", DataType::Int64, false),
        Field::new("tick_size", DataType::Float64, false),
        Field::new("step_size", DataType::Float64, false),
        Field::new("min_qty", DataType::Float64, false),
        Field::new("max_qty", DataType::Float64, false),
        Field::new("min_notional", DataType::Float64, false),
        Field::new("contract_size", DataType::Float64, true),
        Field::new("taker_hold_ms", DataType::Int64, true),
    ]));
    let batch = RecordBatch::try_new(
        old_schema,
        vec![
            Arc::new(Int64Array::from(vec![1_000i64])),
            Arc::new(Float64Array::from(vec![0.01])),
            Arc::new(Float64Array::from(vec![0.001])),
            Arc::new(Float64Array::from(vec![0.001])),
            Arc::new(Float64Array::from(vec![9000.0])),
            Arc::new(Float64Array::from(vec![5.0])),
            Arc::new(Float64Array::from(vec![10.0])),
            Arc::new(Int64Array::from(vec![250i64])),
        ],
    )
    .unwrap();
    let got = properties_from_batch(&batch).expect("a pre-tick_scheme part must still decode");
    assert_eq!(
        got,
        vec![(
            1_000i64,
            SymbolProperties {
                tick_size: 0.01,
                step_size: 0.001,
                min_qty: 0.001,
                max_qty: 9000.0,
                min_notional: 5.0,
                contract_size: 10.0,
                tick_scheme: None,
                taker_hold_ms: 250,
                asset_class: None,
            },
        )]
    );
    assert!(got[0].1.tick_scheme.is_none(), "an old part decodes to a flat grid");
}

/// The `asset_class` twin of the round-trip pins above (0061 STEP 2), and the reason the column
/// exists at all: a `SymbolProperties` field with NO codec column is silently dropped on a store
/// round-trip, so a tape recording spot-vs-perp would have come back unclassified. EVERY variant
/// rides through — iterating `AssetClass::ALL` rather than naming a few, so a new variant joins
/// this test by existing — and the class-LESS row stays a SQL NULL, distinguishable in the tape
/// from a class somebody chose.
#[test]
fn properties_codec_round_trips_every_asset_class() {
    use vike_model::{AssetClass, SymbolProperties};
    let mut rows: Vec<(i64, SymbolProperties)> =
        vec![(1_000, SymbolProperties { tick_size: 0.01, ..Default::default() })];
    for (i, class) in AssetClass::ALL.iter().enumerate() {
        rows.push((
            2_000 + i as i64,
            SymbolProperties { tick_size: 0.01, asset_class: Some(*class), ..Default::default() },
        ));
    }
    let idxs: Vec<usize> = (0..rows.len()).collect();
    let batch =
        RecordBatch::try_new(properties_schema(), properties_columns(&rows, &idxs)).unwrap();
    let col = batch.column_by_name("asset_class").expect("the column must exist");
    assert!(col.is_null(0), "an unclassified row is written NULL, never a sentinel word");
    for i in 1..rows.len() {
        assert!(!col.is_null(i), "a chosen class is written as its stored word");
    }
    assert_eq!(properties_from_batch(&batch).unwrap(), rows);
}

/// A word the closed vocabulary does not know is an ERROR, not a silent `None`. This codec's own
/// writer can only emit `AssetClass::sql_word`, so such a cell means the part came from another
/// tree — or from one whose vocabulary has since been renamed, which is the case an operator
/// needs told rather than folded into "unclassified".
#[test]
fn properties_codec_refuses_an_unknown_asset_class_word() {
    let batch = RecordBatch::try_new(
        properties_schema(),
        vec![
            Arc::new(Int64Array::from(vec![1_000i64])),
            Arc::new(Float64Array::from(vec![0.01])),
            Arc::new(Float64Array::from(vec![0.001])),
            Arc::new(Float64Array::from(vec![0.001])),
            Arc::new(Float64Array::from(vec![9000.0])),
            Arc::new(Float64Array::from(vec![5.0])),
            Arc::new(Float64Array::from(vec![Option::<f64>::None])),
            Arc::new(Int64Array::from(vec![Option::<i64>::None])),
            Arc::new(StringArray::from(vec![Option::<&str>::None])),
            Arc::new(StringArray::from(vec![Some("Perpetual")])),
        ],
    )
    .unwrap();
    let err = properties_from_batch(&batch).expect_err("an unknown word must not decode");
    assert!(format!("{err}").contains("Perpetual"), "the refusal names the offending word: {err}");
}

/// A part written BEFORE `asset_class` existed — i.e. EVERY properties part on every store today
/// — has no such column. It must still decode, to `None` ("nobody recorded a class"), which is
/// why the column is additive+nullable, appended LAST, and the schema version stays "1". Mirrors
/// the contract_size / taker_hold_ms / tick_scheme pins above.
#[test]
fn properties_codec_decodes_parts_written_before_asset_class() {
    use vike_model::SymbolProperties;
    // the PREVIOUS schema: everything through `tick_scheme`, nothing after it.
    let old_schema = Arc::new(Schema::new(vec![
        Field::new("ts", DataType::Int64, false),
        Field::new("tick_size", DataType::Float64, false),
        Field::new("step_size", DataType::Float64, false),
        Field::new("min_qty", DataType::Float64, false),
        Field::new("max_qty", DataType::Float64, false),
        Field::new("min_notional", DataType::Float64, false),
        Field::new("contract_size", DataType::Float64, true),
        Field::new("taker_hold_ms", DataType::Int64, true),
        Field::new("tick_scheme", DataType::Utf8, true),
    ]));
    let batch = RecordBatch::try_new(
        old_schema,
        vec![
            Arc::new(Int64Array::from(vec![1_000i64])),
            Arc::new(Float64Array::from(vec![0.01])),
            Arc::new(Float64Array::from(vec![0.001])),
            Arc::new(Float64Array::from(vec![0.001])),
            Arc::new(Float64Array::from(vec![9000.0])),
            Arc::new(Float64Array::from(vec![5.0])),
            Arc::new(Float64Array::from(vec![10.0])),
            Arc::new(Int64Array::from(vec![250i64])),
            Arc::new(StringArray::from(vec![Option::<&str>::None])),
        ],
    )
    .unwrap();
    let got = properties_from_batch(&batch).expect("a pre-asset_class part must still decode");
    assert_eq!(
        got,
        vec![(
            1_000i64,
            SymbolProperties {
                tick_size: 0.01,
                step_size: 0.001,
                min_qty: 0.001,
                max_qty: 9000.0,
                min_notional: 5.0,
                contract_size: 10.0,
                tick_scheme: None,
                taker_hold_ms: 250,
                asset_class: None,
            },
        )]
    );
}

#[test]
fn chain_codec_round_trips_all_fields() {
    // A fully-populated call and a sparse put: every stored column (incl. the string identity
    // pair and each nullable quote/greek) survives encode→decode; absent stays absent.
    let full = ChainRow {
        ts: 1_780_387_200_000,
        underlying: "BTC".into(),
        instrument: "BTC-27JUN26-100000-C".into(),
        expiry_ms: 1_782_547_200_000,
        strike: 100_000.0,
        is_call: true,
        bid: Some(5_200.0),
        ask: Some(6_240.0),
        mark: Some(5_720.0),
        iv: Some(0.625),
        open_interest: Some(120.0),
        volume: Some(8.0),
        delta: Some(0.55),
        gamma: Some(0.000_01),
        theta: Some(-45.2),
        vega: Some(210.0),
    };
    let sparse = ChainRow {
        instrument: "BTC-27JUN26-100000-P".into(),
        is_call: false,
        bid: None,
        ask: None,
        mark: None,
        iv: None,
        open_interest: None,
        volume: None,
        delta: None,
        gamma: None,
        theta: None,
        vega: None,
        ..full.clone()
    };
    let rows = vec![full, sparse];
    let idxs: Vec<usize> = (0..rows.len()).collect();
    let batch = RecordBatch::try_new(chain_schema(), chain_columns(&rows, &idxs)).unwrap();
    assert_eq!(chain_from_batch(&batch).unwrap(), rows);
    assert_eq!(chain_schema().metadata().get("vike.schema.chain").map(String::as_str), Some("1"));
}

#[test]
fn funding_codec_round_trips_all_fields() {
    // A PAID row (negative usdc, long szi) and a RECEIVED row (positive usdc, short szi, negative
    // rate) — every column (incl. the signed f64s and the string hash) survives encode→decode.
    let rows = vec![
        FundingRow {
            ts: 1_681_222_254_710,
            account: "0xMAIN".into(),
            usdc: -1.25,
            szi: 0.5,
            funding_rate: 0.000_012_5,
            hash: "0xabc".into(),
        },
        FundingRow {
            ts: 1_681_222_254_720,
            account: "0xTEST".into(),
            usdc: 0.75,
            szi: -2.0,
            funding_rate: -0.000_008_8,
            hash: "0xdef".into(),
        },
    ];
    let idxs: Vec<usize> = (0..rows.len()).collect();
    let batch = RecordBatch::try_new(funding_schema(), funding_columns(&rows, &idxs)).unwrap();
    assert_eq!(funding_from_batch(&batch).unwrap(), rows);
}

/// The kinds with a GROUPED write path (quote, trade, and book — which `depth` shares) declare the
/// row symbol a grouped merge keys on, and it reads the row's own `symbol`. Every other kind
/// declares none, so a grouped merge of one is REFUSED (`grouped_symbol_of`) rather than keyed on
/// `sort_key` alone — the shape that dropped other symbols' rows at a shared ts.
#[test]
fn exactly_the_grouped_kinds_declare_a_row_symbol() {
    let quote = QuoteTick {
        ts: 1,
        local_ts: 2,
        bid: 0.4,
        ask: 0.5,
        bid_size: 1.0,
        ask_size: 2.0,
        symbol: "Q".into(),
    };
    let trade = TradeTick {
        ts: 1,
        local_ts: 2,
        price: 0.4,
        size: 1.0,
        is_buyer_maker: false,
        symbol: "T".into(),
    };
    let level = BookRow {
        ts: 1,
        local_ts: 2,
        seq: 3,
        kind: 0,
        is_bid: true,
        price: 0.4,
        size: 1.0,
        tick_size: 0.01,
        symbol: "B".into(),
    };
    assert_eq!(QuoteCodec::ROW_SYMBOL.map(|f| f(&quote)), Some("Q"));
    assert_eq!(TradeCodec::ROW_SYMBOL.map(|f| f(&trade)), Some("T"));
    assert_eq!(BookCodec::ROW_SYMBOL.map(|f| f(&level)), Some("B"));

    assert!(BarCodec::ROW_SYMBOL.is_none());
    assert!(PropertiesCodec::ROW_SYMBOL.is_none());
    assert!(EquityCodec::ROW_SYMBOL.is_none());
    assert!(ExecFillCodec::ROW_SYMBOL.is_none());
    assert!(ExecOrderCodec::ROW_SYMBOL.is_none());
    assert!(FundingCodec::ROW_SYMBOL.is_none());
    assert!(ChainCodec::ROW_SYMBOL.is_none());
    assert!(CohortCodec::ROW_SYMBOL.is_none());
    assert!(PerpMetricsCodec::ROW_SYMBOL.is_none());
}
