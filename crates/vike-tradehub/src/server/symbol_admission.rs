//! `server::symbol_admission` — the control door lets an order name ANY symbol of its exchange: it
//! spells the symbol the venue's way, resolves the symbol's facts, and hands the engine an
//! `AdmitSymbol` ahead of the order (decision 0173; owner ruling 2026-10-03/04: an ACCOUNT trades
//! any symbol of its exchange at any time, no symbol glued to an account or a mount).
//!
//! # The four classes of venue, one explicit table
//!
//! [`SYMBOL_RULES`] says, per roster venue, what its adapter does with an order's symbol (read in
//! each adapter's order builder):
//!
//! * **[`SymbolRule::Admits`]: bybit, binance, aster, okx, hyperliquid, deribit, alpaca.** The adapter signs each order
//!   on the ORDER's symbol, on that symbol's own grid (`crates/bridges/binance/src/per_symbol.rs`'s
//!   `PerSymbolRest`; okx's also on that instrument's own contract size; hyperliquid's through its loaded
//!   `Symbology`, which keys the asset index by the order's coin; deribit's through one client that
//!   resolves each instrument's row on first use), so the door admits the symbol into the engine first.
//!   **deribit admits PERPETUALS of the engine's own money model only** ([`Spelling::PerpetualName`],
//!   `money_model_refusal`): an engine counts money one way, so an inverse engine (an amount of US
//!   dollars) takes other inverse perpetuals and a linear one (an amount of the base coin) other linear
//!   ones; `docs/ops/deribit-any-symbol-plan.md` says why and what mixing would take. **alpaca admits
//!   equities and crypto pairs** ([`Spelling::AssetSymbol`]): every instrument is a USD-quoted share or coin
//!   with a multiplier of 1, so the engine's `qty x price` is the real USD exposure whatever the symbol;
//!   its facts are read with the account's own credentials (`docs/ops/any-symbol-remaining-venues-plan.md`).
//! * **[`SymbolRule::MountedOnly`]: no roster venue today.** An adapter that signs EVERY order on the
//!   symbol its engine was mounted on, whatever the order names, has this row: the door REFUSES an
//!   order on any other symbol, with the row's reason, until that adapter is finished. Deleting a row
//!   here is the whole change that opens a venue, once its adapter reads the order's symbol (deribit's
//!   row was the last one).
//! * **[`SymbolRule::PerOrder`]: oanda, polymarket.** Its adapter already reads the
//!   order's own symbol and the engine judges it well enough; the door says nothing about WHICH symbol,
//!   with two fences for a symbol other than the engine's own (see `per_order_refusal`): a paper
//!   account is refused (one book, fills stamped with the mounted symbol), and an opening order with no
//!   limit price is refused (no price watcher exists for these venues, so the engine would judge it at
//!   notional 0). A reduce-only order is not touched, and polymarket (every token is judged) is exempt.
//! * **[`SymbolRule::PerOrderUnsafe`]: ctrader, ig, fxcm, ibkr, dukascopy.** The adapter reads
//!   the order's symbol but the ENGINE cannot judge a foreign one (it rounds on the mounted symbol's
//!   grid, knows no multiplier or has no price): the door REFUSES a foreign symbol with the row's reason
//!   until admission supports the venue (an interim fence; deleting the row opens it).
//!
//! # What the door does for an `Admits` venue ([`plan_submit_symbol`])
//!
//! 1. The engine's OWN symbol, spelled as the engine spells it, passes UNTOUCHED: no extra command,
//!    byte-identical. A declared leg or an earlier admission (this run's, or a restored one) passes the
//!    spelling and admission steps but still asks the price lane (step 6) on a `demo`/`live` engine.
//! 2. Otherwise the symbol is CANONICALISED to the venue's spelling: upper case; binance and aster
//!    keep the `.P` lane suffix (`vike_catalog::split_perp`), bybit's exec plane is linear perpetuals
//!    only and its engine symbols are bare, so a `.P` is dropped; okx takes its own SWAP instId
//!    (`ETH-USDT-SWAP`) and nothing else ([`canonical_swap_instid`]: a spot, dated or option id is
//!    refused by name, `BTCUSDT.P` is not translated); hyperliquid's coin names keep their case
//!    (`kPEPE`) and have no lane (a spot pair, `@N` and a builder-dex `dex:COIN` are refused by name). A
//!    symbol with a character no instrument has is refused (it reaches a signed query string downstream).
//! 3. Only a `demo` or `live` engine takes a foreign symbol: a paper engine is one book.
//! 4. A binance or aster engine is ONE lane (spot or USDⓈ-M perp, by its mounted symbol): a symbol of
//!    the other lane is refused here, by name, instead of by a late `OrderRejected` (only the mounted
//!    lane has a user-data stream, so its fills would never arrive).
//! 5. The facts come from the [`SymbolFacts`] the surface was given, off the fold thread. Nothing is
//!    ever guessed: a symbol the venue does not list, a failed lookup, a grid without a tick or a lot,
//!    a surface with no source and a 17th extra symbol are each a refusal naming the cause.
//! 6. An OPENING order on a symbol other than the engine's own needs a PRICE (the collar, the exposure
//!    caps and the account margin are judged at the mark). Asking [`SymbolFacts::price`] subscribes the
//!    symbol's quote feed the first time; the door holds the order up to [`PRICE_WAIT`] for the first
//!    price and refuses it, naming the cause, if none arrives. A reduce-only order is never held up.
//!
//! A LABELLED account (`bybit#DEMO`) is admitted a symbol exactly like the default one, under its own
//! route key: the core fans a venue's quote, trade and book out to every engine admitted the symbol
//! (`vike-core`'s `CoreThread::admitted_fanout`), where it used to reach the default engine only, so
//! the door no longer refuses it by name.
//!
//! The verdict is a [`SymbolPlan`]: `accept_command` sends the admission, then the order, on the one
//! command lane; the Preview surfaces call [`submit_symbol_refusal`] and see the same refusal.

use vike_exec::{MAX_EXTRA_SYMBOLS, SymbolAdmission, VenueBlock};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::{AccountLabel, parse_wire_account, route_key_of};
use vike_tradehub_client::wire::WireCommand;

/// Where a surface gets the FACTS of a symbol its engine was not mounted on. Blocking (a lookup may
/// cross the network), so it is called on a connection thread, never on the core's fold; a
/// production source caches.
pub trait SymbolFacts: Send + Sync {
    /// The admission that puts `symbol` (already canonical for `venue`) into the engine addressed by
    /// `route_key`: its `grid` from the venue's own instrument row and its contract value (`None` for
    /// the default). `Err` is the reason, in words an operator reads verbatim: an unknown symbol, an
    /// unreachable venue, an adapter that answers nothing.
    fn admission(
        &self,
        route_key: &str,
        venue: &str,
        symbol: &str,
    ) -> Result<SymbolAdmission, String>;

    /// The latest price this node holds for `symbol`. With `subscribe`, the first ask starts the
    /// subscription that delivers one (an admitted symbol has no feed until then); WITHOUT it the ask
    /// is a read and starts nothing (the Preview surfaces: a control peer must not be able to open
    /// price subscriptions by previewing). `Ok(None)` is "none yet"; `Err` is the reason no price can
    /// come. Never holds a lock across a network call, so a slow lookup does not queue other orders.
    fn price(
        &self,
        route_key: &str,
        venue: &str,
        symbol: &str,
        subscribe: bool,
    ) -> Result<Option<f64>, String>;

    /// The door sent `AdmitSymbol` for `symbol` (or the boot did): the source may now keep its price
    /// subscription for good and refresh its facts later ([`Self::refresh`]).
    fn admitted(&self, _route_key: &str, _venue: &str, _symbol: &str) {}

    /// The door gave up on `symbol` BEFORE admitting it (no price came): drop the price subscription
    /// [`Self::price`] opened for it. A no-op for a symbol that was admitted meanwhile.
    fn release(&self, _route_key: &str, _venue: &str, _symbol: &str) {}

    /// A FRESH admission for a symbol this source admitted earlier, when what it holds is older than
    /// its refresh interval (a venue changes a tick or lot rarely, and announces it): the door sends
    /// it as an ordinary `AdmitSymbol` refresh ahead of the order. `None` when nothing is due or the
    /// lookup failed (the engine keeps the grid it has).
    fn refresh(&self, _route_key: &str, _venue: &str, _symbol: &str) -> Option<SymbolAdmission> {
        None
    }

    /// Whether the door may hold an order for a price ([`await_price`]). A surface that must not
    /// block on a slow lookup (the Telegram poller runs the whole acceptance inline, and a /panic
    /// queues behind it) answers `false` and gets a prompt "no price yet, retry" instead.
    fn waits(&self) -> bool {
        true
    }

    /// Whether the account `route_key` runs with an ACCOUNT-wide exposure ceiling armed
    /// (`policy.max_account_exposure`, or the account's own `max_exposure` row). `max_total_exposure`
    /// bounds ONE symbol, so a live account holding seventeen would carry seventeen times it; the door
    /// admits a symbol into a `live` account only when this is `true`. The default answers `true`: a
    /// source that does not know does not block (the production source answers from the settings).
    fn exposure_cap_armed(&self, _route_key: &str) -> bool {
        true
    }

    /// The account's ENGINE GENERATION: moves whenever the node starts or stops the account's engine.
    /// Work done on an engine's behalf later than the engine's own start (the boot's retry of the
    /// symbols it missed) captures it and ends when it moves. A source that never sees an engine
    /// change answers a constant. REQUIRED, no default: an implementor that forgot it would silently
    /// switch the retry's guard off.
    fn generation(&self, route_key: &str) -> u64;

    /// **Tell the daemon again that this node admitted the symbol** and SAY whether it took: what
    /// [`Self::admitted`] does for the daemon, with the commit's outcome returned instead of warned
    /// about. Idempotent, and it sends the engine nothing. The boot's retry calls it for a symbol whose
    /// admission was sent but whose commit may have stalled, and counts the symbol healed only on `Ok`.
    /// The default is a source with no daemon to tell.
    fn tell_daemon(&self, route_key: &str, venue: &str, symbol: &str) -> Result<(), String> {
        self.admitted(route_key, venue, symbol);
        Ok(())
    }
}

/// A [`SymbolFacts`] that never makes its caller wait for a price: everything else is the wrapped
/// source's. The Telegram channel's source.
pub struct FailFast(pub std::sync::Arc<dyn SymbolFacts>);

impl SymbolFacts for FailFast {
    fn admission(
        &self,
        route_key: &str,
        venue: &str,
        symbol: &str,
    ) -> Result<SymbolAdmission, String> {
        self.0.admission(route_key, venue, symbol)
    }
    fn price(
        &self,
        route_key: &str,
        venue: &str,
        symbol: &str,
        subscribe: bool,
    ) -> Result<Option<f64>, String> {
        self.0.price(route_key, venue, symbol, subscribe)
    }
    fn admitted(&self, route_key: &str, venue: &str, symbol: &str) {
        self.0.admitted(route_key, venue, symbol);
    }
    fn release(&self, route_key: &str, venue: &str, symbol: &str) {
        self.0.release(route_key, venue, symbol);
    }
    fn refresh(&self, route_key: &str, venue: &str, symbol: &str) -> Option<SymbolAdmission> {
        self.0.refresh(route_key, venue, symbol)
    }
    fn waits(&self) -> bool {
        false
    }
    fn exposure_cap_armed(&self, route_key: &str) -> bool {
        self.0.exposure_cap_armed(route_key)
    }
    fn generation(&self, route_key: &str) -> u64 {
        self.0.generation(route_key)
    }
    fn tell_daemon(&self, route_key: &str, venue: &str, symbol: &str) -> Result<(), String> {
        self.0.tell_daemon(route_key, venue, symbol)
    }
}

/// How an `Admits` venue spells a symbol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Spelling {
    /// Upper case with the `.P` lane suffix kept (binance, aster): `ETHUSDT` spot, `ETHUSDT.P` perp.
    LaneSuffixed,
    /// Upper case, no suffix (bybit's linear-only exec plane): `ETHUSDT`.
    BareLinear,
    /// The venue's own SWAP instId, upper case, nothing translated (okx): `ETH-USDT-SWAP`. See
    /// [`canonical_swap_instid`].
    Hyphenated,
    /// The name as the venue lists it, case kept, no lane suffix (hyperliquid's core perps): `BTC`,
    /// `kPEPE`. `kPEPE` and `KPEPE` are different spellings, so nothing is upper-cased.
    CaseKept,
    /// The venue's own PERPETUAL name, upper case, nothing translated (deribit): `ETH-PERPETUAL` (the
    /// coin-margined book) or `ETH_USDC-PERPETUAL` (the linear one). See `canonical_perpetual_name`.
    PerpetualName,
    /// The venue's own asset symbol, upper case, nothing translated (alpaca): an equity ticker `AAPL` /
    /// `BRK.B`, or a crypto pair WITH its slash `BTC/USD`. See `canonical_alpaca_symbol`.
    AssetSymbol,
}

/// What a venue's adapter does with the symbol an order names. See the module doc.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SymbolRule {
    /// Signs on the order's symbol: the door admits it.
    Admits(Spelling),
    /// Signs on the engine's mounted symbol: the door refuses any other, for this reason. ⚠ No roster
    /// venue has this row today (deribit's was the last): the variant is kept because
    /// `just new-venue`'s classification hint offers it to the next adapter.
    #[allow(dead_code, clippy::allow_attributes)]
    MountedOnly(&'static str),
    /// Reads the order's own symbol already; not this gate's.
    PerOrder,
    /// Reads the order's own symbol, but the ENGINE cannot judge a symbol it was not started on (its
    /// grid, multiplier or price are the mounted symbol's): the door refuses a foreign symbol, for this
    /// reason, until admission supports the venue. Deleting the row (to `PerOrder` or `Admits`) opens it.
    PerOrderUnsafe(&'static str),
}

/// Why `PerOrderUnsafe` refuses a foreign symbol, shared by the venues with the same cause.
const ROUNDS_ON_MOUNTED_GRID: &str =
    "the engine rounds the order on the mounted symbol's grid until admission supports this venue";
const NO_PRICE_SOURCE: &str =
    "this venue has no price source for a symbol the engine was not started on";
const UNKNOWN_CONTRACT: &str =
    "the multiplier and lot of a foreign instrument are unknown to the engine";

/// The rule of every roster venue. `every_roster_venue_has_a_symbol_rule` fails on a venue in none,
/// so a new venue is classified by whoever reads its order builder.
// vike:new-venue:note do NOT add a row here by hand-copying one: read the new venue's ORDER BUILDER (does it sign on the order's symbol or on the one its client was bound to?), then add `("{venue}", SymbolRule::PerOrder)` if it reads the order's symbol AND the engine judges a foreign symbol soundly (grid, multiplier, a price source), `SymbolRule::PerOrderUnsafe("<why>")` if it reads the symbol but the engine cannot, or `SymbolRule::MountedOnly("<why>")` if it does not. `Admits` is only for a venue whose adapter has a per-symbol router AND whose `TradeLane::symbol_facts` answers (the gatewayhub), and it also needs a `Spelling`.
pub(super) const SYMBOL_RULES: &[(&str, SymbolRule)] = &[
    ("binance", SymbolRule::Admits(Spelling::LaneSuffixed)),
    ("aster", SymbolRule::Admits(Spelling::LaneSuffixed)),
    ("bybit", SymbolRule::Admits(Spelling::BareLinear)),
    ("okx", SymbolRule::Admits(Spelling::Hyphenated)),
    ("deribit", SymbolRule::Admits(Spelling::PerpetualName)),
    ("hyperliquid", SymbolRule::Admits(Spelling::CaseKept)),
    ("alpaca", SymbolRule::Admits(Spelling::AssetSymbol)),
    ("ctrader", SymbolRule::PerOrderUnsafe(ROUNDS_ON_MOUNTED_GRID)),
    ("ig", SymbolRule::PerOrderUnsafe(NO_PRICE_SOURCE)),
    ("oanda", SymbolRule::PerOrder),
    ("fxcm", SymbolRule::PerOrderUnsafe(NO_PRICE_SOURCE)),
    ("ibkr", SymbolRule::PerOrderUnsafe(UNKNOWN_CONTRACT)),
    ("polymarket", SymbolRule::PerOrder),
    ("dukascopy", SymbolRule::PerOrderUnsafe(NO_PRICE_SOURCE)),
];

/// The venues the door admits symbols for (the `Admits` rows): the root hands them to the mount as
/// `AdapterPolicy::fail_closed_symbol_venues`, so exactly these engines deny a symbol they were never
/// told about.
pub fn admitting_venues() -> Vec<String> {
    SYMBOL_RULES
        .iter()
        .filter(|(_, rule)| matches!(rule, SymbolRule::Admits(_)))
        .map(|(venue, _)| (*venue).to_string())
        .collect()
}

fn rule_of(venue: &str) -> Option<SymbolRule> {
    SYMBOL_RULES.iter().find(|(v, _)| *v == venue).map(|(_, rule)| *rule)
}

/// The door's verdict on a plain Submit's symbol.
#[derive(Debug, PartialEq)]
pub(crate) enum SymbolPlan {
    /// Not this gate's, or already placeable exactly as written: send the order untouched.
    Pass,
    /// Refused, with the words the caller gets.
    Refuse(String),
    /// Send the order on `symbol` (the canonical spelling); `admit`, when present, goes first; `wait`,
    /// when present, says the symbol has no price yet and the order is held for one.
    Place { symbol: String, admit: Option<Box<SymbolAdmission>>, wait: Option<PriceWait> },
}

/// Where to ask for the price an opening order is waiting on.
#[derive(Debug, PartialEq)]
pub(crate) struct PriceWait {
    pub(crate) route_key: String,
    pub(crate) venue: String,
}

/// The engine's trade list as the refusal prints it.
fn trades_text(engine: &VenueBlock) -> String {
    std::iter::once(&engine.symbol)
        .chain(engine.extra_symbols.iter())
        .map(|s| format!("`{s}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The canonical spelling of `raw` for `spelling`, or why it is not a symbol.
pub(super) fn canonical_symbol(spelling: Spelling, raw: &str) -> Result<String, String> {
    if spelling == Spelling::Hyphenated {
        return canonical_swap_instid(raw);
    }
    if spelling == Spelling::PerpetualName {
        return canonical_perpetual_name(raw);
    }
    if spelling == Spelling::AssetSymbol {
        return canonical_asset_symbol(raw);
    }
    let upper = raw.trim().to_ascii_uppercase();
    // A case-kept venue has no lane suffix either: `BTC.P` stays a refused name, never `BTC`.
    let (base, perp) = if spelling == Spelling::CaseKept {
        (raw.trim(), false)
    } else {
        vike_catalog::split_perp(&upper)
    };
    if base.is_empty() || base.len() > 32 || !base.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(format!(
            "`{raw}` is not an exchange symbol (letters and digits, e.g. `ETHUSDT`)"
        ));
    }
    Ok(match spelling {
        Spelling::LaneSuffixed if perp => format!("{base}{}", vike_catalog::PERP_SUFFIX),
        Spelling::LaneSuffixed
        | Spelling::BareLinear
        | Spelling::Hyphenated
        | Spelling::PerpetualName
        | Spelling::AssetSymbol
        | Spelling::CaseKept => base.to_string(),
    })
}

/// **alpaca's symbol is the venue's own asset symbol**, upper-cased, and nothing is translated: an equity
/// ticker (`AAPL`, with a share-class dot, `BRK.B`) or a crypto pair WITH its slash (`BTC/USD`). A bare
/// `BTCUSD` is NOT turned into `BTC/USD` here (the adapter does that on the wire, but guessing a pair
/// from letters at the door could sign a different instrument than the operator meant): it is read as
/// the ticker it looks like and the venue's list refuses it by name. Letters and digits, one inner `.` or
/// one inner `/`, nothing else: the symbol reaches a query string and a JSON body downstream.
fn canonical_asset_symbol(raw: &str) -> Result<String, String> {
    let upper = raw.trim().to_ascii_uppercase();
    let word =
        |p: &str| !p.is_empty() && p.len() <= 10 && p.bytes().all(|b| b.is_ascii_alphanumeric());
    let hint = "an alpaca symbol is an equity ticker such as `AAPL` (or `BRK.B`), or a crypto pair \
                written with its slash such as `BTC/USD`";
    let ok = match upper.split_once(['.', '/']) {
        None => word(&upper),
        Some((a, b)) => word(a) && word(b),
    };
    if ok { Ok(upper) } else { Err(format!("`{raw}` is not an alpaca symbol ({hint})")) }
}

/// Whether two alpaca spellings name ONE instrument: `BTCUSD` and `BTC/USD` do (the adapter slashes a bare
/// crypto pair on the wire), so an engine trading one must not be admitted the other: two symbols in the
/// engine would book one venue position twice.
fn same_alpaca_instrument(a: &str, b: &str) -> bool {
    let bare = |s: &str| s.replace('/', "").to_ascii_uppercase();
    bare(a) == bare(b)
}

/// **deribit's symbol is the venue's own PERPETUAL name**, upper-cased, and nothing is translated:
/// `BASE-PERPETUAL` (`ETH-PERPETUAL`, the coin-margined book) or `BASE_QUOTE-PERPETUAL`
/// (`ETH_USDC-PERPETUAL`, the linear one). What else a trader may type is refused here BY NAME, before
/// any lookup: a dated future (`BTC-27MAR26`), an option (`BTC-27MAR26-60000-C`), a spot pair
/// (`BTC_USDC`) and anything else. Letters, digits, `-` and `_` only: the symbol reaches a JSON-RPC
/// frame downstream.
fn canonical_perpetual_name(raw: &str) -> Result<String, String> {
    let upper = raw.trim().to_ascii_uppercase();
    let hint = "a deribit perpetual is `BASE-PERPETUAL` or `BASE_QUOTE-PERPETUAL`, e.g. \
                `ETH-PERPETUAL`";
    let word =
        |p: &str| !p.is_empty() && p.len() <= 16 && p.bytes().all(|b| b.is_ascii_alphanumeric());
    if upper.len() > 40
        || !upper.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(format!("`{raw}` is not a deribit instrument name ({hint})"));
    }
    let parts: Vec<&str> = upper.split('-').collect();
    match parts.as_slice() {
        [base, "PERPETUAL"] if base.split('_').count() <= 2 && base.split('_').all(word) => {
            Ok(upper.clone())
        }
        [_, _, _, "C" | "P"] => {
            Err(format!("`{raw}` is an option and this adapter trades perpetuals only ({hint})"))
        }
        [_, tail] if tail.bytes().next().is_some_and(|b| b.is_ascii_digit()) => Err(format!(
            "`{raw}` is a dated future and this adapter trades perpetuals only ({hint})"
        )),
        [_] => Err(format!(
            "`{raw}` is a spot pair or a bare symbol and this adapter trades perpetuals only ({hint})"
        )),
        _ => Err(format!("`{raw}` is not a deribit perpetual ({hint})")),
    }
}

/// Why a deribit engine cannot hold the instrument `admission` describes, or `None`. An engine counts
/// money ONE way (decision 0170 and `docs/ops/deribit-any-symbol-plan.md`): a perpetual whose amount is
/// US dollars (inverse) and one whose amount is the base coin (linear) are valued, margined and
/// settled differently, and the engine's ledger is one currency on one balance mode. So the
/// instruments an engine may add are the ones of ITS kind, and an engine mounted on anything but a
/// perpetual (a dated future, an option) adds none. The adapter checks the same rule again, and a
/// linear instrument's settlement currency as well, which the door cannot see.
fn money_model_refusal(engine: &VenueBlock, admission: &SymbolAdmission) -> Option<String> {
    if !engine.symbol.ends_with("-PERPETUAL") {
        return Some(format!(
            "this engine is mounted on `{}`, which is not a perpetual, and only an engine mounted on \
             a perpetual adds another instrument. Use an account mounted on a perpetual",
            engine.symbol
        ));
    }
    use vike_model::NotionalKind::{Inverse, Linear};
    let mounted = engine.contract_value_of(&engine.symbol).kind;
    let theirs = admission.contract_value.unwrap_or_default().kind;
    let word = |k| match k {
        Inverse => "INVERSE (its amount counts US dollars)",
        Linear => "LINEAR (its amount counts the base coin)",
    };
    (mounted != theirs).then(|| {
        format!(
            "`{}` is {} but this engine is mounted on `{}`, which is {}, and one account trades one \
             money model. Use an account mounted on a {} perpetual",
            admission.symbol,
            word(theirs),
            engine.symbol,
            word(mounted),
            if theirs == Inverse { "inverse" } else { "linear" },
        )
    })
}

/// Whether a hyperliquid symbol's TEXT is a core perpetual's name: not a spot pair (`BASE/QUOTE`, `@N`)
/// and not a builder-dex coin (`dex:COIN`). The gatewayhub's facts source reads the same three marks.
fn is_core_perp_name(symbol: &str) -> bool {
    !(symbol.contains('/') || symbol.starts_with('@') || symbol.contains(':'))
}

/// **okx's symbol is its own SWAP instId, `BASE-QUOTE-SWAP`, upper-cased, and nothing is translated**
/// (`BTCUSDT` and `BTCUSDT.P` are binance's names, not okx's: guessing a pair from them would sign a
/// different instrument than the operator meant). This adapter trades the SWAP lane only, so what
/// else a trader may type is refused here BY NAME, before any lookup: a spot id (`BTC-USDT`), a dated
/// future (`BTC-USDT-250926`), an option (`BTC-USD-250926-60000-C`), anything else. Letters and digits
/// between hyphens only: the symbol reaches a signed query string downstream.
fn canonical_swap_instid(raw: &str) -> Result<String, String> {
    let upper = raw.trim().to_ascii_uppercase();
    let parts: Vec<&str> = upper.split('-').collect();
    let hint = "an okx SWAP instrument id is `BASE-QUOTE-SWAP`, e.g. `ETH-USDT-SWAP`";
    let word =
        |p: &&str| !p.is_empty() && p.len() <= 16 && p.bytes().all(|b| b.is_ascii_alphanumeric());
    if upper.len() > 48 || !parts.iter().all(word) {
        return Err(format!(
            "`{raw}` is not an okx SWAP instrument id ({hint}; okx's names are its own, so \
             `BTCUSDT` and `BTCUSDT.P` are not translated)"
        ));
    }
    match parts.as_slice() {
        [_, _, "SWAP"] => Ok(upper),
        [_, _] => Err(format!(
            "`{raw}` is a spot instrument id and this adapter trades SWAP perpetuals only ({hint})"
        )),
        [_, _, tail] if tail.len() == 6 && tail.bytes().all(|b| b.is_ascii_digit()) => {
            Err(format!(
                "`{raw}` is a dated future and this adapter trades SWAP perpetuals only ({hint})"
            ))
        }
        [_, _, _, _, "C" | "P"] => Err(format!(
            "`{raw}` is an option and this adapter trades SWAP perpetuals only ({hint})"
        )),
        _ => Err(format!("`{raw}` is not an okx SWAP instrument id ({hint})")),
    }
}

/// **The plan for a plain Submit's symbol**, judged from the engine's PUBLISHED block (the same
/// composer of the route key as `account_refusal`). Anything that is not a Submit, a venue this gate
/// does not own, an engine that did not say what it trades and an address nobody can parse are
/// somebody else's: `Pass`.
pub(crate) fn plan_submit_symbol(
    cmd: &WireCommand,
    blocks: &[VenueBlock],
    facts: Option<&dyn SymbolFacts>,
    commit: bool,
) -> SymbolPlan {
    let WireCommand::Submit(req) = cmd else { return SymbolPlan::Pass };
    let Some(rule) = rule_of(&req.venue) else { return SymbolPlan::Pass };
    let per_order = matches!(rule, SymbolRule::PerOrder | SymbolRule::PerOrderUnsafe(_));
    // A label this gate cannot parse belongs to `lower_command`'s refusal, as in `account_refusal`.
    let label = match req.account.as_deref() {
        None => AccountLabel::Default,
        Some(text) => match parse_wire_account(text) {
            Ok(label) => label,
            Err(_) => return SymbolPlan::Pass,
        },
    };
    let key = route_key_of(&req.venue, &label);
    let Some(engine) = blocks.iter().find(|b| b.route_key == key) else { return SymbolPlan::Pass };
    if engine.symbol.is_empty() || req.symbol == engine.symbol {
        return SymbolPlan::Pass;
    }
    // A per-order venue's own symbol in another case, a declared leg and no symbol at all are not foreign.
    if per_order
        && (req.symbol.is_empty()
            || req.symbol.eq_ignore_ascii_case(&engine.symbol)
            || engine.trades(&req.symbol))
    {
        return SymbolPlan::Pass;
    }
    let venue = req.venue.as_str();
    let refuse = |why: String| {
        SymbolPlan::Refuse(format!(
            "submit: this node's `{key}` engine trades {} only, not `{}`: {why}. The order was \
             REFUSED",
            trades_text(engine),
            req.symbol
        ))
    };
    if per_order {
        return per_order_refusal(rule, req, engine).map_or(SymbolPlan::Pass, refuse);
    }
    let spelling = match rule {
        SymbolRule::MountedOnly(reason) => {
            // Compared with the engine's OWN symbol and nothing else: a combo leg in `extra_symbols`
            // is a symbol this adapter would still sign on the mounted instrument.
            return refuse(format!(
                "{venue}'s adapter places every order on the symbol the engine was mounted on \
                 ({reason}), so this order would have been placed on another instrument or priced \
                 on the wrong grid. Send it on a symbol that engine trades"
            ));
        }
        SymbolRule::Admits(spelling) => spelling,
        SymbolRule::PerOrder | SymbolRule::PerOrderUnsafe(_) => return SymbolPlan::Pass,
    };
    // An `Admits` venue's engine already trading a symbol (a declared leg, or an earlier admission)
    // takes the order as written; an admission of ours that has grown stale is refreshed first.
    if engine.trades(&req.symbol) {
        return already_trading(facts, &key, venue, req.symbol.clone(), req, engine, commit);
    }
    // Only a real account takes a foreign symbol: a paper engine is one book (the simulator stamps
    // its own symbol on a fill), and a block that names no tier is never wider than the old gate.
    match engine.tier {
        Some(AccountTier::Demo) => {}
        Some(AccountTier::Live) => {
            // `max_total_exposure` bounds ONE symbol; an account admitted seventeen would carry
            // seventeen times it, with leverage left as the venue holds it. A live account takes a new
            // symbol only with an ACCOUNT-wide ceiling armed.
            if !facts.is_some_and(|source| source.exposure_cap_armed(&key)) {
                return refuse(
                    "this is a LIVE account with no account-wide exposure ceiling armed, and the \
                     per-symbol ceiling (`max_total_exposure`) would then apply to each new symbol on \
                     its own. Arm one first: `vike-cli config set policy.max_account_exposure <amount \
                     in the quote currency>` (applied at the next restart), or this account's own \
                     `vike-cli secrets account set-exposure`"
                        .into(),
                );
            }
        }
        Some(AccountTier::Paper) => {
            return refuse("a paper account trades only the symbol it was started on".into());
        }
        None => {
            return refuse(
                "this node does not say what kind of account this is, so it trades only the \
                 symbol it was started on"
                    .into(),
            );
        }
    }
    let symbol = match canonical_symbol(spelling, &req.symbol) {
        Ok(symbol) => symbol,
        Err(why) => return refuse(why),
    };
    if symbol == engine.symbol {
        return SymbolPlan::Place { symbol, admit: None, wait: None };
    }
    if engine.trades(&symbol) {
        return already_trading(facts, &key, venue, symbol, req, engine, commit);
    }
    if spelling == Spelling::AssetSymbol
        && let Some(own) = std::iter::once(&engine.symbol)
            .chain(engine.extra_symbols.iter())
            .find(|own| same_alpaca_instrument(own, &symbol))
    {
        return refuse(format!(
            "`{symbol}` is the instrument this engine already trades as `{own}` (alpaca writes a \
             crypto pair with or without its slash), and one instrument is one symbol in an engine. \
             Send the order on `{own}`"
        ));
    }
    if spelling == Spelling::LaneSuffixed {
        let engine_perp = vike_catalog::split_perp(&engine.symbol).1;
        let want_perp = vike_catalog::split_perp(&symbol).1;
        if engine_perp != want_perp {
            let (this, other) = if engine_perp { ("perp", "spot") } else { ("spot", "perp") };
            return refuse(format!(
                "`{symbol}` is a {other} symbol and this engine runs {venue}'s {this} lane (an \
                 engine has one lane and one user-data stream, so a fill on the other lane would \
                 never arrive). Use a {this} symbol, or an account mounted on the {other} lane"
            ));
        }
    }
    // A hyperliquid engine mounted on a spot pair (`HYPE/USDC`, `@N`) or a builder-dex coin runs that
    // product: its balance read, fills channel and asset ids are not a core perp's, so a core perp
    // is not admitted into it (the lane rule above, for the one venue whose lanes are not suffixes).
    if spelling == Spelling::CaseKept && !is_core_perp_name(&engine.symbol) {
        return refuse(format!(
            "this engine runs {venue}'s spot or builder-dex market (`{}`), not its core perpetuals, \
             and an engine has one product (balance, fills and asset ids differ). Use an account \
             mounted on a core perpetual",
            engine.symbol
        ));
    }
    if engine.extra_symbols.len() >= MAX_EXTRA_SYMBOLS {
        return refuse(format!(
            "an engine admits at most {MAX_EXTRA_SYMBOLS} symbols beyond the one it was started on"
        ));
    }
    let Some(source) = facts else {
        return refuse(
            "this surface cannot look up a symbol's facts (its grid and contract), so it cannot \
             admit a new symbol; place the order from the Trade window or the CLI"
                .into(),
        );
    };
    let admission = match source.admission(&key, venue, &symbol) {
        Ok(admission) => admission,
        Err(why) => return refuse(format!("cannot admit `{symbol}`: {why}")),
    };
    if admission.route_key != key || admission.symbol != symbol {
        return refuse(format!(
            "the facts source answered for `{}` on `{}` when `{symbol}` on `{key}` was asked",
            admission.symbol, admission.route_key
        ));
    }
    if admission.grid.tick_size.is_none() || admission.grid.lot_size.is_none() {
        let missing = match (admission.grid.tick_size, admission.grid.lot_size) {
            (None, None) => "tick size and lot size",
            (None, Some(_)) => "tick size",
            _ => "lot size",
        };
        return refuse(format!(
            "{venue} publishes no {missing} for `{symbol}`, so its price and size cannot be \
             rounded on its own grid"
        ));
    }
    // deribit counts money in two ways, and an engine in one (the module doc).
    if spelling == Spelling::PerpetualName
        && let Some(why) = money_model_refusal(engine, &admission)
    {
        return refuse(why);
    }
    // The slot is spent only by an order worth admitting for. An admission is permanent for the life of
    // the process (the engine has no un-admit and caps a venue account at `MAX_EXTRA_SYMBOLS`), so a
    // reduce-only order (it can only fail: the engine holds nothing in a symbol it was never
    // told about) and an order the venue's own grid would reject must not be able to burn one.
    if req.reduce_only {
        return refuse(
            "a reduce-only order can only close a position this account holds in the symbol, and it \
             holds none; send an opening order to start trading it"
                .into(),
        );
    }
    if let Some(why) = grid_refusal(req, &admission.grid) {
        return refuse(format!("{why} (no symbol slot was spent)"));
    }
    match with_price(req, facts, &key, venue, symbol, Some(Box::new(admission)), commit) {
        SymbolPlan::Refuse(why) => refuse(why),
        other => other,
    }
}

/// Why the venue's own grid would reject the order, judged from the facts the door looked up: a
/// quantity under the minimum or under one lot, or a priced order under the minimum notional. A
/// market order has no price to size, so it is judged on quantity alone.
fn grid_refusal(
    req: &vike_tradehub_client::wire::WireOrderRequest,
    grid: &vike_model::SymbolGrid,
) -> Option<String> {
    let qty = req.qty.abs();
    if let Some(min) = grid.min_qty
        && qty < min
    {
        return Some(format!("the quantity {qty} is below the symbol's minimum {min}"));
    }
    if let Some(lot) = grid.lot_size
        && qty < lot
    {
        return Some(format!("the quantity {qty} is below one lot ({lot})"));
    }
    let price = req.price.or(req.trigger_price)?;
    let min = grid.min_notional?;
    (qty * price.abs() < min).then(|| {
        format!(
            "the order's value {} is below the symbol's minimum notional {min}",
            qty * price.abs()
        )
    })
}

/// Why a per-order venue's engine cannot take `req`, a symbol it was not started on, or `None`: its
/// adapter reads the order's symbol, but the engine is still ONE book on the mounted symbol's grid.
fn per_order_refusal(
    rule: SymbolRule,
    req: &vike_tradehub_client::wire::WireOrderRequest,
    engine: &VenueBlock,
) -> Option<String> {
    // Polymarket judges every token its engine is given (its mounts hold one, or none); live accounts
    // run on this row, so it keeps today's behaviour exactly.
    if req.venue == "polymarket" {
        return None;
    }
    // The paper client stamps its own symbol on every fill and runs one single-symbol book.
    if engine.tier == Some(AccountTier::Paper) {
        return Some("a paper account trades only the symbol it was started on".into());
    }
    if let SymbolRule::PerOrderUnsafe(reason) = rule {
        return Some(format!(
            "{}'s adapter reads the order's symbol, but the engine cannot judge another one \
             ({reason}). Send it on a symbol that engine trades",
            req.venue
        ));
    }
    // No per-venue price watcher exists for these venues: a priceless opening order is judged at
    // notional 0 and passes every ceiling, margin check and collar. (Reduce-only closes what exists.)
    (!req.reduce_only && req.price.is_none()).then(|| {
        format!(
            "{} has no price source for a symbol the engine was not started on, so an opening \
             order without a limit price would be judged at notional 0 (every ceiling, the \
             margin check and the collar would pass). Send a limit price, or a reduce-only order",
            req.venue
        )
    })
}

/// The plan for a symbol the engine ALREADY trades (a declared leg, or an earlier admission): the
/// order goes as written. An admission of ours that has grown stale (a venue changes a tick or lot
/// rarely, and announces it) is refreshed ahead of it by an ordinary `AdmitSymbol`; a Preview
/// (`commit` false) changes nothing.
///
/// **An engine that trades a symbol says nothing about its PRICE lane**: marks are not journaled, so a
/// symbol an earlier run admitted comes back from the restore trading and with no mark, and the lane is
/// subscribed only if the boot's re-admission reached it (v0.1.66 on the CI box: a restored flat ETHUSDT was
/// denied `no-reference-price` for ever). A real account's order therefore ASKS the price lane here too
/// (idempotent: a subscribed symbol is left alone), and an opening order with no price yet is held for the
/// first one ([`PriceWait`]) exactly as for a new symbol. A failed ask changes nothing: the engine's own
/// `no-reference-price` stays the fail-closed answer.
fn already_trading(
    facts: Option<&dyn SymbolFacts>,
    key: &str,
    venue: &str,
    symbol: String,
    req: &vike_tradehub_client::wire::WireOrderRequest,
    engine: &VenueBlock,
    commit: bool,
) -> SymbolPlan {
    let admit = facts
        .filter(|_| commit)
        .and_then(|source| source.refresh(key, venue, &symbol))
        .filter(|a| a.route_key == key && a.symbol == symbol)
        .map(Box::new);
    let real = matches!(engine.tier, Some(AccountTier::Demo | AccountTier::Live));
    let unpriced = commit
        && real
        && facts.is_some_and(|source| matches!(source.price(key, venue, &symbol, true), Ok(None)));
    let wait = (unpriced && !req.reduce_only)
        .then(|| PriceWait { route_key: key.to_string(), venue: venue.to_string() });
    SymbolPlan::Place { symbol, admit, wait }
}

/// Finish a placement of a NEW symbol: an OPENING order needs a price (the collar, the exposure caps
/// and the account margin are all judged at the mark, and an admitted symbol has none until its feed
/// delivers one). With `commit` the source subscribes the feed (once); a Preview only READS. A price
/// already here is a plain placement; none yet is a placement that must WAIT ([`PriceWait`], only
/// when committing); a feed that cannot be had is a refusal. A reduce-only order is never held up or
/// refused for want of a price, but still subscribes the feed so the symbol is priced afterwards.
fn with_price(
    req: &vike_tradehub_client::wire::WireOrderRequest,
    facts: Option<&dyn SymbolFacts>,
    key: &str,
    venue: &str,
    symbol: String,
    mut admit: Option<Box<SymbolAdmission>>,
    commit: bool,
) -> SymbolPlan {
    let mut wait = None;
    if let Some(source) = facts {
        match source.price(key, venue, &symbol, commit) {
            // The price seen NOW rides the admission as the engine's first mark for the symbol.
            Ok(Some(px)) => {
                if let Some(a) = admit.as_mut() {
                    a.initial_mark = Some(px);
                }
            }
            Ok(None) if !req.reduce_only && commit => {
                wait = Some(PriceWait { route_key: key.to_string(), venue: venue.to_string() });
            }
            Ok(None) => {}
            Err(why) if !req.reduce_only => {
                return SymbolPlan::Refuse(format!("no price feed for `{symbol}`: {why}"));
            }
            Err(_) => {}
        }
    }
    SymbolPlan::Place { symbol, admit, wait }
}

/// Whether judging this command would cost the facts source a lookup: a Submit on an admitting venue
/// for a symbol the addressed engine does not already trade. The Preview takes a rate token for
/// exactly these (a lookup is a gatewayhub round trip a peer could otherwise flood for free).
pub(crate) fn needs_lookup(cmd: &WireCommand, blocks: &[VenueBlock]) -> bool {
    let WireCommand::Submit(req) = cmd else { return false };
    if !matches!(rule_of(&req.venue), Some(SymbolRule::Admits(_))) {
        return false;
    }
    let label = match req.account.as_deref() {
        None => AccountLabel::Default,
        Some(text) => match parse_wire_account(text) {
            Ok(label) => label,
            Err(_) => return false,
        },
    };
    let key = route_key_of(&req.venue, &label);
    blocks
        .iter()
        .find(|b| b.route_key == key)
        .is_some_and(|b| !b.symbol.is_empty() && req.symbol != b.symbol && !b.trades(&req.symbol))
}

/// **The contract value of the symbol a Submit names when its engine does not trade it yet**, from the
/// symbol's own facts, or `None` (the symbol is traded already, so the published grid has its row; the
/// plan refuses; the facts say the default). The notional ceiling at the door sizes with it: the engine's
/// published grid lists no row for an unadmitted symbol, so an INVERSE one (an amount of US dollars,
/// no price in its notional) would be sized as `qty · price`. It runs the plan in its Preview form,
/// which reads the facts (cached) and subscribes and refreshes nothing.
pub(crate) fn foreign_contract_value(
    cmd: &WireCommand,
    blocks: &[VenueBlock],
    facts: Option<&dyn SymbolFacts>,
) -> Option<vike_model::ContractValue> {
    if !needs_lookup(cmd, blocks) {
        return None;
    }
    match plan_submit_symbol(cmd, blocks, facts, false) {
        SymbolPlan::Place { admit: Some(admission), .. } => admission.contract_value,
        _ => None,
    }
}

/// The refusal the Preview surfaces print: [`plan_submit_symbol`]'s `Refuse`, nothing else. A
/// PREVIEW: it reads (a facts lookup is cached and read-only) and never subscribes a price feed,
/// refreshes a grid or waits.
pub(crate) fn submit_symbol_refusal(
    cmd: &WireCommand,
    blocks: &[VenueBlock],
    facts: Option<&dyn SymbolFacts>,
) -> Option<String> {
    match plan_submit_symbol(cmd, blocks, facts, false) {
        SymbolPlan::Refuse(why) => Some(why),
        SymbolPlan::Pass | SymbolPlan::Place { .. } => None,
    }
}

/// How long the door holds an opening order for the first price of a symbol whose feed it just
/// subscribed. A pump delivers its first quote in well under a second; past this the order is
/// refused and the operator retries (the subscription stays).
pub(crate) const PRICE_WAIT: std::time::Duration = std::time::Duration::from_secs(4);

/// Hold the order until `wait`'s symbol has a price, or say why not. Blocks the CONNECTION thread
/// (up to [`PRICE_WAIT`]), never the core, and never at all for a source that does not
/// [`SymbolFacts::waits`] (it gets one look and a prompt "retry").
pub(crate) fn await_price(
    facts: &dyn SymbolFacts,
    wait: &PriceWait,
    symbol: &str,
) -> Result<f64, String> {
    let deadline = std::time::Instant::now() + PRICE_WAIT;
    loop {
        match facts.price(&wait.route_key, &wait.venue, symbol, true) {
            Ok(Some(px)) => return Ok(px),
            Ok(None) if facts.waits() && std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Ok(None) => {
                return Err(format!(
                    "submit: `{symbol}` has no price on this node yet (its price feed was just \
                     subscribed and nothing has arrived{}). The order was REFUSED: retry in a moment",
                    if facts.waits() {
                        format!(" in {} s", PRICE_WAIT.as_secs())
                    } else {
                        String::new()
                    }
                ));
            }
            Err(why) => return Err(format!("submit: no price feed for `{symbol}`: {why}")),
        }
    }
}
