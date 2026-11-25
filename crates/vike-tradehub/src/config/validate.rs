//! `ProfileDaemon`'s refusals: everything a mount cannot recover from, checked at profile LOAD
//! (`validate`, its `[[mounts]]` half `validate_multi`, the `[strategy]` half `validate_strategy`,
//! the Rhai half `validate_script`) and the live-arming gate (`validate_for_live`).
//!
//! Split out of `config.rs`'s one `impl ProfileDaemon` by concern. `use super::*` brings in the
//! parent module's imports and items. Load is PURE over the profile text: the strategy file itself
//! is read and judged at resolve (`config/mount.rs`'s `ProfileDaemon::resolve_mount`).

use super::mount::no_file_refusal;
use super::*;
use crate::refusals::MountProfileRefusal;

/// **What a LIVE profile is judged against beyond its own text** ([`ProfileDaemon::validate_for_live`]): the
/// account rows the node mounts from (each venue's DEFAULT account states the symbol its engine is mounted on,
/// `vike_config::account_arming::account_symbol`), and each venue's KEPT instrument list
/// (`<project>/instruments/<venue>.json`, which only the datahub writes). Owner, 2026-11-13: no symbol is compiled
/// in, so both are facts of the box.
pub struct LiveBox<'a> {
    /// The account rows, as the node reads them at start.
    pub accounts: &'a vike_secrets::account_directory::AccountDirectory,
    /// `Ok` when `symbol` is on `venue`'s kept list, else why not (no list, or not on it), in words.
    pub kept: &'a dyn Fn(&str, &str) -> Result<(), String>,
}

impl ProfileDaemon {
    /// Everything a mount cannot recover from, checked at LOAD:
    ///
    /// 1. a non-empty `symbol` — the mount routes and fills nothing without one;
    /// 2. a `[strategy]` table naming a strategy FILE by a path the strategies folder can hold
    ///    ([`Self::validate_strategy`]): no default strategy exists, and the retired `name` and
    ///    `rhai` keys are refused with the one-line fix;
    /// 3. under a `.rhai` file, every `[strategy.params]` value a NUMBER ([`Self::validate_script`]).
    ///
    /// What the FILE declares — where it may run, which keys it reads, of which type — is judged at
    /// resolve, which reads the file ([`Self::resolve_mount`]).
    ///
    /// Three callers: [`ProfileDaemon::from_toml_str`] after the parse, [`Self::validate_multi`]
    /// per lowered `[[mounts]]` row, and `crate::mount_factory`'s `profile_for` on the one-mount
    /// profile it builds from a runtime `MountPlacement` — each a TYPED profile by then, so all three
    /// meet the same refusals.
    pub(crate) fn validate(&self) -> Result<(), String> {
        if !self.mounts.is_empty() {
            return self.validate_multi();
        }
        // The maker's knobs are its own params table (step 10, Q4): the old top-level fields stop
        // being read, so a profile still setting one is refused by name rather than mounting a maker
        // that ignores it.
        let retired: Vec<&str> = [
            ("qty", self.qty.is_some()),
            ("half_spread", self.half_spread.is_some()),
            ("resolution_ts_ms", self.resolution_ts_ms.is_some()),
        ]
        .into_iter()
        .filter_map(|(k, set)| set.then_some(k))
        .collect();
        if !retired.is_empty() {
            return Err(
                MountProfileRefusal::RetiredMakerFields { set: retired.join(", ") }.to_string()
            );
        }
        if self.symbol.is_none() {
            return Err(MountProfileRefusal::NoSymbol.to_string());
        }
        if self.mount_symbol().trim().is_empty() {
            return Err(MountProfileRefusal::EmptySymbol.to_string());
        }
        // The mount's own leverage cap: the store's `CHECK` holds the floor on a row, this holds it on
        // every spelling (and refuses a non-finite figure the column could hold).
        if let Some(cap) = self.max_leverage.filter(|cap| !(cap.is_finite() && *cap >= 1.0)) {
            return Err(MountProfileRefusal::MaxLeverageInvalid { cap }.to_string());
        }
        // One instrument, one name (the C-D4 follow-up, decision 0290): a mount symbol whose kept row's user name is
        // written another way (`btcusdt.p` for `BTCUSDT.P`) is refused, not respelled, because the strategy's own
        // orders and params name the mount as written.
        if let Some(why) = vike_catalog::respelled_symbol(self.venue(), self.mount_symbol()) {
            return Err(MountProfileRefusal::SymbolRespelled { why }.to_string());
        }
        // `[inputs]` (decision 0271): both names of each row in the input grammar, a one-part name this
        // mount's own symbol at that bar length: the backtest profile's one check (`unparsed_input_rows`), in
        // its words; the first refused row refuses the load.
        let own = vike_data::store::series::SeriesOwn {
            venue: self.venue(),
            symbol: self.mount_symbol(),
            lane: None,
        };
        let refused = vike_data::store::series::unparsed_input_rows(&self.inputs, Ok(own));
        if let Some((name, read, e)) = refused.into_iter().next() {
            let (name, read) = (name.to_string(), read.to_string());
            return Err(MountProfileRefusal::InputRowUnparsed { name, read, why: e }.to_string());
        }
        // The data-only declaration is meaningful ONLY where the venue's market data is
        // credentialed (see `crate::feeds::data_only_feed`'s doc): on a keyless-data venue the data-only mount
        // already exists — withhold the venue's credentials from the store — and a declaration
        // here could only re-state the live gate while misleading the arming disclosure.
        if self.data_only_effective() && !crate::feeds::data_only_feed(self.venue()) {
            return Err(MountProfileRefusal::DataOnlyKeylessVenue {
                venues: crate::feeds::data_only_venues().collect::<Vec<_>>().join(", "),
                venue: self.venue().to_string(),
                upper: self.venue().to_uppercase(),
            }
            .to_string());
        }
        self.validate_strategy()
    }

    /// The `[[mounts]]` half of [`Self::validate`] (split-plane I10). THREE refusals, each a way
    /// the profile would otherwise be silently wrong:
    ///
    /// 1. **Both spellings set.** Any top-level mount field beside a `[[mounts]]` array would
    ///    configure NOTHING while reading as real — the declared-but-unread failure. Refused
    ///    naming every offending key.
    /// 2. **A row that fails the single-mount refusals.** Each row lowers to a single-mount
    ///    profile ([`Self::mount_rows`]) and runs the ENTIRE existing `validate` — the required
    ///    symbol, the strategy-file selection, the Rhai numeric rule — so a
    ///    `[[mounts]]` row cannot mount anything a single-mount profile could not. The error names
    ///    the row (`mounts[i]`).
    /// 3. **Two rows deriving ONE mount id** ([`Self::derived_controller_id`]). The runtime
    ///    PANICS on a duplicate controller id (`assemble_core` — a shared id silently shares
    ///    durable state), so the duplicate is refused HERE, at load, naming both rows, never at
    ///    the panic.
    fn validate_multi(&self) -> Result<(), String> {
        let set_besides: Vec<&str> = [
            ("venue", self.venue.is_some()),
            ("asset_class", self.asset_class.is_some()),
            ("symbol", self.symbol.is_some()),
            ("strategy", self.strategy.is_some()),
            ("resolution_ts_ms", self.resolution_ts_ms.is_some()),
            ("interval", self.interval.is_some()),
            ("interval_ms", self.interval_ms.is_some()),
            ("qty", self.qty.is_some()),
            ("half_spread", self.half_spread.is_some()),
            ("tick_size", self.tick_size.is_some()),
            ("seed_cash", self.seed_cash.is_some()),
            ("data_only", self.data_only.is_some()),
            ("max_leverage", self.max_leverage.is_some()),
        ]
        .into_iter()
        .filter_map(|(k, set)| set.then_some(k))
        .collect();
        if !set_besides.is_empty() {
            return Err(
                MountProfileRefusal::BothSpellings { fields: set_besides.join(", ") }.to_string()
            );
        }
        // ⚠ TWO DECLARED PRIMARIES IS A LOAD REFUSAL, naming both rows — the file-side twin of the
        // store's `mount_one_primary_per_profile` partial unique index, so the two spellings cannot
        // disagree about how many primaries a profile may have. There is no defensible tie-break: a
        // primary is the daemon's singular identity (summary token, mode line, seed policy), and
        // picking the earlier of two declarations would silently reinstate exactly the
        // position-decides-it accident `primary` exists to remove.
        let declared: Vec<usize> = self
            .mounts
            .iter()
            .enumerate()
            .filter(|(_, m)| m.primary == Some(true))
            .map(|(i, _)| i)
            .collect();
        if declared.len() > 1 {
            return Err(MountProfileRefusal::TwoPrimaries {
                first: declared[0],
                second: declared[1],
            }
            .to_string());
        }
        let rows = self.mount_rows();
        for (i, row) in rows.iter().enumerate() {
            row.validate().map_err(|why| row_refused(i, row, why))?;
        }
        let ids: Vec<String> = rows.iter().map(ProfileDaemon::derived_controller_id).collect();
        for j in 1..ids.len() {
            if let Some(i) = (0..j).find(|&i| ids[i] == ids[j]) {
                return Err(MountProfileRefusal::SameMountId {
                    first: i,
                    second: j,
                    id: ids[j].clone(),
                }
                .to_string());
            }
        }
        // Rows sharing a VENUE must agree on the data-only verdict: the withhold is per venue
        // account (one exec engine, one credential set), so "row A trades live while row B is
        // data-only" is not a state `live_runtime` can construct — refused here, naming both rows,
        // rather than silently resolved in favour of whichever row is read first.
        for j in 1..rows.len() {
            if let Some(i) = (0..j).find(|&i| {
                rows[i].venue() == rows[j].venue()
                    && rows[i].data_only_effective() != rows[j].data_only_effective()
            }) {
                return Err(MountProfileRefusal::DataOnlyDisagrees {
                    first: i,
                    second: j,
                    venue: rows[j].venue().to_string(),
                }
                .to_string());
            }
        }
        Ok(())
    }

    /// The `[strategy]` half of [`Self::validate`]: WHICH strategy file, by a path the strategies
    /// folder can hold. PURE: whether the file exists, what its header declares and whether the
    /// params table fits it are [`Self::resolve_mount`]'s, which reads the file.
    ///
    /// ⚠ **A mount without a file is REFUSED, naming the folder**: no strategy is compiled into this
    /// program, so there is nothing to default to (the owner's law). The two retired selection keys
    /// are refused before the file is looked at, each with the one-line fix, and nothing rewrites
    /// the stored profile: `name = "…"` named a strategy compiled into the program, and
    /// `rhai = "<path>"` a script anywhere on the box.
    fn validate_strategy(&self) -> Result<(), String> {
        let Some(s) = &self.strategy else { return Err(no_file_refusal()) };
        if let Some(name) = s.name.as_deref() {
            return Err(MountProfileRefusal::NameRetired { name: name.to_string() }.to_string());
        }
        if let Some(path) = s.rhai.as_deref() {
            return Err(MountProfileRefusal::RhaiKeyRetired { path: path.to_string() }.to_string());
        }
        let Some(file) = s.file.as_deref() else { return Err(no_file_refusal()) };
        // The path rule alone (relative, no `..`, `.rs` or `.rhai`): no root is read at load.
        vike_strategy::file::scan::resolve(std::path::Path::new(""), file)?;
        if self.strategy_language() == Some(vike_strategy::file::scan::StrategyFileLanguage::Rhai) {
            Self::validate_script(s)?;
        }
        Ok(())
    }

    /// The `.rhai` half of [`Self::validate`] — the PURE checks only, because this runs at profile
    /// LOAD and load is filesystem-free by contract (the docs-profiles CI gate parses every shipped
    /// profile on runners that have no strategies folder). Reading, hashing and compiling the script
    /// — and refusing an override key the script never asks for, which needs the script TEXT —
    /// happen at [`Self::resolve_mount`], still before any core spawns.
    ///
    /// What IS refusable here is the value-type half of the daemon's no-silently-ignored-key
    /// posture, in the script vocabulary: every `[strategy.params]` value of a `.rhai` file is a
    /// `param(name, default)` override, `param` yields an `f64`, and the override map is built by
    /// a NUMERIC filter (the same lenient reader convention the backtest's script arm uses) — so a
    /// quoted number (`size = "2"`) would be silently DROPPED and the script would run its own
    /// compiled default while the profile states otherwise. That is the mistyped-params hazard,
    /// refused here with the same posture.
    fn validate_script(s: &ProfileStrategy) -> Result<(), String> {
        let non_numeric: Vec<String> = s
            .params
            .iter()
            .filter(|(_, v)| v.as_f64().is_none())
            .map(|(k, v)| format!("{k} ({})", v.type_str()))
            .collect();
        if !non_numeric.is_empty() {
            return Err(MountProfileRefusal::RhaiParamsNotNumbers {
                values: non_numeric.join(", "),
            }
            .to_string());
        }
        Ok(())
    }

    /// Safety gate #5: may this profile be armed LIVE? Called from `main` ONLY when the live gate is
    /// on; the paper path never consults it (a paper profile can name any venue/symbol).
    ///
    /// THREE questions, in order, each with its own authority — it used to be one hardcoded table of
    /// `(venue, symbol)` PAIRS, which conflated them and duplicated `assemble_runtime`'s own symbol
    /// literals:
    ///
    /// 1. **Is the VENUE live-wired in this build?** `crate::feeds::FEED_ARMS` — the venues `live_runtime`
    ///    has a market-feed arm for. This is the gate that stops a silent widening, and it is the
    ///    one an operator can reason about ("did somebody wire polymarket?").
    /// 2. **Would that venue's engine ACCEPT this symbol?** The venue must have an engine row
    ///    (`crates/vike-venues/src/venue_wiring.rs`'s `VENUE_WIRING`), and an account-less row must name the symbol
    ///    the venue's DEFAULT account's row STATES (`account.symbol`, read through [`LiveBox::accounts`]): the
    ///    engine `assemble_runtime` mounts for that account. ⚠ This is not pedantry:
    ///    `vike_tradehub_core::make_engine` sets no `extra_symbols`, so
    ///    [`vike_execution::Engine::accepts_symbol`] is a plain equality test and a foreign
    ///    symbol's orders and fills are SILENTLY DROPPED — the strategy quotes, the daemon logs
    ///    nothing, and nothing ever trades. An account-wide venue (polymarket: exec/fills/reconcile key off
    ///    the wallet, tokens resolve per order) mounts no symbol, so any symbol passes; a default account that
    ///    states none does not run, and the node refuses the mount by name
    ///    (`vike_tradehub_core`'s `refuse_unarmed_mount_accounts`).
    /// 3. **Venue-specific SHAPE.** Polymarket outcome ids are dynamic ERC-1155 ids (a long DECIMAL
    ///    string), not a fixed symbol, so its account-wide row cannot answer (2) — gate on shape
    ///    (`vike_catalog::is_outcome_token`: 20 to 78 ASCII digits) instead. This is what keeps the shipped paper default (`symbol =
    ///    "TOK"`) un-armable. A hyperliquid symbol must be on the venue's KEPT list
    ///    ([`LiveBox::kept`]; owner, 2026-11-13: any symbol of it is admitted, and no other).
    ///
    /// Extending the daemon to another venue remains a deliberate edit HERE (one
    /// `FEED_ARMS` row, which IS the arm) — never a silent widening.
    /// Two gates, each over a different pair: `every_feed_arm_venue_is_mounted_by_assemble_runtime`
    /// (`FEED_ARMS` ⊆ `vike_tradehub_core::wired_markets`, i.e. an ENGINE exists) and
    /// `every_unwired_venue_is_still_refused` (its converse over that table). A third gate over
    /// the feed arms is not needed: a `FEED_ARMS` row IS the arm, so no second list can disagree.
    pub fn validate_for_live(&self, live: &LiveBox<'_>) -> Result<(), String> {
        // Whether the strategy FILE may trade live is its header's `runs_on`, judged where the file
        // is read (`ProfileDaemon::resolve_mount`, with the live gate). A `[[mounts]]` profile arms live only when EVERY row does — the same per-row refusals,
        // each failure naming its row (split-plane I10).
        if !self.mounts.is_empty() {
            for (i, row) in self.mount_rows().iter().enumerate() {
                row.validate_for_live(live).map_err(|why| row_refused(i, row, why))?;
            }
            return Ok(());
        }
        let venue = self.venue();
        let symbol = self.mount_symbol();
        if !crate::feeds::has_feed_arm(venue) {
            return Err(MountProfileRefusal::VenueNotLiveWired {
                venue: venue.to_string(),
                arms: crate::feeds::feed_arm_list(),
            }
            .to_string());
        }
        // (2) routing. `assemble_runtime` mounts the engines from `vike_model`'s `VENUE_WIRING`, so that
        // table answers — and every venue past step (1) is mounted in every build that has its feed
        // arm (`every_feed_arm_venue_is_mounted_by_assemble_runtime`).
        if vike_venues::venue_wiring::mount_of(venue).is_none() {
            return Err(
                MountProfileRefusal::LiveVenueNoEngine { venue: venue.to_string() }.to_string()
            );
        }
        // ⚠ THE DEFAULT ACCOUNT ONLY. `assemble_runtime` mounts a venue's DEFAULT engine on the symbol
        // that account's row STATES and sets no `extra_symbols`, so an account-less row naming
        // anything else is silently dropped — that refusal is unchanged, and it is why this daemon
        // can still reach exactly one instrument per venue on the default account.
        //
        // A row naming an ACCOUNT is a different question with a different answer:
        // `vike_tradehub_core::account_symbols_for` mounts that account's engine on THIS ROW'S OWN symbol, so
        // `accepts_symbol` answers about the symbol the row named and there is nothing to refuse.
        // That is how a second instrument is reached, and refusing it here would have made the
        // account field unusable for the one thing it is for.
        let default_account = self.account.as_ref().is_none_or(|l| l.is_default());
        let stated = vike_config::account_arming::account_symbol(
            Some(live.accounts),
            venue,
            &vike_model::accounts::account_keys::AccountLabel::Default,
        );
        if default_account
            && vike_venues::venue_wiring::mounts_a_symbol(venue)
            && let Some(stated) = stated.filter(|stated| *stated != symbol)
        {
            return Err(MountProfileRefusal::DefaultAccountSymbolDiffers {
                venue: venue.to_string(),
                stated: stated.to_string(),
                symbol: symbol.to_string(),
            }
            .to_string());
        }
        // (3) venue SHAPE. Feature-free on purpose (profile shape only): the REAL capability gate
        // for polymarket is the `polymarket` cargo feature, without which `live_runtime` hard-errors.
        if venue == "polymarket" && !vike_catalog::is_outcome_token(symbol) {
            return Err(
                MountProfileRefusal::NotAnOutcomeToken { symbol: symbol.to_string() }.to_string()
            );
        }
        // A hyperliquid mount's symbol must be one the venue lists (the kept list; owner, 2026-11-13): the
        // feed subscribes it and the engine is mounted on it, so a coin the venue does not list would
        // start an engine nothing prices.
        if venue == "hyperliquid" {
            (live.kept)(venue, symbol).map_err(|why| {
                MountProfileRefusal::SymbolNotKept { symbol: symbol.to_string(), why }.to_string()
            })?;
        }
        // (4) the DRAWDOWN LATCH needs a capital base. `live_runtime` hardcodes
        // `CoreConfig::max_drawdown = Some(0.25)`, and `CoreThread::sweep_drawdown_latch` measures
        // that 25% as a fraction of `Σ seed_cash + own PnL` — configured capital, deliberately NOT
        // the venue's wallet, which on a shared account is not this daemon's money. So a
        // `seed_cash = 0` here leaves the live daemon's one automatic liquidate-only trip with no
        // denominator and it can never arm. Refused rather than warned: an operator reading a
        // profile that says nothing about drawdown believes the compiled-in 25% is protecting them.
        // (`seed_cash` is unset in most profiles, and its default is a positive 1000 — so this
        // fires only on an explicit zero/negative/NaN.)
        if let Some(seed) = self.seed_cash {
            // ⚠ Spelled `!is_finite() || <= 0.0` and NOT `<= 0.0`, which is what
            // `clippy::neg_cmp_op_on_partial_ord` suggests for the equivalent `!(seed > 0.0)`. Every
            // comparison against NaN is false, so `NaN <= 0.0` is FALSE and a NaN capital base would
            // sail through the guard that exists to stop exactly that. The lint is right that
            // `!(a > b)` is a smell on a partial order; the cure is to say which non-orderable values
            // are meant, not to drop them.
            //
            // This also refuses an INFINITE base, which `!(seed > 0.0)` accepted — deliberately, and
            // it is the same hazard: an infinite denominator makes the drop-fraction 0.0 forever, so
            // the latch can never arm. Nothing legitimate sets it.
            if !seed.is_finite() || seed <= 0.0 {
                return Err(MountProfileRefusal::SeedCashDisarmsLatch { seed }.to_string());
            }
        }
        Ok(())
    }
}

/// One `[[mounts]]` row's refusal `why`, naming row `index` (`validate_multi` and `validate_for_live` alike).
fn row_refused(index: usize, row: &ProfileDaemon, why: String) -> String {
    let (venue, symbol) = (row.venue().to_string(), row.mount_symbol().to_string());
    MountProfileRefusal::RowRefused { index, venue, symbol, why }.to_string()
}
