//! The order client id, version 2 (`docs/decisions/0270-client-order-ids-carry-source-strategy-account-intent-and-group.md`):
//! an id that SAYS who made the order, for which strategy and account, whether it opens or closes,
//! which leg of which group it is and which replacement, in a form every venue that echoes the id
//! can carry, read back by ONE parser. Its public names are `order_client_id`'s (this module is
//! private); the generator it mints with is [`OrderClientIdGenerator`].
//!
//! # The shape
//!
//! ```text
//! [<tag>W] <src 1> <strat 4> [<acct 2>] <U> <intent 2> [<role 2-3>] [R<n>]
//! ```
//!
//! - `<tag>W`: the instance origin (decision 0044), carried by the SESSION string `[<tag>]W<base>`,
//!   so a replay reproduces it from the journal alone and no configuration can change a minted id.
//! - `src`: [`OrderClientIdSource`]. `strat`: [`OrderClientIdStrategyCode`]. `account`: [`OrderClientIdAccountCode`], dropped in the
//!   Binance compact shape ([`OrderClientIdShape::account`] `false`).
//! - `U`: the session's `base` plus the sequence number, rendered as [`OrderClientIdShape::u_width`]
//!   LOWER-case base36 digits: the low digits of `base + seq`, so consecutive ids differ in the last
//!   one and the values stay distinct while `seq < 36^width`.
//! - `intent`: [`OrderClientIdIntent`]. `role`: [`OrderClientIdLegRole`], on a leg of a group only. `R<n>`: replacement n
//!   of a cancel-then-new amend ([`next_generation`]).
//!
//! Every label is UPPER-case and `U` is lower-case, so two ids of one shape can never differ only by
//! case, and a venue that folds case can never merge two of them.
//!
//! # The HFT rule
//!
//! The shape is chosen ONCE per engine into a [`OrderClientIdPlan`], which pre-renders the account bytes and
//! picks a mint function monomorphized on the `U` width. A mint is that one call: a fixed sequence of
//! byte copies into a stack buffer and ONE allocation, no venue read, no table, no `format!`. The one
//! branch is the generator's own version (below), fixed for the life of the session.
//!
//! # What does not change
//!
//! A v1 session (every session minted before this module, and every one a journal resumes) mints
//! v1 bytes through every function here, ignoring the labels: [`OrderClientIdPlan::mint`] is then exactly
//! [`OrderClientIdGenerator::generate`], and a stem mints each leg whole. Only a v2 session (a
//! `W`-marked session string, [`OrderClientIdGenerator::new_v2`]) mints v2 ids, and the switch to one
//! is the core's assembly's, not this module's.

use super::{OrderClientIdGenerator, is_valid_crypto_order_client_id};
use crate::orders::client_id::origin::{
    MAX_ORIGIN_LEN, ORIGIN_MARK_V2, OrderClientIdOrigin, origin_of_order_client_id,
};
use crate::orders::order::{OrderListLeg, OrderType};

/// The `U` width of the full shape: 12 lower-case base36 digits, about 2^62 values. D8's fallback,
/// should the latency job need it, is THIS constant set to 10: the parser reads the full shape at
/// this width only, and the compact widths above it ([`U_WIDTHS`]) leave the parser with it, which
/// is what keeps a full id with an all-digit account code from reading as a compact one two digits
/// wider.
pub const FULL_U_WIDTH: u8 = 12;

/// Every `U` width a v2 id may have, widest first: the full one and the Binance compact trims
/// (12, 11, 10, and 7 on a tagged engine). The full shape uses [`FULL_U_WIDTH`] alone; a compact
/// shape any of these not wider than it.
pub const U_WIDTHS: [u8; 4] = [12, 11, 10, 7];

/// The highest replacement number, `R99`.
pub const MAX_GENERATION: u8 = 99;

/// The highest leg number of one role (`TP<n>`, `M<n>`): `1`–`9`, then `A`–`Z`.
pub const MAX_ROLE_NUMBER: u8 = 35;

const LOWER36: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
const UPPER36: &[u8; 36] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";

const fn pow36(w: u8) -> u64 {
    let mut p = 1u64;
    let mut i = 0;
    while i < w {
        p *= 36;
        i += 1;
    }
    p
}

/// A fresh v2 session's `base` is drawn below this, so `base + seq` stays below `36^12` (no wrap at
/// the full width) for every `seq < 2^40`.
pub(super) const V2_BASE_LIMIT: u64 = pow36(12) - (1 << 40);

/// Renders the low `width` base36 digits of `n` (that is `n mod 36^width`), zero-padded and
/// LOWER-case, right-to-left into `buffer`, returning just those digits. The [`super::write_decimal`]
/// twin behind the v2 mint: allocation-free, and a constant `width` (the mint passes one) unrolls it.
#[inline]
pub fn write_base36(n: u64, width: usize, buffer: &mut [u8; 12]) -> &[u8] {
    assert!(width <= buffer.len(), "a base36 render is at most 12 digits wide, asked {width}");
    let start = buffer.len() - width;
    let mut n = n;
    for slot in buffer[start..].iter_mut().rev() {
        *slot = LOWER36[(n % 36) as usize];
        n /= 36;
    }
    &buffer[start..]
}

/// The value of `digits` read as base36 in the alphabet `alphabet` (lower- or upper-case), `None`
/// on any byte outside it.
fn read_base36(digits: &[u8], upper: bool) -> Option<u64> {
    let mut v = 0u64;
    for &b in digits {
        let d = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'z' if !upper => b - b'a' + 10,
            b'A'..=b'Z' if upper => b - b'A' + 10,
            _ => return None,
        };
        v = v.checked_mul(36)?.checked_add(u64::from(d))?;
    }
    Some(v)
}

/// The v2 session string `[<tag>]W<base as 12 base36 digits>`: self-describing (a `W` mark means
/// v2), so the journal's `order_client_id_session` carries it with no schema change.
pub(super) fn v2_session(origin: Option<&OrderClientIdOrigin>, base: u64) -> String {
    let mut buffer = [0u8; 12];
    let digits = write_base36(base, 12, &mut buffer);
    let mut s = String::with_capacity(MAX_ORIGIN_LEN + 1 + 12);
    if let Some(o) = origin {
        s.push_str(o.as_str());
    }
    s.push(ORIGIN_MARK_V2);
    s.push_str(std::str::from_utf8(digits).expect("ASCII digits"));
    s
}

/// A v2 session string's `(base, tag prefix length)`, the prefix being `<tag>W` (0 when untagged:
/// an untagged v2 id carries no mark). `None` for anything else, which is a v1 session.
pub(super) fn decode_v2_session(session: &str) -> Option<(u64, usize)> {
    let b = session.as_bytes();
    let mark = b.iter().position(|&c| c == ORIGIN_MARK_V2 as u8)?;
    if mark > MAX_ORIGIN_LEN
        || !b[..mark].iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    {
        return None;
    }
    let body = &b[mark + 1..];
    if body.len() != 12 {
        return None;
    }
    let base = read_base36(body, false)?;
    Some((base, if mark == 0 { 0 } else { mark + 1 }))
}

/// Where a session should roll to a fresh v2 one at boot (decision 0270's D1): half of the smallest
/// `36^width` among the core's engine shapes, and at most 2^39. No shape means the full width.
/// [`OrderClientIdGenerator::due_to_roll`] compares a session's sequence with it.
pub fn order_client_id_roll_point(shapes: impl IntoIterator<Item = OrderClientIdShape>) -> u64 {
    let w = shapes.into_iter().map(|s| s.u_width).min().unwrap_or(FULL_U_WIDTH);
    (pow36(w) / 2).min(1 << 39)
}

/// Who made an order: a v2 id's first character (after an instance tag). Each letter is its word's first
/// (the owner's choice 2026-11-09, "Вариант 2"); `W` (the instance tag's mark) and `V` (the v1 tag's) are never one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrderClientIdSource {
    /// `G`: GUI order entry, every panel.
    Gui,
    /// `C`: the CLI.
    Cli,
    /// `T`: Telegram.
    Telegram,
    /// `M`: MCP (an AI agent).
    Mcp,
    /// `S`: a strategy.
    Strategy,
    /// `E`: the engine (the core) on its own: an operator's fired core-held stop, a margin-call flatten, a leg
    /// of a bracket an old client sent with no id.
    Engine,
}

impl OrderClientIdSource {
    /// Every source, in letter order of the decision record.
    pub const ALL: [OrderClientIdSource; 6] = [
        OrderClientIdSource::Gui,
        OrderClientIdSource::Cli,
        OrderClientIdSource::Telegram,
        OrderClientIdSource::Mcp,
        OrderClientIdSource::Strategy,
        OrderClientIdSource::Engine,
    ];

    /// The id's letter.
    pub const fn letter(self) -> u8 {
        match self {
            OrderClientIdSource::Gui => b'G',
            OrderClientIdSource::Cli => b'C',
            OrderClientIdSource::Telegram => b'T',
            OrderClientIdSource::Mcp => b'M',
            OrderClientIdSource::Strategy => b'S',
            OrderClientIdSource::Engine => b'E',
        }
    }

    /// The source a letter names.
    pub fn from_letter(letter: u8) -> Option<OrderClientIdSource> {
        OrderClientIdSource::ALL.into_iter().find(|s| s.letter() == letter)
    }

    /// The friendly words.
    pub fn words(self) -> &'static str {
        match self {
            OrderClientIdSource::Gui => "GUI (by hand)",
            OrderClientIdSource::Cli => "CLI (by hand)",
            OrderClientIdSource::Telegram => "Telegram (by hand)",
            OrderClientIdSource::Mcp => "MCP (AI agent)",
            OrderClientIdSource::Strategy => "strategy (auto)",
            OrderClientIdSource::Engine => "engine (the core itself)",
        }
    }
}

/// A strategy's 4-character code: base36 UPPER-case of its vike.db `strategy_code` row id
/// (1..=[`OrderClientIdStrategyCode::MAX_ID`], never reused). [`OrderClientIdStrategyCode::NONE`] (`0000`) means no strategy.
/// A label, never the ownership mechanism (decision 0113 stands).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct OrderClientIdStrategyCode([u8; 4]);

impl OrderClientIdStrategyCode {
    /// `0000`: no strategy.
    pub const NONE: OrderClientIdStrategyCode = OrderClientIdStrategyCode(*b"0000");
    /// `36^4 - 1`, `ZZZZ`: THE cap of the `strategy_code` table's row id (the one definition; the
    /// store's refusal at the cap reads it). An `i64` because that is the row id's type.
    pub const MAX_ID: i64 = 1_679_615;

    /// The code of row id `id`; `0` is [`OrderClientIdStrategyCode::NONE`], a negative id or one above
    /// [`OrderClientIdStrategyCode::MAX_ID`] is `None`.
    pub const fn new(id: i64) -> Option<OrderClientIdStrategyCode> {
        if !matches!(id, 0..=Self::MAX_ID) {
            return None;
        }
        let mut b = [0u8; 4];
        let mut n = id;
        let mut i = 4;
        while i > 0 {
            i -= 1;
            b[i] = UPPER36[(n % 36) as usize];
            n /= 36;
        }
        Some(OrderClientIdStrategyCode(b))
    }

    /// Four `[0-9A-Z]` characters.
    pub fn parse(code: &str) -> Option<OrderClientIdStrategyCode> {
        let b: [u8; 4] = code.as_bytes().try_into().ok()?;
        read_base36(&b, true)?;
        Some(OrderClientIdStrategyCode(b))
    }

    /// The row id it encodes (`0` for none).
    pub fn id(self) -> i64 {
        read_base36(&self.0, true).expect("a code is base36") as i64
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("ASCII")
    }
}

impl std::fmt::Display for OrderClientIdStrategyCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl TryFrom<String> for OrderClientIdStrategyCode {
    type Error = String;
    fn try_from(v: String) -> Result<Self, String> {
        OrderClientIdStrategyCode::parse(&v)
            .ok_or_else(|| format!("{v:?} is not a 4-character strategy code"))
    }
}

impl From<OrderClientIdStrategyCode> for String {
    fn from(v: OrderClientIdStrategyCode) -> String {
        v.as_str().to_string()
    }
}

/// An account's 2-character code: base36 UPPER-case of its vike.db `account.id` (1..=1,295,
/// `AUTOINCREMENT`, never reused). [`OrderClientIdAccountCode::NONE`] (`00`) means no account row, or an id the
/// two characters cannot hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct OrderClientIdAccountCode([u8; 2]);

impl OrderClientIdAccountCode {
    /// `00`: no account row.
    pub const NONE: OrderClientIdAccountCode = OrderClientIdAccountCode(*b"00");
    /// `36^2 - 1`, `ZZ`.
    pub const MAX_ID: i64 = 1_295;

    /// The code of `account.id`; anything outside 1..=[`OrderClientIdAccountCode::MAX_ID`] is
    /// [`OrderClientIdAccountCode::NONE`].
    pub const fn of_id(id: i64) -> OrderClientIdAccountCode {
        match id {
            1..=Self::MAX_ID => {
                OrderClientIdAccountCode([UPPER36[(id / 36) as usize], UPPER36[(id % 36) as usize]])
            }
            _ => Self::NONE,
        }
    }

    /// Two `[0-9A-Z]` characters.
    pub fn parse(code: &str) -> Option<OrderClientIdAccountCode> {
        let b: [u8; 2] = code.as_bytes().try_into().ok()?;
        read_base36(&b, true)?;
        Some(OrderClientIdAccountCode(b))
    }

    /// The account number it encodes, `None` for [`OrderClientIdAccountCode::NONE`].
    pub fn number(self) -> Option<u16> {
        let n = read_base36(&self.0, true).expect("a code is base36") as u16;
        (n != 0).then_some(n)
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("ASCII")
    }
}

impl std::fmt::Display for OrderClientIdAccountCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl TryFrom<String> for OrderClientIdAccountCode {
    type Error = String;
    fn try_from(v: String) -> Result<Self, String> {
        OrderClientIdAccountCode::parse(&v)
            .ok_or_else(|| format!("{v:?} is not a 2-character account code"))
    }
}

impl From<OrderClientIdAccountCode> for String {
    fn from(v: OrderClientIdAccountCode) -> String {
        v.as_str().to_string()
    }
}

/// What an order does to the position, as a LABEL that decides nothing: two characters after `U`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrderClientIdIntent {
    /// `OL`: a buy, not reduce-only, while not short. Every spot buy.
    OpenLong,
    /// `OS`: a sell, not reduce-only, while not long (margin only).
    OpenShort,
    /// `CL`: a reduce-only sell, or ANY spot sell: closes long and cannot open anything.
    CloseLong,
    /// `CS`: a reduce-only buy: closes short.
    CloseShort,
    /// `FL`: a PLAIN sell while long: closes the long and may open a short.
    PlainCloseLong,
    /// `FS`: a plain buy while short: closes the short and may open a long.
    PlainCloseShort,
}

impl OrderClientIdIntent {
    pub const ALL: [OrderClientIdIntent; 6] = [
        OrderClientIdIntent::OpenLong,
        OrderClientIdIntent::OpenShort,
        OrderClientIdIntent::CloseLong,
        OrderClientIdIntent::CloseShort,
        OrderClientIdIntent::PlainCloseLong,
        OrderClientIdIntent::PlainCloseShort,
    ];

    /// THE one rule (decision 0270's 4.6): `side` > 0 is a buy; `cash` is a spot symbol (margin is
    /// assumed when a caller cannot tell); `position` is the net position before the order. A spot
    /// id only ever carries `OL` or `CL`.
    pub fn of(side: i32, reduce_only: bool, cash: bool, position: f64) -> OrderClientIdIntent {
        let buy = side > 0;
        match (cash, reduce_only, buy) {
            (true, _, true) => OrderClientIdIntent::OpenLong,
            (true, _, false) | (false, true, false) => OrderClientIdIntent::CloseLong,
            (false, true, true) => OrderClientIdIntent::CloseShort,
            (false, false, true) if position < 0.0 => OrderClientIdIntent::PlainCloseShort,
            (false, false, true) => OrderClientIdIntent::OpenLong,
            (false, false, false) if position > 0.0 => OrderClientIdIntent::PlainCloseLong,
            (false, false, false) => OrderClientIdIntent::OpenShort,
        }
    }

    /// The two characters.
    pub const fn code(self) -> &'static str {
        match self {
            OrderClientIdIntent::OpenLong => "OL",
            OrderClientIdIntent::OpenShort => "OS",
            OrderClientIdIntent::CloseLong => "CL",
            OrderClientIdIntent::CloseShort => "CS",
            OrderClientIdIntent::PlainCloseLong => "FL",
            OrderClientIdIntent::PlainCloseShort => "FS",
        }
    }

    fn from_code(code: &[u8]) -> Option<OrderClientIdIntent> {
        OrderClientIdIntent::ALL.into_iter().find(|i| i.code().as_bytes() == code)
    }

    /// The friendly words.
    pub fn words(self) -> &'static str {
        match self {
            OrderClientIdIntent::OpenLong => "opens or adds long",
            OrderClientIdIntent::OpenShort => "opens or adds short",
            OrderClientIdIntent::CloseLong => "closes long (cannot open)",
            OrderClientIdIntent::CloseShort => "closes short (cannot open)",
            OrderClientIdIntent::PlainCloseLong => {
                "closes long with a plain order (may open a short)"
            }
            OrderClientIdIntent::PlainCloseShort => {
                "closes short with a plain order (may open a long)"
            }
        }
    }
}

/// A leg's role in its bracket or order list, after the intent: `EN`, `SL`, `TP<n>`, `M<n>`, with
/// `n` in 1..=[`MAX_ROLE_NUMBER`] rendered `1`–`9` then `A`–`Z`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrderClientIdLegRole {
    /// `EN`: the entry.
    Entry,
    /// `SL`: the stop.
    Stop,
    /// `TP<n>`: target n.
    Target(u8),
    /// `M<n>`: any other member.
    Member(u8),
}

impl OrderClientIdLegRole {
    /// A bracket's legs, in `build_bracket`'s order: entry, stop, take-profit.
    pub const BRACKET: [OrderClientIdLegRole; 3] =
        [OrderClientIdLegRole::Entry, OrderClientIdLegRole::Stop, OrderClientIdLegRole::Target(1)];

    /// The roles of an order list's legs, in leg order (decision 0270's 4.1): `EN` for the first leg
    /// another names as parent; `SL` for the first reduce-only stop-family leg (stop, stop-limit,
    /// trailing, trailing-limit); `TP<n>` for reduce-only limit and take-profit legs (the trailing
    /// take-profit included) in list order; `M<n>` for the rest. `None` when one numbered role would
    /// pass [`MAX_ROLE_NUMBER`]: the list then takes one plain id per leg, as before groups had stems.
    /// Only the FIRST parent and the first stop take the unnumbered roles, so no two legs share one.
    pub fn of(legs: &[OrderListLeg]) -> Option<Vec<OrderClientIdLegRole>> {
        let (mut entry, mut stop, mut targets, mut members) = (false, false, 0u8, 0u8);
        let mut roles = Vec::with_capacity(legs.len());
        for (i, leg) in legs.iter().enumerate() {
            let is_parent = legs.iter().any(|l| l.parent == Some(i));
            let stop_family = matches!(
                leg.order_type,
                OrderType::Stop
                    | OrderType::StopLimit
                    | OrderType::Trailing
                    | OrderType::TrailingLimit
            );
            let target_family = matches!(
                leg.order_type,
                OrderType::Limit | OrderType::TakeProfit | OrderType::TrailingTakeProfit
            );
            let role = if is_parent && !entry {
                entry = true;
                OrderClientIdLegRole::Entry
            } else if leg.reduce_only && stop_family && !stop {
                stop = true;
                OrderClientIdLegRole::Stop
            } else if leg.reduce_only && target_family {
                targets += 1;
                OrderClientIdLegRole::Target(targets)
            } else {
                members += 1;
                OrderClientIdLegRole::Member(members)
            };
            if targets > MAX_ROLE_NUMBER || members > MAX_ROLE_NUMBER {
                return None;
            }
            roles.push(role);
        }
        Some(roles)
    }

    fn put(self, b: &mut IdBuffer) {
        match self {
            OrderClientIdLegRole::Entry => b.put(b"EN"),
            OrderClientIdLegRole::Stop => b.put(b"SL"),
            OrderClientIdLegRole::Target(n) => b.put(&[b'T', b'P', role_digit(n)]),
            OrderClientIdLegRole::Member(n) => b.put(&[b'M', role_digit(n)]),
        }
    }

    /// The friendly words: `entry`, `stop`, `target 2`, `member 3`.
    pub fn words(self) -> String {
        match self {
            OrderClientIdLegRole::Entry => "entry".to_string(),
            OrderClientIdLegRole::Stop => "stop".to_string(),
            OrderClientIdLegRole::Target(n) => format!("target {n}"),
            OrderClientIdLegRole::Member(n) => format!("member {n}"),
        }
    }
}

fn role_digit(n: u8) -> u8 {
    assert!((1..=MAX_ROLE_NUMBER).contains(&n), "a leg number is 1..=35, got {n}");
    UPPER36[n as usize]
}

fn read_role_digit(b: u8) -> Option<u8> {
    let n = read_base36(&[b], true)? as u8;
    (n != 0).then_some(n)
}

/// The labels of ONE order, on the stack: what a mint renders around `U`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderClientIdLabels {
    pub src: OrderClientIdSource,
    pub strat: OrderClientIdStrategyCode,
    pub intent: OrderClientIdIntent,
    /// A leg's role; `None` for a plain order.
    pub role: Option<OrderClientIdLegRole>,
    /// The replacement number, `0` for none. A successor id comes from [`next_generation`]; a mint
    /// renders this only for completeness.
    pub generation: u8,
}

impl OrderClientIdLabels {
    /// The labels a site passes before its labels are wired: engine-made, no strategy, opens long.
    /// On a v1 session (every session today) no label is rendered, so it mints today's id exactly.
    pub const NEUTRAL: OrderClientIdLabels = OrderClientIdLabels::new(
        OrderClientIdSource::Engine,
        OrderClientIdStrategyCode::NONE,
        OrderClientIdIntent::OpenLong,
    );

    /// A plain order: no role, no replacement.
    pub const fn new(
        src: OrderClientIdSource,
        strat: OrderClientIdStrategyCode,
        intent: OrderClientIdIntent,
    ) -> OrderClientIdLabels {
        OrderClientIdLabels { src, strat, intent, role: None, generation: 0 }
    }
}

/// The shape an engine mints, chosen once (by `vike-venues` for each venue): plain data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct OrderClientIdShape {
    /// The 2-character account code is carried (`false` in the Binance compact shape).
    pub account: bool,
    /// The width of `U`: [`FULL_U_WIDTH`] with the account, one of [`U_WIDTHS`] not wider without.
    pub u_width: u8,
    /// The highest replacement number this engine's ids may carry (`99`, `9`, or `0` for none).
    pub max_gen: u8,
    /// The session's instance tag is stamped (`false`: dropped on THIS engine, whose budget has no
    /// room for it).
    pub tag: bool,
}

impl OrderClientIdShape {
    /// The full shape: every venue but Binance, and any venue nothing has fitted.
    pub const FULL: OrderClientIdShape = OrderClientIdShape {
        account: true,
        u_width: FULL_U_WIDTH,
        max_gen: MAX_GENERATION,
        tag: true,
    };

    /// **Whether [`OrderClientIdPlan::new`] takes this shape**: a full `U` exactly [`FULL_U_WIDTH`] wide, a compact one in
    /// [`U_WIDTHS`] and not wider, a replacement cap within [`MAX_GENERATION`]. The one spelling of that check: a
    /// client handed a shape by a node (the wire's `order_client_id_shape`) asks this before building a plan, and falls back to
    /// [`OrderClientIdShape::FULL`] on `false` instead of panicking.
    #[must_use]
    pub const fn is_mintable(self) -> bool {
        width_allowed(self.account, self.u_width) && self.max_gen <= MAX_GENERATION
    }
}

/// Whether a parsed or planned id may have a `U` this wide.
const fn width_allowed(account: bool, w: u8) -> bool {
    if account {
        return w == FULL_U_WIDTH;
    }
    let mut i = 0;
    while i < U_WIDTHS.len() {
        if U_WIDTHS[i] == w {
            return w <= FULL_U_WIDTH;
        }
        i += 1;
    }
    false
}

type MintFn = fn(&mut OrderClientIdGenerator, &OrderClientIdPlan, OrderClientIdLabels) -> String;
type StemFn = fn(
    &mut OrderClientIdGenerator,
    &OrderClientIdPlan,
    OrderClientIdSource,
    OrderClientIdStrategyCode,
) -> OrderClientIdStem;

/// One engine's minting plan (the HFT rule): its [`OrderClientIdShape`] and account code pre-rendered, and
/// the mint picked by `U` width, built once when the core is assembled or an engine appended.
#[derive(Debug, Clone, Copy)]
pub struct OrderClientIdPlan {
    shape: OrderClientIdShape,
    account: OrderClientIdAccountCode,
    /// `2` when the shape carries the account, `0` when not: a slice length, not a branch.
    account_len: usize,
    /// `usize::MAX` when the shape stamps the tag, `0` when not: a slice bound, not a branch.
    tag_keep: usize,
    mint_fn: MintFn,
    stem_fn: StemFn,
}

impl OrderClientIdPlan {
    /// The full shape with no account: what a site passes before its plan is wired. On a v1 session
    /// (every session today) it mints exactly [`OrderClientIdGenerator::generate`]'s id.
    pub const NEUTRAL: OrderClientIdPlan =
        OrderClientIdPlan::new(OrderClientIdShape::FULL, OrderClientIdAccountCode::NONE);

    /// The plan of an engine minting `shape` for `account`. Panics on a shape the parser cannot read
    /// back (a full width other than [`FULL_U_WIDTH`], a compact one outside [`U_WIDTHS`] or wider
    /// than it, a replacement cap past [`MAX_GENERATION`]): a programming error of the fit, met at
    /// assembly, never per order.
    pub const fn new(
        shape: OrderClientIdShape,
        account: OrderClientIdAccountCode,
    ) -> OrderClientIdPlan {
        assert!(
            shape.is_mintable(),
            "an order client id shape's U width is not one a v2 id may have, or its replacement cap is past R99"
        );
        let (mint_fn, stem_fn): (MintFn, StemFn) = match shape.u_width {
            12 => (mint_w::<12>, stem_w::<12>),
            11 => (mint_w::<11>, stem_w::<11>),
            10 => (mint_w::<10>, stem_w::<10>),
            7 => (mint_w::<7>, stem_w::<7>),
            _ => panic!("an order client id shape's U width is not one a v2 id may have"),
        };
        OrderClientIdPlan {
            shape,
            account,
            account_len: if shape.account { 2 } else { 0 },
            tag_keep: if shape.tag { usize::MAX } else { 0 },
            mint_fn,
            stem_fn,
        }
    }

    pub fn shape(&self) -> OrderClientIdShape {
        self.shape
    }

    pub fn account(&self) -> OrderClientIdAccountCode {
        self.account
    }

    /// One id, one sequence number.
    #[inline]
    pub fn mint(
        &self,
        order_client_ids: &mut OrderClientIdGenerator,
        order: OrderClientIdLabels,
    ) -> String {
        (self.mint_fn)(order_client_ids, self, order)
    }

    /// One group's stem, one sequence number: its legs are [`OrderClientIdStem::leg`]. On a v1 session it
    /// consumes nothing, and each leg mints a whole v1 id, as every group did before stems.
    #[inline]
    pub fn generate_stem(
        &self,
        order_client_ids: &mut OrderClientIdGenerator,
        src: OrderClientIdSource,
        strat: OrderClientIdStrategyCode,
    ) -> OrderClientIdStem {
        (self.stem_fn)(order_client_ids, self, src, strat)
    }
}

/// The ≤32 bytes of an id, on the stack, before its one allocation.
struct IdBuffer {
    b: [u8; 32],
    n: usize,
}

impl IdBuffer {
    #[inline]
    fn new() -> IdBuffer {
        IdBuffer { b: [0; 32], n: 0 }
    }

    #[inline]
    fn put(&mut self, s: &[u8]) {
        self.b[self.n..self.n + s.len()].copy_from_slice(s);
        self.n += s.len();
    }

    /// The id, checked against the venue charset as every mint is.
    #[inline]
    fn finish(&self) -> String {
        let id = std::str::from_utf8(&self.b[..self.n]).expect("ASCII");
        assert!(
            is_valid_crypto_order_client_id(id),
            "order_client_id violates crypto charset: {id:?}"
        );
        id.to_string()
    }
}

/// `[tag W] src strat [account] U`: the stem, through `U`.
#[inline]
fn put_stem<const W: usize>(
    b: &mut IdBuffer,
    order_client_ids: &mut OrderClientIdGenerator,
    plan: &OrderClientIdPlan,
    src: OrderClientIdSource,
    strat: OrderClientIdStrategyCode,
    base: u64,
    tag_len: usize,
) {
    let seq = order_client_ids.take_sequence();
    b.put(&order_client_ids.session_str().as_bytes()[..tag_len.min(plan.tag_keep)]);
    b.put(&[src.letter()]);
    b.put(&strat.0);
    b.put(&plan.account.0[..plan.account_len]);
    let mut u = [0u8; 12];
    b.put(write_base36(base + seq, W, &mut u));
}

fn put_generation(b: &mut IdBuffer, generation: u8) {
    if generation == 0 {
        return;
    }
    if generation >= 10 {
        b.put(&[b'R', b'0' + generation / 10, b'0' + generation % 10]);
    } else {
        b.put(&[b'R', b'0' + generation]);
    }
}

fn mint_w<const W: usize>(
    order_client_ids: &mut OrderClientIdGenerator,
    plan: &OrderClientIdPlan,
    o: OrderClientIdLabels,
) -> String {
    let Some((base, tag_len)) = order_client_ids.v2_state() else {
        return order_client_ids.generate();
    };
    let mut b = IdBuffer::new();
    put_stem::<W>(&mut b, order_client_ids, plan, o.src, o.strat, base, tag_len);
    b.put(o.intent.code().as_bytes());
    if let Some(role) = o.role {
        role.put(&mut b);
    }
    put_generation(&mut b, o.generation);
    b.finish()
}

fn stem_w<const W: usize>(
    order_client_ids: &mut OrderClientIdGenerator,
    plan: &OrderClientIdPlan,
    src: OrderClientIdSource,
    strat: OrderClientIdStrategyCode,
) -> OrderClientIdStem {
    let Some((base, tag_len)) = order_client_ids.v2_state() else { return OrderClientIdStem(None) };
    let mut b = IdBuffer::new();
    put_stem::<W>(&mut b, order_client_ids, plan, src, strat, base, tag_len);
    OrderClientIdStem(Some(std::str::from_utf8(&b.b[..b.n]).expect("ASCII").to_string()))
}

/// A group's shared head, `[tag W] src strat [account] U`: every leg is the stem plus its intent and
/// role, so a group sorts together and costs one sequence number. `None` inside is a v1 session's
/// stem, which renders nothing and mints each leg whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderClientIdStem(Option<String>);

impl OrderClientIdStem {
    /// A stem a client sent (a bracket's `order_client_id`): `None` unless it is a v2 stem exactly
    /// (tag, source, strategy, account when full, and `U`, nothing after it).
    pub fn given(stem: &str) -> Option<OrderClientIdStem> {
        let probe = format!("{stem}{}", OrderClientIdIntent::OpenLong.code());
        match parse_order_client_id(&probe)? {
            OrderClientIdReading::V2(p) if p.stem == stem && p.role.is_none() => {
                Some(OrderClientIdStem(Some(stem.to_string())))
            }
            _ => None,
        }
    }

    /// The stem's text; `None` for a v1 session's.
    pub fn as_str(&self) -> Option<&str> {
        self.0.as_deref()
    }

    /// One leg's id: the stem, `intent` and `role`. A v1 stem mints a whole v1 id from `order_client_ids`
    /// instead, one sequence number per leg in the order the legs are asked.
    pub fn leg(
        &self,
        order_client_ids: &mut OrderClientIdGenerator,
        intent: OrderClientIdIntent,
        role: OrderClientIdLegRole,
    ) -> String {
        let Some(stem) = &self.0 else { return order_client_ids.generate() };
        let mut b = IdBuffer::new();
        b.put(stem.as_bytes());
        b.put(intent.code().as_bytes());
        role.put(&mut b);
        b.finish()
    }
}

/// The successor of a cancel-then-new amend: `…` becomes `…R1`, `…R<n>` becomes `…R<n+1>`, the rest
/// of the id unchanged. `None` for an id that is not v2, or past `max_gen` (the engine shape's cap,
/// `0` on a no-`R` shape): the caller then mints a fresh id, and `OrderRecord::replaces` still links
/// the two.
pub fn next_generation(old: &str, max_gen: u8) -> Option<String> {
    let OrderClientIdReading::V2(p) = parse_order_client_id(old)? else { return None };
    let next = p.generation + 1;
    if next > max_gen.min(MAX_GENERATION) {
        return None;
    }
    let mut b = IdBuffer::new();
    b.put(p.chain.as_bytes());
    put_generation(&mut b, next);
    Some(b.finish())
}

/// What [`parse_order_client_id`] reads out of an id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderClientIdReading<'a> {
    /// An id minted before v2 (`deadbeef17`, `westVdeadbeef17`, `uideadbeef17`): its bytes and
    /// meaning are unchanged, and its instance tag, if any, is `origin`.
    V1 {
        origin: Option<&'a str>,
    },
    V2(OrderClientIdParts<'a>),
}

/// The fields of a v2 id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderClientIdParts<'a> {
    pub origin: Option<&'a str>,
    pub src: OrderClientIdSource,
    pub strat: OrderClientIdStrategyCode,
    /// `None` in the Binance compact shape, which carries no account.
    pub account: Option<OrderClientIdAccountCode>,
    /// The unique part `U`.
    pub unique: &'a str,
    pub intent: OrderClientIdIntent,
    pub role: Option<OrderClientIdLegRole>,
    /// `0` when not a replacement.
    pub generation: u8,
    /// The id through `U`, the tag included: two legs are in one group when both carry a role and
    /// their stems are equal.
    pub stem: &'a str,
    /// The id without its `R<n>`: an amend chain is the ids with one chain.
    pub chain: &'a str,
}

/// Read an id: [`OrderClientIdReading::V2`] for every v2 shape (full, its D8 width, Binance compact at every
/// width, tagged or not), [`OrderClientIdReading::V1`] for an alphanumeric id of at most 32 characters whose
/// first character is lower-case or a digit (every id minted before v2, the desktop's `ui…`, `dom…`,
/// `polymarket…` and `opt…` included), and `None` for anything else (`EXT-…`, a Hyperliquid `0x…` hash, a
/// `vkrp…` replace id, "my hedge"). The v2 shapes cannot overlap: see [`FULL_U_WIDTH`].
pub fn parse_order_client_id(id: &str) -> Option<OrderClientIdReading<'_>> {
    if let Some(p) = read_v2(id, true).or_else(|| read_v2(id, false)) {
        return Some(OrderClientIdReading::V2(p));
    }
    let first = *id.as_bytes().first()?;
    (is_valid_crypto_order_client_id(id) && (first.is_ascii_lowercase() || first.is_ascii_digit()))
        .then(|| OrderClientIdReading::V1 { origin: origin_of_order_client_id(id) })
}

/// How many v2 shapes read `id` (0 or 1 for every string; the property test asserts it).
#[cfg(any(test, feature = "test-support"))]
pub fn order_client_id_readings(id: &str) -> usize {
    usize::from(read_v2(id, true).is_some()) + usize::from(read_v2(id, false).is_some())
}

/// `id` read as the full shape (`account`) or the compact one.
fn read_v2(id: &str, account: bool) -> Option<OrderClientIdParts<'_>> {
    let b = id.as_bytes();
    let (origin, t) = match b.iter().position(|&c| c == ORIGIN_MARK_V2 as u8) {
        Some(w)
            if (1..=MAX_ORIGIN_LEN).contains(&w)
                && b[..w].iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()) =>
        {
            (Some(&id[..w]), w + 1)
        }
        _ => (None, 0),
    };
    let src = OrderClientIdSource::from_letter(*b.get(t)?)?;
    let strat = OrderClientIdStrategyCode::parse(id.get(t + 1..t + 5)?)?;
    let (account_name, h) = if account {
        (Some(OrderClientIdAccountCode::parse(id.get(t + 5..t + 7)?)?), t + 7)
    } else {
        (None, t + 5)
    };
    let u_len = b[h..].iter().take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit()).count();
    if !width_allowed(account, u8::try_from(u_len).ok()?) {
        return None;
    }
    let u_end = h + u_len;
    let intent = OrderClientIdIntent::from_code(b.get(u_end..u_end + 2)?)?;
    let mut i = u_end + 2;
    let role = match b.get(i..) {
        Some([b'E', b'N', ..]) => Some((OrderClientIdLegRole::Entry, 2)),
        Some([b'S', b'L', ..]) => Some((OrderClientIdLegRole::Stop, 2)),
        Some([b'T', b'P', d, ..]) => Some((OrderClientIdLegRole::Target(read_role_digit(*d)?), 3)),
        Some([b'M', d, ..]) => Some((OrderClientIdLegRole::Member(read_role_digit(*d)?), 2)),
        _ => None,
    }
    .map(|(role, len)| {
        i += len;
        role
    });
    let chain_end = i;
    let generation = match &b[i..] {
        [] => 0,
        [b'R', d @ b'1'..=b'9'] => d - b'0',
        [b'R', d @ b'1'..=b'9', e @ b'0'..=b'9'] => (d - b'0') * 10 + (e - b'0'),
        _ => return None,
    };
    Some(OrderClientIdParts {
        origin,
        src,
        strat,
        account: account_name,
        unique: &id[h..u_end],
        intent,
        role,
        generation,
        stem: &id[..u_end],
        chain: &id[..chain_end],
    })
}

/// The friendly words of an id ([`describe_order_client_id`]). Names are the CALLER's: it fills
/// [`OrderClientIdWords::account_label`] from the frame's account code and [`OrderClientIdWords::strategy_name`] from
/// the frame's or the local vike.db `strategy_code` row; `Display` renders the words joined by ` · `.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderClientIdWords {
    /// `true` for an id minted before v2, which says nothing more than its origin.
    pub v1: bool,
    pub origin: Option<String>,
    pub src: Option<OrderClientIdSource>,
    /// `None` for no strategy (`0000`) and for a v1 id.
    pub strat: Option<OrderClientIdStrategyCode>,
    pub strategy_name: Option<String>,
    /// `None` where the id carries no account (Binance compact, v1); `Some(NONE)` reads "no account".
    pub account: Option<OrderClientIdAccountCode>,
    pub account_label: Option<String>,
    pub intent: Option<OrderClientIdIntent>,
    pub role: Option<OrderClientIdLegRole>,
    /// The group's `U`, on a leg.
    pub group: Option<String>,
    pub generation: u8,
}

/// The words of a parsed id. Pure: no names (see [`OrderClientIdWords`]).
pub fn describe_order_client_id(parsed: &OrderClientIdReading<'_>) -> OrderClientIdWords {
    let mut w = OrderClientIdWords {
        v1: false,
        origin: None,
        src: None,
        strat: None,
        strategy_name: None,
        account: None,
        account_label: None,
        intent: None,
        role: None,
        group: None,
        generation: 0,
    };
    match parsed {
        OrderClientIdReading::V1 { origin } => {
            w.v1 = true;
            w.origin = origin.map(str::to_string);
        }
        OrderClientIdReading::V2(p) => {
            w.origin = p.origin.map(str::to_string);
            w.src = Some(p.src);
            w.strat = (p.strat != OrderClientIdStrategyCode::NONE).then_some(p.strat);
            w.account = p.account;
            w.intent = Some(p.intent);
            w.role = p.role;
            w.group = p.role.map(|_| p.unique.to_string());
            w.generation = p.generation;
        }
    }
    w
}

impl std::fmt::Display for OrderClientIdWords {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut words: Vec<String> = Vec::new();
        if self.v1 {
            words.push("minted before v2".to_string());
        }
        if let Some(src) = self.src {
            words.push(src.words().to_string());
            words.push(match (self.strat, &self.strategy_name) {
                (None, _) => "no strategy".to_string(),
                (Some(code), None) => format!("strategy {code}"),
                (Some(code), Some(name)) => format!("strategy {code} {name}"),
            });
        }
        if let Some(account) = self.account {
            words.push(match (account.number(), &self.account_label) {
                (None, _) => "no account".to_string(),
                (Some(n), None) => format!("account #{n}"),
                (Some(n), Some(label)) => format!("account #{n} {label}"),
            });
        }
        if let Some(intent) = self.intent {
            words.push(intent.words().to_string());
        }
        if let (Some(role), Some(group)) = (self.role, &self.group) {
            words.push(format!("{} of group {group}", role.words()));
        }
        if self.generation > 0 {
            words.push(format!("replacement {}", self.generation));
        }
        if let Some(origin) = &self.origin {
            words.push(format!("instance {origin}"));
        }
        f.write_str(&words.join(" · "))
    }
}

#[cfg(test)]
#[path = "format_tests.rs"]
mod tests;
