//! **The `trade` plane's row renderer — one implementation per row type, each a human table and a
//! flat JSON form.** `crate::cmd::trade::order`'s `ls` was this module's first caller
//! ([`OrderRow`]/[`order_rows`]/[`rows_table`]/[`rows_json`]); `crate::cmd::trade::position`'s `ls`
//! ([`PositionRow`]) and `crate::cmd::trade::strategy`'s `ls` ([`MountRow`]) are Task 6's siblings,
//! each reusing the same SHAPE (a projector feeding a `*_table`/`*_json` pair) rather than growing a
//! per-verb renderer — the same "one implementation, not a family of near-identical ones" argument
//! `crate::cmd::trade::selector`'s own doc makes for the book grammar.
//!
//! # The JSON form is FLAT
//!
//! A top-level ARRAY, one object per row, each row carrying its OWN `venue` and (where the row type
//! has one) `account` — never a nested `{"venue": {"orders": [...]}}` shape. A consumer scripting
//! against this must not have to infer which book a row belongs to from its position in a tree.
//!
//! ⚠ **`account` is `None` on every [`OrderRow`] today, and that is a decision of this read rather
//! than a wire limitation.** [`WireOrderView`] has an optional `account` (absent for the default
//! account), but its absence cannot tell the default account from a node that predates the field —
//! only the venue blocks' `mode` can — so this read does not fill the column, and
//! `selector::refuse_an_unaddressable_book` refuses a labelled book for the same reason (not lifted:
//! a follow-up). The order's bare `venue` string cannot stand in for it: two accounts of one venue
//! publish IDENTICAL `venue` strings and different `route_key`s on their
//! [`vike_tradehub_client::wire::WireVenueBlock`]s. The `account` key is present on every row
//! regardless — `null` rather than omitted — because a machine reader must not have to treat "no
//! account" and "this reader forgot to ask" as the same silence.
//!
//! ⚠ **[`PositionRow`] is NOT the same story, and this is the one place the two row types genuinely
//! diverge.** A position lives inside a [`WireVenueBlock`], and that block DOES carry `account` —
//! so [`position_rows`] fills the column for real, `None` only while the wire itself says "the
//! unlabelled account". See [`PositionRow`]'s own doc for the evidence and
//! `crate::cmd::trade::position`'s module doc for why its `ls` narrows a labelled book instead of
//! refusing it the way `order ls` must.
//!
//! # An empty result is an empty array and a surviving header, never an error
//!
//! Every `*_table` function prints its header row even over zero rows; every `*_json` function
//! serializes zero rows as `[]`. Neither is a special case in the code — an empty row slice runs the
//! same rendering path as a populated one.
//!
//! # An aggregate is a rendering, never a row
//!
//! [`positions_table`] appends a Σ-marked total line when it has rows to sum; [`positions_json`]
//! never does, on the same "flat, self-describing rows" argument above — a total row would be a row
//! with no `symbol`, and a human table's total must never share its VENUE/ACCOUNT columns with a real
//! row's, or a summed number reads as one more book's balance. [`MountRow`] carries nothing that
//! sums to a figure with the same failure mode, so [`mounts_table`] has no aggregate at all.

use vike_tradehub_client::wire::{
    WireMountRow, WireOrderView, WirePositionView, WireSnapshot, WireStrategyStatus, WireVenueBlock,
};

use super::selector::Book;
use super::{opt_num, side_word};

/// One row of the rendered order set — the flat, JSON-friendly projection [`rows_table`] and
/// [`rows_json`] share.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OrderRow {
    pub coid: String,
    pub venue: String,
    /// See this module's doc: `None` on every row today because this read does not attribute an
    /// order to an account (`WireOrderView::account`'s absence is ambiguous), not because the wire
    /// has no such field.
    pub account: Option<String>,
    pub symbol: String,
    pub side: &'static str,
    pub qty: f64,
    pub order_type: String,
    pub price: Option<f64>,
    pub status: String,
    pub filled_qty: f64,
}

/// Project `snap`'s orders into rows, optionally narrowed to one `book`'s venue and/or one `symbol`
/// (case-insensitive, exact match — the same rule `crate::cmd::trade`'s own REPL tables use).
///
/// ⚠ **The book filter narrows by VENUE only.** [`Book`]'s account label has no column to compare
/// against here — see [`OrderRow::account`] — so `order ls binance/ALT` and `order ls binance`
/// would return the same rows until this read can tell their orders apart (a labelled book is
/// refused before it gets here, so an operator never sees the two agree).
pub(crate) fn order_rows(
    snap: &WireSnapshot,
    book: Option<&Book>,
    symbol: Option<&str>,
) -> Vec<OrderRow> {
    snap.orders
        .iter()
        .filter(|o| book.is_none_or(|b| o.venue.eq_ignore_ascii_case(&b.venue)))
        .filter(|o| symbol.is_none_or(|s| o.symbol.eq_ignore_ascii_case(s)))
        .map(order_row)
        .collect()
}

fn order_row(o: &WireOrderView) -> OrderRow {
    OrderRow {
        coid: o.client_order_id.clone(),
        venue: o.venue.clone(),
        account: None,
        symbol: o.symbol.clone(),
        side: side_word(o.side),
        qty: o.qty,
        order_type: o.order_type.clone(),
        price: o.price,
        status: o.status.clone(),
        filled_qty: o.filled_qty,
    }
}

/// The human table form. The header row survives an empty `rows` — an operator asking "what is
/// working" must see the COLUMNS even when the answer is "nothing", never a blank screen.
pub(crate) fn rows_table(rows: &[OrderRow]) -> String {
    let w_coid = col_width("COID", rows.iter().map(|r| r.coid.as_str()));
    let w_venue = col_width("VENUE", rows.iter().map(|r| r.venue.as_str()));
    let w_account = col_width("ACCOUNT", rows.iter().map(|r| r.account.as_deref().unwrap_or("-")));
    let w_symbol = col_width("SYMBOL", rows.iter().map(|r| r.symbol.as_str()));
    let w_side = col_width("SIDE", rows.iter().map(|r| r.side));
    let w_type = col_width("TYPE", rows.iter().map(|r| r.order_type.as_str()));
    let w_status = col_width("STATUS", rows.iter().map(|r| r.status.as_str()));

    let mut out = format!(
        "{:<w_coid$} {:<w_venue$} {:<w_account$} {:<w_symbol$} {:<w_side$} {:>10} {:<w_type$} \
         {:>10} {:<w_status$} {:>10}",
        "COID", "VENUE", "ACCOUNT", "SYMBOL", "SIDE", "QTY", "TYPE", "PRICE", "STATUS", "FILLED"
    );
    if rows.is_empty() {
        out.push_str("\n  (none)");
        return out;
    }
    for r in rows {
        out.push('\n');
        out.push_str(&format!(
            "{:<w_coid$} {:<w_venue$} {:<w_account$} {:<w_symbol$} {:<w_side$} {:>10} \
             {:<w_type$} {:>10} {:<w_status$} {:>10}",
            r.coid,
            r.venue,
            r.account.as_deref().unwrap_or("-"),
            r.symbol,
            r.side,
            r.qty,
            r.order_type,
            opt_num(&r.price),
            r.status,
            r.filled_qty,
        ));
    }
    out
}

/// Width for one column: wide enough for the header and every cell.
fn col_width<'a>(header: &str, cells: impl Iterator<Item = &'a str>) -> usize {
    cells.map(str::len).max().unwrap_or(0).max(header.len())
}

/// The flat JSON form — see this module's doc for why it is an array of self-describing rows rather
/// than a nested document.
///
/// ⚠ Built with `serde_json::json!` rather than a `#[derive(Serialize)]` on [`OrderRow`]: this crate
/// depends on `serde_json` only (no direct `serde`), and the macro reaches `Serialize` impls that
/// already exist for `String`/`f64`/`Option<_>`/`&str` through `serde_json`'s own dependency —
/// exactly how `crate::cmd::trade::status`'s `json_body` builds its document.
pub(crate) fn rows_json(rows: &[OrderRow]) -> String {
    let arr: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "coid": r.coid,
                "venue": r.venue,
                "account": r.account,
                "symbol": r.symbol,
                "side": r.side,
                "qty": r.qty,
                "order_type": r.order_type,
                "price": r.price,
                "status": r.status,
                "filled_qty": r.filled_qty,
            })
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::Value::Array(arr))
        .expect("every field is a String/f64/Option/&str; serialization is total")
}

// -------------------------------------------------------------------------------------------
// POSITION rows
// -------------------------------------------------------------------------------------------

/// One row of the rendered position set — the flat, JSON-friendly projection [`positions_table`] and
/// [`positions_json`] share.
///
/// ⚠ **`account` is populated FOR REAL here, unlike [`OrderRow::account`].** [`WirePositionView`]
/// itself carries no account field — but the enclosing [`WireVenueBlock`] DOES (`account: None` for
/// the unlabelled account, `Some(label)` for a named one), and every position in
/// [`WireSnapshot::venues`] sits inside exactly one such block. [`position_rows`] reads the label
/// off the BLOCK, so this column is `None` only while the wire itself says "the unlabelled
/// account" — never as a placeholder for "nowhere to look".
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PositionRow {
    pub venue: String,
    pub account: Option<String>,
    pub symbol: String,
    pub size: f64,
    pub avg_px: f64,
    pub unrealized: f64,
}

/// Project every venue block's positions into rows, optionally narrowed to one `book` and/or one
/// `symbol` (case-insensitive, exact match).
///
/// ⚠ **Walks [`WireSnapshot::venues`], never the top-level [`WireSnapshot::positions`].** That
/// top-level list is documented as the mirror of `venues[0].positions` — the PRIMARY venue only — so
/// a multi-venue node's second and later venue blocks would be silently invisible to a reader that
/// took the flat shortcut. Walking `venues` reaches every block, and each block already carries the
/// `account` label its positions have nowhere else to borrow (see [`PositionRow::account`]).
///
/// ⚠ **Unlike [`order_rows`], a LABELLED `book` genuinely NARROWS here rather than needing a
/// refusal.** `order ls` must call `selector::refuse_an_unaddressable_book` because an order row's
/// optional `account` cannot tell the default account from an older node; a position's venue BLOCK
/// carries a field that can, so [`book_matches_venue_block`] can ask the real question instead of
/// refusing to ask it.
pub(crate) fn position_rows(
    snap: &WireSnapshot,
    book: Option<&Book>,
    symbol: Option<&str>,
) -> Vec<PositionRow> {
    snap.venues
        .iter()
        .filter(|v| book.is_none_or(|b| book_matches_venue_block(b, v)))
        .flat_map(|v| {
            v.positions
                .iter()
                .filter(|p| symbol.is_none_or(|s| p.symbol.eq_ignore_ascii_case(s)))
                .map(move |p| position_row(v, p))
        })
        .collect()
}

/// Does `book` name this venue block? The venue by name (case-insensitive, the same rule
/// [`order_rows`] uses); the account by EXACT match against the block's own label text —
/// `book.label.text()` is `None` for the default account and `Some(text)` for a named one, exactly
/// the shape [`WireVenueBlock::account`] is already in, so the two compare directly.
fn book_matches_venue_block(book: &Book, block: &WireVenueBlock) -> bool {
    block.venue.eq_ignore_ascii_case(&book.venue) && block.account.as_deref() == book.label.text()
}

fn position_row(block: &WireVenueBlock, p: &WirePositionView) -> PositionRow {
    PositionRow {
        venue: block.venue.clone(),
        account: block.account.clone(),
        symbol: p.symbol.clone(),
        size: p.size,
        avg_px: p.avg_px,
        unrealized: p.unrealized,
    }
}

/// The human table form, plus a Σ-marked aggregate line summed over `unrealized` when there is at
/// least one row.
///
/// ⚠ **The aggregate is a SEPARATE line, never one more table row.** A summed `unrealized` sharing
/// the VENUE/ACCOUNT columns with real rows would present as one more book's number — the exact
/// misreading this line exists to rule out. So it names itself as a SUM over N rows and carries no
/// venue/account cell of its own; the Σ prefix is the visual mark a reader (and this crate's own
/// test) can key on. Omitted entirely for zero rows: there is nothing to sum, and printing "Σ ...: 0"
/// over an empty book would itself look like a balance rather than an absence.
pub(crate) fn positions_table(rows: &[PositionRow]) -> String {
    let w_venue = col_width("VENUE", rows.iter().map(|r| r.venue.as_str()));
    let w_account = col_width("ACCOUNT", rows.iter().map(|r| r.account.as_deref().unwrap_or("-")));
    let w_symbol = col_width("SYMBOL", rows.iter().map(|r| r.symbol.as_str()));

    let mut out = format!(
        "{:<w_venue$} {:<w_account$} {:<w_symbol$} {:>12} {:>12} {:>12}",
        "VENUE", "ACCOUNT", "SYMBOL", "SIZE", "AVG_PX", "UNREALIZED"
    );
    if rows.is_empty() {
        out.push_str("\n  (none)");
        return out;
    }
    let mut total_unrealized = 0.0;
    for r in rows {
        total_unrealized += r.unrealized;
        out.push('\n');
        out.push_str(&format!(
            "{:<w_venue$} {:<w_account$} {:<w_symbol$} {:>12} {:>12} {:>12}",
            r.venue,
            r.account.as_deref().unwrap_or("-"),
            r.symbol,
            r.size,
            r.avg_px,
            r.unrealized,
        ));
    }
    out.push_str(&format!(
        "\n  \u{03a3} total unrealized across {} row{} (NOT one account's balance): {total_unrealized}",
        rows.len(),
        if rows.len() == 1 { "" } else { "s" },
    ));
    out
}

/// The flat JSON form — see this module's doc for why it is an array of self-describing rows.
///
/// ⚠ **No total row, ever — a total is a RENDERING, not a row a machine folds.** A JSON consumer that
/// wants the aggregate sums the `unrealized` field itself; baking a total into the array would be a
/// row with no `symbol` a strict schema could not distinguish from a real position, and exactly the
/// kind of machine-shaped footgun [`positions_table`]'s human-only aggregate line avoids.
pub(crate) fn positions_json(rows: &[PositionRow]) -> String {
    let arr: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "venue": r.venue,
                "account": r.account,
                "symbol": r.symbol,
                "size": r.size,
                "avg_px": r.avg_px,
                "unrealized": r.unrealized,
            })
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::Value::Array(arr))
        .expect("every field is a String/f64/Option; serialization is total")
}

// -------------------------------------------------------------------------------------------
// MOUNT (strategy registry) rows
// -------------------------------------------------------------------------------------------

/// One row of the rendered mount registry — `crate::cmd::trade::strategy`'s `ls` projection of
/// [`WireStrategyStatus::mounts`].
///
/// ⚠ **`asset_class: None` has THREE meanings, and this row does NOT resolve which — see
/// [`WireMountRow::asset_class`]'s own doc.** Two are indistinguishable by looking at the field
/// alone: a node too old to carry it at all, and a current node whose mount is still TOML-backed and
/// has genuinely named no product. Only `vike_tradehub_client::proto::FEATURE_MOUNT_CLASS` (a fact
/// about the ANSWERING NODE, carried nowhere on this row) tells them apart, which is why
/// [`mounts_table`] takes that capability as an explicit parameter rather than reading it off
/// `asset_class` — see that function's own doc for the defect this guards against.
///
/// ⚠ **No `account` field and [`mounts_table`] carries no aggregate.** A mount is addressed by
/// venue+symbol+interval, not by an account — [`WireMountRow`] carries no such field either — and
/// none of this row's numeric-shaped data (there is none; `live` is a bool and `asset_class` a
/// label) sums to a figure that could be misread as one account's balance the way summed
/// `unrealized` can. So the aggregate rule [`positions_table`] carries has nothing to apply to here.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MountRow {
    pub strategy: String,
    pub venue: String,
    pub symbol: String,
    pub interval: String,
    pub live: bool,
    pub asset_class: Option<String>,
    pub params: String,
}

/// Project a [`WireStrategyStatus`]'s mounts into rows. PURE — the read itself is
/// `crate::cmd::trade::strategy`'s `run_ls`, over the SAME `vike_tradehub_client::
/// strategy_status_with_features` call `crate::cmd::trade::status`'s registry half already makes;
/// this function opens no second connection and takes no node address.
///
/// ⚠ Deliberately does NOT take `knows_class`: `asset_class` rides through VERBATIM, whatever the
/// wire said (which is already `None` from an old node — that field's own `#[serde(default)]`
/// guarantees it). The capability question belongs to the RENDERER
/// ([`mounts_table`]'s `knows_class` parameter), not to this projection — folding it in here would
/// still leave a caller needing to know the capability separately for the EMPTY-`rows` case, where
/// there is no row left to carry it.
pub(crate) fn mount_rows(status: &WireStrategyStatus) -> Vec<MountRow> {
    status.mounts.iter().map(mount_row).collect()
}

fn mount_row(m: &WireMountRow) -> MountRow {
    MountRow {
        strategy: m.strategy.clone(),
        venue: m.venue.clone(),
        symbol: m.symbol.clone(),
        interval: m.interval.clone(),
        live: m.live,
        asset_class: m.asset_class.clone(),
        params: m.params.clone(),
    }
}

/// The human table form. The header row survives an empty `rows`, the same rule [`positions_table`]
/// and `crate::cmd::trade::order::rows_table` both keep. `PARAMS` is left unpadded on purpose — it is
/// prose-length, the same choice `crate::cmd::trade::status::registry_lines` makes for the identical
/// cell.
///
/// ⚠ **`knows_class` GATES the PRODUCT column's very PRESENCE, and an earlier version of this
/// function did not take it — that was a defect, not a simplification.** It rendered an em dash for
/// every `asset_class: None` unconditionally, which collapses two of the three meanings
/// [`WireMountRow::asset_class`]'s own doc names into one: "an old node that cannot carry the field"
/// read exactly like "a current, unmigrated mount that genuinely names no product," and the
/// consequence that doc states in as many words is an operator sent to upgrade a daemon that is
/// already current. `crate::cmd::trade::status::registry_lines(status, knows_class)` had already
/// solved this — it OMITS the column entirely when `knows_class` is false, on the argument that "a
/// column of dashes would claim no mount here names a product, which a client talking to a node that
/// cannot carry the field is in no position to say." This function copied that function's cosmetics
/// (the em dash, the char-count width handling) while first shipping without the gate that makes them
/// honest; it now mirrors `registry_lines` exactly, including the early return.
///
/// `knows_class` is a property of the ANSWERING NODE (`Welcome.features` advertising
/// `vike_tradehub_client::proto::FEATURE_MOUNT_CLASS`), not of any one row, so it cannot be read off
/// `rows` — an empty result carries no row to read it from, which is exactly why
/// `crate::cmd::trade::strategy::run_ls` reads it once, off the SAME `strategy_status_with_features`
/// call that fetched `rows`, and passes it here explicitly rather than this function guessing.
pub(crate) fn mounts_table(rows: &[MountRow], knows_class: bool) -> String {
    let mode = |live: bool| if live { "LIVE" } else { "paper" };

    let w_strategy = col_width("STRATEGY", rows.iter().map(|r| r.strategy.as_str()));
    let w_venue = col_width("VENUE", rows.iter().map(|r| r.venue.as_str()));
    let w_symbol = col_width("SYMBOL", rows.iter().map(|r| r.symbol.as_str()));
    let w_interval = col_width("INTERVAL", rows.iter().map(|r| r.interval.as_str()));
    let w_mode = col_width("MODE", rows.iter().map(|r| mode(r.live)));

    if !knows_class {
        let mut out = format!(
            "{:<w_strategy$} {:<w_venue$} {:<w_symbol$} {:<w_interval$} {:<w_mode$} PARAMS",
            "STRATEGY", "VENUE", "SYMBOL", "INTERVAL", "MODE"
        );
        if rows.is_empty() {
            out.push_str("\n  (none)");
            return out;
        }
        for r in rows {
            out.push('\n');
            out.push_str(&format!(
                "{:<w_strategy$} {:<w_venue$} {:<w_symbol$} {:<w_interval$} {:<w_mode$} {}",
                r.strategy,
                r.venue,
                r.symbol,
                r.interval,
                mode(r.live),
                r.params,
            ));
        }
        return out;
    }

    // ⚠ Materialized once, up front: the em-dash placeholder must be measured by CHARACTER count,
    // not byte length (the em dash is 3 bytes and 1 column) — the same trap
    // `crate::cmd::trade::status::registry_lines` names for this exact cell.
    let products: Vec<String> =
        rows.iter().map(|r| r.asset_class.clone().unwrap_or_else(|| "—".to_string())).collect();
    let w_product =
        products.iter().map(|p| p.chars().count()).chain(["PRODUCT".len()]).max().unwrap_or(0);

    let mut out = format!(
        "{:<w_strategy$} {:<w_venue$} {:<w_symbol$} {:<w_interval$} {:<w_mode$} {:<w_product$} \
         PARAMS",
        "STRATEGY", "VENUE", "SYMBOL", "INTERVAL", "MODE", "PRODUCT"
    );
    if rows.is_empty() {
        out.push_str("\n  (none)");
        return out;
    }
    for (r, product) in rows.iter().zip(products.iter()) {
        out.push('\n');
        out.push_str(&format!(
            "{:<w_strategy$} {:<w_venue$} {:<w_symbol$} {:<w_interval$} {:<w_mode$} {:<w_product$} \
             {}",
            r.strategy,
            r.venue,
            r.symbol,
            r.interval,
            mode(r.live),
            product,
            r.params,
        ));
    }
    out
}

/// The flat JSON form — one object per mount, no nesting, no total row (see this module's doc for
/// why a rendering-only aggregate never becomes a row).
///
/// ⚠ **`asset_class_known` carries the SAME `knows_class` fact [`mounts_table`] gates its PRODUCT
/// column on, and this parameter is not optional.** `mounts_table`'s own doc names the defect: an
/// `asset_class: None` this function serialized with no capability marker collapsed the SAME
/// three-way ambiguity `WireMountRow::asset_class` warns about into one JSON value — and for a
/// machine consumer that is arguably WORSE than the table's dash was for a human, because an agent
/// parsing `--json` has no outside context about its own fleet's age and no review step before
/// acting on the false inference ("N mounts have no asset class, migrate them" against a node that
/// is already current). Matching `crate::cmd::trade::status`'s own `--json` shape (which omits this
/// marker entirely) would PROPAGATE that gap rather than defend it — that shape's silence is a
/// known pre-existing hole, not a considered decision that the ambiguity is fine there.
///
/// The flag rides PER-ROW rather than as a top-level object field: this function's output is a bare
/// ARRAY with nowhere to hang a node-level flag without a breaking shape change (breaking
/// [`mounts_json_is_flat_with_no_nesting_and_no_total_row`]'s own flat-array contract), and a
/// per-row placement sidesteps the empty-result question entirely — an empty array conveys no
/// mounts either way, under either `knows_class` value, so there is no header-like case here the
/// way [`mounts_table`] has to solve for zero rows.
pub(crate) fn mounts_json(rows: &[MountRow], knows_class: bool) -> String {
    let arr: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "strategy": r.strategy,
                "venue": r.venue,
                "symbol": r.symbol,
                "interval": r.interval,
                "live": r.live,
                "asset_class": r.asset_class,
                "asset_class_known": knows_class,
                "params": r.params,
            })
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::Value::Array(arr))
        .expect("every field is a String/bool/Option; serialization is total")
}

#[path = "render_tests.rs"]
#[cfg(test)]
mod render_tests;
