use super::*;

#[test]
fn quotes_from_batch_defaults_missing_local_timestamp() {
    // A batch built with the PRE-plan schema (no local_timestamp column) must decode with
    // local_timestamp = 0 — the additive-column contract for already-recorded data.
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
    assert_eq!(rows[0].local_timestamp, 0);
}

/// The two `exec_fill` old-part pins below, which differ only in WHERE the column list is cut.
///
/// The lists hold the `exec_fill` columns in the order they were APPENDED, each with the one cell
/// the pins write. A part written before column `before` holds exactly the columns ahead of it, so
/// the lists are cut there — which is why they run through `liquidity_side`, the newest column a
/// pin cuts at, whose own cell is never written. The decoded part must keep the additive-column
/// contract: one row, the pin's expected `mark_price`, and `liquidity_side`/`commission_asset` —
/// which every part cut here predates — defaulted to `""` rather than erroring.
fn assert_old_execution_fill_part_decodes(before: &str, mark_price: Option<f64>) {
    let mut fields = vec![
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
        Field::new("liquidity_side", DataType::Utf8, true),
    ];
    let mut cells: Vec<ArrayRef> = vec![
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
        Arc::new(StringArray::from(vec!["maker"])),
    ];
    let kept =
        fields.iter().position(|f| f.name() == before).expect("an appended exec_fill column");
    fields.truncate(kept);
    cells.truncate(kept);
    let batch = RecordBatch::try_new(Arc::new(Schema::new(fields)), cells).unwrap();
    let rows = exec_fills_from_batch(&batch).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].mark_price, mark_price);
    assert_eq!(rows[0].liquidity_side, "");
    assert_eq!(rows[0].commission_asset, "");
}

#[test]
fn execution_fills_from_batch_defaults_missing_mark_price() {
    // A batch built with the PRE-mark_price schema (no mark_price column at all — the exact
    // shape of an exec_fill part written before this field existed) must decode with
    // mark_price = None rather than erroring — the additive-column contract, mirroring
    // `quotes_from_batch_defaults_missing_local_timestamp` above for `local_timestamp`.
    // this same oldest-shape batch also predates liquidity_side/commission_asset — both must
    // default to "" rather than erroring.
    assert_old_execution_fill_part_decodes("mark_price", None);
}

#[test]
fn execution_fills_from_batch_defaults_missing_liquidity_and_commission() {
    // A batch built with the schema as it stood right after mark_price shipped (#374) — i.e. it
    // HAS mark_price but predates liquidity_side/commission_asset — must decode those two with
    // "" rather than erroring. This is the realistic upgrade path: parts already on disk carry
    // mark_price but not the two fields added in this change.
    // mark_price = Some(65_001.0): it still decodes fine
    assert_old_execution_fill_part_decodes("liquidity_side", Some(65_001.0));
}

/// One `exec_fill` row carrying BOTH additive id columns, for the round-trip and old-part pins.
fn fill_with_ids() -> vike_data::ExecFillRow {
    vike_data::ExecFillRow {
        ts: 5,
        trade_id: "t1".into(),
        order_client_id: "c1".into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 0.5,
        px: 65_000.0,
        commission: 0.13,
        mark_price: Some(65_001.0),
        liquidity_side: "maker".into(),
        commission_asset: "BNB".into(),
        venue_order_id: Some("100234".into()),
        account: Some("ALT".into()),
        position_side: Some("SHORT".into()),
        corrects_trade_id: Some("t0".into()),
        correction_seq: Some(7),
        mount_id: Some("maker-a".into()),
        received_ms: Some(9),
        ownership: Some(vike_data::exec_log::OWNERSHIP_OURS.into()),
        journal_sequence: Some(41),
        journal_generation: Some(2),
    }
}

/// THE schema-compat pin for `received_ms` on `kind=exec_fill`: a part written with every column before it
/// (every part on disk before the column) decodes it as `None` and keeps the rest, and a part that has it
/// round-trips it.
#[test]
fn execution_fills_from_a_part_without_the_received_column_read_none() {
    let fills = vec![fill_with_ids()];
    let full = exec_fill_schema();
    let kept = full.fields().iter().position(|f| f.name() == "received_ms").unwrap();
    let mut cells = exec_fill_columns(&fills, &[0]);
    cells.truncate(kept);
    let old_schema = Arc::new(Schema::new(full.fields()[..kept].to_vec()));
    let got = exec_fills_from_batch(&RecordBatch::try_new(old_schema, cells).unwrap()).unwrap();
    // `ownership` and the journal columns came after it, so a part without `received_ms` has none of them.
    let expected = vike_data::ExecFillRow {
        received_ms: None,
        ownership: None,
        journal_sequence: None,
        journal_generation: None,
        ..fill_with_ids()
    };
    assert_eq!(got, vec![expected]);
    let batch = RecordBatch::try_new(full, exec_fill_columns(&fills, &[0])).unwrap();
    assert_eq!(exec_fills_from_batch(&batch).unwrap(), fills, "a part that has it round-trips");
}

/// THE schema-compat pin for `mount_id` on `kind=exec_fill` and `kind=execution_order`: a part written with
/// every column before it (every part on disk before the column) decodes it as `None` and keeps the rest.
#[test]
fn execution_rows_from_a_part_without_the_mount_column_read_none() {
    let fills = vec![fill_with_ids()];
    let full = exec_fill_schema();
    let kept = full.fields().iter().position(|f| f.name() == "mount_id").unwrap();
    let mut cells = exec_fill_columns(&fills, &[0]);
    cells.truncate(kept);
    let old_schema = Arc::new(Schema::new(full.fields()[..kept].to_vec()));
    let got = exec_fills_from_batch(&RecordBatch::try_new(old_schema, cells).unwrap()).unwrap();
    // `received_ms`, `ownership` and the journal columns came after it, so a part without `mount_id` has none.
    let expected = vike_data::ExecFillRow {
        mount_id: None,
        received_ms: None,
        ownership: None,
        journal_sequence: None,
        journal_generation: None,
        ..fill_with_ids()
    };
    assert_eq!(got, vec![expected]);

    let orders = vec![vike_data::ExecOrderRow {
        ts: 7,
        order_client_id: "c1".into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 0.5,
        order_type: "LIMIT".into(),
        status: "FILLED".into(),
        price: Some(65_000.0),
        trigger_price: None,
        venue_order_id: Some("v1".into()),
        filled_qty: 0.5,
        avg_fill_px: 65_000.0,
        account: Some("ALT".into()),
        mount_id: Some("maker-a".into()),
        ownership: Some(vike_data::exec_log::OWNERSHIP_OURS.into()),
        journal_sequence: Some(41),
        journal_generation: None,
        reason: None,
    }];
    let full = exec_order_schema();
    let kept = full.fields().iter().position(|f| f.name() == "mount_id").unwrap();
    let mut cells = exec_order_columns(&orders, &[0]);
    cells.truncate(kept);
    let old_schema = Arc::new(Schema::new(full.fields()[..kept].to_vec()));
    let got = exec_orders_from_batch(&RecordBatch::try_new(old_schema, cells).unwrap()).unwrap();
    assert_eq!(got[0].mount_id, None, "a part older than the column reads None");
    assert_eq!(got[0].account.as_deref(), Some("ALT"), "the older additive column still decodes");
    let batch =
        RecordBatch::try_new(exec_order_schema(), exec_order_columns(&orders, &[0])).unwrap();
    assert_eq!(exec_orders_from_batch(&batch).unwrap(), orders, "a part that has it round-trips");
}

/// THE schema-compat pin for the execution stream's two columns: `ownership` on `kind=exec_fill` and
/// `kind=execution_order`, and `journal_sequence` on `kind=execution_order`, each appended LAST. A part written
/// with every column before them (every part the retired materializer or an import wrote) decodes them as `None`
/// and keeps the rest; a part that has them round-trips them.
#[test]
fn execution_rows_from_a_part_without_the_stream_columns_read_none() {
    let fills = vec![fill_with_ids()];
    let full = exec_fill_schema();
    let kept = full.fields().iter().position(|f| f.name() == "ownership").unwrap();
    assert_eq!(kept + 3, full.fields().len(), "appended LAST, the two journal columns after it");
    let mut cells = exec_fill_columns(&fills, &[0]);
    cells.truncate(kept);
    let old_schema = Arc::new(Schema::new(full.fields()[..kept].to_vec()));
    let got = exec_fills_from_batch(&RecordBatch::try_new(old_schema, cells).unwrap()).unwrap();
    let none = vike_data::ExecFillRow {
        ownership: None,
        journal_sequence: None,
        journal_generation: None,
        ..fill_with_ids()
    };
    assert_eq!(got, vec![none]);
    // A part the stream wrote before the journal columns (none shipped, but the contract is the same) keeps its
    // ownership and reads the two `None`.
    let at = full.fields().iter().position(|f| f.name() == "journal_sequence").unwrap();
    let mut cells = exec_fill_columns(&fills, &[0]);
    cells.truncate(at);
    let old_schema = Arc::new(Schema::new(full.fields()[..at].to_vec()));
    let got = exec_fills_from_batch(&RecordBatch::try_new(old_schema, cells).unwrap()).unwrap();
    let unsequenced = vike_data::ExecFillRow {
        journal_sequence: None,
        journal_generation: None,
        ..fill_with_ids()
    };
    assert_eq!(got, vec![unsequenced]);
    let batch = RecordBatch::try_new(full, exec_fill_columns(&fills, &[0])).unwrap();
    assert_eq!(exec_fills_from_batch(&batch).unwrap(), fills, "a part that has it round-trips");

    let orders = vec![vike_data::ExecOrderRow {
        ts: 7,
        order_client_id: "c1".into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 0.5,
        order_type: "LIMIT".into(),
        status: "FILLED".into(),
        price: Some(65_000.0),
        trigger_price: None,
        venue_order_id: Some("v1".into()),
        filled_qty: 0.5,
        avg_fill_px: 65_000.0,
        account: Some("ALT".into()),
        mount_id: Some("maker-a".into()),
        ownership: Some(vike_data::exec_log::OWNERSHIP_FOREIGN.into()),
        journal_sequence: Some(41),
        journal_generation: None,
        reason: None,
    }];
    let full = exec_order_schema();
    let kept = full.fields().iter().position(|f| f.name() == "ownership").unwrap();
    let names: Vec<&str> = full.fields()[kept..].iter().map(|f| f.name().as_str()).collect();
    assert_eq!(
        names,
        ["ownership", "journal_sequence", "journal_generation", "reason"],
        "appended LAST, in that order"
    );
    let mut cells = exec_order_columns(&orders, &[0]);
    cells.truncate(kept);
    let old_schema = Arc::new(Schema::new(full.fields()[..kept].to_vec()));
    let got = exec_orders_from_batch(&RecordBatch::try_new(old_schema, cells).unwrap()).unwrap();
    let expected = vike_data::ExecOrderRow {
        ownership: None,
        journal_sequence: None,
        journal_generation: None,
        reason: None,
        ..orders[0].clone()
    };
    assert_eq!(got, vec![expected], "a part older than the columns reads None and keeps the rest");
    let batch =
        RecordBatch::try_new(exec_order_schema(), exec_order_columns(&orders, &[0])).unwrap();
    assert_eq!(exec_orders_from_batch(&batch).unwrap(), orders, "a part that has them round-trips");
}

/// THE schema-compat pin for the hedge leg and the fee correction columns on `kind=exec_fill`: a part
/// written with every column through `account`, the shape of every exec_fill part on disk before them,
/// decodes all three as `None`, which [`vike_data::ExecFillRow`] reads as the one-way fill it
/// was (`docs/decisions/0117-there-are-no-migrations.md`: nothing rewrites the old parts).
#[test]
fn execution_fills_from_batch_reads_a_part_without_the_leg_and_correction_columns_as_none() {
    let rows = vec![fill_with_ids()];
    let full = exec_fill_schema();
    let kept = full.fields().iter().position(|f| f.name() == "position_side").unwrap();
    let old_schema = Arc::new(Schema::new(full.fields()[..kept].to_vec()));
    let mut cells = exec_fill_columns(&rows, &[0]);
    cells.truncate(kept);
    let batch = RecordBatch::try_new(old_schema, cells).unwrap();
    let got = exec_fills_from_batch(&batch).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].position_side, None);
    assert_eq!(got[0].corrects_trade_id, None);
    assert_eq!(got[0].correction_seq, None);
    assert_eq!(got[0].account.as_deref(), Some("ALT"), "the older additive columns still decode");
}

/// The other direction: a decoder reads its columns by name and ignores the rest, so a build older than
/// a column reads a part that has it, as this one reads a part with a column added after it.
#[test]
fn execution_fills_from_batch_ignores_a_column_it_does_not_know() {
    let rows = vec![fill_with_ids()];
    let mut fields: Vec<_> = exec_fill_schema().fields().iter().cloned().collect();
    fields.push(Arc::new(Field::new("a_later_column", DataType::Utf8, true)));
    let mut cells = exec_fill_columns(&rows, &[0]);
    cells.push(Arc::new(StringArray::from(vec!["later"])));
    let batch = RecordBatch::try_new(Arc::new(Schema::new(fields)), cells).unwrap();
    assert_eq!(exec_fills_from_batch(&batch).unwrap(), rows);
}

/// THE schema-compat pin for `venue_order_id` and `account` on `kind=exec_fill`: a part written
/// with every column through `commission_asset` — the shape of EVERY exec_fill part on disk before
/// the two columns existed — decodes both as `None` rather than erroring, and keeps every older
/// column it does carry (`docs/decisions/0117-there-are-no-migrations.md`: a missing column reads
/// as absent; nothing rewrites the old parts).
#[test]
fn execution_fills_from_batch_reads_a_part_without_the_id_columns_as_none() {
    let rows = vec![fill_with_ids()];
    let full = exec_fill_schema();
    let kept = full.fields().iter().position(|f| f.name() == "venue_order_id").unwrap();
    let old_schema = Arc::new(Schema::new(full.fields()[..kept].to_vec()));
    let mut cells = exec_fill_columns(&rows, &[0]);
    cells.truncate(kept);
    let batch = RecordBatch::try_new(old_schema, cells).unwrap();
    let got = exec_fills_from_batch(&batch).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].venue_order_id, None, "a part older than the column reads None");
    assert_eq!(got[0].account, None, "a part older than the column reads None");
    assert_eq!(got[0].liquidity_side, "maker", "the older additive columns still decode");
    assert_eq!(got[0].commission_asset, "BNB");
    assert_eq!(got[0].order_client_id, "c1");
}

/// …and a part that HAS the additive columns round-trips them, a NULL cell included.
#[test]
fn execution_fill_id_columns_round_trip_present_and_null() {
    let mut unknown = fill_with_ids();
    unknown.ts = 6;
    unknown.venue_order_id = None;
    unknown.account = None;
    unknown.position_side = None;
    unknown.corrects_trade_id = None;
    unknown.correction_seq = None;
    unknown.mount_id = None;
    unknown.received_ms = None;
    unknown.ownership = None;
    unknown.journal_sequence = None;
    unknown.journal_generation = None;
    let rows = vec![fill_with_ids(), unknown];
    let batch =
        RecordBatch::try_new(exec_fill_schema(), exec_fill_columns(&rows, &[0, 1])).unwrap();
    assert_eq!(exec_fills_from_batch(&batch).unwrap(), rows);
}

/// The `kind=execution_order` twin: a part written before `account` decodes it as `None`, and a part
/// that has it round-trips it.
#[test]
fn execution_orders_from_batch_reads_a_part_without_account_as_none() {
    let row = vike_data::ExecOrderRow {
        ts: 7,
        order_client_id: "c1".into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 0.5,
        order_type: "LIMIT".into(),
        status: "FILLED".into(),
        price: Some(65_000.0),
        trigger_price: None,
        venue_order_id: Some("v1".into()),
        filled_qty: 0.5,
        avg_fill_px: 65_000.0,
        account: Some("ALT".into()),
        mount_id: None,
        ownership: None,
        journal_sequence: None,
        journal_generation: None,
        reason: None,
    };
    let rows = vec![row];
    let full = exec_order_schema();
    let kept = full.fields().iter().position(|f| f.name() == "account").unwrap();
    let old_schema = Arc::new(Schema::new(full.fields()[..kept].to_vec()));
    let mut cells = exec_order_columns(&rows, &[0]);
    cells.truncate(kept);
    let old = exec_orders_from_batch(&RecordBatch::try_new(old_schema, cells).unwrap()).unwrap();
    assert_eq!(old[0].account, None, "a part older than the column reads None");
    assert_eq!(old[0].venue_order_id.as_deref(), Some("v1"));

    let batch = RecordBatch::try_new(exec_order_schema(), exec_order_columns(&rows, &[0])).unwrap();
    assert_eq!(exec_orders_from_batch(&batch).unwrap(), rows);
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

/// The four `kind=properties` old-part pins below, which differ only in WHERE the column list is
/// cut.
///
/// The lists hold the properties columns in the order they were APPENDED, each with the one cell
/// the pins write. A part written before column `before` holds exactly the columns ahead of it, so
/// the lists are cut there — which is why they run through `asset_class`, the newest column a pin
/// cuts at, whose own cell is never written. The part must still decode, to the one row written
/// with every column it lacks at its absent default; `contract_size` and `taker_hold_ms` are the
/// two cells whose expected value depends on the cut, so each pin passes them. Returns the decoded
/// rows for a pin's own extra check.
fn assert_old_properties_part_decodes(
    before: &str,
    contract_size: f64,
    taker_hold_ms: u32,
) -> Vec<(i64, vike_model::SymbolProperties)> {
    use vike_model::SymbolProperties;
    let mut fields = vec![
        Field::new("ts", DataType::Int64, false),
        Field::new("tick_size", DataType::Float64, false),
        Field::new("step_size", DataType::Float64, false),
        Field::new("min_qty", DataType::Float64, false),
        Field::new("max_qty", DataType::Float64, false),
        Field::new("min_notional", DataType::Float64, false),
        Field::new("contract_size", DataType::Float64, true),
        Field::new("taker_hold_ms", DataType::Int64, true),
        Field::new("tick_scheme", DataType::Utf8, true),
        Field::new("asset_class", DataType::Utf8, true),
    ];
    let mut cells: Vec<ArrayRef> = vec![
        Arc::new(Int64Array::from(vec![1_000i64])),
        Arc::new(Float64Array::from(vec![0.01])),
        Arc::new(Float64Array::from(vec![0.001])),
        Arc::new(Float64Array::from(vec![0.001])),
        Arc::new(Float64Array::from(vec![9000.0])),
        Arc::new(Float64Array::from(vec![5.0])),
        Arc::new(Float64Array::from(vec![10.0])),
        Arc::new(Int64Array::from(vec![250i64])),
        Arc::new(StringArray::from(vec![Option::<&str>::None])),
        Arc::new(StringArray::from(vec![Option::<&str>::None])),
    ];
    let kept =
        fields.iter().position(|f| f.name() == before).expect("an appended properties column");
    fields.truncate(kept);
    cells.truncate(kept);
    let batch = RecordBatch::try_new(Arc::new(Schema::new(fields)), cells).unwrap();
    let got = properties_from_batch(&batch)
        .unwrap_or_else(|e| panic!("a pre-{before} part must still decode: {e:?}"));
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
                contract_size,
                tick_scheme: None,
                taker_hold_ms,
                asset_class: None,
            },
        )]
    );
    got
}

/// THE schema-compat pin for the PIT properties series: a part written BEFORE `contract_size`
/// existed has no such column at all. It must still decode (to the absent `0.0`), exactly like
/// exec_fill's `mark_price` — this is why the column is `f64_add` (nullable, appended LAST) and
/// why the schema version stays "1". A non-additive column here would break every recorded
/// properties part on disk.
#[test]
fn properties_codec_decodes_parts_written_before_contract_size() {
    // rebuild the OLD schema/batch: every column except the appended `contract_size`
    let got = assert_old_properties_part_decodes("contract_size", 0.0, 0);
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
    // the PREVIOUS schema: everything through the `contract_size` column, nothing after it
    assert_old_properties_part_decodes("taker_hold_ms", 10.0, 0);
}

/// The `tick_scheme` twin of `properties_codec_round_trips_the_venue_taker_hold`, and the whole
/// point of this change: a `SymbolProperties` field with NO codec column is silently dropped on a
/// store round-trip. A scheme-BEARING row (a real Deribit option grid) survives encode→decode
/// with its scheme intact and still resolves by price; a scheme-LESS row stays `None` — written
/// as a SQL NULL, distinguishable in the tape from a present value.
#[test]
fn properties_codec_round_trips_the_tick_scheme() {
    use vike_model::{GridTickScheme, GridTickTier, SymbolProperties};
    // the real Deribit BTC-option grid: base 0.0001, 0.0005 above 0.005.
    let scheme =
        GridTickScheme::new(0.0001, &[GridTickTier { above_price: 0.005, tick_size: 0.0005 }])
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
    // the PREVIOUS schema: everything through `taker_hold_ms`, nothing after it.
    let got = assert_old_properties_part_decodes("tick_scheme", 10.0, 250);
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
    // the PREVIOUS schema: everything through `tick_scheme`, nothing after it.
    assert_old_properties_part_decodes("asset_class", 10.0, 250);
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
        kind: vike_marketdata::OptionKind::Call,
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
        kind: vike_marketdata::OptionKind::Put,
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
        ExecutionHistoryFundingRow {
            ts: 1_681_222_254_710,
            account: "0xMAIN".into(),
            usdc: -1.25,
            szi: 0.5,
            funding_rate: 0.000_012_5,
            hash: "0xabc".into(),
        },
        ExecutionHistoryFundingRow {
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
    let quote = TickQuote {
        timestamp: 1,
        local_timestamp: 2,
        bid: 0.4,
        ask: 0.5,
        bid_size: 1.0,
        ask_size: 2.0,
        symbol: "Q".into(),
    };
    let trade = TickTrade {
        timestamp: 1,
        local_timestamp: 2,
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

/// The additive-column readers (`opt_i64_col`, `opt_f64_col`, one body since 2026-11-01): an ABSENT
/// column is `Ok(None)`, a present one of the right type is the column, and a present one of the
/// wrong type is the error naming that type, word for word. A mutant that swaps the type words, or
/// reads an absent column as an error, goes red here.
#[test]
fn an_additive_column_is_absent_present_or_named_as_the_wrong_type() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("i", DataType::Int64, true),
        Field::new("f", DataType::Float64, true),
    ]));
    let batch = RecordBatch::try_new(
        schema,
        vec![Arc::new(Int64Array::from(vec![7i64])), Arc::new(Float64Array::from(vec![0.5]))],
    )
    .unwrap();
    assert!(opt_i64_col(&batch, "absent").unwrap().is_none());
    assert!(opt_f64_col(&batch, "absent").unwrap().is_none());
    assert_eq!(opt_i64_col(&batch, "i").unwrap().map(|c| c.value(0)), Some(7));
    assert_eq!(opt_f64_col(&batch, "f").unwrap().map(|c| c.value(0)), Some(0.5));
    let wrong_i = opt_i64_col(&batch, "f").err().map(|e| e.to_string()).unwrap_or_default();
    let wrong_f = opt_f64_col(&batch, "i").err().map(|e| e.to_string()).unwrap_or_default();
    assert!(wrong_i.contains("column f present but !i64"), "{wrong_i}");
    assert!(wrong_f.contains("column i present but !f64"), "{wrong_f}");
}
