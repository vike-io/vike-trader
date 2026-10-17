//! Building a `CoreThread` (`assemble_core`) and resolving each mount's declared account to an engine index.

use super::*;
use vike_model::orders::client_order_id::ClientOrderIdGenerator;

/// **THE mount → engine resolution, as DATA** — `Ok(index)` or the refusal sentence, so the two
/// doors a mount can arrive through cannot disagree about either the RULE or the WORDING.
///
/// The two doors need different SHAPES and the same DECISION, which is why this returns a `Result`
/// instead of acting: `resolve_mount_engines` (spawn, from [`assemble_core`]) turns an `Err` into a
/// tombstoned slot with an `error!` and a note — that ONE mount is not mounted, nothing else
/// stops (it used to PANIC, and under systemd that was a crash loop re-arming every live account
/// every few seconds) — while [`CoreThread::mount_strategy_runtime`] turns it into a refused
/// command with a note, because a live daemon must stay up through a bad mount request. They used
/// to be two hand-written twins, and they had drifted: the runtime door refused BOTH misses and
/// the spawn door refused only the account one.
///
/// Two arms, and they say the same thing about different nouns:
///
/// * **`account: None`** — the venue's DEFAULT account, looked up as
///   `RouteKey::sole_account_of(venue)`. A miss is a REFUSAL.
///
///   ⚠ **It used to be `unwrap_or(0)`, and this is the change
///   `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md`'s Phase 0 asks for.**
///   The fall-through was inherited from the per-venue strategy lanes this function replaced, and
///   it was defended as "byte-identical BY CONSTRUCTION, including the historical tolerance of a
///   mount on a venue this core runs no engine for (a paper/test core), which must keep working".
///   What that tolerance actually does on a LIVE daemon is route the mount to ENGINE ZERO —
///   whichever venue `vike_tradehub_core::assemble_runtime` mounted first, in practice binance — so a strategy
///   whose profile named another venue quotes, submits and fills on a book its author did not
///   choose, silently. That is the account arm's catastrophe wearing a venue instead of a label,
///   and it had the opposite answer for no reason anyone wrote down.
/// * **`account: Some(label)`** — a DECLARED route key (`venue#LABEL`, rendered by
///   `vike_model::accounts::account_keys::route_key_of`, the ONE spelling the mount fan-out stamps and the
///   live lock names its sentinel after). A miss has always been a refusal here: `unwrap_or(0)`
///   would be the catastrophe stated as three characters. The composition root refuses this case
///   first, loudly and by name (`crates/vike-tradehub-core/src/node/accounts.rs`'s `refuse_unarmed_mount_accounts`); this is the backstop no
///   future root can reach past.
///
/// `mounted` is every route key this core runs, primary first — printed in both refusals because
/// "which engines DO exist" is the first thing an operator needs and neither door could answer it
/// from the outside.
///
/// ⚠ The `Err` names NO mount id and starts mid-sentence, on purpose: each door prefixes it with
/// its OWN opener, and those openers are load-bearing. The runtime door's siblings (the duplicate
/// -id and factory refusals) all read ``MOUNT REFUSED: `{mid}` …``, and a shared prefix here would
/// have made this one refusal read differently from the two beside it in the same ring.
pub(crate) fn mount_engine_resolution(
    mounted: &[&str],
    venue: &str,
    account: Option<&vike_model::accounts::account_keys::AccountLabel>,
) -> Result<usize, String> {
    let idx_of = |key: &str| -> Option<usize> { mounted.iter().position(|k| *k == key) };
    match account.filter(|l| !l.is_default()) {
        None => {
            // ⚠ **AN ACCOUNT-LESS MOUNT REFUSES WHEN THE VENUE HAS MORE THAN ONE ENGINE**, and
            // this is the hole the `sole_account_of` spelling names but does not close on its own:
            // that call asserts *"this venue has ONE account in this process"*, and on a box with
            // an `[accounts]` table the assertion is simply false — the lookup then SUCCEEDS,
            // because the default account's route key IS the bare venue, and the mount lands on it
            // silently. A strategy executing on an account its author did not choose is the same
            // failure the labelled arm below refuses, wearing an absence instead of a label.
            //
            // ⚠ **`account.is_none()`, NOT "the label is the default one"** — the two are a
            // DIFFERENT ROW of the design's sender table and collapsing them is a defect that
            // wears a fix's clothes. An ABSENT account is the sender naming none, which at two
            // engines is ambiguous; an explicit `DEFAULT` is the sender NAMING the unlabelled
            // account, which is never ambiguous and routes to it at any engine count. Refusing on
            // `is_default()` — the shape this arm's guard already has, since it folds `DEFAULT`
            // down here to share the resolution below — leaves the default account with NO
            // spelling that mounts it on a multi-account venue: absence refuses, and `DEFAULT`
            // reaches this same arm. The refusal would then be unsatisfiable by the very message
            // it prints.
            //
            // Counted with `label_of_route_key`, the exact inverse of the `route_key_of` that
            // minted these keys, so `binanceus` is not read as an account of `binance`.
            if account.is_none() {
                let carriers = mounted
                    .iter()
                    .filter(|k| {
                        vike_model::accounts::account_keys::label_of_route_key(venue, k).is_some()
                    })
                    .count();
                if carriers > 1 {
                    // The accounts to CHOOSE FROM, spelled the way the mount's `account` field
                    // takes them: a label as itself, the unlabelled account as `DEFAULT`.
                    let choices = mounted
                        .iter()
                        .filter_map(|k| {
                            vike_model::accounts::account_keys::label_of_route_key(venue, k)
                        })
                        .map(|l| format!("`{l}`"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(format!(
                        "names venue `{venue}` and no account, and this core runs {carriers} \
                         engines of it (mounted: {mounted:?}). Which one it meant cannot be \
                         inferred: the default account's route key IS the bare venue, so an \
                         account-less mount would resolve to it and look correct. Name the account \
                         on the mount (`account = \"<one of>\"`), choosing from {choices}: a \
                         label addresses the labelled account armed from an ACTIVE {venue} account \
                         row carrying it (`vike-cli secrets accounts` lists them), and `DEFAULT` \
                         addresses the unlabelled account this venue already had (its route key \
                         is the bare `{venue}`); the mounted keys above are the spellings this \
                         core answers to"
                    ));
                }
            }
            idx_of(RouteKey::sole_account_of(venue).as_str()).ok_or_else(|| {
                format!(
                    "names venue `{venue}`, and this core runs no engine for it (mounted: \
                     {mounted:?}). It must NOT fall through to ENGINE ZERO — on a live daemon that \
                     is whichever venue was mounted first, so every order this strategy places \
                     would be signed and sent on a book its author did not choose. That is the \
                     same failure the account arm refuses, wearing a venue instead of a label. \
                     Mount `{venue}` on this core, or drop the mount"
                )
            })
        }
        Some(label) => {
            let key = vike_model::accounts::account_keys::route_key_of(venue, label);
            idx_of(RouteKey::declared(&key).as_str()).ok_or_else(|| {
                format!(
                    "names account `{label}` of venue `{venue}`, and this core runs no engine with \
                     route key `{key}` (mounted: {mounted:?}). It must NOT fall through to that \
                     venue's default account — a strategy executing on an account its author did \
                     not choose is silent and unrecoverable. Arm the account (an ACTIVE {venue} \
                     account row labelled {label}: `vike-cli secrets account add --venue {venue} \
                     --tier <tier> --label {label}`, or `vike-cli secrets account activate --id \
                     N` on the row `vike-cli secrets accounts` lists, plus its `__{label}` \
                     credential keys), or drop `account` from the mount"
                )
            })
        }
    }
}

/// Every route key this core runs, PRIMARY FIRST — the slice [`mount_engine_resolution`] resolves
/// an index into, and the one place the "index 0 is the primary, extras follow in order" layout is
/// spelled for it.
pub(crate) fn mounted_route_keys<'a, C: ExecutionClient>(
    engine: &'a ExecutionEngine<C>,
    extra_engines: &'a [(f64, ExecutionEngine<C>)],
) -> Vec<&'a str> {
    std::iter::once(engine.route_key.as_str())
        .chain(extra_engines.iter().map(|(_, e)| e.route_key.as_str()))
        .collect()
}

/// **[`mount_engine_resolution`] at the SPAWN door: a miss REFUSES THAT ONE MOUNT, it never panics.**
///
/// It used to `panic!`, defended as "a configuration fault caught before a single order exists".
/// On a daemon under systemd that is a CRASH LOOP: the unit restarts every few seconds and re-arms
/// every live account each time, for a fault in ONE mount (an account-less mount on a venue that
/// gained a second account was the incident). Decision 0156 says a failure to run is NOT RUNNING
/// with a reason, never a crash, so the miss now tombstones the slot — exactly the shape
/// [`CoreThread::unmount_strategy_runtime`] leaves, so every iterator already skips it — says so
/// with an `error!` and a recent-events note, and every other mount and account keeps running.
///
/// (It replaces `mount_engine_idx`, the panicking twin that decision 0057 and the account-routing
/// spec cite by that name.)
///
/// Nothing is lost silently: the strategy never runs, so it never emits an order, and an order from
/// any OTHER surface that names the venue is refused by the ambiguity / unheld-account refusals, a
/// terminal reject rather than a vanished order. Returns the engine index per slot (`0` for a
/// refused slot, which no lane reads: its `mounts[i]` is `None`) and the refusal notes, in slot
/// order.
fn resolve_mount_engines<C: ExecutionClient>(
    engine: &ExecutionEngine<C>,
    extra_engines: &[(f64, ExecutionEngine<C>)],
    mounts: &mut [Option<StrategyMount>],
    mount_ids: &[String],
) -> (Vec<usize>, Vec<String>) {
    let mounted = mounted_route_keys(engine, extra_engines);
    let mut notes = Vec::new();
    let idx = mounts
        .iter_mut()
        .zip(mount_ids)
        .map(|(slot, mount_id)| {
            let Some(m) = slot.as_ref() else { return 0 };
            match mount_engine_resolution(&mounted, &m.venue, m.account.as_ref()) {
                Ok(i) => i,
                Err(reason) => {
                    let note = format!("MOUNT REFUSED: `{mount_id}` {reason}");
                    tracing::error!(
                        target: "vike_core::core",
                        mount_id = %mount_id,
                        venue = %m.venue,
                        "strategy mount NOT RUNNING at startup, every other mount and account \
                         keeps running: {note}"
                    );
                    notes.push(note);
                    *slot = None;
                    0
                }
            }
        })
        .collect();
    (idx, notes)
}

/// **WHICH ENGINE an order intent is lowered onto** — the parameter that makes an account-scoped
/// mount's writes structural rather than a lookup over a venue string.
///
/// The two variants are the two kinds of caller, and the split exists because they carry different
/// information rather than because one is safer:
///
/// * **[`Self::Payload`]** — every EXTERNAL command path (a DOM click, a tradehub ticket, a CLI
///   verb, a margin-call or drawdown liquidation). None of them can name an account: they carry a
///   canonical venue and reach that venue's DEFAULT account, exactly as they always have.
/// * **[`Self::Mount`]** — a strategy's own buffered intent, lowered through
///   `CoreThread::apply_strategy_intent`, which already holds the mount index. It resolves through
///   `CoreThread::mount_engine` — the account the mount DECLARED, resolved once at assemble — so a
///   labelled mount's orders cannot reach the default account's engine by any spelling of any
///   string.
///
/// ⚠ **A `Mount` route still defers to the payload for a FOREIGN venue.** A declared leg
/// (`MountLeg::at(sym, other_venue)`) is a cross-exchange hedge; the mount's account is a fact about
/// its OWN venue and says nothing about another exchange, so a payload whose venue is not the
/// mount's engine's venue routes by the payload. That is what keeps xEMM working, and it is checked
/// against the ENGINE's canonical venue rather than the mount's string so the two can never drift.
/// * **[`Self::Engine`]** — an engine index ALREADY resolved, by a core-internal producer that
///   knows which book it is acting on but owns no mount index: the per-engine margin-call sweep
///   ([`CoreThread::sweep_margin_call_engine`]), the conditional FIRE
///   (`CoreThread::submit_fired`, which recovers the arm's engine from `cond_engine`) and the
///   PANIC BUTTON's flatten legs (`CoreThread::market_exit_flatten_legs`, which reads a position
///   out of one engine and hands back the index it read it from). All three used to
///   lower through the payload, which meant the ORDER that protects one account's book was
///   submitted to the venue's DEFAULT account — see those three sites. The exit's own mass-cancel
///   leg is the shape this variant CANNOT express, because it names an exchange rather than a
///   book: it resolves through `CoreThread::exit_scope_engines` instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EngineRoute {
    /// Route by the payload's own venue string.
    Payload,
    /// Route to the engine the mount at this index declared.
    Mount(usize),
    /// Route to THIS engine index, resolved by the caller.
    Engine(usize),
}

/// The ONE `CoreThread` struct-literal construction, shared by `spawn_core` (the live runtime)
/// and the test-only `test_core` (which drives the core synchronously with no OS thread) so the
/// field literal exists in exactly one place.
pub(crate) fn assemble_core<C: ExecutionClient>(
    mut engine: ExecutionEngine<C>,
    mut extra_engines: Vec<(f64, ExecutionEngine<C>)>,
    mut config: CoreConfig,
    market: Arc<Conflated>,
    snapshot: Arc<ArcSwap<CoreSnapshot>>,
    rejected: Arc<AtomicU64>,
) -> CoreThread<C> {
    // **Does this process hold TWO ENGINES OF ONE EXCHANGE?** — see [`CoreThread::multi_account`].
    // Computed once, here, from the engines themselves rather than from a flag a caller could get
    // wrong: `vike_tradehub_core::make_engine_accounts` is what produces a second one, and it stamps a
    // distinct `route_key` on it while leaving `venue` the canonical id, so a shared `venue` IS the
    // question. `false` on every single-account process, which is every process with no
    // `[accounts]` table.
    let multi_account = {
        let mut venues: Vec<&str> = std::iter::once(engine.venue.as_str())
            .chain(extra_engines.iter().map(|(_, e)| e.venue.as_str()))
            .collect();
        venues.sort_unstable();
        let before = venues.len();
        venues.dedup();
        venues.len() != before
    };
    // **THE AMBIGUITY REPORT** (the account-routing spec's Stage 0), said ONCE per assembled core
    // and BEFORE the first command can reach one of these engines.
    //
    // ⚠ It is emitted HERE, from the assembly, rather than from `vike_tradehub_core::make_engine_accounts`
    // where every other arming line is said — and the reason is the DENOMINATOR. Stage 1's refusal
    // counts `CoreThread::engines_of_venue`, i.e. the engines this core actually holds; the mount
    // fan-out counts ARMED accounts, which is a different set on any venue whose arm mounts fewer
    // engines than it armed (dukascopy's `pick_holder` rule is the live example: two credential sets,
    // one engine, and therefore nothing for Stage 1 to refuse). Warning from one set about a
    // refusal made from the other is the failure `ReconcileReports::route_key`'s producer already
    // paid for once — it counted LEGS while `reconcile_reports` counted ENGINES, and a two-engine
    // one-leg venue refused the surviving account's legitimate pass every interval. One set.
    //
    // ⚠ NOT `Once`-latched: this is called once per core and names every venue at once, so a
    // latch would only hide the second core a test (or a future multi-core host) assembles.
    //
    // Silent on every single-account process: `ambiguous_venues` is empty there, so this is
    // byte-identical in the trace file as well as in behaviour.
    if let Some(message) =
        crate::account_ambiguity::ambiguity_warning(&crate::account_ambiguity::ambiguous_venues(
            std::iter::once((engine.venue.as_str(), engine.route_key.as_str()))
                .chain(extra_engines.iter().map(|(_, e)| (e.venue.as_str(), e.route_key.as_str()))),
        ))
    {
        tracing::warn!(target: "vike_core::core", "{message}");
    }
    // Phase D: unify the primary mount + extra_mounts into one slot vector (Option per
    // slot for the take/replace dance around strategy calls)
    let mut mounts: Vec<Option<StrategyMount>> = Vec::new();
    if let Some(m) = config.strategy.take() {
        mounts.push(Some(m));
    }
    mounts.extend(config.extra_mounts.drain(..).map(Some));
    // Extra engines capture applied fills only when a mount TRADES their venue.
    //
    // ⚠ "trades" includes a DECLARED CROSS-VENUE LEG, not just the mount's own series. This test
    // used to be `m.venue == e.venue` alone, and a `MountLeg::at(sym, other_venue)` leg — the shape
    // an xEMM hedge requires by construction — left the hedge engine's capture DISARMED. That
    // engine folded the fill into its account normally, but recorded no `AppliedFill`, so
    // `dispatch_applied_fills` had nothing to deliver and `Strategy::on_fill` NEVER FIRED for the
    // hedge leg. A cross-venue maker was never told its hedge filled: no error, no event, and the
    // account quietly correct while the strategy's own view was not.
    //
    // It stayed invisible because nothing exercised it — no test anywhere in the workspace paired
    // `spawn_core_multi` with a hook assertion on a SECONDARY engine's venue, so every existing
    // multi-engine test either had no mount on the extra venue or never asked whether the hook ran.
    // `a_cross_venue_leg_fill_reaches_on_fill` is that test.
    //
    // Byte-identical for every single-venue configuration: with no declared leg naming another
    // venue the added disjunct is always false, so a mount whose own venue matches arms exactly as
    // before, and one that matches nothing stays disarmed (a GUI-only engine never grows the
    // buffer — the reason this gate exists at all).
    for (_, e) in extra_engines.iter_mut() {
        e.collect_applied_fills = mounts.iter().flatten().any(|m| {
            m.venue == e.venue
                || m.symbols.iter().any(|leg| leg.venue.as_deref() == Some(e.venue.as_str()))
        });
    }
    let mount_ids = load_mount_ids(&mut mounts, config.state_dir.as_deref());
    // **WHICH ENGINE each mount trades on**, resolved ONCE, here, parallel to `mounts`/`mount_ids`
    // (same indices, never resized after — only appended to by `mount_strategy_runtime`). A mount
    // that resolves NO engine is tombstoned HERE, before any per-slot table below is derived from
    // `mounts`, so the tables see exactly what an unmounted slot leaves. See
    // [`resolve_mount_engines`]; the whole of "a strategy names its account" is the block below.
    let (mount_engine, mut mount_refusals) =
        resolve_mount_engines(&engine, &extra_engines, &mut mounts, &mount_ids);
    // Portfolio-observer PR-4 T5: seed each mount's readiness, parallel to `mounts`/`mount_ids`
    // (same indices, computed once, never resized after). `readiness_gate: false` (the default)
    // seeds every slot `Ready` immediately — the gate is then permanently inert (no mount is ever
    // `Pending`, so `maintain_mount_readiness` never has anything to probe and `drain_broker`'s
    // check is a compare against a constant `Ready`), which is what makes the whole feature
    // byte-identical to today when off.
    let initial_state = if config.readiness_gate { MountState::Pending } else { MountState::Ready };
    let mount_states: Vec<MountState> = mounts
        .iter()
        .map(|slot| if slot.is_some() { initial_state } else { MountState::Ready })
        .collect();
    // steal/core-per-mount-budget: resolve each mount's optional [`MountBudget`] by its MOUNT ID
    // (the unique identity asserted just above — NOT the coarser `(venue, symbol, interval)` triple
    // two mounts may legitimately share), and capture its `(venue, symbol)` for the resolver — all
    // parallel to `mounts`/`mount_ids`/`mount_states` (same indices, computed once, never resized).
    // `any_mount_budget` is the byte-identical gate: false (no active budget) means the per-closed-
    // bar sweep is never called and `mount_latched` stays all-false forever, so `drain_broker`'s
    // new check is a compare against a constant. `mount_attr`/`mount_latched` seed empty/false.
    let mount_budget: Vec<Option<MountBudget>> = mount_ids
        .iter()
        .zip(&mounts)
        .map(|(id, slot)| slot.as_ref().and_then(|_| config.mount_budgets.get(id).copied()))
        .collect();
    let mount_vs: Vec<(String, String)> = mounts
        .iter()
        .map(|slot| slot.as_ref().map(|m| (m.venue.clone(), m.symbol.clone())).unwrap_or_default())
        .collect();
    let any_mount_budget = mount_budget.iter().flatten().any(|b| b.is_active());
    // `mount_engine` above is the whole of "a strategy names its account": every strategy lane used to ask
    // `engine_idx_for_route_key(RouteKey::sole_account_of(<some venue string>))`, which by
    // construction can only ever answer with a venue's DEFAULT account — so a mount tagged with a
    // canonical venue resolved engine 0 whatever account the operator meant. The account is turned
    // into a route key exactly ONCE (right here), and the answer is an INDEX every lane reads
    // through `CoreThread::mount_eng` / `CoreThread::route_of`.
    //
    // ⚠ The two lanes that still resolve by VENUE are the MARKET ones (`drive_strategy`'s mark +
    // price-board write and `drive_strategy_tick`'s), and they are correct: a mark is a fact about
    // the exchange, not about an account. `CoreThread::mirror_venue_price` is what carries it to the
    // venue's other accounts. `crates/vike-core/src/runtime/tests/mount_account/routing.rs`'s
    // `a_labelled_mount_trades_and_reads_its_own_account` is the gate on the mount half.
    // ORDER OWNERSHIP AND LEDGERS across a restart (decisions 0113 and 0127): the ORDER-OWNERSHIP
    // FILE is the authority on who owns an order and on every mount's ledger, and it is ALWAYS
    // applied — with a journal restore too, which only fills what the file does not know (until
    // 0127 a journal restore skipped the file outright, and with it `restore_owned_order`, so
    // decision 0120's adoption, 0121's translation and the restored-tag refusal were off on every
    // box with a journal). Entries are bound by MOUNT ID, because a slot index is not stable across
    // restarts (a reordered profile, a runtime mount resurrected later); an entry whose mount has
    // no slot WAITS in `pending_owners` for a runtime mount of that id. The log is started either
    // way, so this session's orders are recorded. Empty inputs (the default, and every
    // non-restoring caller) leave every ledger zeroed and the map empty, byte-identical to today.
    let mut boot_notes: Vec<String> = std::mem::take(&mut config.restore_notes);
    // …and the mounts refused at the resolution above, said in the same ring as the restore's own lines.
    boot_notes.append(&mut mount_refusals);
    let mut mount_attr: Vec<MountAttribution> = vec![MountAttribution::default(); mounts.len()];
    let mut coid_mount: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut order_owners = config.order_owners.take();
    let mut pending_owners: std::collections::HashMap<String, PendingOwner> =
        std::collections::HashMap::new();
    // The slot-bound entries again, WITH their facts, for `CoreThread::restore_owned_order` once the
    // core exists (it needs the routing and the tag registry). Oldest first, as loaded; the
    // journal's own candidates after them.
    let mut slot_bound: Vec<(usize, crate::order_owners::OwnedOrder)> = Vec::new();
    // ⚠ The clock is read ONLY when a log is configured, exactly as before: a replay core's
    // `QueueClock` hands out one recorded stamp per read, so an extra read at assembly would shift
    // every stamp of the tail it refolds.
    let boot_ms = order_owners.as_ref().map(|_| config.clock.now_ms()).unwrap_or_default();
    let loaded = order_owners.as_mut().map(|log| log.start(boot_ms)).unwrap_or_default();
    let file_usable = order_owners.as_ref().is_some_and(|log| log.is_persisting());
    let owned = merge_journal_owners(
        loaded.coids,
        std::mem::take(&mut config.journal_orders),
        order_owners.as_mut(),
        file_usable,
        boot_ms,
        &mut boot_notes,
    );
    for order in owned {
        match mount_ids.iter().position(|id| *id == order.mount_id) {
            Some(i) => {
                coid_mount.insert(order.coid.clone(), i);
                slot_bound.push((i, order));
            }
            None => pending_owners.entry(order.mount_id.clone()).or_default().coids.push(order),
        }
    }
    let file_ledgers: std::collections::HashSet<String> =
        loaded.ledgers.iter().map(|(mount_id, _)| mount_id.clone()).collect();
    for (mount_id, ledger) in loaded.ledgers {
        match mount_ids.iter().position(|id| *id == mount_id) {
            Some(i) => mount_attr[i] = ledger,
            None => pending_owners.entry(mount_id).or_default().ledger = Some(ledger),
        }
    }
    // Journal v18: the gateway rings' resume points the boot restore handed in, held by the core so the next
    // `Snap` keeps the cursor of an account that stays quiet (empty on a box with no gateway and on every
    // replay core). Taken here, before `config` moves into the struct below.
    let gateway_cursors: Vec<(ustr::Ustr, vike_exec::GatewayCursor)> = config
        .gateway_cursors
        .drain(..)
        .map(|(key, cursor)| (ustr::Ustr::from(key.as_str()), cursor))
        .collect();
    // The JOURNAL's ledgers (`CoreConfig::mount_attr`, the latest `Snap`'s rows) only for a mount
    // the file holds no ledger for: the file writes one per attributed fill, the `Snap` at its
    // cadence, so after a crash the file is the fresher of the two.
    for row in config.mount_attr.drain(..) {
        if file_ledgers.contains(&row.mount_id) {
            continue;
        }
        let ledger = MountAttribution {
            size: row.size,
            avg_px: row.avg_px,
            realized_pnl: row.realized_pnl,
            fees_paid: row.fees_paid,
        };
        match mount_ids.iter().position(|id| *id == row.mount_id) {
            Some(i) => mount_attr[i] = ledger,
            None => pending_owners.entry(row.mount_id).or_default().ledger = Some(ledger),
        }
    }
    // Each journaled engine's ACCOUNT state back onto the live engine with its route key (decision
    // 0127); the trading state only when stricter. Before the struct literal: the engines are still
    // these two locals.
    apply_restored_engines(
        &mut engine,
        &mut extra_engines,
        std::mem::take(&mut config.restored_engines),
        &mut boot_notes,
    );
    let mount_latched: Vec<bool> = vec![false; mounts.len()];
    // Per-mount DECLARED extra symbols (`StrategyMount::symbols`), parallel to `mounts` (same
    // indices) — the opt-in that makes the `Broker` verbs' `symbol` argument authoritative for
    // that mount. `any_mount_multi` is the byte-identical gate (mirrors `any_mount_budget`):
    // false — every mount that exists today — means the drain takes the identical
    // stamp-the-mount's-symbol path and every per-symbol read falls through to its scalar.
    let mount_symbols: Vec<Vec<MountLeg>> =
        mounts.iter().map(|m| m.as_ref().map(|m| m.symbols.clone()).unwrap_or_default()).collect();
    let any_mount_multi = mount_symbols.iter().any(|v| !v.is_empty());
    // The CROSS-VENUE reference-quote gate (the xEMM lane) — the narrower sibling of
    // `any_mount_multi`: `true` only when some mount declared a leg on a venue OTHER than its own,
    // which is the only shape [`CoreThread::drive_strategy_reference_quote`] can ever match. It is
    // computed HERE, once, precisely so the two per-market-message call sites in the `Ingest::Quote`
    // / `Ingest::Book` arms cost ONE BOOL LOAD on every runtime that has no such mount (i.e. every
    // runtime today) — the `p99 < 10µs` fold must not grow a `mounts` scan for a feature nobody
    // mounted. A same-venue-only declared mount (`MountLeg::same_venue`) leaves this `false`.
    let any_mount_ref = mounts.iter().enumerate().any(|(i, m)| {
        m.as_ref().is_some_and(|m| {
            mount_symbols[i].iter().any(|l| l.venue.as_deref().is_some_and(|v| v != m.venue))
        })
    });
    // steal/core-live-scheduler: resolve each mount's optional wall-clock LiveSchedule by its MOUNT
    // ID (the budget map's key, and for the same reason — see `CoreConfig::mount_schedules`) —
    // parallel to `mounts` (same indices), MOVED out of the config map (a LiveSchedule owns a Vec,
    // so unlike a Copy MountBudget it is removed, not copied).
    // `any_mount_schedule` is the byte-identical gate (mirrors `any_mount_budget`): false (no mount
    // has a non-empty schedule) ⇒ the boundary check is one bool read and `drive_schedule` never runs.
    let mount_schedule: Vec<LiveSchedule> = mount_ids
        .iter()
        .zip(&mounts)
        .map(|(id, slot)| {
            let schedule = config.mount_schedules.remove(id).unwrap_or_default();
            if slot.is_some() { schedule } else { LiveSchedule::default() }
        })
        .collect();
    let any_mount_schedule = mount_schedule.iter().any(|s| !s.is_empty());
    // Open the write-ahead journal ONCE, here, so BOTH spawn paths (single- and multi-symbol,
    // via spawn_core/spawn_core_multi) and the synchronous test core get it. A broken journal
    // dir MUST fail loudly (`expect`), never be silently dropped — a live core that thinks it is
    // journaling but is not is worse than one that refuses to start.
    let journal = config.journal.as_ref().map(|jc| {
        // Fire once at startup (here), not per-message: `snapshot_every == 0` makes the after-fold
        // `journaled_since_snap >= snapshot_every` cadence check ALWAYS true, so the core would take
        // a full-state Snap + blocking `flush()` on EVERY exec-lane message — a silent p99 cliff.
        // Turn that perf cliff loud.
        debug_assert!(
            jc.snapshot_every > 0,
            "snapshot_every must be >= 1 (0 would snapshot+flush every message)"
        );
        // TODO(follow-up): the single-writer interlock gave this `expect` a new, ROUTINE
        // operator-facing failure mode — a double-launch now lands here as an `AddrInUse`
        // io::Error and surfaces as a panic + backtrace, burying the actionable message the
        // lock error carefully composed. Follow-up: print the io::Error's Display and exit
        // non-zero instead of panicking. Left as-is in this lane (fail-loud is still correct).
        vike_journal::CommandJournal::open(&jc.dir, jc.file.clone())
            .expect("open write-ahead command journal")
    });
    // Open the opt-in mmap counter mirror ONCE here (audit co9), the same place + spirit as the
    // journal: a broken path the operator explicitly opted into MUST fail loudly at startup (before
    // any trading), never be silently dropped. `None` (default) = no file, byte-identical publish.
    let counters = config
        .counters_path
        .as_ref()
        .map(|p| crate::counters::CountersFile::create(p).expect("open mmap counters file"));
    let coid_gen = resume_coid_generator(&config);
    // Dead-man's switch (trading-hardening): build the pure trip-logic state machine ONLY when the
    // feature is opted in, BEFORE `config` moves into the struct literal below. `None` (default)
    // means the observe hook + boundary sweep are both inert, byte-identical to today.
    let deadman = config.deadman.as_ref().map(DeadMan::new);
    // The LINK dead-man (M13), on the same pattern and independently: `None` (default) means the
    // `Ingest::StreamStatus` observe hook and the boundary sweep are both inert.
    let link_deadman = config.link_deadman.as_ref().map(LinkDeadMan::new);
    // Read before `config` moves into the struct literal below: the arm-id counter's resume seed
    // (0 = fresh session, today's behavior). See `CoreConfig::arm_seq` for why it is resumed at all.
    let config_arm_seq = config.arm_seq;
    let (conditional_books, arm_routes) = restore_conditional_books(&mut config.conditionals);
    let conditional_owners = std::mem::take(&mut config.conditional_owners);
    let lost_held_exits = std::mem::take(&mut config.lost_held_exits);
    let (contingency, held_orders) = restore_contingencies(&mut config.contingencies);
    // Read before `config` moves into the struct literal below; see the field's construction
    // comment for why these three (and only these three) arm the waker-record write.
    let config_journal_waker_records = config.submit_ack_timeout.is_some()
        || config.deadman.is_some()
        || config.link_deadman.is_some();
    let mut core = CoreThread {
        engine,
        bus: EventBus::new(),
        config,
        market,
        snapshot,
        rejected,
        recent: VecDeque::new(),
        fault: None,
        seq: 0,
        dirty: false,
        bars: indexmap::IndexMap::new(),
        coid_gen,
        strategy_tags: indexmap::IndexMap::new(),
        mounts,
        mount_ids,
        mount_states,
        conditional_books,
        contingency,
        held_orders,
        arm_seq: config_arm_seq,
        // Refusal ids start at 0 every session and are deliberately NOT resumed — see the field.
        refusal_seq: 0,
        extra_engines,
        multi_account,
        coid_venue: std::collections::HashMap::new(),
        cond_engine: std::collections::HashMap::new(),
        journal,
        journaled_since_snap: 0,
        gateway_cursors,
        // WAKER-RECORD GATE (core-ergonomics review fix; NARROWED by emulator PR-5):
        // `Ingest::Watchdog` is a pure waker whose dispatch arm is `{}` — it can only matter
        // through the boundary sweeps it lets run. Of those, exactly TWO still mutate order/
        // trading state off wall clock in a way replay cannot reproduce: the stuck-order sweep and
        // the dead-man sweep. Journal the waker record only when one of those is enabled — that
        // record is what makes `replay_from` return `Unsupported`, and it MUST keep doing so for
        // those two (their wall-clock effects are not re-derivable).
        //
        // The GTD expiry sweep USED to be the third member and no longer is: it now journals each
        // expiry write-ahead (`JournalRecord::GtdExpire`) and its only local action —
        // `ExecutionEngine::cancel_order` — publishes nothing and mutates no fenced state (the
        // authoritative `OrderCanceled` arrives as its own journaled `Ingest::Event` and replays
        // independently). So a `gtd_sweep`-only core writes NO waker record and replays/crash-
        // restores normally. Enabling it ALONGSIDE the stuck-order watchdog or the dead-man switch
        // still forfeits replay — for those features' own reasons, loudly, exactly as before.
        // When none is on, the boundary can only run
        // replay-NEUTRAL observers (equity sample, strategy-state save, portfolio snapshot,
        // readiness probe), so the waker provably cannot move fenced state and journaling it would
        // only make an otherwise perfectly replayable session unreplayable. Byte-identical for
        // every pre-existing config: before this branch the waker thread was spawned ONLY when the
        // stuck-order watchdog or the dead-man switch was configured, i.e. exactly when this stays
        // `true`.
        journal_waker_records: config_journal_waker_records,
        counters,
        confirm_issued_ms: std::collections::HashMap::new(),
        exec_link: std::collections::BTreeMap::new(),
        pnl_curve_peak: None,
        pnl_curve_disarmed_noted: false,
        // 50ms resolution × 1024 slots ⇒ ~51s rotation; covers a typical watchdog half-timeout in
        // the bounded stepping path and falls back to the full-rotation pass for larger ones. Empty
        // until `arm_boundary_timers` arms the sweep cadence (only when the watchdog is enabled).
        timers: DeadlineTimerWheel::new(50, 1024),
        due_timers: Vec::new(),
        watchdog_tick_ms: 0,
        equity_timer: None,
        equity_rows: Vec::new(),
        state_save_timer: None,
        recon_alerts: indexmap::IndexMap::new(),
        recon_announce: recon_held::HeldAnnouncer::default(),
        recon_coin_deltas: indexmap::IndexMap::new(),
        recon_next_alert_id: 1,
        recon_last_pass_ts: 0,
        // Dead-man's switch (trading-hardening): the state machine built just above (only when the
        // feature is opted in). `deadman_tick_ms` is set later by `arm_boundary_timers`.
        deadman,
        deadman_tick_ms: 0,
        // The LINK dead-man (M13): same shape, same inert-when-`None` contract.
        link_deadman,
        link_deadman_tick_ms: 0,
        // Core-ergonomics: both tick cadences are set by `arm_boundary_timers` (only when their
        // config knob is `Some`); 0 + an empty set means the features are inert.
        gtd_tick_ms: 0,
        gtd_canceled: std::collections::HashSet::new(),
        portfolio_snap_tick_ms: 0,
        // Recon path-to-superset (F1-A): 0 tick + an empty dedup map ⇒ the fast in-flight confirm
        // is inert; `arm_boundary_timers` sets the tick only when `inflight_confirm` is `Some`.
        inflight_confirm_tick_ms: 0,
        inflight_confirm_last_ms: std::collections::HashMap::new(),
        // steal/core-per-mount-budget: attribution ledgers + coid->mount map + per-mount budget
        // latch. Both start EMPTY/zeroed unless a restart seeded them above (gap D — the map is
        // otherwise populated at strategy-submit); `any_mount_budget` gates the whole budget feature
        // to a byte-identical no-op when no mount has an active budget.
        mount_attr,
        coid_mount,
        // A RESTORED `coid_mount` seeds NO prune candidates: whether those pre-restart orders are
        // still live is not knowable from the map alone, and the queue only ever fills from a
        // TERMINAL this core actually observes. A restored entry whose order died before the restart
        // therefore lingers — bounded by the restore, not by session length, and strictly no worse
        // than before this queue existed.
        coid_terminal: VecDeque::new(),
        order_owners,
        pending_owners,
        restored_orders: indexmap::IndexMap::new(),
        restore_counters: RestoreCounters::default(),
        restored_tag_deny_until: None,
        mount_budget,
        mount_delays: vec![None; mount_vs.len()],
        mount_vs,
        data_delays: std::collections::HashMap::new(),
        mount_engine,
        mount_latched,
        mount_symbols,
        any_mount_multi,
        any_mount_ref,
        any_mount_budget,
        // steal/core-live-scheduler: per-mount wall-clock schedules + their boundary gate.
        // All-empty schedules ⇒ `any_mount_schedule` false ⇒ the boundary check is one bool read
        // and the feature is inert (byte-identical).
        mount_schedule,
        any_mount_schedule,
        // The tick lane's table, filled just below by the one builder, once `mounts`,
        // `mount_symbols` and `any_mount_multi` (all three read by the rule it is built from) are
        // on the struct.
        tick_audience: std::collections::HashMap::new(),
        // The mount-row cache starts EMPTY at epoch 0 (`CoreThread::mount_epoch`): see its doc.
        mount_rows: MountRowCache::empty(),
        mount_epoch: 0,
    };
    core.rebuild_tick_audience();
    // The boot restore's own lines first (decision 0127: its startup line, a torn or broken
    // journal, PROTECTION LOST, the ownership and engine notes above), then what the steps below
    // find.
    for n in boot_notes {
        core.note(n);
    }
    // Put each slot-bound restored order back under its owner: its strategy tag, its engine and its
    // row in the restored collection (all of it a no-op under `CoreConfig::restore_orders_off`).
    for (slot, order) in slot_bound {
        core.restore_owned_order(slot, order);
    }
    // The rest of the boot restore (`boot_restore.rs`): restored arms get their engine and owner
    // back, a held exit a broken journal lost is a terminal reject its mount hears, and a restored
    // order on a paper book is gone.
    core.restore_arm_routes(arm_routes, conditional_owners);
    core.tell_lost_held_exits(lost_held_exits);
    core.restored_paper_orders_gone_at_boot();
    // The operator's trace of ownership that found no mount: one line per mount id, in id order.
    let mut held: Vec<(&String, &PendingOwner)> = core.pending_owners.iter().collect();
    held.sort_unstable_by(|a, b| a.0.cmp(b.0));
    let notes: Vec<String> = held
        .into_iter()
        .map(|(id, p)| {
            format!(
                "ORDER OWNERS HELD: mount `{id}` is not mounted — {} resting order(s){} wait for a \
                 mount of that id; until one lands their fills reach no mount's ledger",
                p.coids.len(),
                if p.ledger.is_some() { " and its ledger" } else { "" }
            )
        })
        .collect();
    for n in notes {
        core.note(n);
    }
    core.note_gated_restores();
    core
}

/// [`assemble_core`]'s mount-id derivation, durable-state load and uniqueness assert, run once per
/// spawned core.
fn load_mount_ids(
    mounts: &mut [Option<StrategyMount>],
    state_dir: Option<&std::path::Path>,
) -> Vec<String> {
    // Portfolio-observer PR-4 T2: derive each mount's deterministic mount_id here, at the ONE
    // mount choke point, and — when a state_dir is configured — load its durable-state sidecar
    // (if one exists) into the strategy BEFORE it ever runs. `on_start` is not called anywhere
    // today; this is load-only, no lifecycle hook is added. `mount_ids` is stored on `CoreThread`
    // (parallel to `mounts`, same indices) so save-on-stop reuses it without recomputing.
    //
    // Multi-mount correctness (gap C): the id is now `mount_id_with`, i.e. this mount's OWN
    // `StrategyMount::controller_id` when it has one, else the SAME legacy
    // `{venue}__{symbol}__{interval}` derivation as before — so every controller-id-free config
    // (all of them today) computes byte-identical ids. The uniqueness assert below is what makes
    // the id an identity rather than a hint. Reading the id OFF THE MOUNT is load-bearing, not
    // cosmetic: the id is a state-sidecar filename AND the journal attribution key, so an
    // assembly-order-indexed source would silently hand mount B the id (and therefore the durable
    // state) of mount A the moment `extra_mounts` is reordered.
    let mount_ids: Vec<String> = mounts
        .iter_mut()
        .map(|slot| {
            let m = slot.as_mut().expect("just populated above");
            let mid = crate::strategy_state::mount_id_with(
                m.controller_id.as_deref(),
                &m.venue,
                &m.symbol,
                &m.interval,
            );
            if let Some(dir) = state_dir {
                let sidecar = crate::strategy_state::sidecar_path(dir, &mid);
                if let Some(v) = crate::strategy_state::read_json(&sidecar) {
                    // `load_state` is arbitrary user strategy code — guard it like every other
                    // strategy-hook call site in this file (see `dispatch()`'s `catch_unwind`).
                    // A corrupt/incompatible sidecar must fail OPEN (start fresh, un-loaded)
                    // rather than crash app startup; `AssertUnwindSafe` is sound for the same
                    // reason the module doc gives for the dispatch guard — a strategy left
                    // mid-panic is only ever read/overwritten again, never relied on to be
                    // consistent.
                    if let Err(payload) =
                        catch_unwind(AssertUnwindSafe(|| m.strategy.load_state(&v)))
                    {
                        tracing::warn!(
                            target: "vike_core::strategy_state",
                            mount_id = %mid,
                            reason = %panic_text(payload),
                            "strategy load_state panicked — starting fresh"
                        );
                    }
                }
            }
            mid
        })
        .collect();
    // Multi-mount correctness (gap C): mount identity must be UNIQUE, and a duplicate is a
    // configuration fault the runtime must not paper over. Two mounts sharing an id share ONE state
    // sidecar file (`<state_dir>/<mount_id>.json` — the later save silently overwrites the earlier
    // strategy's durable state), ONE journal `mount_id` provenance (so the coid->mount rebuild on
    // restart cannot tell them apart), and ONE identity everywhere else the id is a key. Failing
    // LOUDLY at mount time — before a single order can be minted — is the same discipline the
    // journal open a few lines below applies (`expect`: "a live core that thinks it is journaling but
    // is not is worse than one that refuses to start"). Cost is O(mounts²) over a handful of slots,
    // once, at assembly.
    for (i, id) in mount_ids.iter().enumerate() {
        if let Some(j) = mount_ids[..i].iter().position(|prev| prev == id) {
            panic!(
                "duplicate strategy-mount id `{id}` (mounts {j} and {i}): two mounts on one \
                 (venue, symbol, interval) share a state sidecar, a journal identity and a budget \
                 key — give each a distinct `StrategyMount::controller_id`"
            );
        }
    }
    mount_ids
}

/// [`assemble_core`]'s coid generator (resumed or fresh) and its instance-origin startup line,
/// run once per spawned core.
fn resume_coid_generator(config: &CoreConfig) -> ClientOrderIdGenerator {
    // Resume the coid generator across a restart when a session was persisted; else a fresh
    // random session (today's behavior).
    let coid_gen = match &config.coid_session {
        Some((session, seq)) => ClientOrderIdGenerator::resume(session.clone(), *seq),
        // A FRESH session is the only place the configured origin can be stamped — see
        // `CoreConfig::instance_origin` for why a resume is left verbatim.
        None => ClientOrderIdGenerator::with_origin(config.instance_origin.as_ref(), None),
    };
    // Say so when the ids this core is about to mint do NOT carry the origin the operator
    // configured. Silence here is the failure mode that matters: an operator who set
    // `instance_origin` to make a second deployment recognisable would otherwise believe the
    // claim is on the wire while a resumed session keeps minting untagged (or previously-tagged)
    // ids for as long as the journal survives.
    {
        let minting = coid_gen.origin();
        let configured = config.instance_origin.as_ref().map(|o| o.as_str());
        if minting != configured {
            tracing::warn!(
                minting = ?minting,
                configured = ?configured,
                "client order ids carry a DIFFERENT instance origin than the configured one — a \
                 restart resumes its persisted coid session verbatim, so the configured tag \
                 reaches the wire on the next FRESH session (see docs/ops/double-live-instances.md)"
            );
        } else if let Some(tag) = minting {
            // The POSITIVE line, and it is not decoration: `vike-cli config show` reports what was
            // CONFIGURED, and the two can differ for a whole journal's lifetime (the warn above).
            // This is the only place an operator can read what is actually going on the wire.
            tracing::info!(
                origin = %tag,
                "client order ids carry this instance's origin claim — another deployment's \
                 orders on this venue account will be recognised, never folded"
            );
        }
    }
    coid_gen
}

/// [`assemble_core`]'s conditional-book re-seed from the restored Snap capture, run once per
/// spawned core. Also hands back the `ArmRoute` of every arm it seeded, for
/// `CoreThread::restore_arm_routes` once the core exists.
fn restore_conditional_books(
    conditionals: &mut Vec<vike_journal::SnapConditional>,
) -> (indexmap::IndexMap<(String, String), ConditionalBook>, Vec<ArmRoute>) {
    // Emulator PR-3 (re-arm-on-restore): seed the conditional books from the restored Snap
    // capture, ONCE, here — [`CoreConfig::conditionals`]'s doc has the contract. Entry order is
    // fire order (the Snap captured the books' insertion order), so `entry().or_default()` +
    // per-book insert reproduces both the books map's and each book's insertion order exactly.
    // Empty (default) leaves the map empty — byte-identical to a pre-PR-3 runtime. Malformed or
    // duplicate entries fail OPEN and LOUD (skip + warn), never a panic at assembly: a restore
    // must not refuse to start over one bad arm, and losing that arm silently would be worse.
    let mut conditional_books: indexmap::IndexMap<(String, String), ConditionalBook> =
        indexmap::IndexMap::new();
    let mut routes: Vec<ArmRoute> = Vec::new();
    for c in conditionals.drain(..) {
        let vike_journal::SnapConditional { arm_id, terms, route_key } = c;
        let vike_journal::ConditionalRecord {
            venue,
            symbol,
            side,
            qty,
            price,
            trail,
            extreme,
            trigger_by,
        } = terms;
        let book_venue = venue.clone();
        let book = conditional_books.entry((venue, symbol)).or_default();
        let armed = if let (Some(trail), Some(extreme)) = (trail, extreme) {
            book.add_trailing(&arm_id, side, qty, trail, extreme, trigger_by)
        } else if let Some(px) = price {
            book.add_stop(&arm_id, side, qty, px, trigger_by)
        } else {
            tracing::warn!(
                target: "vike_core::core",
                arm_id = %arm_id,
                "restore: conditional with neither price nor trail+extreme — skipped"
            );
            continue;
        };
        if !armed {
            tracing::warn!(
                target: "vike_core::core",
                arm_id = %arm_id,
                "restore: duplicate conditional arm id — skipped (first entry kept)"
            );
            continue;
        }
        routes.push(ArmRoute { arm_id, venue: book_venue, route_key });
    }
    (conditional_books, routes)
}

/// [`assemble_core`]'s contingency-book and held-order re-seed from the restored Snap capture, run
/// once per spawned core.
fn restore_contingencies(
    contingencies: &mut Vec<vike_journal::SnapContingency>,
) -> (ContingencyBook, indexmap::IndexMap<String, OrderRequest>) {
    // Live-runtime OTO/OCO re-arm-on-restore (the contingency twin of the conditional re-seed just
    // above): rebuild the shared `ContingencyBook` + the held-order map from the restore base's Snap
    // capture, ONCE, here. `insert_active` restores each leg's EXACT armed/held state (plain
    // `insert` would re-HOLD an already-armed exit); a HELD leg's captured request is put back into
    // `held_orders` so its parent's fill can release it. Order is the captured insertion order (the
    // book's arm/cancel iteration order). Empty (default) leaves both empty — byte-identical to a
    // pre-OCO/OTO runtime (and a pre-v11 journal's Snap carries none, so it restores empty). Zero
    // hot-fold cost: consumed entirely here.
    let mut contingency = ContingencyBook::new();
    let mut held_orders: indexmap::IndexMap<String, OrderRequest> = indexmap::IndexMap::new();
    for c in contingencies.drain(..) {
        let vike_journal::SnapContingency { coid, parent, linked, active, held_request } = c;
        contingency.insert_active(coid.clone(), parent, linked, active);
        if let Some(req) = held_request {
            held_orders.insert(coid, req);
        }
    }
    (contingency, held_orders)
}
