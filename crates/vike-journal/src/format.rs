//! The segment format: header layout, the stamped version and the range this build reads.

use std::io;

#[cfg(doc)]
use crate::JournalRecord;

pub(crate) const MAGIC: u32 = 0x314C_4A56; // "VJL1" LE
/// The format version THIS build stamps on every segment it creates. See
/// [`MIN_READABLE_VERSION`] for which older versions it still accepts.
///
/// Not every added field steps it: `ParamsUpdate::mount_id` (`Command::UpdateParams` rides inside
/// `Ingest::Command`) is `serde(default)` and skipped when `None`, so a journal written before it
/// reads unchanged and an unaddressed update keeps its old bytes; an older build reading a journal
/// that DOES carry one would ignore the key. Replay does nothing with an update either way: the
/// replay core mounts only an inert strategy (`crates/vike-core/src/replay/refold.rs`'s
/// `replay_from`, on an EMPTY interval no real update names, with the default no-op
/// `on_params_updated`), and the refusal note it writes sits in a recent-events ring that no
/// snapshot or state hash covers. The precedent is `ConditionalIntent::trigger_by`.
pub(crate) const VERSION: u32 = 16; // added MintedSubmit.route_key + MarginCallLiquidate.route_key (15: AccountState.route_key; 14: MarginCallLiquidate.mount_id; 13: Snap.mount_attr; 12: ScheduleFire; 11: Snap.contingencies; 10: GtdExpire; 9: MarginCallLiquidate; 8: Snap.conditionals; 7: ConditionalDisarmed + Snap.arm_seq; 6: ConditionalArmed/Fire; 5: PortfolioSnap; 4: MintedSubmit; 3: StrategySubmit; 2 before)
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
pub(crate) const MIN_READABLE_VERSION: u32 = 4;
pub(crate) const HEADER: usize = 16; // magic u32 | version u32 | first_seq u64

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
