//! **What the boot restore tells the gatewayhub lease** (wave 2, W13): the journal cursor each account
//! resumes its event ring from, and the client order ids the restore will ask `wire_id_for` about.
//!
//! # The cursor (`TradeOpen.resume`)
//!
//! `BootRestore::gateway_cursors` is, per route key, where the previous process's core stood in the
//! gatewayhub's per-account event ring (journal v18). The lease presents it so the gatewayhub replays
//! what this tradehub missed while it was down; the core's own redelivery guard
//! (`vike-core`'s `gateway_redelivered`) and the interplay tests (`runtime/tests/boot_restore.rs`) make
//! a replay of an already-booked fill a no-op. An account the restore holds no cursor for opens with
//! NONE and the reconcile pass recovers what it missed.
//!
//! ⚠ A replay and a reconcile policy that rewrites positions double-count (`PositionDrift` over a ring
//! replay: the W3c finding, pinned `#[ignore]`d in `vike-core`): the live start OVERRIDES `hybrid` and
//! `synthesize` with `quarantine` over the gatewayhub, said at `error`, never refusing the start
//! (`crate::reconcile_config`'s `quarantine_when_replaying`, over `refuse_replaying_policy`'s verdict),
//! which is what makes sending a cursor sound.
//!
//! # The restored coids (`WireIds` prefetch)
//!
//! The restore translates a venue's wire ids back to our coids with `ExecutionClient::wire_id_for`,
//! on the fold thread, which may not block. A remote client cannot compute Hyperliquid's
//! `keccak(coid)`, so the answers are fetched on the BOOT thread, before the core exists
//! (`vike_gatewayhub_client::trade::TradeSession::prefetch_wire_ids`). That needs the set the restore
//! will ask about: the coids the journal says were resting and the ones the order-ownership file
//! remembers. The ownership file is read by the core itself at its spawn, through a crate-private
//! loader, so this module reads the same documented JSON-lines file (`vike_core::order_owners`'s module
//! doc: `own`, `forget`, `unmount`, `ledger`; a torn or foreign line is skipped) and mirrors its fold:
//! `forget` and `unmount` act on the FILE's entries only (the core lets the journal re-add a coid the
//! file forgot, so a coid the journal calls resting is always asked about), an `unmount` removes the
//! coids its mount CURRENTLY owns, ownership older than `TTL_MS` is dropped and at most `OWNER_CAP`
//! (the newest) are kept. The set is a superset on purpose: a coid asked about and not restored costs
//! one row in a request, and a restored coid that was NOT asked about is a restore that plans with an
//! incomplete translation map (a resting order judged gone). So **nothing is cut**: the set goes to the
//! gatewayhub in pages (`TradeSession::prefetch_wire_ids` pages it), the journal's resting orders
//! first, and a failed page leaves the whole prefetch owed, which keeps the account's reconcile NOT
//! READY.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use vike_core::order_owners::{OWNER_CAP, TTL_MS};
use vike_core::replay::BootRestore;
use vike_exec::GatewayCursor;

/// One coid the restore may ask about, with the facts that say which account's lease needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Restored {
    /// The venue the order was routed to; `None` for an old ownership line (asked of every venue).
    venue: Option<String>,
    /// The account label text; `None` for the default account (and for an old line).
    account: Option<String>,
}

impl Restored {
    /// Whether the lease of `venue`'s `account` must be asked about this coid. An entry of unknown venue
    /// is everyone's; one of a known venue with no account is every account of that venue's (the old
    /// line cannot say, and a superset is safe).
    fn is_for(&self, venue: &str, account: Option<&str>) -> bool {
        self.venue.as_deref().is_none_or(|v| v == venue)
            && (self.account.is_none() || self.account.as_deref() == account)
    }
}

/// One ownership-file entry as the fold keeps it.
#[derive(Debug, Clone)]
struct Owned {
    restored: Restored,
    /// The `own` line's `ts` (the core's clock, ms): its age and its eviction order.
    ts: i64,
    /// The mount that owns it now (an `unmount` removes by this).
    mount: String,
}

/// The multi-currency ledger of each restored engine that held a symbol quoted in another currency, by route key.
/// A stopped engine's tombstone is no engine of this boot (`crate::readmit::targets` skips it too).
fn journaled_fx(engines: &[vike_exec::EngineSnapshot]) -> HashMap<String, JournaledFx> {
    let mut out = HashMap::new();
    for snap in engines {
        let route_key = snap.route_key.clone().unwrap_or_else(|| snap.venue.clone());
        if route_key.ends_with(vike_core::RETIRED_ROUTE_KEY_SUFFIX) {
            continue;
        }
        if let Some(fx) = &snap.account.fx {
            out.insert(
                route_key,
                JournaledFx { account_ccy: fx.account_ccy.clone(), symbols: fx.symbols.clone() },
            );
        }
    }
    out
}

/// What the boot restore hands the leases. See the module doc.
#[derive(Debug, Clone, Default)]
pub struct BootHints {
    cursors: HashMap<String, GatewayCursor>,
    /// The orders the JOURNAL says rest: asked about first, and never removed by the ownership file.
    resting: BTreeMap<String, Vec<Restored>>,
    /// The ownership file's live entries (after its TTL and cap).
    owned: BTreeMap<String, Owned>,
    /// Per route key, the multi-currency half of the engine the journal restores ([`JournaledFx`]).
    fx: HashMap<String, JournaledFx>,
}

/// **What the journal restores of an account's multi-currency ledger** (decision 0183): the currency its rates
/// are expressed in and the symbols it holds that are quoted in another one, with those currencies. It exists
/// exactly when the previous run's engine held such a symbol (the engine snapshot's fx table). The node needs it
/// at the account's start, before the engine is assembled: a start whose gatewayhub cannot state the account's
/// currency right then anchors the engine to this one, so the restored foreign positions can be given a fresh
/// rate, and the rate timer knows which restored symbols to keep an eye on whether or not the boot re-admitted
/// them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournaledFx {
    /// The currency every rate of the restored ledger is expressed in.
    pub account_ccy: String,
    /// `(symbol, quote currency)` of every foreign-quoted symbol, in the engine's spelling.
    pub symbols: Vec<(String, String)>,
}

impl BootHints {
    /// No restore: every lease opens fresh and nothing is prefetched.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// The hints of `restore`, plus the ownership file at `owners_file` (`None`: no project, no file),
    /// whose entries age against `now_ms` as the core's own load does.
    #[must_use]
    pub fn from_restore(restore: &BootRestore, owners_file: Option<&Path>, now_ms: i64) -> Self {
        let mut hints = BootHints {
            cursors: restore.gateway_cursors().iter().cloned().collect(),
            ..BootHints::default()
        };
        hints.fx = journaled_fx(restore.restored_engines());
        for o in restore.journal_orders() {
            hints.add(&o.coid, o.venue.as_deref(), o.account.as_deref());
        }
        if let Some(path) = owners_file {
            hints.read_owners(path, now_ms);
        }
        hints
    }

    /// A hints value over explicit parts (the tests').
    #[must_use]
    pub fn of(
        cursors: Vec<(String, GatewayCursor)>,
        orders: &[(&str, &str, Option<&str>)],
    ) -> Self {
        let mut hints = BootHints { cursors: cursors.into_iter().collect(), ..Self::default() };
        for (coid, venue, account) in orders {
            hints.add(coid, Some(venue), *account);
        }
        hints
    }

    /// A coid the journal says rests.
    fn add(&mut self, coid: &str, venue: Option<&str>, account: Option<&str>) {
        self.resting.entry(coid.to_string()).or_default().push(Restored {
            venue: venue.map(str::to_string),
            account: account.map(str::to_string),
        });
    }

    /// The resume cursor of the account named by `route_key` (the spelling the core stamped on its
    /// `GatewayEvent`s: the bare venue id for a default account).
    #[must_use]
    pub fn cursor_for(&self, route_key: &str) -> Option<GatewayCursor> {
        self.cursors.get(route_key).copied()
    }

    /// The multi-currency ledger the journal restores for the account named by `route_key`, if the previous
    /// run's engine held a symbol quoted in another currency.
    #[must_use]
    pub fn fx_for(&self, route_key: &str) -> Option<&JournaledFx> {
        self.fx.get(route_key)
    }

    /// A hints value carrying only `fx` for `route_key` (the tests').
    #[must_use]
    pub fn with_fx(mut self, route_key: &str, fx: JournaledFx) -> Self {
        self.fx.insert(route_key.to_string(), fx);
        self
    }

    /// Every coid the restore may ask `venue`'s `account` lease about, none cut: the journal's resting
    /// orders first (sorted), then the ownership file's, newest first.
    #[must_use]
    pub fn coids_for(&self, venue: &str, account: Option<&str>) -> Vec<String> {
        let mut coids: Vec<String> = self
            .resting
            .iter()
            .filter(|(_, rows)| rows.iter().any(|r| r.is_for(venue, account)))
            .map(|(coid, _)| coid.clone())
            .collect();
        let mut owned: Vec<(&String, &Owned)> = self
            .owned
            .iter()
            .filter(|(coid, o)| {
                o.restored.is_for(venue, account) && !self.resting.contains_key(*coid)
            })
            .collect();
        owned.sort_by(|a, b| b.1.ts.cmp(&a.1.ts).then_with(|| a.0.cmp(b.0)));
        coids.extend(owned.into_iter().map(|(coid, _)| coid.clone()));
        coids
    }

    /// Fold the ownership file the way the core's load does: an `own` adds (or re-points) a coid, a
    /// `forget` removes it, an `unmount` removes every coid its mount owns NOW; then ownership older
    /// than `TTL_MS` before `now_ms` goes and the newest `OWNER_CAP` stay. An unreadable file is said at
    /// `error` and adds nothing (the core says the same at its own load and restores no owner).
    fn read_owners(&mut self, path: &Path, now_ms: i64) {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                tracing::error!(
                    path = %path.display(), error = %e,
                    "the order-ownership file exists and cannot be read: no restored coid is \
                     prefetched from it (the core refuses it the same way and restores no owner)"
                );
                return;
            }
        };
        let mut owned: BTreeMap<String, Owned> = BTreeMap::new();
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
            let field = |k: &str| v.get(k).and_then(serde_json::Value::as_str);
            match field("kind") {
                Some("own") => {
                    let (Some(coid), Some(mount), Some(ts)) = (
                        field("coid"),
                        field("mount_id"),
                        v.get("ts").and_then(serde_json::Value::as_i64),
                    ) else {
                        continue;
                    };
                    owned.insert(
                        coid.to_string(),
                        Owned {
                            restored: Restored {
                                venue: field("venue").map(str::to_string),
                                account: field("account").map(str::to_string),
                            },
                            ts,
                            mount: mount.to_string(),
                        },
                    );
                }
                Some("forget") => {
                    if let Some(coid) = field("coid") {
                        owned.remove(coid);
                    }
                }
                Some("unmount") => {
                    if let Some(mount) = field("mount_id") {
                        owned.retain(|_, o| o.mount != mount);
                    }
                }
                _ => {}
            }
        }
        owned.retain(|_, o| now_ms.saturating_sub(o.ts) <= TTL_MS);
        if owned.len() > OWNER_CAP {
            let mut by_age: Vec<(i64, String)> =
                owned.iter().map(|(c, o)| (o.ts, c.clone())).collect();
            by_age.sort_unstable();
            for (_, coid) in by_age.into_iter().take(owned.len() - OWNER_CAP) {
                owned.remove(&coid);
            }
        }
        self.owned = owned;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference clock of the tests' ownership lines.
    const NOW: i64 = 1_000_000_000_000;

    fn cursor(seq: u64) -> GatewayCursor {
        GatewayCursor { gw_epoch: 7, seq, ts_ms: 1 }
    }

    fn own_at(
        coid: &str,
        mount: &str,
        venue: Option<&str>,
        account: Option<&str>,
        ts: i64,
    ) -> String {
        let mut v = serde_json::json!({"kind": "own", "coid": coid, "mount_id": mount, "ts": ts});
        if let Some(venue) = venue {
            v["venue"] = venue.into();
        }
        if let Some(account) = account {
            v["account"] = account.into();
        }
        v.to_string()
    }

    fn own(coid: &str, mount: &str, venue: Option<&str>, account: Option<&str>) -> String {
        own_at(coid, mount, venue, account, NOW)
    }

    /// Hints over an ownership file holding `lines`, and (as the journal's resting orders) `resting`.
    fn hints_of(lines: &[String], resting: &[(&str, &str)]) -> BootHints {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("order_owners.jsonl");
        std::fs::write(&path, lines.join("\n")).expect("write");
        let mut hints = BootHints::from_restore(&BootRestore::off(), Some(&path), NOW);
        for (coid, venue) in resting {
            hints.add(coid, Some(venue), None);
        }
        hints
    }

    #[test]
    fn a_cursor_is_found_by_the_route_key_spelling_and_an_absent_account_has_none() {
        let hints = BootHints::of(vec![("binance#ALT".to_string(), cursor(9))], &[]);
        assert_eq!(hints.cursor_for("binance#ALT"), Some(cursor(9)));
        assert_eq!(hints.cursor_for("binance"), None, "another account opens with no cursor");
    }

    #[test]
    fn coids_are_asked_of_their_own_account_and_a_vague_line_of_every_account_of_its_venue() {
        let mut hints = BootHints::default();
        hints.add("hl-default", Some("hyperliquid"), None);
        hints.add("hl-alt", Some("hyperliquid"), Some("ALT"));
        hints.add("bn", Some("binance"), None);
        hints.add("old", None, None);
        assert_eq!(hints.coids_for("hyperliquid", Some("ALT")), ["hl-alt", "hl-default", "old"]);
        assert_eq!(hints.coids_for("hyperliquid", None), ["hl-default", "old"]);
        assert_eq!(hints.coids_for("binance", None), ["bn", "old"]);
    }

    #[test]
    fn the_ownership_file_is_folded_own_forget_unmount_and_skips_what_it_cannot_read() {
        let lines = [
            own("a", "m1", Some("hyperliquid"), None),
            own("b", "m1", Some("hyperliquid"), None),
            own("c", "m2", Some("hyperliquid"), None),
            own("d", "m2", None, None),
            "{\"kind\":\"forget\",\"coid\":\"a\"}".to_string(),
            "{\"kind\":\"unmount\",\"mount_id\":\"m2\"}".to_string(),
            "{\"kind\":\"ledger\",\"mount_id\":\"m1\",\"size\":1.0}".to_string(),
            "this line is torn".to_string(),
        ];
        let hints = hints_of(&lines, &[]);
        assert_eq!(hints.coids_for("hyperliquid", None), ["b"], "forgotten and unmounted are gone");
    }

    /// W13b finding 4/12: the core lets the JOURNAL re-add a coid the file forgot, and an unmount removes
    /// the coids its mount owns NOW (an `own` that re-pointed a coid to another mount survives it).
    #[test]
    fn a_coid_the_journal_calls_resting_survives_the_files_forget_and_unmount_follows_the_current_owner()
     {
        let lines = [
            own("forgotten", "m1", Some("hyperliquid"), None),
            "{\"kind\":\"forget\",\"coid\":\"forgotten\"}".to_string(),
            own("moved", "m1", Some("hyperliquid"), None),
            own("moved", "m2", Some("hyperliquid"), None),
            "{\"kind\":\"unmount\",\"mount_id\":\"m1\"}".to_string(),
        ];
        let hints = hints_of(&lines, &[("forgotten", "hyperliquid")]);
        assert_eq!(
            hints.coids_for("hyperliquid", None),
            ["forgotten", "moved"],
            "the journal's resting order is asked about whatever the file forgot, and `moved` now \
             belongs to m2, which was not unmounted"
        );
    }

    /// W13b finding 4: the TTL and the cap are the core's, so the set is the one the restore will ask
    /// about and no more.
    #[test]
    fn ownership_older_than_the_ttl_is_dropped_and_the_cap_keeps_the_newest() {
        let lines = [
            own_at("ancient", "m", Some("hyperliquid"), None, NOW - TTL_MS - 1),
            own_at("edge", "m", Some("hyperliquid"), None, NOW - TTL_MS),
        ];
        let hints = hints_of(&lines, &[]);
        assert_eq!(hints.coids_for("hyperliquid", None), ["edge"], "30 days exactly stays");

        let mut many: Vec<String> = (0..(OWNER_CAP + 3))
            .map(|i| {
                own_at(&format!("c{i:06}"), "m", Some("hyperliquid"), None, NOW - 1_000 + i as i64)
            })
            .collect();
        many.push("{\"kind\":\"ledger\",\"mount_id\":\"m\"}".to_string());
        let hints = hints_of(&many, &[]);
        let asked = hints.coids_for("hyperliquid", None);
        assert_eq!(asked.len(), OWNER_CAP, "the cap");
        assert_eq!(asked[0], format!("c{:06}", OWNER_CAP + 2), "newest first");
        assert!(!asked.contains(&"c000000".to_string()), "the oldest three are evicted");
    }

    /// W13b finding 4: the old cut kept the lexically smallest 4096; a resting order must never be the
    /// one left out. Nothing is cut, and the journal's resting orders lead.
    #[test]
    fn nothing_is_cut_and_the_journals_resting_orders_come_first() {
        let lines: Vec<String> = (0..6_000)
            .map(|i| own_at(&format!("a{i:05}"), "m", Some("hyperliquid"), None, NOW - 10_000 + i))
            .collect();
        let hints = hints_of(&lines, &[("zz-resting", "hyperliquid")]);
        let asked = hints.coids_for("hyperliquid", None);
        assert_eq!(asked.len(), 6_001, "every coid is asked about");
        assert_eq!(asked[0], "zz-resting", "the order the journal says rests is in the first page");
        assert_eq!(asked[1], "a05999", "then the file's, newest first");
    }

    #[test]
    fn a_missing_file_and_no_file_add_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let hints =
            BootHints::from_restore(&BootRestore::off(), Some(&dir.path().join("none")), NOW);
        assert!(hints.coids_for("binance", None).is_empty());
        assert!(
            BootHints::from_restore(&BootRestore::off(), None, NOW).coids_for("x", None).is_empty()
        );
    }

    /// **The hint is the fx table of each restored engine that held a symbol in another currency**, by route key:
    /// the currency its rates are expressed in and the symbols with theirs. An engine that held none has no table
    /// and gives no hint, a labelled account is keyed by its own route key, and a stopped engine's tombstone is no
    /// engine of this boot. Nothing restored, nothing hinted.
    #[test]
    fn a_restored_engines_foreign_ledger_is_the_hint_and_a_tombstone_is_not() {
        use vike_exec::testing::RecordingClient;
        use vike_exec::{Account, BalanceMode, ExecutionEngine, RiskGate, SymbolAdmission};
        use vike_model::{RiskLimits, SymbolGrid};

        let snapshot = |venue: &str, route_key: Option<&str>, foreign: bool| {
            let mut engine = ExecutionEngine::new(
                Account::new(1.0, venue, None, BalanceMode::Delta),
                RiskGate::new(RiskLimits::new()),
                RecordingClient::default(),
                venue,
                "AAPL.SMART.USD",
            );
            engine.set_account_ccy("USD").expect("a currency code");
            engine.now_ms = 1_000;
            if foreign {
                let outcome = engine.admit_symbol(&SymbolAdmission {
                    route_key: venue.into(),
                    symbol: "SAP.SMART.EUR".into(),
                    grid: SymbolGrid {
                        tick_size: Some(0.01),
                        lot_size: Some(1.0),
                        ..SymbolGrid::default()
                    },
                    contract_value: None,
                    initial_mark: None,
                    quote_ccy: Some("EUR".into()),
                    quote_rate: Some(1.10),
                    rate_as_of_ms: Some(1_000),
                    im_requirement: None,
                    release: false,
                    margin_mode: None,
                });
                assert!(!matches!(outcome, vike_exec::AdmitOutcome::Refused(_)), "{outcome:?}");
            }
            let mut snap = engine.snapshot_state();
            snap.route_key = route_key.map(str::to_string);
            snap
        };
        let snaps = [
            snapshot("ibkr", None, true),
            snapshot("ibkr", Some("ibkr#ALT"), true),
            snapshot("ibkr", Some("ibkr#OLD#stopped"), true),
            snapshot("bybit", None, false),
        ];
        let fx = journaled_fx(&snaps);
        assert_eq!(fx.len(), 2, "{fx:?}");
        let want = JournaledFx {
            account_ccy: "USD".to_string(),
            symbols: vec![("SAP.SMART.EUR".to_string(), "EUR".to_string())],
        };
        assert_eq!(fx.get("ibkr"), Some(&want));
        assert_eq!(fx.get("ibkr#ALT"), Some(&want));
        assert!(!fx.contains_key("bybit") && !fx.contains_key("ibkr#OLD#stopped"));
        assert!(journaled_fx(&[]).is_empty(), "nothing restored, nothing hinted");
        assert_eq!(BootHints::none().fx_for("ibkr"), None);
    }
}
