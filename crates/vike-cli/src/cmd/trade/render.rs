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
//! ⚠ **An [`OrderRow`]'s `account` comes off [`OrderViewWire::account`], whose ABSENCE says two
//! things**: the default account, or a node that predates the field. [`order_account`] tells them
//! apart by the tell that field's doc names, and a row it cannot attribute says so
//! ([`OrderRow::account_known`]), so a `null` beside `account_known: false` is never read as the
//! default account. The order's bare `venue` string cannot stand in for it: two accounts of one venue
//! publish IDENTICAL `venue` strings and different `route_key`s on their
//! [`vike_tradehub_client::wire::VenueBlockWire`]s. The `account` key is present on every row —
//! `null` rather than omitted — because a machine reader must not have to treat "no account" and
//! "this reader forgot to ask" as the same silence.
//!
//! A position lives inside a [`VenueBlockWire`], and that block carries `account`, so
//! [`position_rows`] has no such ambiguity: see [`PositionRow`]'s own doc.
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

use vike_model::{OrderAttributes, OrderStopAdjust, OrderStopAdjustTo};
use vike_tradehub_client::wire::{
    CoreSnapshotWire, MountViewWire, OrderEmulationViewWire, OrderHeldViewWire,
    OrderReleasePriceWire, OrderReleaseWire, OrderViewWire, PositionViewWire, StrategyStatusWire,
    VenueBlockWire,
};

use super::selector::Book;
use super::{opt_number, side_word};
use vike_utility::math::fmt_number;

/// One row of the rendered order set — the flat, JSON-friendly projection [`rows_table`] and
/// [`rows_json`] share.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OrderRow {
    pub order_client_id: String,
    pub venue: String,
    /// The labelled account the order rests in; `None` for the default account, or for one the frame
    /// cannot name, which [`Self::account_known`] tells apart.
    pub account: Option<String>,
    /// Whether the frame says whose order this is ([`order_account`]). `false` only on a node that
    /// predates `OrderViewWire::account`.
    pub account_known: bool,
    pub symbol: String,
    pub side: &'static str,
    pub qty: f64,
    pub order_type: String,
    pub price: Option<f64>,
    pub status: String,
    pub filled_qty: f64,
    /// WHY the node refused the order (`OrderViewWire::reason`): the venue's or the gate's words.
    /// `None` for an order that was not refused, for a node that predates the field, and for a
    /// refusal that carried none. A gate veto has no order row at all, so it never shows here.
    pub reason: Option<String>,
    /// WHEN the node sends an order it HOLDS (`OrderHeldViewWire::release`, with the venue its price condition
    /// reads, `OrderHeldViewWire::release_venue`, folded back in); on an order at a venue, the time release that
    /// venue holds itself (`OrderViewWire::release`). `None` on every other order.
    pub release: Option<OrderReleaseWire>,
    /// Why a HELD order whose time came still waits (`OrderHeldViewWire::waiting`: no fresh price), in the
    /// node's words; never in [`Self::reason`], which is a refusal's. `None` on every other row.
    pub waiting: Option<String>,
    /// A trailing order's trail (`OrderViewWire::trail`); `None` on every other kind, whose row keeps its bytes.
    pub trail: Option<f64>,
    /// A trailing order's trail as a percent (`OrderViewWire::trail_percent`), in place of [`Self::trail`].
    pub trail_percent: Option<f64>,
    /// A trailing-limit order's limit offset (`OrderViewWire::limit_offset`).
    pub limit_offset: Option<f64>,
    /// A trailing order's activation, its trigger price; `None` on every other kind (a stop's trigger is not this).
    pub activation: Option<f64>,
    /// A resting limit order's attributes (`OrderViewWire::attributes`).
    pub attributes: Option<OrderAttributes>,
    /// A resting stop's adjustment not yet made (`OrderViewWire::adjust`).
    pub adjust: Option<OrderStopAdjust>,
    /// The quantity the order shows at its venue (`OrderViewWire::display_quantity`); only zero (HIDDEN) is said.
    pub display_quantity: Option<f64>,
    /// The emulated order this one is the live CHILD of (`OrderEmulationViewWire::child`, decision 0275): the plain
    /// limit a chase or an iceberg keeps resting. `None` on every other row.
    pub child_of: Option<String>,
}

impl OrderRow {
    /// A HELD row's last cell: what it waits for ([`held_words`]), then its terms ([`Self::terms`]) when it states
    /// any, then why it still waits when the node says (`waiting: <words>`, `OrderHeldViewWire::waiting`). `None` on
    /// a row at a venue. The ONE rendering `trade order ls` and the REPL's `orders` share.
    pub(crate) fn held_cell(&self) -> Option<String> {
        let release = self.release.as_ref().filter(|_| self.status == HELD)?;
        let mut cell = held_words(release);
        if let Some(terms) = self.terms() {
            cell.push(' ');
            cell.push_str(&terms);
        }
        if let Some(why) = &self.waiting {
            cell.push_str("; waiting: ");
            cell.push_str(why);
        }
        Some(cell)
    }

    /// A RESTING order's terms past its price, in the words the order tables share: a trailing order's
    /// (`trail=<price>` or `trail=<p>%`, then `offset=` and `activation=` when it states them), a limit order's
    /// attributes and a stop's adjustment ([`attribute_words`]), `hidden` for a display of zero, and the time
    /// release its venue holds (`releases at <t>`). `None` when it states none, so a plain row keeps its bytes.
    pub(crate) fn terms(&self) -> Option<String> {
        let mut words = Vec::new();
        let trail = match (self.trail, self.trail_percent) {
            (Some(t), _) => Some(fmt_number(t)),
            (None, Some(p)) => Some(format!("{}%", fmt_number(p))),
            (None, None) => None,
        };
        if let Some(trail) = trail {
            words.push(format!("trail={trail}"));
            for (word, v) in [("offset", self.limit_offset), ("activation", self.activation)] {
                if let Some(v) = v {
                    words.push(format!("{word}={}", fmt_number(v)));
                }
            }
        }
        words.extend(attribute_words(self.attributes.as_ref(), self.adjust.as_ref()));
        // an iceberg's slice above zero says nothing new, so its row keeps its bytes
        if self.display_quantity == Some(0.0) {
            words.push("hidden".to_string());
        }
        if self.status != HELD
            && let Some(r) = &self.release
        {
            // a venue holds a TIME release only: a price condition is always the node's to hold
            words
                .push(format!("releases at {}", vike_model::time::epoch_ms_to_utc_timestamp(r.at)));
        }
        (!words.is_empty()).then(|| words.join(" "))
    }
}

/// A limit order's attributes (`all-or-none`, `sweep-to-fill`, `discretion=<d>`) and a stop's adjustment
/// (`adjust at <x> to stop <s>`, `to stop-limit <s> limit <l>`, `to trailing from <s> by <t>` or `<p>%`), one
/// entry each, in the words the order tables and the preview line share; empty when it states none.
pub(crate) fn attribute_words(
    attributes: Option<&OrderAttributes>,
    adjust: Option<&OrderStopAdjust>,
) -> Vec<String> {
    let mut words = Vec::new();
    if let Some(a) = attributes {
        for (on, word) in [(a.all_or_none, "all-or-none"), (a.sweep_to_fill, "sweep-to-fill")] {
            if on {
                words.push(word.to_string());
            }
        }
        if let Some(d) = a.discretionary_amount {
            words.push(format!("discretion={}", fmt_number(d)));
        }
    }
    if let Some(adj) = adjust {
        let to = match adj.to {
            OrderStopAdjustTo::Stop { stop } => format!("stop {}", fmt_number(stop)),
            OrderStopAdjustTo::StopLimit { stop, limit } => {
                format!("stop-limit {} limit {}", fmt_number(stop), fmt_number(limit))
            }
            OrderStopAdjustTo::Trailing { stop, trail, trail_percent } => {
                let by = trail
                    .map(fmt_number)
                    .or_else(|| trail_percent.map(|p| format!("{}%", fmt_number(p))))
                    .unwrap_or_default();
                format!("trailing from {} by {by}", fmt_number(stop))
            }
        };
        words.push(format!("adjust at {} to {to}", fmt_number(adj.at)));
    }
    words
}

/// Project `snap`'s orders into rows, optionally narrowed to one `book`'s venue and/or one `symbol`
/// (case-insensitive, exact match — the same rule `crate::cmd::trade`'s own REPL tables use). The orders the
/// node HOLDS until a release follow, status `HELD` ([`held_row`]); a held bracket exit stays out, as before.
///
/// ⚠ **The book filter narrows by VENUE only**: a bare venue lists every account of it, each row naming
/// its own. A book that names an account narrows further in `crate::cmd::trade::order`'s `run_ls`,
/// over these rows.
pub(crate) fn order_rows(
    snap: &CoreSnapshotWire,
    book: Option<&Book>,
    symbol: Option<&str>,
) -> Vec<OrderRow> {
    let shown = |venue: &str, symbol_name: &str| {
        book.is_none_or(|b| venue.eq_ignore_ascii_case(&b.venue))
            && symbol.is_none_or(|s| symbol_name.eq_ignore_ascii_case(s))
    };
    let held = snap.held_exits.iter().filter(|h| h.release.is_some() && shown(&h.venue, &h.symbol));
    snap.orders
        .iter()
        .filter(|o| shown(&o.venue, &o.symbol))
        .map(|o| order_row(snap, o))
        .chain(held.map(held_row))
        .collect()
}

/// An order the node HOLDS until its release, as a row: status `HELD`, nothing filled, the release beside it
/// with the venue its price condition reads folded back in, and its terms. A node that writes the release names
/// accounts, so the row's account is known (absent = the default one).
pub(crate) fn held_row(h: &OrderHeldViewWire) -> OrderRow {
    OrderRow {
        order_client_id: h.order_client_id.clone(),
        venue: h.venue.clone(),
        account: h.account.clone(),
        account_known: true,
        symbol: h.symbol.clone(),
        side: side_word(h.side),
        qty: h.qty,
        order_type: h.order_type.clone(),
        price: h.price.or(h.trigger_price),
        status: HELD.to_string(),
        filled_qty: 0.0,
        reason: None,
        release: h
            .release
            .clone()
            .map(|r| OrderReleaseWire { venue: h.release_venue.clone(), ..r }),
        waiting: h.waiting.clone(),
        trail: None,
        trail_percent: h.trail_percent,
        limit_offset: None,
        activation: None,
        attributes: h.attributes,
        adjust: h.adjust,
        display_quantity: h.display_quantity,
        child_of: None,
    }
}

/// The status word of an order the node holds ([`held_row`]).
pub(crate) const HELD: &str = "HELD";

/// `held until <instant>`, with the price condition when it states one (and its symbol when it reads another than
/// the order's own); `releases at <instant> once <SYMBOL> on <venue> is at or above <price>` when the condition
/// reads another venue ([`OrderReleaseWire::venue`]): what a held order waits for, in the words the order tables and
/// the preview line share.
pub(crate) fn held_words(r: &OrderReleaseWire) -> String {
    let at = vike_model::time::epoch_ms_to_utc_timestamp(r.at);
    let (side, px) = match r.price {
        None => return format!("held until {at}"),
        Some(OrderReleasePriceWire::Above(px)) => ("above", fmt_number(px)),
        Some(OrderReleasePriceWire::Below(px)) => ("below", fmt_number(px)),
    };
    match &r.venue {
        // a price read on another venue names its symbol (the doors refuse one that does not)
        Some(venue) => format!(
            "releases at {at} once {} on {venue} is at or {side} {px}",
            r.symbol.as_deref().unwrap_or("the price")
        ),
        None => {
            let of = r.symbol.as_deref().map(|s| format!(" of {s}")).unwrap_or_default();
            format!("held until {at} and the price{of} at or {side} {px}")
        }
    }
}

/// Whose book `o` rests in, as the frame says it, by the rule of `OrderViewWire::account`'s doc:
/// `Some(Some(label))` a labelled account, `Some(None)` the default one, `None` the frame cannot say.
///
/// A named order is that account's. An unnamed one is the default account's once any block carries a
/// `tier` (presence, not value: such a node names its orders' accounts); on a node older than that it
/// is the account of the one block its venue has, and with several it is UNKNOWN, never the default.
fn order_account<'a>(snap: &'a CoreSnapshotWire, o: &'a OrderViewWire) -> Option<Option<&'a str>> {
    if o.account.is_some() || snap.venues.iter().any(|b| b.tier.is_some()) {
        return Some(o.account.as_deref());
    }
    let mut blocks = snap.venues.iter().filter(|b| b.venue.eq_ignore_ascii_case(&o.venue));
    match (blocks.next(), blocks.next()) {
        (Some(only), None) => Some(only.account.as_deref()),
        _ => None,
    }
}

/// An order at a venue, as a row (`trade order ls` and the REPL's `orders` both read one).
pub(crate) fn order_row(snap: &CoreSnapshotWire, o: &OrderViewWire) -> OrderRow {
    let account = order_account(snap, o);
    OrderRow {
        order_client_id: o.order_client_id.clone(),
        venue: o.venue.clone(),
        account: account.flatten().map(str::to_string),
        account_known: account.is_some(),
        symbol: o.symbol.clone(),
        side: side_word(o.side),
        qty: o.qty,
        order_type: o.order_type.clone(),
        price: o.price,
        status: o.status.clone(),
        filled_qty: o.filled_qty,
        reason: o.reason.clone(),
        release: o.release.clone(),
        waiting: None,
        trail: o.trail,
        trail_percent: o.trail_percent,
        limit_offset: o.limit_offset,
        activation: o.trigger_price.filter(|_| o.trail.is_some() || o.trail_percent.is_some()),
        attributes: o.attributes,
        adjust: o.adjust,
        display_quantity: o.display_quantity,
        child_of: snap
            .emulated_orders
            .iter()
            .find(|e| e.child.as_deref() == Some(o.order_client_id.as_str()))
            .map(|e| e.id.clone()),
    }
}

/// The ACCOUNT cell: the label, `-` for the default account as every table of this plane prints it,
/// `?` for an order the frame cannot attribute.
fn account_cell(r: &OrderRow) -> &str {
    r.account.as_deref().unwrap_or(if r.account_known { "-" } else { "?" })
}

/// The human table form. The header row survives an empty `rows` — an operator asking "what is
/// working" must see the COLUMNS even when the answer is "nothing", never a blank screen.
pub(crate) fn rows_table(rows: &[OrderRow]) -> String {
    let w_order_client_id =
        col_width("ORDER_CLIENT_ID", rows.iter().map(|r| r.order_client_id.as_str()));
    let w_venue = col_width("VENUE", rows.iter().map(|r| r.venue.as_str()));
    let w_account = col_width("ACCOUNT", rows.iter().map(account_cell));
    let w_symbol = col_width("SYMBOL", rows.iter().map(|r| r.symbol.as_str()));
    let w_side = col_width("SIDE", rows.iter().map(|r| r.side));
    let w_type = col_width("TYPE", rows.iter().map(|r| r.order_type.as_str()));
    let w_status = col_width("STATUS", rows.iter().map(|r| r.status.as_str()));
    // A v2 id's words beside it (`describe.rs`'s `order_client_id_words`; the ACCOUNT column names the label), a column only a
    // table holding one has: a table of v1 or foreign ids reads as it always did.
    let words: Vec<Option<String>> = rows
        .iter()
        .map(|r| super::describe::order_client_id_words(&r.order_client_id, None))
        .collect();
    let w_words = words
        .iter()
        .any(Option::is_some)
        .then(|| super::width_of("WORDS", words.iter().flatten().map(String::as_str), None));
    let lead = |order_client_id: &str, said: Option<&str>| match w_words {
        None => format!("{order_client_id:<w_order_client_id$}"),
        Some(w) => format!("{order_client_id:<w_order_client_id$} {:<w$}", said.unwrap_or("-")),
    };

    let mut out = format!(
        "{} {:<w_venue$} {:<w_account$} {:<w_symbol$} {:<w_side$} {:>10} {:<w_type$} \
         {:>10} {:<w_status$} {:>10}",
        lead("ORDER_CLIENT_ID", Some("WORDS")),
        "VENUE",
        "ACCOUNT",
        "SYMBOL",
        "SIDE",
        "QTY",
        "TYPE",
        "PRICE",
        "STATUS",
        "FILLED"
    );
    out.push_str(" REASON");
    if rows.is_empty() {
        out.push_str("\n  (none)");
        return out;
    }
    for (r, said) in rows.iter().zip(&words) {
        out.push('\n');
        out.push_str(&format!(
            "{} {:<w_venue$} {:<w_account$} {:<w_symbol$} {:<w_side$} {:>10} \
             {:<w_type$} {:>10} {:<w_status$} {:>10}",
            lead(&r.order_client_id, said.as_deref()),
            r.venue,
            account_cell(r),
            r.symbol,
            r.side,
            fmt_number(r.qty),
            r.order_type,
            opt_number(&r.price),
            r.status,
            fmt_number(r.filled_qty),
        ));
        out.push(' ');
        // An emulated order's live child names its parent first (the EMULATED block lists the parent).
        if let Some(parent) = &r.child_of {
            out.push_str("child of ");
            out.push_str(parent);
            if r.reason.is_none() && r.terms().is_none() {
                continue;
            }
            out.push_str("; ");
        }
        // A held order has no venue words yet: what it waits for goes in their place, and why it
        // still waits when the node says.
        if let Some(cell) = r.held_cell() {
            out.push_str(&cell);
            continue;
        }
        // Free text from a venue: one line per order, whatever it sent.
        let one_line = r.reason.as_deref().unwrap_or("-").replace(['\r', '\n'], " ");
        // A resting order's terms lead the cell (a trailing order's price column is empty: it states none).
        if let Some(terms) = r.terms() {
            out.push_str(&terms);
            if r.reason.is_some() {
                out.push_str("; ");
                out.push_str(&one_line);
            }
            continue;
        }
        out.push_str(&one_line);
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
///
/// The EMULATED orders follow the order rows in the SAME flat array, one row each ([`emulated_value`]), each marked
/// by its `emulated` kind word: a reader tells them apart by that key, never by position.
pub(crate) fn rows_json(rows: &[OrderRow], emulated: &[&OrderEmulationViewWire]) -> String {
    let arr: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            let mut row = serde_json::json!({
                "order_client_id": r.order_client_id,
                "venue": r.venue,
                "account": r.account,
                "account_known": r.account_known,
                "symbol": r.symbol,
                "side": r.side,
                "qty": r.qty,
                "order_type": r.order_type,
                "price": r.price,
                "status": r.status,
                "filled_qty": r.filled_qty,
                "reason": r.reason,
            });
            // Only on a row whose id is a v2 one (`describe.rs`'s `order_client_id_words`), so a v1 or foreign row keeps its bytes.
            if let Some(words) = super::describe::order_client_id_words(&r.order_client_id, None) {
                row["words"] = serde_json::json!(words);
            }
            // Only on a HELD row (the wire's `{"at": .., "price": ..}`, and `venue` when its condition reads
            // another) and on a row whose venue holds its release, so every other row keeps its bytes.
            if let Some(release) = &r.release {
                row["release"] = serde_json::json!(release);
            }
            // Only on a HELD row that still waits, in its own key (never `reason`, a refusal's).
            if let Some(waiting) = &r.waiting {
                row["waiting"] = serde_json::json!(waiting);
            }
            // Only on a TRAILING row (`trail_percent` only when it trails by one), so every other row keeps
            // its bytes.
            if r.trail.is_some() || r.trail_percent.is_some() {
                row["trail"] = serde_json::json!(r.trail);
                row["limit_offset"] = serde_json::json!(r.limit_offset);
                row["activation"] = serde_json::json!(r.activation);
                if let Some(p) = r.trail_percent {
                    row["trail_percent"] = serde_json::json!(p);
                }
            }
            // The wire's own shapes, each only on a row that states it.
            if let Some(a) = &r.attributes {
                row["attributes"] = serde_json::json!(a);
            }
            if let Some(adj) = &r.adjust {
                row["adjust"] = serde_json::json!(adj);
            }
            // Only for a display of zero: an iceberg's slice adds no key.
            if r.display_quantity == Some(0.0) {
                row["hidden"] = serde_json::Value::Bool(true);
            }
            // Only on an emulated order's live child: the id of the order it works for.
            if let Some(parent) = &r.child_of {
                row["child_of"] = serde_json::json!(parent);
            }
            row
        })
        .chain(emulated.iter().map(|e| emulated_value(e)))
        .collect();
    serde_json::to_string_pretty(&serde_json::Value::Array(arr))
        .expect("every field is a String/f64/Option/&str; serialization is total")
}

// -------------------------------------------------------------------------------------------
// EMULATED orders
// -------------------------------------------------------------------------------------------

/// The orders the node EMULATES (`CoreSnapshotWire::emulated_orders`, decision 0275), narrowed as [`order_rows`]
/// narrows (a book by VENUE, a symbol exactly, case-insensitive). ONE list for every kind: a core-held stop is
/// one of its rows, so the CLI lists each emulated order once.
pub(crate) fn emulated_rows<'a>(
    snap: &'a CoreSnapshotWire,
    book: Option<&Book>,
    symbol: Option<&str>,
) -> Vec<&'a OrderEmulationViewWire> {
    snap.emulated_orders
        .iter()
        .filter(|e| book.is_none_or(|b| e.venue.eq_ignore_ascii_case(&b.venue)))
        .filter(|e| symbol.is_none_or(|s| e.symbol.eq_ignore_ascii_case(s)))
        .collect()
}

/// An emulated order's terms in the order tables' words: `trigger=`, `price=`, `trail=`, `peg=<anchor><offset>`,
/// `visible=` and `reduce-only`, each only when the kind states it.
fn emulated_terms(e: &OrderEmulationViewWire) -> String {
    let mut words: Vec<String> = [("trigger", e.trigger), ("price", e.price), ("trail", e.trail)]
        .into_iter()
        .filter_map(|(word, v)| v.map(|v| format!("{word}={}", fmt_number(v))))
        .collect();
    if let Some(p) = &e.peg {
        let sign = if p.offset < 0.0 { "" } else { "+" };
        words.push(format!("peg={}{sign}{}", p.anchor, fmt_number(p.offset)));
    }
    if let Some(v) = e.display_quantity {
        words.push(format!("visible={}", fmt_number(v)));
    }
    if e.reduce_only {
        words.push("reduce-only".to_string());
    }
    if words.is_empty() { "-".to_string() } else { words.join(" ") }
}

/// The EMULATED block `trade order ls` and the REPL's `orders` print below the orders: a title line saying what
/// these are, a header, and a row per emulated order (its kind, id, account, terms, filled size at its average,
/// live child and waiting words). The callers print it only when a row exists, so a node that emulates nothing
/// renders as before.
pub(crate) fn emulated_table(rows: &[&OrderEmulationViewWire]) -> String {
    let account = |e: &OrderEmulationViewWire| e.account.clone().unwrap_or_else(|| "-".to_string());
    let filled = |e: &OrderEmulationViewWire| {
        if e.filled > 0.0 {
            format!("{} @ {}", fmt_number(e.filled), fmt_number(e.average_price))
        } else {
            fmt_number(e.filled)
        }
    };
    let cells: Vec<[String; 10]> = rows
        .iter()
        .map(|e| {
            [
                e.id.clone(),
                e.kind.clone(),
                e.venue.clone(),
                account(e),
                e.symbol.clone(),
                side_word(e.side).to_string(),
                fmt_number(e.qty),
                emulated_terms(e),
                filled(e),
                e.child.clone().unwrap_or_else(|| "-".to_string()),
            ]
        })
        .collect();
    let header =
        ["ID", "KIND", "VENUE", "ACCOUNT", "SYMBOL", "SIDE", "QTY", "TERMS", "FILLED", "CHILD"];
    let widths: Vec<usize> = (0..header.len())
        .map(|i| col_width(header[i], cells.iter().map(|c| c[i].as_str())))
        .collect();
    let line = |cells: [&str; 10], state: &str| {
        let mut out = String::new();
        for (cell, &w) in cells.iter().zip(&widths) {
            out.push_str(&format!("{cell:<w$} "));
        }
        out.push_str(state);
        out
    };
    let mut out = format!(
        "EMULATED (the node holds or works each itself, not the venue; `trade order cancel <ID>` cancels one)\n{}",
        line(header, "STATE")
    );
    for (e, c) in rows.iter().zip(&cells) {
        let mut state = match &e.waiting {
            Some(why) => format!("waiting: {}", why.replace(['\r', '\n'], " ")),
            None => "working".to_string(),
        };
        if e.rearms > 0 {
            state.push_str(&format!(
                ", re-armed {}x after a refused or part-filled close",
                e.rearms
            ));
        }
        out.push('\n');
        out.push_str(&line(c.each_ref().map(String::as_str), &state));
    }
    out
}

/// One emulated order as a flat JSON row of `trade order ls --json`: its `emulated` kind word, its `id` (what a
/// cancel names: an arm id or an order client id), its book and terms, each optional term only when stated.
fn emulated_value(e: &OrderEmulationViewWire) -> serde_json::Value {
    let mut row = serde_json::json!({
        "emulated": e.kind,
        "id": e.id,
        "venue": e.venue,
        "account": e.account,
        "account_known": true,
        "symbol": e.symbol,
        "side": side_word(e.side),
        "qty": e.qty,
        "reduce_only": e.reduce_only,
        "filled_qty": e.filled,
        "average_price": e.average_price,
    });
    for (key, v) in [
        ("trigger", e.trigger),
        ("price", e.price),
        ("trail", e.trail),
        ("display_quantity", e.display_quantity),
    ] {
        if let Some(v) = v {
            row[key] = serde_json::json!(v);
        }
    }
    if let Some(p) = &e.peg {
        row["peg"] = serde_json::json!(p);
    }
    if let Some(child) = &e.child {
        row["child"] = serde_json::json!(child);
    }
    if let Some(why) = &e.waiting {
        row["waiting"] = serde_json::json!(why);
    }
    if e.rearms > 0 {
        row["rearms"] = serde_json::json!(e.rearms);
    }
    row
}

// -------------------------------------------------------------------------------------------
// POSITION rows
// -------------------------------------------------------------------------------------------

/// One row of the rendered position set — the flat, JSON-friendly projection [`positions_table`] and
/// [`positions_json`] share.
///
/// ⚠ **`account` is unambiguous here, unlike [`OrderRow::account`].** [`PositionViewWire`]
/// itself carries no account field — but the enclosing [`VenueBlockWire`] DOES (`account: None` for
/// the unlabelled account, `Some(label)` for a named one), and every position in
/// [`CoreSnapshotWire::venues`] sits inside exactly one such block. [`position_rows`] reads the label
/// off the BLOCK, so this column is `None` only while the wire itself says "the unlabelled
/// account" — never as a placeholder for "nowhere to look".
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PositionRow {
    pub venue: String,
    pub account: Option<String>,
    pub symbol: String,
    pub size: f64,
    pub average_price: f64,
    pub unrealized: f64,
    /// The currency GROUP the row's `unrealized` is in: its enclosing block's `VenueBlockWire::ledger_currency`,
    /// EMPTY for the dollar group. Only rows of one group may be added together ([`positions_table`]).
    pub currency: String,
}

/// Project every venue block's positions into rows, optionally narrowed to one `book` and/or one
/// `symbol` (case-insensitive, exact match).
///
/// ⚠ **Walks [`CoreSnapshotWire::venues`], never the top-level [`CoreSnapshotWire::positions`].** That
/// top-level list is documented as the mirror of `venues[0].positions` — the PRIMARY venue only — so
/// a multi-venue node's second and later venue blocks would be silently invisible to a reader that
/// took the flat shortcut. Walking `venues` reaches every block, and each block already carries the
/// `account` label its positions have nowhere else to borrow (see [`PositionRow::account`]).
///
/// ⚠ **The book filter narrows by VENUE only, as [`order_rows`]'s does**: a bare venue lists every
/// account of it, each row naming its own. A book that names an account narrows further in
/// `crate::cmd::trade::position`'s `run_ls`, over these rows, and no book is ever refused: a
/// position's venue BLOCK carries the account, where `order ls` can still meet a row it cannot
/// attribute (`selector::refuse_an_unaddressable_book`).
pub(crate) fn position_rows(
    snap: &CoreSnapshotWire,
    book: Option<&Book>,
    symbol: Option<&str>,
) -> Vec<PositionRow> {
    snap.venues
        .iter()
        .filter(|v| book.is_none_or(|b| v.venue.eq_ignore_ascii_case(&b.venue)))
        .flat_map(|v| {
            v.positions
                .iter()
                .filter(|p| p.size != 0.0)
                .filter(|p| symbol.is_none_or(|s| p.symbol.eq_ignore_ascii_case(s)))
                .map(move |p| position_row(v, p))
        })
        .collect()
}

fn position_row(block: &VenueBlockWire, p: &PositionViewWire) -> PositionRow {
    PositionRow {
        venue: block.venue.clone(),
        account: block.account.clone(),
        symbol: p.symbol.clone(),
        size: p.size,
        average_price: p.average_price,
        unrealized: p.unrealized,
        currency: block.ledger_currency.clone(),
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
///
/// ⚠ **One Σ line per CURRENCY group** (decision 0183, item 13): a row's `unrealized` is in its account's
/// ledger currency, and a EUR figure added to a dollar one is no money. A table whose rows are all in one
/// group, which is every node of one currency, prints the one line it always did; with several, each row
/// gains a `CCY` cell and each group its own Σ line, in the order the groups first appear.
pub(crate) fn positions_table(rows: &[PositionRow]) -> String {
    let w_venue = col_width("VENUE", rows.iter().map(|r| r.venue.as_str()));
    let w_account = col_width("ACCOUNT", rows.iter().map(|r| r.account.as_deref().unwrap_or("-")));
    let w_symbol = col_width("SYMBOL", rows.iter().map(|r| r.symbol.as_str()));
    let code =
        |currency: &str| if currency.is_empty() { "USD".to_string() } else { currency.to_string() };
    let mut sums: Vec<(&str, f64, usize)> = Vec::new();
    for r in rows {
        match sums.iter_mut().find(|(currency, ..)| *currency == r.currency) {
            Some((_, total, n)) => {
                *total += r.unrealized;
                *n += 1;
            }
            // `0.0 + x`, not `x`: the fold the line always had starts at +0.0, so a `-0.0` still prints `0`.
            None => sums.push((r.currency.as_str(), 0.0 + r.unrealized, 1)),
        }
    }
    let multi = sums.len() > 1;

    let mut out = format!(
        "{:<w_venue$} {:<w_account$} {:<w_symbol$} {:>12} {:>12} {:>12}{}",
        "VENUE",
        "ACCOUNT",
        "SYMBOL",
        "SIZE",
        "AVG_PX",
        "UNREALIZED",
        if multi { " CCY" } else { "" }
    );
    if rows.is_empty() {
        out.push_str("\n  (none)");
        return out;
    }
    // ⚠ Through `fmt_number`, like the order tables: the engine's size and average price are `n * step`
    // products one ULP off their decimal (a spot position of `0.00001` printed `0.000010000000000000026`),
    // and a Σ of several rows adds its own tail. `--json` stays exact ([`positions_json`]).
    for r in rows {
        out.push('\n');
        out.push_str(&format!(
            "{:<w_venue$} {:<w_account$} {:<w_symbol$} {:>12} {:>12} {:>12}{}",
            r.venue,
            r.account.as_deref().unwrap_or("-"),
            r.symbol,
            fmt_number(r.size),
            fmt_number(r.average_price),
            fmt_number(r.unrealized),
            if multi { format!(" {}", code(&r.currency)) } else { String::new() },
        ));
    }
    for (currency, total, n) in sums {
        out.push_str(&format!(
            "\n  \u{03a3} total unrealized across {n} row{}{} (NOT one account's balance): {}",
            if n == 1 { "" } else { "s" },
            if multi { format!(" in {}", code(currency)) } else { String::new() },
            fmt_number(total),
        ));
    }
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
            let mut row = serde_json::json!({
                "venue": r.venue,
                "account": r.account,
                "symbol": r.symbol,
                "size": r.size,
                "average_price": r.average_price,
                "unrealized": r.unrealized,
            });
            // The currency the row's figure is in, only off the dollar: a row of a node of one currency is
            // the row it always was.
            if !r.currency.is_empty() {
                row["ledger_currency"] = serde_json::Value::String(r.currency.clone());
            }
            row
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::Value::Array(arr))
        .expect("every field is a String/f64/Option; serialization is total")
}

// -------------------------------------------------------------------------------------------
// MOUNT (strategy registry) rows
// -------------------------------------------------------------------------------------------

/// One row of the rendered mount registry — `crate::cmd::trade::strategy`'s `ls` projection of
/// [`StrategyStatusWire::mounts`].
///
/// ⚠ **`asset_class: None` has THREE meanings, and this row does NOT resolve which — see
/// [`MountViewWire::asset_class`]'s own doc.** Two are indistinguishable by looking at the field
/// alone: a node too old to carry it at all, and a current node whose mount is still TOML-backed and
/// has genuinely named no product. Only `vike_tradehub_client::proto::FEATURE_MOUNT_CLASS` (a fact
/// about the ANSWERING NODE, carried nowhere on this row) tells them apart, which is why
/// [`mounts_table`] takes that capability as an explicit parameter rather than reading it off
/// `asset_class` — see that function's own doc for the defect this guards against.
///
/// ⚠ **[`mounts_table`] carries no aggregate.** A mount is addressed by venue+symbol+interval, and
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
    /// The cell the tables print ([`mount_mode`]).
    pub mode: String,
    /// `MountViewWire::route_key`; empty from a node that predates it, and then no key in the JSON.
    pub route_key: String,
    /// `MountViewWire::tier`.
    pub tier: Option<String>,
    /// `MountViewWire::not_running`. ⚠ Only `true` is a claim, like [`Self::latched`].
    pub not_running: bool,
    pub asset_class: Option<String>,
    pub params: String,
    /// The id `trade strategy reset-budget` takes (`MountViewWire::mount_id`; empty from a node that predates it).
    pub mount_id: String,
    /// The mount's budget latch is spent (`MountViewWire::latched`, decision 0239). ⚠ Only `true` is a claim: a node that
    /// predates `FEATURE_MOUNT_UNLATCH` writes no flag, so `false` is not "not latched" and the renderers print
    /// nothing for it.
    pub latched: bool,
}

/// The note a LATCHED mount's row ends with: its budget is breached and it is liquidate-only until the operator
/// clears it (decision 0239). EMPTY for every other row, so a node with no latched mount renders byte-identically to
/// before the flag existed.
pub(crate) fn latch_note(latched: bool, mount_id: &str) -> String {
    if latched {
        format!(
            "  [BUDGET LATCHED: liquidate-only; `vike-cli trade strategy reset-budget {mount_id}` clears it once the mount is flat]"
        )
    } else {
        String::new()
    }
}

/// The MODE cell of a mount: the tier its account runs at NOW and which account that is
/// (`demo (bybit#DEMO)`), `LIVE` uppercase because it is the one that matters. A demo and a live mount of one venue
/// used to read alike here, and a tier change did not show at all. A row that states no tier (a node from before the
/// field, a row derived from the identity block, no block yet) reads `?`, decision 0156's word for "the node did not
/// say": `live` is true for a demo account and false for a stopped one, so neither `LIVE` nor `paper` follows from it.
pub(crate) fn mount_mode(m: &MountViewWire) -> String {
    let word = match m.tier.as_deref() {
        _ if m.not_running => "NOT RUNNING",
        Some("live") => "LIVE",
        Some(tier) => tier,
        None => "?",
    };
    if m.route_key.is_empty() { word.to_string() } else { format!("{word} ({})", m.route_key) }
}

/// Project a [`StrategyStatusWire`]'s mounts into rows. PURE — the read itself is
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
pub(crate) fn mount_rows(status: &StrategyStatusWire) -> Vec<MountRow> {
    status.mounts.iter().map(mount_row).collect()
}

fn mount_row(m: &MountViewWire) -> MountRow {
    MountRow {
        strategy: m.strategy.clone(),
        venue: m.venue.clone(),
        symbol: m.symbol.clone(),
        interval: m.interval.clone(),
        live: m.live,
        mode: mount_mode(m),
        route_key: m.route_key.clone(),
        tier: m.tier.clone(),
        not_running: m.not_running,
        asset_class: m.asset_class.clone(),
        params: m.params.clone(),
        mount_id: m.mount_id.clone(),
        latched: m.latched,
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
/// [`MountViewWire::asset_class`]'s own doc names into one: "an old node that cannot carry the field"
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
    let w_strategy = col_width("STRATEGY", rows.iter().map(|r| r.strategy.as_str()));
    let w_venue = col_width("VENUE", rows.iter().map(|r| r.venue.as_str()));
    let w_symbol = col_width("SYMBOL", rows.iter().map(|r| r.symbol.as_str()));
    let w_interval = col_width("INTERVAL", rows.iter().map(|r| r.interval.as_str()));
    let w_mode = col_width("MODE", rows.iter().map(|r| r.mode.as_str()));

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
                "{:<w_strategy$} {:<w_venue$} {:<w_symbol$} {:<w_interval$} {:<w_mode$} {}{}",
                r.strategy,
                r.venue,
                r.symbol,
                r.interval,
                r.mode,
                r.params,
                latch_note(r.latched, &r.mount_id),
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
             {}{}",
            r.strategy,
            r.venue,
            r.symbol,
            r.interval,
            r.mode,
            product,
            r.params,
            latch_note(r.latched, &r.mount_id),
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
/// three-way ambiguity `MountViewWire::asset_class` warns about into one JSON value — and for a
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
            let mut row = serde_json::json!({
                "strategy": r.strategy,
                "venue": r.venue,
                "symbol": r.symbol,
                "interval": r.interval,
                "live": r.live,
                "asset_class": r.asset_class,
                "asset_class_known": knows_class,
                "params": r.params,
                "mount_id": r.mount_id,
            });
            // Only `true` is a claim (a node that predates the flag writes none, `MountRow::latched`), so the key
            // is written for a latched mount alone and a machine reader never mistakes silence for "not latched".
            if r.latched {
                row["latched"] = serde_json::Value::Bool(true);
            }
            // The same rule: a key only for what the node said.
            if !r.route_key.is_empty() {
                row["route_key"] = serde_json::Value::String(r.route_key.clone());
            }
            if let Some(tier) = &r.tier {
                row["tier"] = serde_json::Value::String(tier.clone());
            }
            if r.not_running {
                row["not_running"] = serde_json::Value::Bool(true);
            }
            row
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::Value::Array(arr))
        .expect("every field is a String/bool/Option; serialization is total")
}

#[path = "render_tests.rs"]
#[cfg(test)]
mod render_tests;
