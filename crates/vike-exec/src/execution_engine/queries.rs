//! Engine read-side helpers: symbol admission, sizes, marks, exposure.

use indexmap::IndexMap;
use vike_model::events::PositionSide;

use super::{ExecutionClient, ExecutionEngine, StaleMark};

/// Most symbols [`ExecutionEngine::admit_symbol`] admits beyond the mounted one (decision 0173). A
/// LATENCY cap, not a policy: [`ExecutionEngine::accepts_symbol`] scans `extra_symbols` for a fill
/// that is not on the mounted symbol, and `CoreSnapshot::build` makes a position row per held symbol
/// on every published event. Raising it needs the the latency box latency A/B with a many-symbol variant.
pub const MAX_EXTRA_SYMBOLS: usize = 16;

/// What [`ExecutionEngine::admit_symbol`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmitOutcome {
    /// A new symbol joined the engine.
    Admitted,
    /// The symbol was already the engine's: its grid and contract value were replaced.
    Refreshed,
    /// The symbol is the mounted one: no grid, no contract value and no extra symbol is written, only
    /// its margin rate and first mark and, when it is quoted in a currency other than the ledger's,
    /// its conversion rate (`ExecutionEngine::declare_mount_quote_ccy`).
    Mounted,
    /// Nothing was written. The reason is a stable word for the note: `empty-symbol`,
    /// `grid-incomplete`, `symbol-cap`, or one of the currency words — `account-ccy-unset` (the
    /// engine was never told its account currency), `quote-ccy-missing`, `quote-ccy-changed`,
    /// `no-quote-rate`, `quote-rate-future`, `quote-rate-stale`, `quote-rate-jump`,
    /// `quote-rate-range` (`Account::admit_quoted` says what each means). One exception to "nothing
    /// was written": `quote-rate-jump` remembers the rate it refused (and its stamp), so that the
    /// same rate offered again by a LATER observation is taken as a real move.
    ///
    /// A margin rate that is not in `(0, 1]` is no longer a refusal: the margin part of the
    /// admission is ignored (the symbol keeps the engine-wide rate), as decision 0183 item 11
    /// states, so a bad figure cannot hold back the rate refresh the same admission carries.
    Refused(&'static str),
}

impl<C: ExecutionClient> ExecutionEngine<C> {
    // --- internals ---

    /// WHY the registry order `coid` was refused: the reason its `OrderRejected` (or `OrderDenied`)
    /// carried. `None` for an order that was not refused, a refusal with an empty reason, an order
    /// restored from a snapshot (the table is not part of one), and the oldest refusals once
    /// `MAX_REJECTION_REASONS` newer ones have landed.
    pub fn rejection_reason(&self, coid: &str) -> Option<&str> {
        self.rejection_reasons.get(coid).map(String::as_str)
    }

    /// Remember `reason` for `coid` (`Self::rejection_reason`). Cold: one call per refused order.
    pub(super) fn note_rejection(&mut self, coid: &str, reason: &str) {
        if reason.is_empty() {
            return;
        }
        if self.rejection_reasons.len() >= super::MAX_REJECTION_REASONS
            && !self.rejection_reasons.contains_key(coid)
        {
            self.rejection_reasons.shift_remove_index(0);
        }
        self.rejection_reasons.insert(coid.to_string(), reason.to_string());
    }

    /// Drop `coid`'s remembered reason: a modify landed, so the refusal that was shown beside the
    /// live order is stale. Cold: one call per `OrderModified`.
    pub(super) fn forget_rejection(&mut self, coid: &str) {
        if !self.rejection_reasons.is_empty() {
            self.rejection_reasons.shift_remove(coid);
        }
    }

    /// True when this engine folds venue events for `symbol` — the primary symbol or any
    /// `extra_symbols` entry.
    pub fn accepts_symbol(&self, symbol: &str) -> bool {
        symbol == self.symbol || self.extra_symbols.iter().any(|s| s == symbol)
    }

    /// [`Self::extra_symbols`] in the form the read model publishes ([`crate::VenueBlock::extra_symbols`]),
    /// shared between publishes while the symbols do not change.
    ///
    /// `vike_core::snapshot::build` calls this once per engine per publish, per event on a sporadic
    /// feed. An engine with no extra symbol (every venue mount) answers with one `is_empty` test and
    /// no allocation, lock or refcount. Otherwise the answer is the list from the last call when it
    /// still equals the field, one comparison of the names, and a rebuilt one when it does not: the
    /// field is public and assigned directly (a combo lowering, a test, `from_snapshot`), so nothing
    /// is trusted but the comparison. The value is always what `extra_symbols.clone()` was.
    pub fn published_extra_symbols(&self) -> crate::SymbolList {
        if self.extra_symbols.is_empty() {
            return crate::SymbolList::default();
        }
        // One thread reads this; a poisoned lock holds a whole list or none, both valid.
        let mut published =
            self.extra_symbols_published.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if published.as_slice() != self.extra_symbols.as_slice() {
            *published = self.extra_symbols.clone().into();
        }
        published.clone()
    }

    /// Take one more symbol into this engine ON DEMAND (decision 0173): the body of
    /// [`crate::Command::AdmitSymbol`], run on the fold thread with no network because the command
    /// carries the facts.
    ///
    /// What it writes, and nothing else: the symbol joins [`Self::extra_symbols`] (so
    /// [`Self::accepts_symbol`], funding and liquidation folds and the published
    /// `VenueBlock::trades` see it), its grid joins `RiskLimits::grid_by_symbol` marked `on_demand`
    /// (so [`vike_model::RiskLimits::grid_for`] stops falling back to the mounted symbol's scalars),
    /// and its contract value joins the account's grid when it is not the default. The price collar
    /// needs no row: `collar_for` falls back to the engine's global rate. Neither does the initial
    /// margin, unless the admission states the symbol's own (`SymbolAdmission::im_requirement`, which
    /// writes its `im_by_symbol` row); otherwise `im_for` falls back to the engine-wide rate. The
    /// admission is the COMPLETE statement of that row for a symbol it admitted: sent again with no
    /// figure (or an ignored one), it REMOVES the row it wrote before.
    ///
    /// **A symbol quoted in a currency other than the account's** (`SymbolAdmission::quote_ccy`) is
    /// admitted only once the engine has been told its account currency (`Self::set_account_ccy`),
    /// and with a usable, fresh `quote_rate` stamped no further ahead of the engine's clock than
    /// `QUOTE_RATE_FUTURE_SKEW_MS`, and its contract value is installed as the
    /// EFFECTIVE value — the venue's unit times the rate — so every money number on the order path
    /// comes out in the account's currency (`Account::admit_quoted`). The same admission, sent again
    /// with a newer rate, is how the rate is refreshed, for every symbol quoted in that currency.
    ///
    /// Refused with a named reason and NO state change: an empty symbol, a grid without a tick or
    /// without a lot (a `None` field inherits the MOUNTED symbol's scalar, which is the defect), and
    /// a new symbol past [`MAX_EXTRA_SYMBOLS`]. The mounted symbol takes no grid (its scalars ARE its
    /// grid) and only its margin rate, its `initial_mark` (when the engine holds no mark for it) and,
    /// on a ledger that is not in the mount's money, its quote currency and rate (the same words and
    /// rules as any foreign-quoted symbol: a EUR account on `EURUSD` admits the mount quoted in `USD`
    /// and the rate is how it is renewed; see [`Self::declare_mount_quote_ccy`]). A known symbol is
    /// REFRESHED, which is what makes the command safe to re-send.
    ///
    /// COLD: once per admission, never per message. Replay-safe: every input rides the journaled
    /// command, and every field it writes is in `EngineSnapshot` (`extra_symbols`, `limits`, the
    /// account's contract values), so a `Snap` taken after it restores the same engine.
    pub fn admit_symbol(&mut self, a: &crate::SymbolAdmission) -> AdmitOutcome {
        let symbol = a.symbol.as_str();
        if symbol.trim().is_empty() {
            return AdmitOutcome::Refused("empty-symbol");
        }
        // The symbol's own margin rate, judged before anything is written. A rate outside (0, 1] is
        // no leverage anybody holds (0 is infinite leverage, above 1 is a negative one), and
        // decision 0183 item 11 says such a figure is IGNORED, like a bad `initial_mark`: the symbol
        // keeps the engine-wide rate. It does NOT refuse the admission, because the same admission
        // carries the quote-currency rate refresh, and a bad margin figure must not keep a position's
        // conversion rate from being renewed. Ignored means exactly `None` below (so a live engine and
        // a replay agree: a NaN here is journaled as `null`, which reads back as `None`).
        let im = match a.im_requirement {
            None => None,
            Some(x) if x.is_finite() && x > 0.0 && x <= 1.0 => Some(x),
            Some(x) => {
                tracing::warn!(
                    target: "vike_exec::oms",
                    symbol,
                    im_requirement = x,
                    "AdmitSymbol: a margin rate outside (0, 1] is ignored; the symbol is margined at \
                     the engine-wide rate"
                );
                None
            }
        };
        if symbol == self.symbol {
            // THE MOUNT'S OWN MONEY (decision 0183, the mounted-symbol amendment). The mounted symbol
            // is booked in the account's currency when it is quoted in it (a USD account on EURUSD:
            // no admission is needed and none is read). On a ledger that is NOT in the mount's money
            // (a EUR account on EURUSD) it is a foreign-quoted symbol like any other and its rate
            // rides THIS admission: a currency named here, or a mount already held foreign, is judged
            // by the same rules as an admitted symbol (`rate_mounted_symbol`), and a refusal leaves
            // the engine untouched. An admission that names no currency of a mount that is not held
            // foreign is read as it always was, so an engine with no foreign mount (every USD
            // account) never reaches the judgement.
            if (a.quote_ccy.is_some() || self.account.is_foreign_quoted(symbol))
                && let Err(why) = self.rate_mounted_symbol(a)
            {
                return AdmitOutcome::Refused(why);
            }
            // Beyond that the mounted symbol takes no grid and no contract value from an admission
            // (its scalars ARE its grid), but it may be margined differently from the engine-wide
            // rate: a spot mount on a perp's leverage says so here, and only this (and the first
            // mark, below) is written. A `None` writes NOTHING here, unlike a re-admitted symbol
            // below: the mounted symbol's row may be the operator's (`Command::SetMargin`, the
            // leverage pill), which no admission owns.
            if let Some(im) = im {
                self.gate.limits.im_by_symbol.insert(symbol.to_string(), im);
            }
            // ...and the caller's observed price, when the engine holds NO mark for its own symbol: a
            // labelled engine's mounted symbol (a lane twin's `BTCUSDT.P`) gets no tick from the
            // venue's quote route, so this admission is the only price that can reach it, and the
            // first order is otherwise judged with no mark (a market order at notional zero).
            self.seed_initial_mark(symbol, a.initial_mark);
            return AdmitOutcome::Mounted;
        }
        if a.grid.tick_size.is_none() || a.grid.lot_size.is_none() {
            return AdmitOutcome::Refused("grid-incomplete");
        }
        let known = self.accepts_symbol(symbol);
        if !known && self.extra_symbols.len() >= MAX_EXTRA_SYMBOLS {
            return AdmitOutcome::Refused("symbol-cap");
        }
        // THE SYMBOL'S CURRENCY, judged before anything is written (every refusal below leaves the
        // engine untouched, bar the jump memory `Account::admit_quoted` documents). A symbol quoted
        // in the account's own currency (no `quote_ccy`, or one equal to `quote_asset`) takes the
        // single-currency path unchanged. One quoted in another currency is admitted only with a
        // usable rate, and its contract value is installed by the account as the EFFECTIVE value
        // (the unit times the rate): see `account/fx.rs`.
        let foreign = match self.quote_currency_of(a) {
            Ok(foreign) => foreign,
            Err(why) => return AdmitOutcome::Refused(why),
        };
        if let Some(ccy) = foreign.as_deref() {
            // Every rate is "account-currency units per one unit of `ccy`", so a rate against a
            // currency the engine was never TOLD is a number in an unknown unit: `quote_asset` still
            // being the constructor's `"USDT"` would make the fee rule drop an OANDA fee in `USD` as a
            // third currency and `apply_account_state` pick the wrong balance, with no error.
            if !self.account_ccy_known() {
                return AdmitOutcome::Refused("account-ccy-unset");
            }
            let rate = a.quote_rate.zip(a.rate_as_of_ms);
            if let Err(why) = self.account.admit_quoted(
                symbol,
                ccy,
                a.contract_value,
                rate,
                &self.quote_asset,
                self.now_ms,
            ) {
                return AdmitOutcome::Refused(why);
            }
        }
        if !known {
            self.extra_symbols.push(symbol.to_string());
        }
        // Keyed by the RAW symbol, as `declared_symbol_grids` keys a declared leg: `grid_for` looks
        // it up by the string the order carries. A leg DECLARED at assembly keeps the row (and the
        // feed-backed pricing) it was declared with: re-admitting it must not mark it `on_demand`,
        // which would deny its market orders. A combo leg with no row, and a new symbol, get the
        // admission's; only a symbol that was not known is `on_demand`.
        let prior = self.gate.limits.grid_by_symbol.get(symbol).map(|g| g.on_demand);
        // `RiskLimits::im_for` reads this row ahead of the engine-wide rate everywhere. The admission
        // is the COMPLETE statement of an admitted symbol's margin row (the node re-sends it with
        // every refresh): `Some` writes or replaces the row, and `None` REMOVES the row an earlier
        // admission wrote, so a venue that has dropped its leverage figure cannot leave the old,
        // more permissive one margining the symbol. Removal is for a symbol this admission owns: a
        // RE-admitted one (not the first sight of the name, whose row may be an operator's) that is
        // not a leg declared at assembly (`prior == Some(false)`, which keeps what it was given).
        match im {
            Some(im) => {
                self.gate.limits.im_by_symbol.insert(symbol.to_string(), im);
            }
            None if known && prior != Some(false) => {
                self.gate.limits.im_by_symbol.shift_remove(symbol);
            }
            None => {}
        }
        if prior != Some(false) {
            let on_demand = !known || prior == Some(true);
            let row = vike_model::SymbolGrid { on_demand, ..a.grid.clone() };
            self.gate.limits.grid_by_symbol.insert(symbol.to_string(), row);
        }
        // A refresh that carries no contract value leaves the one an earlier admission (or the
        // restore) installed: `None` means "the default", which a first admission already is.
        // (A foreign-quoted symbol's value went in with its rate above.)
        if foreign.is_none()
            && let Some(value) = a.contract_value
        {
            self.account.admit_contract_value(symbol, value);
        }
        // The caller's observed price becomes the symbol's first mark when the engine has none: it is the
        // ONLY way a price reaches an engine before the symbol is accepted (quotes for an unaccepted
        // symbol are dropped), and without it the first order is judged with no mark.
        self.seed_initial_mark(symbol, a.initial_mark);
        if known { AdmitOutcome::Refreshed } else { AdmitOutcome::Admitted }
    }

    /// The currency half of an admission of the MOUNTED symbol (`Self::admit_symbol`): the symbol the
    /// engine was started on, quoted in a currency other than the ledger's. `Ok(())` when there is
    /// nothing to book (the admission names the ledger's own currency) or the rate was taken; the
    /// refusal word otherwise, with nothing written but the jump memory (`Account::admit_quoted`).
    ///
    /// The mount's unit is what its grid row already held (the mount's configuration, `Account::new`'s
    /// values, else the default), never the admission's `contract_value`: the mount takes no contract
    /// value from an admission, and a refresh keeps the unit the first one fixed. Cold: once per
    /// admission.
    fn rate_mounted_symbol(&mut self, a: &crate::SymbolAdmission) -> Result<(), &'static str> {
        let Some(ccy) = self.quote_currency_of(a)? else { return Ok(()) };
        // Every rate is "account-currency units per one unit of `ccy`": against a currency the engine
        // was never TOLD it is a number in an unknown unit (`admit_symbol`, the same word).
        if !self.account_ccy_known() {
            return Err("account-ccy-unset");
        }
        let base = (!self.account.is_foreign_quoted(&a.symbol))
            .then(|| self.account.contract_value_of(&a.symbol));
        let rate = a.quote_rate.zip(a.rate_as_of_ms);
        self.account.admit_quoted(&a.symbol, &ccy, base, rate, &self.quote_asset, self.now_ms)
    }

    /// DECLARE the engine's MOUNTED symbol as quoted in `ccy`, a currency other than the ledger's,
    /// with no rate yet (decision 0183, the mounted-symbol amendment): a EUR account mounted on
    /// `EURUSD` declares `USD`. The node calls it at assembly, right after
    /// [`Self::set_account_ccy`], so an engine whose ledger is not in its mount's money is never
    /// open to an order priced at par: until the first admission of the mount carries a rate
    /// (`Self::admit_symbol`), the mount is stale by `Account::quote_leg`, an OPENING order on it is
    /// denied `quote-rate-stale` and a covered reduce passes. Configuration like `set_account_ccy`,
    /// not a command and not journaled; what a replay needs rides the snapshot (`AccountSnapshot::fx`).
    ///
    /// `Err` (nothing changed): `quote-ccy-missing` for an empty code, `account-ccy-unset` when the
    /// engine was never told its currency (the rate's unit), and what `Account::declare_quoted` says.
    /// A code equal to the ledger's own is `Ok` and declares nothing: the mount is in the ledger's
    /// money and needs no rate.
    pub fn declare_mount_quote_ccy(&mut self, ccy: &str) -> Result<(), &'static str> {
        let Some(code) = crate::account::fx::ccy_code(ccy) else {
            return Err("quote-ccy-missing");
        };
        if !self.account_ccy_known() {
            return Err("account-ccy-unset");
        }
        if crate::account::fx::same_ccy(&code, &self.quote_asset) {
            return Ok(());
        }
        let base = self.account.contract_value_of(&self.symbol);
        self.account.declare_quoted(&self.symbol, &code, base, &self.quote_asset)
    }

    /// An admission's `initial_mark` becomes `symbol`'s first mark, and only when the engine holds none
    /// for it (a tick that already priced the symbol is never overwritten by an older observation). A
    /// price that is not finite and positive is ignored, like a bad `im_requirement`. Written to the
    /// board AND the account slot: the slot is what a snapshot carries (and `from_snapshot` re-seeds
    /// the board from), so a replay that starts from a Snap taken after the command holds the same mark.
    /// COLD: one call per admission.
    fn seed_initial_mark(&mut self, symbol: &str, initial_mark: Option<f64>) {
        if let Some(px) = initial_mark
            && px.is_finite()
            && px > 0.0
            && self.resolved_position_price(&self.venue, symbol, 0.0, &self.price_cfg).is_none()
        {
            self.price_board.set_mark(&self.venue, symbol, px, self.now_ms);
            self.account.set_mark_from(
                &self.venue,
                symbol,
                px,
                crate::account::MarkSource::TradeTick,
                self.now_ms,
            );
        }
    }

    /// The foreign currency an admission puts its symbol in, if any: `Ok(None)` for a symbol quoted in
    /// the account's own currency (the admission names none, or names `quote_asset`), `Ok(Some(ccy))`
    /// for one quoted in another, and the refusal word for a currency that cannot be taken as given.
    /// **An admission that carries a rate and no currency is refused, and so is one that drops the
    /// currency a symbol was admitted under**: an absent currency is never read as the account's.
    ///
    /// Codes are compared TRIMMED and UPPER-CASED (`usd` is `USD`), and the foreign currency comes
    /// back in that canonical spelling, so one currency is one rate row whatever the caller typed.
    fn quote_currency_of(
        &self,
        a: &crate::SymbolAdmission,
    ) -> Result<Option<String>, &'static str> {
        let held = self.account.is_foreign_quoted(&a.symbol);
        match a.quote_ccy.as_deref() {
            None if a.quote_rate.is_some() || a.rate_as_of_ms.is_some() || held => {
                Err("quote-ccy-missing")
            }
            None => Ok(None),
            Some(raw) => match crate::account::fx::ccy_code(raw) {
                None => Err("quote-ccy-missing"),
                Some(ccy) if crate::account::fx::same_ccy(&ccy, &self.quote_asset) => {
                    if held {
                        Err("quote-ccy-changed")
                    } else {
                        Ok(None)
                    }
                }
                Some(ccy) => Ok(Some(ccy)),
            },
        }
    }

    /// TELL the engine the currency its ledger is in: the account's own, the one every
    /// foreign-quoted symbol's rate is expressed in. The node calls it at assembly for an account
    /// that converts a quote currency: today OANDA on its `demo` tier with an account-wide exposure
    /// ceiling armed, whose account summary states the currency (ctrader's deposit asset and ig's
    /// account list are decision 0183 item 1's other sources and are not wired); until it has, a
    /// symbol quoted in another currency is refused (`account-ccy-unset`) and
    /// [`Self::quote_asset`] is only the constructor's guess.
    ///
    /// The code is stored trimmed and upper-cased. Refused (`Err`, nothing changed) when it names
    /// nothing (`account-ccy-empty`) or when this ledger already holds foreign-quoted symbols whose
    /// rates are expressed in a different currency (`account-ccy-changed`): re-labelling the unit
    /// under a standing rate book would make every rate in it wrong at once. Telling it the
    /// currency it already has is a no-op. Not a command and not journaled: it is assembly
    /// configuration like [`Self::tier`]; what a replay needs of it rides the snapshot
    /// (`Self::account_ccy_known`).
    pub fn set_account_ccy(&mut self, ccy: &str) -> Result<(), &'static str> {
        let Some(code) = crate::account::fx::ccy_code(ccy) else {
            return Err("account-ccy-empty");
        };
        if self
            .account
            .fx_account_ccy()
            .is_some_and(|held| !crate::account::fx::same_ccy(held, &code))
        {
            return Err("account-ccy-changed");
        }
        self.quote_asset = code;
        self.account_ccy_set = true;
        self.refresh_ledger_ccy();
        Ok(())
    }

    /// TELL the engine its tier: what an order to it goes to (decision 0156). The mount calls it once,
    /// at assembly, for every engine it builds (`demo`, `live` and `paper` alike). Beyond writing
    /// [`Self::tier`] it records that the tier is a STATEMENT, which is what puts a `paper` tier into
    /// the journal's snapshot (`EngineSnapshot::tier`); the field alone stays a seed. Configuration
    /// like [`Self::set_account_ccy`]: not a command and not journaled as one.
    pub fn set_tier(&mut self, tier: vike_model::AccountTier) {
        self.tier = tier;
        self.tier_set = true;
    }

    /// Whether this engine's account currency is a STATEMENT and not the constructor's guess: the
    /// node told it ([`Self::set_account_ccy`]), or [`Self::quote_asset`] is anything but the
    /// guess (`"USDT"`) however it got there. A symbol quoted in another currency is admitted only
    /// when it is (`account-ccy-unset`): three behaviours key on the label (`apply_account_state`'s
    /// balance pick, the fee rule for a fee in the account's currency, the foreign-versus-par test of
    /// an admission) and each fails QUIETLY on the wrong one. A function of `quote_asset` and one
    /// snapshotted flag, so a replay from a `Snap` takes the verdict the live engine took.
    pub fn account_ccy_known(&self) -> bool {
        self.account_ccy_set || self.quote_asset != super::DEFAULT_QUOTE_ASSET
    }

    /// The CURRENCY GROUP this engine's money is summed in across engines (decision 0183, item 13):
    /// every engine books in its own account's currency, and a total across engines may add only
    /// figures of one currency. EMPTY for the dollar group (`USD`, `USDT`, an empty code, and an
    /// engine whose currency nobody told, [`Self::account_ccy_known`] being false: every engine of
    /// today's nodes), else the upper-cased code. Compared by value, so two engines are in one
    /// group exactly when their keys are equal.
    ///
    /// Called by `vike_core::snapshot::build` once per engine per publish, so it is a COPY of a key
    /// kept in the engine: it takes no allocation and no intern lookup (interning a `Ustr` is a hash
    /// and a locked table probe, even for `""`). The key is derived once, where the currency is
    /// written: [`Self::set_account_ccy`], [`Self::from_snapshot`] and the constructor (the dollar
    /// group). ⚠ Not where `quote_asset` is written directly: that field is read by other code and
    /// stays public, but a currency is SAID through `set_account_ccy`.
    #[must_use]
    pub fn ledger_ccy_key(&self) -> crate::Interned {
        self.ledger_ccy
    }

    /// Derive [`Self::ledger_ccy_key`]'s key from `quote_asset` and the told flag. Interns (cold).
    pub(super) fn refresh_ledger_ccy(&mut self) {
        let code = self.quote_asset.trim();
        self.ledger_ccy = if !self.account_ccy_known()
            || code.is_empty()
            || code.eq_ignore_ascii_case("USD")
            || code.eq_ignore_ascii_case("USDT")
        {
            crate::Interned::default()
        } else {
            crate::Interned::from(code.to_ascii_uppercase())
        };
    }

    /// Why this engine refuses `request` BEFORE the gate prices it, or `None` (decision 0173):
    /// `symbol-not-admitted` when [`vike_model::RiskLimits::deny_unadmitted_symbols`] is set (the mount
    /// sets it for the venues whose adapter signs each order on its own symbol) and the symbol is
    /// neither this engine's own nor one it was told about (`extra_symbols`). Without it the order is
    /// rounded onto the MOUNTED symbol's grid and signed: a refused or lost `AdmitSymbol` must stop an
    /// order, not degrade it. An empty symbol is the mounted one (the adapters read it so).
    ///
    /// ⚠ Deliberately a function of FOLDED state only (the flag and the symbol set), never of a mark:
    /// marks are not journaled, so a verdict that read one would differ on replay and break the
    /// determinism fence. The node's door asks for a price before it sends an opening order.
    ///
    /// COLD: one call per order, never per market message.
    pub(super) fn symbol_denial(&self, request: &vike_model::OrderRequest) -> Option<&'static str> {
        let symbol = request.symbol.as_str();
        (self.gate.limits.deny_unadmitted_symbols
            && !symbol.is_empty()
            && !self.accepts_symbol(symbol))
        .then_some("symbol-not-admitted")
    }

    /// Signed size of one leg (`Both` = the one-way/spot leg; hedge callers pass `Long`/`Short`).
    pub fn position_size(&self, position_side: PositionSide) -> f64 {
        self.position_size_of(&self.symbol.clone(), position_side)
    }

    /// Per-symbol twin of [`ExecutionEngine::position_size`] (multi-mount).
    pub fn position_size_of(&self, symbol: &str, position_side: PositionSide) -> f64 {
        self.account
            .positions
            .get(&(ustr::Ustr::from(self.venue.as_str()), ustr::Ustr::from(symbol), position_side))
            .map(|p| p.size)
            .unwrap_or(0.0)
    }

    /// The signed position size the PRE-TRADE GATE's `RiskContext` carries — hedge-mode-aware,
    /// unlike a bare `position_size(PositionSide::Both)`, which sees 0 on a venue reporting
    /// hedge-mode `LONG`/`SHORT` buckets, so `is_covered_reduce`/`is_implicit_reduce` would judge
    /// every hedge-mode close as OPENING and DENY below-min flattens (the #458 anti-stranding floor
    /// bypass).
    ///
    /// Law: the one-way `BOTH` bucket when non-zero (hedge buckets never consulted); otherwise the
    /// NET of the hedge buckets (`LONG + SHORT`, signed as the venue recon parsers fold them:
    /// LONG ≥ 0, SHORT ≤ 0). NET is the honest one-number answer for a request that cannot name a
    /// bucket — [`vike_model::OrderRequest`] carries no `position_side`, so which bucket a
    /// hedge-mode order reduces is the VENUE's decision; it errs conservative (a both-buckets
    /// account nets toward 0, so coverage-gated bypasses stay off) while a single-bucket hedge
    /// account gets exactly its bucket size.
    ///
    /// `pub` for `vike_core`'s core-held reduce-only stop, which caps its fire to THIS number so the
    /// order it sends is judged covered on merit, by the same figure the gate judges it with.
    pub fn gate_position_size(&self, symbol: &str) -> f64 {
        let both = self.position_size_of(symbol, PositionSide::Both);
        if both != 0.0 {
            return both;
        }
        self.position_size_of(symbol, PositionSide::Long)
            + self.position_size_of(symbol, PositionSide::Short)
    }

    /// The position size a `reduce_only` order's COVERAGE is judged against: the book's own
    /// [`Self::gate_position_size`], or - only when that is FLAT and the order is flagged
    /// `reduce_only` - the venue's net position for `symbol` from the last reconcile pass
    /// ([`Self::venue_reported_positions`]).
    ///
    /// ⚠ Why it cannot be wider. `is_covered_reduce` (the one predicate the gate's account-exposure
    /// bypass reads) still demands DIRECTION and COVERAGE, so a venue position of the SAME sign as
    /// the order, or smaller than its qty, covers nothing, and an order that is not flagged
    /// `reduce_only` never borrows the evidence: a reduce-only order cannot grow exposure at the
    /// venue (binance and bybit reject it), and the evidence says there is something to reduce.
    /// ⚠ The evidence is at most one pass old; an engine that has since closed the position
    /// locally still reads it, and the venue is then the backstop (it rejects a reduce-only order
    /// on a flat book) - ONLY on a venue whose adapter puts `reduce_only` on the wire
    /// (`vike_venues::caps_for(..).supports_reduce_only`; alpaca, ibkr and every venue the table does
    /// not know carry no such backstop, so they borrow NOTHING: fail closed).
    /// ⚠ The evidence is NETTED: the remaining quantity of every OTHER live reduce-only order on
    /// this symbol and SIDE (`judging` is the order being judged, excluded - its own coid, so an
    /// AMEND and the registry walks do not count it against itself) is already spoken for, so two
    /// reduce-only SELL 5 orders are not both "covered" by one +5. Documented, not hidden:
    /// `docs/ops/reconcile-on-restart.md`.
    pub(super) fn cover_position_size(
        &self,
        symbol: &str,
        reduce_only: bool,
        side: i32,
        judging: &str,
    ) -> f64 {
        let own = self.gate_position_size(symbol);
        if own != 0.0 || !reduce_only {
            return own;
        }
        if !vike_venues::caps_for(&self.venue).supports_reduce_only {
            return 0.0;
        }
        let venue = self
            .venue_reported_positions
            .get(symbol)
            .copied()
            .filter(|q| q.is_finite())
            .unwrap_or(0.0);
        if venue * side as f64 >= 0.0 {
            return venue; // same sign: covers nothing anyway (`is_covered_reduce` decides)
        }
        let spoken_for: f64 = self
            .registry
            .iter()
            .filter(|(coid, mo)| {
                coid.as_str() != judging
                    && mo.status.is_live()
                    && mo.request.reduce_only
                    && mo.request.side == side
                    && mo.request.symbol == symbol
            })
            .map(|(_, mo)| mo.request.qty - mo.filled_qty)
            .filter(|r| r.is_finite() && *r > 0.0)
            .sum();
        let net = venue + side as f64 * spoken_for;
        if net * venue <= 0.0 { 0.0 } else { net }
    }

    /// Gross notional across ALL legs of this engine's symbol at the current mark — SUMS abs()
    /// values, never nets LONG against SHORT. 0.0 if no mark recorded yet.
    pub fn total_exposure(&self) -> f64 {
        let mark = self.account.mark_of(&self.venue, &self.symbol).unwrap_or(0.0);
        // py_sum, insertion order
        vike_model::py_sum(
            self.account
                .positions
                .iter()
                .filter(|((v, s, _ps), _)| *v == self.venue && *s == self.symbol)
                .map(|(_, pos)| pos.size.abs() * mark),
        )
    }

    /// Per-symbol latest recorded mark (pub for the runtime's per-mount ctx).
    pub fn mark_of(&self, symbol: &str) -> f64 {
        self.account.mark_of(&self.venue, symbol).unwrap_or(0.0)
    }

    // --- mark health: which open positions can't be valued, and why ---

    /// Open positions this engine has NO usable price for — resolver-`Missing` even with the
    /// last-known floor (the cell is empty or every slot is dead). Each `(venue, symbol,
    /// position_side)` is one leg; FLAT legs (size 0, the entry a closed position leaves behind)
    /// are excluded, as in the snapshot's `missing_price_instruments`. Read-only; cold path only.
    ///
    /// Independent of [`crate::price_board::PriceCfg::stale_fallback`]: a position priced off a
    /// stale-but-present value is NOT missing — it is reported by [`Self::stale_marks`] — so the
    /// two sets never overlap.
    pub fn missing_marks(
        &self,
        cfg: &crate::price_board::PriceCfg,
    ) -> Vec<(String, String, PositionSide)> {
        self.account
            .positions
            .iter()
            .filter(|(_k, p)| p.size != 0.0)
            .filter(|((v, s, _side), p)| {
                matches!(
                    self.price_board.classify(v, s, p.size >= 0.0, self.now_ms, cfg),
                    crate::price_board::MarkStatus::Missing
                )
            })
            .map(|((v, s, side), _p)| (v.to_string(), s.to_string(), *side))
            .collect()
    }

    /// Open positions this engine is valuing off a STALE last-known price — priced, but every slot
    /// is past its freshness window (only possible once a caller sets a freshness window on `cfg`;
    /// empty under the permissive default). Each [`StaleMark`] carries the source and age so an
    /// operator sees "we're marking this off a 4-minute-old trade". Excludes FLAT legs, like
    /// [`Self::missing_marks`]. Independent of `stale_fallback` — staleness is reported whether or
    /// not valuation is configured to fall back to it. Read-only; cold path only.
    pub fn stale_marks(&self, cfg: &crate::price_board::PriceCfg) -> Vec<StaleMark> {
        self.account
            .positions
            .iter()
            .filter(|(_k, p)| p.size != 0.0)
            .filter_map(|((v, s, side), p)| {
                match self.price_board.classify(v, s, p.size >= 0.0, self.now_ms, cfg) {
                    crate::price_board::MarkStatus::Stale { source, age_ms, .. } => {
                        Some(StaleMark {
                            venue: v.to_string(),
                            symbol: s.to_string(),
                            position_side: *side,
                            source,
                            age_ms,
                        })
                    }
                    _ => None,
                }
            })
            .collect()
    }

    // --- net-exposure queries (resolver-priced, one-price law) ---

    /// SIGNED net notional exposure across this engine's open positions (long +, short −),
    /// resolver-priced through the SAME chain [`Self::resolved_equity`] uses (the one-price law):
    /// [`crate::Account::net_notional_priced`] with [`Self::resolved_position_price`] as the price
    /// input. An unpriceable position is excluded exactly as it is from equity/margin. Read-only;
    /// cold / per-order path only.
    pub fn net_exposure(&self, cfg: &crate::price_board::PriceCfg) -> f64 {
        self.account
            .net_notional_priced(|(v, s, _side), p| self.resolved_position_price(v, s, p.size, cfg))
    }

    /// GROSS notional exposure across this engine's open positions (Σ |notional|, never netting a
    /// long against a short), resolver-priced like [`Self::net_exposure`]. Read-only; cold path.
    pub fn gross_exposure(&self, cfg: &crate::price_board::PriceCfg) -> f64 {
        self.account.gross_notional_priced(|(v, s, _side), p| {
            self.resolved_position_price(v, s, p.size, cfg)
        })
    }

    /// Per-symbol `(symbol, net_qty, net_notional)` exposure across this engine's open positions,
    /// aggregating hedge legs of the same symbol. `net_qty` is the signed size sum (no price);
    /// `net_notional` is the signed resolver-priced notional — an unpriceable leg adds 0 to the
    /// notional while its qty still counts. Symbols in `positions` insertion order. Read-only; cold
    /// path only — the GUI's per-symbol exposure row.
    pub fn exposure_by_symbol(
        &self,
        cfg: &crate::price_board::PriceCfg,
    ) -> Vec<(String, f64, f64)> {
        let mut out: IndexMap<String, (f64, f64)> = IndexMap::new();
        for ((v, s, _side), p) in self.account.positions.iter() {
            if p.size == 0.0 {
                continue;
            }
            let e = out.entry(s.to_string()).or_insert((0.0, 0.0));
            e.0 += p.size;
            if let Some(px) = self.resolved_position_price(v, s, p.size, cfg) {
                e.1 += self.account.contract_value_of(s).signed_notional(p.size, px);
            }
        }
        out.into_iter().map(|(s, (q, n))| (s, q, n)).collect()
    }
}
