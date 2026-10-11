//! `DaemonProfile`'s refusals: everything a mount cannot recover from, checked at profile LOAD
//! (`validate`, its `[[mounts]]` half `validate_multi`, the `[strategy]` half `validate_strategy`,
//! the Rhai half `validate_script`) and the live-arming gate (`validate_for_live`).
//!
//! Split out of `config.rs`'s one `impl DaemonProfile` by concern (behaviour byte-identical; the
//! methods moved verbatim). `use super::*` brings in the parent module's imports and items, so
//! nothing about resolution changes.

use super::*;

impl DaemonProfile {
    /// Everything a mount cannot recover from, checked at LOAD:
    ///
    /// 1. exactly one of `symbol` / `token_id`, non-empty — the mount routes and fills nothing
    ///    without a symbol, and two disagreeing spellings have no defensible winner;
    /// 2. `[strategy]`, if present, names exactly ONE thing to mount (`name` XOR `rhai` — the same
    ///    mutual-exclusion shape as (1), and for the same reason: two disagreeing selections have
    ///    no defensible winner, and a table with neither selects nothing for its params to
    ///    configure);
    /// 3. `[strategy].name`, if set, is a strategy this daemon can actually MOUNT AND TRADE;
    /// 4. `[strategy.params]`, if present, contains only keys that strategy actually READS, each
    ///    carrying a value its reader can TAKE and — for the keys that name a market — naming THIS
    ///    mount's own `(venue, symbol)`; see `DaemonProfile::validate_strategy` for why each of the
    ///    three is a live hazard rather than a tidiness question. Under `rhai` the same posture in
    ///    the script vocabulary: every value must be a NUMBER (see [`Self::validate_script`]).
    ///
    /// (3) is the honest half. `vike_strategy::capability` distinguishes four answers, and three of
    /// them are failures with DIFFERENT causes: a typo, a simulator-only strategy (it exists, but
    /// only inside `vike-backtest` — this daemon deliberately does not link the simulator), and a
    /// strategy that RESOLVES but whose input never arrives live (a funding reader with no live
    /// funding series; a two-leg strategy with no second mounted leg). That third class is the one
    /// worth failing loudly for: it mounts cleanly, logs nothing unusual and simply never submits —
    /// the live-vs-backtest divergence shape an operator discovers days later.
    pub(super) fn validate(&self) -> Result<(), String> {
        if !self.mounts.is_empty() {
            return self.validate_multi();
        }
        match (self.symbol.as_deref(), self.token_id.as_deref()) {
            (Some(_), Some(_)) => {
                return Err(
                    "set `symbol` OR `token_id`, not both (they name the same field)".to_string()
                );
            }
            (None, None) => {
                return Err("a mount symbol is required: set `symbol` (or, on polymarket, \
                            `token_id`)"
                    .to_string());
            }
            _ => {}
        }
        if self.mount_symbol().trim().is_empty() {
            return Err("the mount symbol must not be empty".to_string());
        }
        // The data-only declaration is meaningful ONLY where the venue's market data is
        // credentialed (see `DATA_ONLY_VENUES`' doc): on a keyless-data venue the data-only mount
        // already exists — withhold the venue's credentials from the store — and a declaration
        // here could only re-state the live gate while misleading the arming disclosure.
        if self.data_only_effective() && !DATA_ONLY_VENUES.contains(&self.venue()) {
            return Err(format!(
                "`data_only = true` is only meaningful for a venue whose MARKET DATA is \
                 credentialed ({}), where the same credentials would otherwise arm exec; {} has a \
                 keyless data plane, so its data-only mount is simply \"no {} credentials in the \
                 store\" (absent credentials ARE the live gate). Drop the key",
                DATA_ONLY_VENUES.join(", "),
                self.venue(),
                self.venue().to_uppercase()
            ));
        }
        if let Some(s) = &self.strategy {
            match (s.name.as_deref(), s.rhai.as_deref()) {
                (Some(_), Some(_)) => {
                    return Err("set `[strategy] name` OR `[strategy] rhai`, not both: they each \
                                select the whole strategy, and two disagreeing selections have no \
                                defensible winner"
                        .to_string());
                }
                (None, None) => {
                    return Err("a `[strategy]` table must say WHAT to mount: set \
                                `name = \"<registry strategy>\"` or `rhai = \"<path to .rhai \
                                script>\"` (or drop the table for the default A-S maker) — a \
                                table with neither selects nothing for its `[strategy.params]` to \
                                configure"
                        .to_string());
                }
                (Some(name), None) => self.validate_strategy(name, s)?,
                (None, Some(path)) => Self::validate_script(path, s)?,
            }
        }
        Ok(())
    }

    /// The `[[mounts]]` half of [`Self::validate`] (split-plane I10). THREE refusals, each a way
    /// the profile would otherwise be silently wrong:
    ///
    /// 1. **Both spellings set.** Any top-level mount field beside a `[[mounts]]` array would
    ///    configure NOTHING while reading as real — the declared-but-unread failure. Refused
    ///    naming every offending key.
    /// 2. **A row that fails the single-mount refusals.** Each row lowers to a single-mount
    ///    profile ([`Self::mount_rows`]) and runs the ENTIRE existing `validate` — symbol
    ///    mutual-exclusion, the strategy-capability gate, all four params refusals — so a
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
            ("token_id", self.token_id.is_some()),
            ("strategy", self.strategy.is_some()),
            ("resolution_ts_ms", self.resolution_ts_ms.is_some()),
            ("interval", self.interval.is_some()),
            ("interval_ms", self.interval_ms.is_some()),
            ("qty", self.qty.is_some()),
            ("half_spread", self.half_spread.is_some()),
            ("tick_size", self.tick_size.is_some()),
            ("seed_cash", self.seed_cash.is_some()),
            ("data_only", self.data_only.is_some()),
        ]
        .into_iter()
        .filter_map(|(k, set)| set.then_some(k))
        .collect();
        if !set_besides.is_empty() {
            return Err(format!(
                "this profile sets BOTH spellings: `[[mounts]]` AND top-level mount field(s) {}. \
                 With a `[[mounts]]` array every mount field lives inside its own row — a \
                 top-level knob beside the array would configure nothing while reading as real. \
                 Move the field(s) into a `[[mounts]]` row, or drop the array",
                set_besides.join(", ")
            ));
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
            return Err(format!(
                "mounts[{}] and mounts[{}] both set `primary = true` — at most one mount may be \
                 the primary, because the primary IS the daemon's singular identity (its summary \
                 token, its mode line and its seed policy all read that one row). Drop the key \
                 from every row but one; dropping it from all of them keeps the historical answer, \
                 which is the FIRST row",
                declared[0], declared[1]
            ));
        }
        let rows = self.mount_rows();
        for (i, row) in rows.iter().enumerate() {
            row.validate().map_err(|e| {
                format!(
                    "mounts[{i}] (venue={:?}, symbol={:?}): {e}",
                    row.venue(),
                    row.mount_symbol()
                )
            })?;
        }
        let ids: Vec<String> = rows.iter().map(DaemonProfile::derived_controller_id).collect();
        for j in 1..ids.len() {
            if let Some(i) = (0..j).find(|&i| ids[i] == ids[j]) {
                return Err(format!(
                    "mounts[{i}] and mounts[{j}] derive the SAME mount id {:?} — venue, symbol, \
                     interval, strategy AND account all equal, so the two rows would share one \
                     durable-state sidecar and one journal attribution key (the runtime refuses \
                     that with a panic; this refusal is the load-time version that can name the \
                     rows). Make one row distinct — a different interval, symbol, strategy or \
                     `account` — or delete the duplicate. ⚠ Two rows on one venue and one symbol \
                     that differ by `account` are an ordinary SPREAD and are NOT this error: the \
                     account is part of the mount id, so they derive different ids",
                    ids[j]
                ));
            }
        }
        // Rows sharing a VENUE must agree on the data-only verdict: the withhold is per venue
        // account (one exec engine, one credential set), so "row A trades live while row B is
        // data-only" is not a state `live_mount` can construct — refused here, naming both rows,
        // rather than silently resolved in favour of whichever row is read first.
        for j in 1..rows.len() {
            if let Some(i) = (0..j).find(|&i| {
                rows[i].venue() == rows[j].venue()
                    && rows[i].data_only_effective() != rows[j].data_only_effective()
            }) {
                return Err(format!(
                    "mounts[{i}] and mounts[{j}] share venue {:?} but DISAGREE on `data_only` — \
                     the declaration withholds that venue's credentials from the ONE exec engine \
                     both rows share, so the two rows cannot have different answers. Set the same \
                     value on both (or drop the key from both)",
                    rows[j].venue()
                ));
            }
        }
        Ok(())
    }

    /// The `[strategy]` half of [`Self::validate`]: WHICH strategy, then WHAT it was handed.
    ///
    /// ⚠ **Four refusals over one table, and each one is the case the previous one passes.** A key
    /// no reader reads ([`vike_strategy::unknown_params`]); a key whose VALUE no reader can take
    /// ([`vike_strategy::mistyped_params`]); a key that is read, well-typed, and OVERRIDDEN BY THE
    /// MOUNT ([`vike_strategy::misrouted_params`]); and a whole TABLE that is spelled, typed and
    /// routed right and still describes no order at all ([`vike_strategy::unarmable_params`]). The
    /// first three end in the same place — the knob runs at something the profile does not state —
    /// and the third of them is the worst, because the knobs it covers name WHICH INSTRUMENT and
    /// WHICH VENUE real orders go to. MEASURED on the CI box before it existed: a `buy_hold` profile with
    /// `symbol = "MOUNTED_SYMBOL"` and `[strategy.params] symbol = "A_COMPLETELY_DIFFERENT_SYMBOL"`
    /// loaded, announced `symbol=A_COMPLETELY_DIFFERENT_SYMBOL`, and filled on `MOUNTED_SYMBOL`.
    ///
    /// The second half exists because a `from_params` reader CANNOT FAIL — every one in this
    /// workspace ignores a key it does not recognise, so `qtyy = 0.005` is not an error, it is `qty`
    /// at the strategy's compiled default. `deny_unknown_fields` on [`DaemonProfile`] makes a
    /// top-level typo fatal and stops at the `[strategy.params]` boundary, because the field is a
    /// free-form `toml::Value`. That asymmetry was safe while this table only fed the backtest
    /// simulator (a wrong number is a wrong chart); it is not safe now that the same table is the
    /// SIZE INPUT TO REAL ORDERS, where the compiled default may be hundreds of times the intended
    /// clip and the only thing behind it is the OPTIONAL `policy.max_notional_per_order`.
    ///
    /// ⚠ Deliberately the SAME strictness on the paper mount as on the live one. A paper rehearsal
    /// exists to predict the live mount, so a rehearsal that silently ran different parameters would
    /// conceal exactly what it is for — and two strictness levels over one table is how this daemon
    /// got two spellings of the A-S maker in the first place.
    ///
    /// ⚠ **An INERT KEY is a fifth member of the family, and it is deliberately NOT a refusal.** A
    /// key can pass all four checks above and still be read by nobody, because another key in the
    /// same table sent the strategy down a branch that never looks at it (`anchor_price` with
    /// `anchor` unset; `tick` outside a `bounded01` market). A refusal would be wrong for a reason
    /// a misroute does not share: a misrouted symbol is wrong under every configuration and
    /// unrecoverable at runtime, while such a key is armed from the SAME table — and one shipped
    /// caller supplies half of an inert pair as a matter of course
    /// (`crates/vike-strategy/src/strategies/trailing_scalper.rs`'s module doc: the batch tool passes
    /// `market_open_ms`/`market_close_ms` per run while the delay/cutoff knobs stay off by default),
    /// so a refusal would reject that tool's own documented default. Nothing annotates it either —
    /// see [`Self::effective_params`]. What keeps that class from growing is
    /// `crates/vike-strategy/tests/param_gates.rs`, which drives every strategy and fails when a
    /// declared key changes nothing.
    ///
    /// ⚠ **The fourth refusal is NOT that rule wearing a coat, and the difference is exactly the
    /// one above.** It refuses a table whose LADDER is empty, never a key whose value looks wrong:
    /// no missing line arms it (every key is present and no value of any other key rescues it), it
    /// rejects no shipped profile shape, and its consequence is the class this daemon most needs to
    /// fail loudly for — a mount that starts clean, announces a full configuration line and then
    /// never submits, discovered days later. `vike_strategy::unarmable_params`' own doc carries the
    /// argument, including why the tempting wider rule ("refuse a mount that would place no
    /// orders") is NOT safe: a strategy waiting on a market condition places none either, and no
    /// load-time check can tell the two apart without simulating a market.
    fn validate_strategy(&self, name: &str, s: &StrategyCfg) -> Result<(), String> {
        // The ONE registry name that is not mounted BY NAME here — and no longer a refusal of the
        // script path itself. Scripts ARE live-mountable on this daemon
        // (`docs/decisions/0024-rhai-strategies-live.md`), through the `rhai = "<path>"` spelling;
        // the `name = "rhai"` arm stays the BACKTEST's, because it takes the script as an inline
        // `src` param and lives in `vike-backtest`'s registry (`vike-script` declares the same
        // layer rank as `vike-strategy`, so the shared registry cannot name it). Redirect rather
        // than letting `capability` call it simulator-only, which stopped being the whole truth
        // the day the reversal landed.
        if name == "rhai" {
            return Err("scripts mount here by PATH, not by registry name: set `[strategy] \
                        rhai = \"<path to .rhai script>\"` (the `name = \"rhai\"` spelling is the \
                        backtest registry's inline-`src` arm, which this daemon does not link — \
                        see docs/decisions/0024-rhai-strategies-live.md)"
                .to_string());
        }
        match vike_strategy::capability(name) {
            Capability::Live => {}
            Capability::NotLive(why) => {
                return Err(format!(
                    "strategy {name:?} resolves but cannot trade on this daemon: {why}"
                ));
            }
            Capability::SimulatorOnly(why) => {
                return Err(format!(
                    "strategy {name:?} is simulator-only ({why}) — it backtests, but this daemon \
                         does not link the simulator and could not mount it"
                ));
            }
            // Not a built-in: the USER registry (compiled from user_data/strategies/rust — empty
            // in any checkout without one) is consulted LAST, so a user folder can never shadow a
            // built-in name. Live mounting keeps the LIVE_CAPABLE default-deny posture: a user
            // strategy trades here only if its own folder manifest opted in.
            Capability::Unknown if vike_user_strategies::USER_STRATEGIES.contains(&name) => {
                if !vike_user_strategies::USER_LIVE_CAPABLE.contains(&name) {
                    return Err(format!(
                        "user strategy {:?} resolves but cannot trade on this daemon: its \
                         folder's strategy.toml declares no `live = true` (the user-tier \
                         LIVE_CAPABLE opt-in)",
                        name
                    ));
                }
            }
            Capability::Unknown => {
                return Err(format!(
                    "unknown strategy {:?} (mountable here: {}{})",
                    name,
                    vike_strategy::LIVE_CAPABLE
                        .iter()
                        .filter(|(_, v)| v.is_live())
                        .map(|(n, _)| *n)
                        .collect::<Vec<_>>()
                        .join(", "),
                    if vike_user_strategies::USER_STRATEGIES.is_empty() {
                        String::new()
                    } else {
                        format!("; user: {}", vike_user_strategies::USER_STRATEGIES.join(", "))
                    },
                ));
            }
        }
        // WHAT it was handed. `[strategy.params]` is a free-form table by design (the registry is
        // shared with the backtest, whose strategies each read their own knobs), so the profile's
        // serde layer cannot judge it — but the registry can.
        let Some(table) = s.params.as_table() else {
            return Err(format!(
                "`[strategy.params]` must be a TABLE of keys, got {}",
                s.params.type_str()
            ));
        };
        if AS_MAKER_NAMES.contains(&name) {
            if !table.is_empty() {
                let keys: Vec<&str> = table.keys().map(String::as_str).collect();
                return Err(format!(
                    "strategy {:?} takes no `[strategy.params]` on this daemon (got: {}). It is \
                     built from the profile's OWN maker fields — `qty` / `half_spread` / \
                     `tick_size` / `resolution_ts_ms` at the top level — through the same \
                     `vike_mount::build_maker` call a profile with no `[strategy]` table makes, so \
                     the two spellings cannot differ. A-S knob tuning (`gamma`, `kappa_*`, \
                     `price_domain`, …) has NO profile surface here yet: these keys would have been \
                     silently dropped, along with the venue-selected price domain, which on a \
                     $-scale venue means a maker that never quotes.",
                    name,
                    keys.join(", ")
                ));
            }
        } else {
            let unknown = vike_strategy::unknown_params(name, &s.params);
            if !unknown.is_empty() {
                return Err(format!(
                    "strategy {:?} does not read these `[strategy.params]` keys: {}. A params \
                     reader IGNORES what it does not recognise, so each of them would mount at the \
                     strategy's COMPILED DEFAULT — a size knob among them is a live order at a size \
                     nobody typed. It reads: {}.",
                    name,
                    unknown.join(", "),
                    readable_keys(name)
                ));
            }
            // ...and the same hazard one level down: a key it DOES read, carrying a value of a type
            // it cannot take. `and_then(Value::as_…)` yields `None` for a wrong type exactly as it
            // does for an absent key, so the knob lands on the compiled default either way — but
            // this time the operator's own profile states a number and the daemon signs orders at
            // another. Quoting a number (`size = "2"`) is the ordinary way to get there, and it is
            // invisible to `deny_unknown_fields`, to `unknown_params`, and to the reader itself.
            let mistyped = vike_strategy::mistyped_params(name, &s.params);
            if !mistyped.is_empty() {
                return Err(format!(
                    "strategy {:?} was handed `[strategy.params]` values of the wrong TYPE: {}. A \
                     params reader takes a value only when its TOML type matches, and IGNORES it \
                     otherwise — exactly as it ignores an unknown key — so each of these would \
                     mount at the strategy's COMPILED DEFAULT while the profile says otherwise. It \
                     reads: {}.",
                    name,
                    mistyped.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("; "),
                    readable_keys(name)
                ));
            }
            // ...and the third case, which the two above BOTH pass: a key that is read, whose value
            // is well-typed, and which the MOUNT then overrides. See the message for the mechanism.
            let misrouted =
                vike_strategy::misrouted_params(name, &s.params, self.venue(), self.mount_symbol());
            if !misrouted.is_empty() {
                return Err(format!(
                    "strategy {:?} was handed `[strategy.params]` keys naming a market this mount \
                     does not trade: {}. A live mount routes EVERY order to its OWN (venue, \
                     symbol) — `crates/vike-core/src/runtime/strategy_drive/broker_drain.rs`'s \
                     `resolve_intent_symbol` returns the mount's symbol and `resolve_intent_venue` \
                     its venue, unconditionally, because no mount declares extra legs today \
                     (`vike_mount::MountSpec`'s `legs` is empty on every spec and \
                     `build_paper_strategy_core_with` asserts it). So each of these configures \
                     NOTHING while the mount line echoes it as real — the operator reads one \
                     instrument and the orders hit another. Set it to this mount's own \
                     `venue = {:?}` / `symbol = {:?}`, or drop the key.",
                    name,
                    misrouted.iter().map(|m| m.to_string()).collect::<Vec<_>>().join("; "),
                    self.venue(),
                    self.mount_symbol(),
                ));
            }
            // ...and the fourth, which all three above pass: every key is spelled right, typed right
            // and routed right, and the ORDER SET they describe between them is empty. The other
            // three are "this knob runs at something you did not state"; this one is "nothing runs
            // at all", and it is the quietest failure of the four — a clean mount line and then
            // silence.
            if let Some(why) = vike_strategy::unarmable_params(name, &s.params) {
                return Err(format!(
                    "{why}. Both `arm`s in `crates/vike-strategy/src/strategies/grid_dca.rs` build their WHOLE \
                     order set from the params plus one anchor, once, on the first bar or tick — so \
                     an empty ladder is not a state the market moves this mount out of; it is a \
                     daemon that starts clean, echoes a full configuration line and then never \
                     submits. The rung builders (`legs_at` and `entries_at`) admit nothing when the \
                     ladder is degenerate (`rungs`, `size` or `step` at zero or below), when a \
                     `bounded01` market's `step` does not fit inside `(tick, 1 - tick)` — the \
                     compiled `step = 1.0` spans that whole domain — or when every rung prices at \
                     or below zero, which the compiled `anchor_price = 0` does to every long ladder \
                     the moment `anchor = \"fixed\"` selects it. Give the ladder a rung it can \
                     rest, or drop the `[strategy]` table."
                ));
            }
        }
        Ok(())
    }

    /// The `rhai = "<path>"` half of [`Self::validate`] — the PURE checks only, because this runs
    /// at profile LOAD and load is filesystem-free by contract (the docs-profiles CI gate parses
    /// every shipped profile on runners that have no script tree). Reading, hashing and compiling
    /// the script — and refusing an override key the script never asks for, which needs the script
    /// TEXT — happen at [`Self::resolve_script`], still before any core spawns.
    ///
    /// What IS refusable here is the value-type half of the daemon's no-silently-ignored-key
    /// posture, in the script vocabulary: every `[strategy.params]` value under `rhai` is a
    /// `param(name, default)` override, `param` yields an `f64`, and the override map is built by
    /// a NUMERIC filter (the same lenient reader convention the backtest's `rhai` arm uses) — so a
    /// quoted number (`size = "2"`) would be silently DROPPED and the script would run its own
    /// compiled default while the profile states otherwise. That is `mistyped_params`' exact
    /// hazard, refused here with the same posture.
    fn validate_script(path: &str, s: &StrategyCfg) -> Result<(), String> {
        if path.trim().is_empty() {
            return Err("`[strategy] rhai` must name a script file (absolute, or relative to the \
                        daemon's working directory)"
                .to_string());
        }
        let Some(table) = s.params.as_table() else {
            return Err(format!(
                "`[strategy.params]` must be a TABLE of keys, got {}",
                s.params.type_str()
            ));
        };
        let non_numeric: Vec<String> = table
            .iter()
            .filter(|(_, v)| v.as_float().is_none() && v.as_integer().is_none())
            .map(|(k, v)| format!("{k} ({})", v.type_str()))
            .collect();
        if !non_numeric.is_empty() {
            return Err(format!(
                "a Rhai `[strategy.params]` value must be a NUMBER, got: {}. Every key here is a \
                 `param(name, default)` override baked into the script at compile; `param` takes \
                 an f64, so any other TOML type would be silently dropped and the script would run \
                 its own compiled default while this profile states otherwise.",
                non_numeric.join(", ")
            ));
        }
        Ok(())
    }

    /// Safety gate #5: may this profile be armed LIVE? Called from `main` ONLY when the live gate is
    /// on; the paper path never consults it (a paper profile can name any venue/symbol).
    ///
    /// THREE questions, in order, each with its own authority — it used to be one hardcoded table of
    /// `(venue, symbol)` PAIRS, which conflated them and duplicated `build_node`'s own symbol
    /// literals:
    ///
    /// 1. **Is the VENUE live-wired in this build?** [`LIVE_WIRED_VENUES`] — the venues `live_mount`
    ///    has a market-feed arm for. This is the gate that stops a silent widening, and it is the
    ///    one an operator can reason about ("did somebody wire polymarket?").
    /// 2. **Would that venue's engine ACCEPT this symbol?** Derived from [`crate::wired_markets::WIRED_MARKETS`],
    ///    the table `build_node`'s own `make_engine` calls read their venue+symbol from. ⚠ This is
    ///    not pedantry: `vike_mount::make_engine` sets no `extra_symbols`, so
    ///    [`vike_exec::ExecutionEngine::accepts_symbol`] is a plain equality test and a foreign
    ///    symbol's orders and fills are SILENTLY DROPPED — the strategy quotes, the daemon logs
    ///    nothing, and nothing ever trades. A row whose symbol is EMPTY is ACCOUNT-WIDE (polymarket:
    ///    exec/fills/reconcile key off the wallet, tokens resolve per order), so any symbol passes.
    /// 3. **Venue-specific SHAPE.** Polymarket outcome ids are dynamic ERC-1155 ids (a long DECIMAL
    ///    string), not a fixed symbol, so its account-wide row cannot answer (2) — gate on shape
    ///    (≥20 ASCII digits) instead. This is what keeps the shipped paper default (`token_id =
    ///    "TOK"`) un-armable.
    ///
    /// Extending the daemon to another venue remains a deliberate edit HERE (one
    /// [`LIVE_WIRED_VENUES`] row) **and** the matching `live_mount` arm — never a silent widening.
    /// Three gates, each over a different pair: `live_wired_venues_are_all_mounted_by_build_node`
    /// (this list ⊆ `crate::wired_markets::WIRED_MARKETS`, i.e. an ENGINE exists) and
    /// `every_unwired_venue_is_still_refused` (its converse over that table), plus
    /// `crates/vike-tradehub/tests/daemon/live_wired_venues_pin.rs` (this list == `live_mount`'s actual
    /// FEED arms) — which is the one that catches a row added here alone, and the one that did not
    /// exist until it was measured missing.
    pub fn validate_for_live(&self) -> Result<(), String> {
        // A `[[mounts]]` profile arms live only when EVERY row does — the same per-row refusals,
        // each failure naming its row (split-plane I10).
        if !self.mounts.is_empty() {
            for (i, row) in self.mount_rows().iter().enumerate() {
                row.validate_for_live().map_err(|e| {
                    format!(
                        "mounts[{i}] (venue={:?}, symbol={:?}): {e}",
                        row.venue(),
                        row.mount_symbol()
                    )
                })?;
            }
            return Ok(());
        }
        let venue = self.venue();
        let symbol = self.mount_symbol();
        if !LIVE_WIRED_VENUES.contains(&venue) {
            return Err(format!(
                "venue {venue:?} is not live-wired in this build; the daemon can mount: {}",
                LIVE_WIRED_VENUES.join(", ")
            ));
        }
        // (2) routing. `build_node` is what actually mounts the engines, so its table answers.
        let Some(wired_symbol) =
            crate::wired_markets::WIRED_MARKETS.iter().find(|m| m.venue == venue).map(|m| m.symbol)
        else {
            return Err(format!(
                "venue {venue:?} is live-wired for a feed but `build_node` mounts no engine for it \
                 — orders would have nowhere to go"
            ));
        };
        // ⚠ THE DEFAULT ACCOUNT ONLY. `build_node` mounts a venue's DEFAULT engine on the
        // `WIRED_MARKETS` symbol and sets no `extra_symbols`, so an account-less row naming
        // anything else is silently dropped — that refusal is unchanged, and it is why this daemon
        // can still reach exactly one instrument per venue on the default account.
        //
        // A row naming an ACCOUNT is a different question with a different answer:
        // `vike_mount::account_symbols_for` mounts that account's engine on THIS ROW'S OWN symbol, so
        // `accepts_symbol` answers about the symbol the row named and there is nothing to refuse.
        // That is how a second instrument is reached, and refusing it here would have made the
        // account field unusable for the one thing it is for.
        let default_account = self.account.as_ref().is_none_or(|l| l.is_default());
        if default_account && !wired_symbol.is_empty() && wired_symbol != symbol {
            return Err(format!(
                "{venue} is mounted on {wired_symbol:?} by build_node, but this profile names \
                 {symbol:?}: that engine accepts exactly its mounted symbol \
                 (`ExecutionEngine::accepts_symbol` — no `extra_symbols` are wired), so every order \
                 and fill would be SILENTLY DROPPED. A row naming a second `account` is exempt — \
                 that account's engine is mounted on the row's own symbol"
            ));
        }
        // (3) venue SHAPE. Feature-free on purpose (profile shape only): the REAL capability gate
        // for polymarket is the `polymarket` cargo feature, without which `live_mount` hard-errors.
        if venue == "polymarket"
            && !(symbol.len() >= 20 && symbol.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(format!(
                "polymarket needs an outcome token id (a ≥20-digit decimal ERC-1155 id), got \
                 {symbol:?}"
            ));
        }
        // (4) the DRAWDOWN LATCH needs a capital base. `live_mount` hardcodes
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
                return Err(format!(
                    "`seed_cash = {seed}` disarms the live daemon's 25% drawdown latch: it \
                     measures the drop as a fraction of configured capital plus own PnL, so a \
                     non-positive base leaves nothing to measure against. Omit it (default 1000) \
                     or set the capital this mount is meant to risk"
                ));
            }
        }
        Ok(())
    }
}
