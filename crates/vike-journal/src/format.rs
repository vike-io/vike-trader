//! The segment format: header layout, the stamped version and the range this build reads.

use std::io;

#[cfg(doc)]
use crate::JournalRecord;

pub(crate) const MAGIC: u32 = 0x314C_4A56; // "VJL1" LE
/// The NEWEST format version this build writes and reads. A segment carries it only once a record
/// that needs it has been appended ([`BASE_VERSION`] is what a segment is stamped until then). See
/// [`MIN_READABLE_VERSION`] for which older versions it still accepts.
///
/// Not every added field steps it: `ParamsUpdate::mount_id` (`Command::UpdateParams` rides inside
/// `Ingest::Command`) is `serde(default)` and skipped when `None`, so a journal written before it
/// reads unchanged and an unaddressed update keeps its old bytes; an older build reading a journal
/// that DOES carry one would ignore the key. Replay does nothing with an update either way: the
/// replay core mounts only an inert strategy (`crates/vike-core/src/replay/refold.rs`'s
/// `replay_from`, on an EMPTY interval no real update names, with the default no-op
/// `on_params_updated`), and the refusal note it writes sits in a recent-events ring that no
/// snapshot or state hash covers. The precedent is `ConditionalIntent::trigger_by`. The same holds
/// for `route_key` on [`JournalRecord::StrategySubmit`] and [`JournalRecord::ConditionalFire`]
/// (the engine a re-applied write-ahead record lowers onto): written only by a core whose engines an
/// order's venue cannot tell apart, `serde(default)` and omitted when `None`, and a build from before
/// it ignores the key and restores such a core from the latest `Snap` alone, which is all it ever did
/// with one.
pub(crate) const VERSION: u32 = 21; // added Command::SetAccountTradingState and Command::HoldAccount (20: the quote-currency and margin-rate fields of Command::AdmitSymbol and AccountSnapshot.fx; 19: Command::AdmitSymbol; 18: Ingest::GatewayEvent + Snap.gateway_cursors; 17: EngineSnapshot.route_key + SnapConditional.route_key; 16: MintedSubmit.route_key + MarginCallLiquidate.route_key; 15: AccountState.route_key; 14: MarginCallLiquidate.mount_id; 13: Snap.mount_attr; 12: ScheduleFire; 11: Snap.contingencies; 10: GtdExpire; 9: MarginCallLiquidate; 8: Snap.conditionals; 7: ConditionalDisarmed + Snap.arm_seq; 6: ConditionalArmed/Fire; 5: PortfolioSnap; 4: MintedSubmit; 3: StrategySubmit; 2 before)
/// The OLDEST on-disk format version this build can read and resume (see the module doc's
/// "Version compatibility" contract). Raise this — never silently — only when a version step stops
/// being purely additive (a renamed/removed variant, or a changed `Ingest`/`EngineSnapshot` field
/// shape); that is what turns a stale journal back into a loud cold-start error.
///
/// 4 -> 5 added ONE variant ([`JournalRecord::PortfolioSnap`]), 5 -> 6 added TWO
/// ([`JournalRecord::ConditionalArmed`], [`JournalRecord::ConditionalFire`]), 6 -> 7 added
/// ONE variant ([`JournalRecord::ConditionalDisarmed`]) plus ONE `#[serde(default)]` `Option`
/// field on `Snap` (`arm_seq`, `None` ⇔ absent), 7 -> 8 added ONE `#[serde(default)]` `Vec`
/// field on `Snap` (`conditionals`, empty ⇔ absent — a pre-v8 journal restores with EMPTY books,
/// today's behavior, never an error), 8 -> 9 added ONE variant
/// ([`JournalRecord::MarginCallLiquidate`]) to an externally-tagged serde_json enum and nothing
/// else, so every v4..v8 frame is byte-for-byte a valid v9 frame; 10 added `GtdExpire`; 10 -> 11
/// added ONE `#[serde(default)]` `Vec` field on `Snap` (`contingencies`, empty ⇔ absent — a pre-v11
/// journal restores with EMPTY held/linked contingency state, today's behavior, never an error); and
/// 11 -> 12 added ONE variant ([`JournalRecord::ScheduleFire`]) to the externally-tagged enum and
/// nothing else, so every v4..v11 frame is byte-for-byte a valid v12 frame; 12 -> 13 added ONE
/// `#[serde(default)]` `Vec` field on `Snap` (`mount_attr`, empty ⇔ absent — a pre-v13 journal
/// restores with EMPTY per-mount attribution ledgers, exactly its pre-feature behavior, never an
/// error), so every v4..v12 frame is still a valid v13 frame; and 13 -> 14 added ONE
/// `#[serde(default)]` `Option` field on [`JournalRecord::MarginCallLiquidate`] (`mount_id`, `None`
/// ⇔ absent — a pre-v14 frame reads back as the ACCOUNT-wide margin-call case, which is what every
/// such frame written before the per-mount budget latch existed actually was, and a pre-v14 latch
/// flatten restores unattributed exactly as it did before, never an error).
///
/// 14 -> 15 added ONE `#[serde(default)]` `Option` field to `vike_model::events::AccountState`
/// (`route_key`, `None` ⇔ absent — WHICH account of an exchange an account-wide balance snapshot
/// belongs to), reached through the nested [`vike_exec::Ingest`] enum. Additive in both directions
/// that matter: every v4..v14 frame is a valid v15 frame and reads back `None`, which is exactly
/// what every such frame meant — one account per venue.
///
/// ⚠ It is also the first step whose bump is load-bearing in the OTHER direction, which is worth
/// saying because the field is `skip_serializing_if`-omitted and a v14 build would therefore parse
/// a v15 frame without complaint. It would parse it WRONG: serde ignores the unknown key, so a
/// labelled account's balance snapshot would read back unrouted and overwrite the DEFAULT
/// account's balance. `check_version`'s "any version ABOVE `VERSION`" rejection is the only thing
/// that can catch that, and it can only catch it if this number moved.
///
/// 15 -> 16 added TWO `#[serde(default)]` `Option<String>` fields, both
/// `skip_serializing_if`-omitted and both meaning WHICH ACCOUNT of a venue a write-ahead record's
/// order was lowered onto: [`JournalRecord::MintedSubmit`]`::route_key` and
/// [`JournalRecord::MarginCallLiquidate`]`::route_key` (the account-routing spec's §9 item 12).
/// Additive in the direction that matters: every v4..v15 frame is a valid v16 frame and reads back
/// `None`, which is exactly what every such frame meant — that venue's sole account.
///
/// ⚠ The bump is load-bearing in the OTHER direction for the same reason v15's was, and the two
/// cases are NOT equally bad. A v15 build parsing a v16 `MintedSubmit` would ignore the key and
/// mis-ATTRIBUTE a materialized order row — bad, and recoverable. A v15 build parsing a v16
/// `MarginCallLiquidate` would ignore the key and re-apply the liquidation through a venue-string
/// route, i.e. against a book it was not read from. `check_version`'s "any version ABOVE `VERSION`"
/// rejection is the only thing that catches either, and it catches them only because this moved.
///
/// 16 -> 17 added TWO `#[serde(default)]` `Option<String>` fields, both
/// `skip_serializing_if`-omitted and both meaning WHICH ACCOUNT of a venue a `Snap` entry belongs
/// to: `vike_exec::EngineSnapshot::route_key` (written only when the engine's route key is not its
/// venue, i.e. for a labelled account) and [`crate::SnapConditional`]`::route_key` (written only on
/// a core holding two engines of one exchange). They are what lets a boot restore
/// (`vike_core::replay::restore_for_boot`, decision 0127) put each engine's account state and each
/// armed stop back on the book it came from. Additive: every v4..v16 frame is a valid v17 frame and
/// reads back `None`, and a single-account box writes byte-identical `Snap` RECORDS — only the
/// header version moves. The bump is load-bearing in the other direction for the reason v15's was:
/// a v16 build parsing a v17 `Snap` would drop the keys and restore a labelled account's positions
/// onto the venue's default account.
///
/// 17 -> 18 added ONE variant to the nested [`vike_exec::Ingest`] enum, `GatewayEvent` (a venue event
/// that arrived over a gateway daemon's trade link, with the position it held in that daemon's
/// per-account event ring), and ONE `#[serde(default)]` `Vec` field on `Snap`, `gateway_cursors`
/// (one `(route key, cursor)` pair per account, empty ⇔ absent). Additive: every v4..v17 frame is
/// a valid v18 frame and reads back with no cursors, and a run that never receives a gateway event
/// writes byte-identical RECORDS (the Snap field is `skip_serializing_if`-omitted when empty), so
/// only the header version moves.
///
/// ⚠ The bump is load-bearing in the OTHER direction and the failure it prevents is not a
/// mis-routing but a LOSS: a v17 build handed a v18 journal would fail to parse the first
/// `GatewayEvent` frame (an unknown variant), and a reader that halted there on a torn-tail
/// reading would drop every venue event after it. `check_version`'s "any version ABOVE `VERSION`"
/// rejection turns that into a refused start, which is the only thing that can, and it can only
/// because this number moved. A ROLLBACK past this step therefore needs the journal directory
/// moved aside first (the trade daemon's ops runbook, its rollback section).
///
/// 18 -> 19 added ONE variant to the nested `vike_exec::Command` enum, `AdmitSymbol` (a symbol an
/// account's engine takes on demand, carrying that symbol's own grid and contract value, decision
/// 0173). Additive: every v4..v18 frame is a valid v19 frame, and a box that never admits a symbol
/// writes the records it always did, so only the header version moves. The bump is load-bearing in
/// the other direction for v18's reason: a v18 build handed a v19 journal would fail to parse the
/// first `AdmitSymbol` frame, and `check_version`'s rejection of a forward version turns that into
/// a refused start instead of a half-read journal.
///
/// ⚠ v19 is the first step stamped LAZILY (see [`BASE_VERSION`]): opening a v18 journal with this
/// build leaves its header at 18, and the header becomes 19 only when the first `AdmitSymbol` is
/// appended (`CommandJournal::append_cmd`). So the one-way door closes on first USE of any-symbol
/// trading, not on the first start, and a deploy this build loses before then rolls back onto a
/// v18 build without moving the journal aside.
///
/// 19 -> 20 gave an OLD record a NEW MEANING: `Command::AdmitSymbol` gained four `serde(default)`,
/// `skip_serializing_if`-omitted fields (`quote_ccy`, `quote_rate`, `rate_as_of_ms`: the currency a
/// symbol is quoted in and the rate that turns its money into the account's; `im_requirement`: the
/// symbol's own initial-margin rate), and
/// `vike_exec::AccountSnapshot` gained a sparse `fx` field (the foreign-quoted symbols and a rate per
/// currency, absent while there are none). Additive for the parser, and that is exactly the danger: a
/// v19 build would read a rate-carrying admission, silently ignore the unknown keys, and fold a JPY
/// symbol at par: every PnL, margin and notional of it 150x wrong, with no error anywhere. Only
/// the header refuses it, so the stamp rises to 20 LAZILY, before the first `AdmitSymbol` that names
/// a quote currency or states a margin rate (`version_needed`) or the first `Snap` holding an `fx` table
/// (`snap_version_needed`), and a journal that never trades a foreign-quoted symbol stays at 19 (or
/// 18) and keeps its rollback. Every other `AdmitSymbol` still needs only 19. A roll carries the
/// stamp, as it did for 19.
///
/// 20 -> 21 added TWO variants to the nested `vike_exec::Command` enum, `SetAccountTradingState` (the
/// trading state of ONE account's engine, addressed by route key) and `HoldAccount` (the node's own
/// closing-only hold of a lane twin it keeps up to be closed). Additive for the parser in the
/// way v19's was: a v20 build handed a v21 journal would fail to parse the first such frame, and
/// `check_version`'s rejection of a forward version turns that into a refused start instead of a
/// half-read journal. It is stamped LAZILY (`version_needed`): a journal that never carries either
/// record stays at its old stamp (18, 19 or 20) and keeps its rollback; once it has, a build older
/// than the one that wrote the stamp refuses the directory, so a rollback then moves the journal
/// aside first (the closed rollback door the v18 and v19 steps already are). This is a stamp, not a
/// migration: nothing is rewritten and every v4..v20 frame reads unchanged.
///
/// ⚠ **The door can close with NO operator action**, and there are THREE ways in: (1) the operator's
/// `trade account-state`; (2) the account watcher (`vike-tradehub`'s `sync_twin_holds`) holding a lane
/// twin kept up with something open (`HoldAccount`, raised by the NODE); (3) the startup path that
/// re-halts a restored engine under a `reducing`/`halted` profile (`send_initial_trading_state`'s
/// `SetAccountTradingState`, raised at boot when the restore was not uniform). A box that does any of the
/// three is stamped 21 and a build that reads `4..=20` refuses its journal: a rollback then means
/// restoring the journal as it was before the deploy. Neither record is journaled when it would change
/// nothing (a release with no hold, an unknown or stopped account, a state the engine already holds, a
/// word dropped for a link-held engine), so a no-op never stamps.
pub(crate) const MIN_READABLE_VERSION: u32 = 4;
/// The format version a segment is stamped with until a record only a newer one can read is
/// appended: a fresh segment, and a resumed older one (restamped UP to this, never past it).
/// [`VERSION`] is the newest this build can write; a segment is raised to it by
/// `CommandJournal::append_cmd` when the record needs it and never lowered. Raise this, in the same
/// commit as the step it opens, only when that step is NOT gated on a record (a changed field of a
/// record every box writes), and say in the step's paragraph above why no record can gate it.
pub(crate) const BASE_VERSION: u32 = 18;
pub(crate) const HEADER: usize = 16; // magic u32 | version u32 | first_seq u64

/// The version `Command::AdmitSymbol` first needed: a [`BASE_VERSION`] reader cannot parse the variant.
pub(crate) const ADMIT_SYMBOL_VERSION: u32 = 19;

/// The version an `AdmitSymbol` naming a quote currency or stating a margin rate first needed (and a
/// `Snap` holding the rate table): a v19 reader would fold the symbol at par.
pub(crate) const QUOTE_CCY_VERSION: u32 = 20;

/// The version `Command::SetAccountTradingState` and `Command::HoldAccount` first needed: a v20 reader
/// cannot parse either variant.
pub(crate) const ACCOUNT_STATE_VERSION: u32 = 21;

/// The version a segment must carry BEFORE `msg` is appended to it: [`ACCOUNT_STATE_VERSION`] for a
/// `SetAccountTradingState` or `HoldAccount`, [`QUOTE_CCY_VERSION`] for an
/// `AdmitSymbol` that names a quote currency (a v19 reader would fold its symbol at par) or states
/// the symbol's own margin rate (a v19 reader would margin it at the engine-wide rate, which for a
/// 1x spot symbol on a leveraged engine UNDER-states the margin it needs),
/// [`ADMIT_SYMBOL_VERSION`] for any other `AdmitSymbol`, [`BASE_VERSION`] for the rest.
pub(crate) fn version_needed(msg: &vike_exec::Ingest) -> u32 {
    match msg {
        vike_exec::Ingest::Command(vike_exec::Command::AdmitSymbol(a)) => {
            if a.quote_ccy.is_some() || a.im_requirement.is_some() {
                QUOTE_CCY_VERSION
            } else {
                ADMIT_SYMBOL_VERSION
            }
        }
        vike_exec::Ingest::Command(
            vike_exec::Command::SetAccountTradingState { .. }
            | vike_exec::Command::HoldAccount { .. },
        ) => ACCOUNT_STATE_VERSION,
        _ => BASE_VERSION,
    }
}

/// The version a segment must carry BEFORE a `Snap` of `engines` is appended: [`QUOTE_CCY_VERSION`]
/// when any engine holds a foreign-quoted symbol (the table that makes its symbols' units the
/// account's — without it a v19 build restores their base units at par), [`BASE_VERSION`] otherwise.
///
/// The `AdmitSymbol` that created the table normally raised the stamp already and a roll carries it;
/// this is the belt for a `Snap` that outlives its `AdmitSymbol` in a segment that does not.
pub(crate) fn snap_version_needed(engines: &[vike_exec::EngineSnapshot]) -> u32 {
    if engines.iter().any(|e| e.account.fx.is_some()) { QUOTE_CCY_VERSION } else { BASE_VERSION }
}

/// Gate an on-disk segment's stamped version against what this build understands.
///
/// ACCEPTS `MIN_READABLE_VERSION..=VERSION`. REJECTS anything older (a shape this build can no
/// longer parse) and anything NEWER (a forward version written by a later build, whose frames may
/// carry variants that do not exist here — a hard `InvalidData`, never a best-effort parse).
pub(crate) fn check_version(version: u32) -> io::Result<()> {
    if (MIN_READABLE_VERSION..=VERSION).contains(&version) {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "journal format version mismatch: got {version}, this build reads \
             {MIN_READABLE_VERSION}..={VERSION}"
        ),
    ))
}

#[path = "format_tests.rs"]
#[cfg(test)]
mod format_tests;
