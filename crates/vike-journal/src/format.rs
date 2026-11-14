//! The segment format: header layout, the stamped version and the range this build reads.

use std::io;

#[cfg(doc)]
use crate::JournalRecord;

pub(crate) const MAGIC: u32 = 0x314C_4A56; // "VJL1" LE
/// The step that SPELLED OUT the stored words (phase 4, P4.5): `JournalRecord::Command` and `Snapshot`,
/// `PortfolioSnapshot`, `OrderEmulation{Armed,Fire,Disarmed}`, the `order_client_id_*` keys and the
/// batch-C words (`sequence`, `message`, `request`, `average_price`, `mount_attribution`, ...). A RENAME
/// is not additive: an older segment's keys are unknown here and its defaulted ones would read back
/// EMPTY, so this one number is the write version, the base AND the floor ([`VERSION`],
/// [`BASE_VERSION`], [`MIN_READABLE_VERSION`]): a journal written before it is a loud cold-start
/// refusal, and the deploy moves it aside on the owner's `allow_journal_move` (`deploy/docker/vikectl`'s
/// `move_journal_aside`: in the stop window, a checked copy in `var/deploy/journal.pre.<stamp>`, then the
/// directory renamed to `<journal>.moved.<stamp>`; without the word that deploy is refused). A record added
/// with the same deploy (the 2b strategy-file mount) joins THIS version rather than stepping again.
pub(crate) const STORED_WORDS_VERSION: u32 = 30;
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
/// for `route_key` on [`JournalRecord::StrategySubmit`] and [`JournalRecord::OrderEmulationFire`]
/// (the engine a re-applied write-ahead record lowers onto): written only by a core whose engines an
/// order's venue cannot tell apart, `serde(default)` and omitted when `None`, and a build from before
/// it ignores the key and restores such a core from the latest `Snapshot` alone, which is all it ever did
/// with one.
pub(crate) const VERSION: u32 = STORED_WORDS_VERSION; // the stored words spelled out (29: added EngineSnapshot.fill_floor_ms, a Snapshot's fill ledger kept as a window of days (28: SharedCloseTimeout, a shared close's timeout; 27: Command::Emulated and Snapshot.emulated, an emulated order; 26: OrderListLeg::trail, an order list's trailing leg; 25: OrderBracket::order_client_id, a bracket's group stem; 24: OrderReleasePrice::venue, a release read on another venue; 23: OrderIntent::Exercise and the combo leg limit; 22: OrderIntent::OrderList and the OUO link; 21: Command::SetAccountTradingState and Command::HoldAccount; 20: the quote-currency and margin-rate fields of Command::AdmitSymbol and EngineSnapshotAccount.fx; 19: Command::AdmitSymbol; 18: Ingest::GatewayEvent + Snapshot.gateway_cursors; 17: EngineSnapshot.route_key + JournalSnapshotOrderEmulationArm.route_key; 16: MintedSubmit.route_key + MarginCallLiquidate.route_key; 15: AccountState.route_key; 14: MarginCallLiquidate.mount_id; 13: Snapshot.mount_attribution; 12: ScheduleFire; 11: Snapshot.contingencies; 10: GtdExpire; 9: MarginCallLiquidate; 8: Snapshot.conditionals; 7: OrderEmulationDisarmed + Snapshot.arm_sequence; 6: OrderEmulationArmed/Fire; 5: PortfolioSnapshot; 4: MintedSubmit; 3: StrategySubmit; 2 before)
/// The OLDEST on-disk format version this build can read and resume (see the module doc's
/// "Version compatibility" contract). Raise this — never silently — only when a version step stops
/// being purely additive (a renamed/removed variant, or a changed `Ingest`/`EngineSnapshot` field
/// shape); that is what turns a stale journal back into a loud cold-start error.
///
/// 4 -> 5 added ONE variant ([`JournalRecord::PortfolioSnapshot`]), 5 -> 6 added TWO
/// ([`JournalRecord::OrderEmulationArmed`], [`JournalRecord::OrderEmulationFire`]), 6 -> 7 added
/// ONE variant ([`JournalRecord::OrderEmulationDisarmed`]) plus ONE `#[serde(default)]` `Option`
/// field on `Snapshot` (`arm_sequence`, `None` ⇔ absent), 7 -> 8 added ONE `#[serde(default)]` `Vec`
/// field on `Snapshot` (`conditionals`, empty ⇔ absent — a pre-v8 journal restores with EMPTY books,
/// today's behavior, never an error), 8 -> 9 added ONE variant
/// ([`JournalRecord::MarginCallLiquidate`]) to an externally-tagged serde_json enum and nothing
/// else, so every v4..v8 frame is byte-for-byte a valid v9 frame; 10 added `GtdExpire`; 10 -> 11
/// added ONE `#[serde(default)]` `Vec` field on `Snapshot` (`contingencies`, empty ⇔ absent — a pre-v11
/// journal restores with EMPTY held/linked contingency state, today's behavior, never an error); and
/// 11 -> 12 added ONE variant ([`JournalRecord::ScheduleFire`]) to the externally-tagged enum and
/// nothing else, so every v4..v11 frame is byte-for-byte a valid v12 frame; 12 -> 13 added ONE
/// `#[serde(default)]` `Vec` field on `Snapshot` (`mount_attribution`, empty ⇔ absent — a pre-v13 journal
/// restores with EMPTY per-mount attribution ledgers, exactly its pre-feature behavior, never an
/// error), so every v4..v12 frame is still a valid v13 frame; and 13 -> 14 added ONE
/// `#[serde(default)]` `Option` field on [`JournalRecord::MarginCallLiquidate`] (`mount_id`, `None`
/// ⇔ absent — a pre-v14 frame reads back as the ACCOUNT-wide margin-call case, which is what every
/// such frame written before the per-mount budget latch existed actually was, and a pre-v14 latch
/// flatten restores unattributed exactly as it did before, never an error).
///
/// 14 -> 15 added ONE `#[serde(default)]` `Option` field to `vike_model::events::AccountState`
/// (`route_key`, `None` ⇔ absent — WHICH account of an exchange an account-wide balance snapshot
/// belongs to), reached through the nested [`vike_execution::Ingest`] enum. Additive in both directions
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
/// `skip_serializing_if`-omitted and both meaning WHICH ACCOUNT of a venue a `Snapshot` entry belongs
/// to: `vike_execution::EngineSnapshot::route_key` (written only when the engine's route key is not its
/// venue, i.e. for a labelled account) and [`crate::JournalSnapshotOrderEmulationArm`]`::route_key` (written only on
/// a core holding two engines of one exchange). They are what lets a boot restore
/// (`vike_core::replay::restore_for_boot`, decision 0127) put each engine's account state and each
/// armed stop back on the book it came from. Additive: every v4..v16 frame is a valid v17 frame and
/// reads back `None`, and a single-account box writes byte-identical `Snapshot` RECORDS — only the
/// header version moves. The bump is load-bearing in the other direction for the reason v15's was:
/// a v16 build parsing a v17 `Snapshot` would drop the keys and restore a labelled account's positions
/// onto the venue's default account.
///
/// 17 -> 18 added ONE variant to the nested [`vike_execution::Ingest`] enum, `GatewayEvent` (a venue event
/// that arrived over a gateway daemon's trade link, with the position it held in that daemon's
/// per-account event ring), and ONE `#[serde(default)]` `Vec` field on `Snapshot`, `gateway_cursors`
/// (one `(route key, cursor)` pair per account, empty ⇔ absent). Additive: every v4..v17 frame is
/// a valid v18 frame and reads back with no cursors, and a run that never receives a gateway event
/// writes byte-identical RECORDS (the Snapshot field is `skip_serializing_if`-omitted when empty), so
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
/// 18 -> 19 added ONE variant to the nested `vike_execution::Command` enum, `AdmitSymbol` (a symbol an
/// account's engine takes on demand, carrying that symbol's own grid and contract value, decision
/// 0173). Additive: every v4..v18 frame is a valid v19 frame, and a box that never admits a symbol
/// writes the records it always did, so only the header version moves. The bump is load-bearing in
/// the other direction for v18's reason: a v18 build handed a v19 journal would fail to parse the
/// first `AdmitSymbol` frame, and `check_version`'s rejection of a forward version turns that into
/// a refused start instead of a half-read journal.
///
/// ⚠ v19 is the first step stamped LAZILY (see [`BASE_VERSION`]): opening a v18 journal with this
/// build leaves its header at 18, and the header becomes 19 only when the first `AdmitSymbol` is
/// appended (`Journal::append_command`). So the one-way door closes on first USE of any-symbol
/// trading, not on the first start, and a deploy this build loses before then rolls back onto a
/// v18 build without moving the journal aside.
///
/// 19 -> 20 gave an OLD record a NEW MEANING: `Command::AdmitSymbol` gained four `serde(default)`,
/// `skip_serializing_if`-omitted fields (`quote_currency`, `quote_rate`, `rate_as_of_ms`: the currency a
/// symbol is quoted in and the rate that turns its money into the account's; `initial_margin_requirement`: the
/// symbol's own initial-margin rate), and
/// `vike_execution::EngineSnapshotAccount` gained a sparse `fx` field (the foreign-quoted symbols and a rate per
/// currency, absent while there are none). Additive for the parser, and that is exactly the danger: a
/// v19 build would read a rate-carrying admission, silently ignore the unknown keys, and fold a JPY
/// symbol at par: every PnL, margin and notional of it 150x wrong, with no error anywhere. Only
/// the header refuses it, so the stamp rises to 20 LAZILY, before the first `AdmitSymbol` that names
/// a quote currency or states a margin rate (`version_needed`) or the first `Snapshot` holding an `fx` table
/// (`snap_version_needed`), and a journal that never trades a foreign-quoted symbol stays at 19 (or
/// 18) and keeps its rollback. Every other `AdmitSymbol` still needs only 19. A roll carries the
/// stamp, as it did for 19.
///
/// 20 -> 21 added TWO variants to the nested `vike_execution::Command` enum, `SetAccountTradingState` (the
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
/// `trade account`; (2) the account watcher (`vike-tradehub`'s `sync_twin_holds`) holding a lane
/// twin kept up with something open (`HoldAccount`, raised by the NODE); (3) the startup path that
/// re-halts a restored engine under a `reducing`/`halted` profile (`send_initial_trading_state`'s
/// `SetAccountTradingState`, raised at boot when the restore was not uniform). A box that does any of the
/// three is stamped 21 and a build that reads `4..=20` refuses its journal: a rollback then means
/// restoring the journal as it was before the deploy. Neither record is journaled when it would change
/// nothing (a release with no hold, an unknown or stopped account, a state the engine already holds, a
/// word dropped for a link-held engine), so a no-op never stamps.
///
/// 21 -> 22 (wave 4 of the order kinds) added ONE variant to the nested `vike_execution::OrderIntent` enum,
/// `OrderList` (a linked group of orders: an OCO, OTO or OUO group, a ladder), and gave the OLD
/// `contingency_type` slot of an `OrderRequest` and of `JournalSnapshotContingency` a NEW word, `OUO` (a leg
/// whose fill RESIZES its siblings). A v21 build fails to parse the first `OrderList` frame, which
/// restores nothing from the directory, and it reads an `OUO` leg back as the `String` it knows and
/// folds it by its shape, as OCO: a ladder's first target fill would CANCEL the protective stop it
/// should shrink. So it is stamped LAZILY, by three things only: the first `OrderList` frame, accepted
/// or refused ([`version_needed`] and [`intent_version_needed`]: the write-ahead frame is journaled
/// before the core judges it, and a v21 build that cannot parse it would move the directory aside
/// instead of refusing the header in words), the first lone `OUO` leg the core ACCEPTS
/// (`Journal::stamp_order_list_version`: its `Submit` frame parses everywhere), and, as the belt,
/// a `Snapshot` whose book holds an `OUO` leg ([`snap_version_needed`]). A journal that never saw either
/// keeps its stamp and its bytes. The rollback line: mass-cancel every order list before a rollback,
/// and move the journal aside once it is stamped 22.
///
/// 22 -> 23 (wave 6 of the order kinds) added ONE variant to `vike_execution::OrderIntent`, `Exercise` (an
/// option exercise, refused on every venue), and gave the OLD `OrderCombo` three NEW keys: `limit` on
/// its legs (a combo priced by its legs), `account` and `order_client_id`. A v22 build fails to parse
/// the first `Exercise` frame, which restores nothing from the directory; it reads a leg-limit combo's
/// legs WITHOUT their limits, so its `build_combo` lowers the spec as a combo MARKET order (no net
/// limit, no leg limit); it routes a combo naming an account by its venue alone; and it mints its own
/// id for a combo its sender named, so the order the venue holds is not the one it restores. Its refold
/// would then mint, send or route what this build refused, held at a limit or put on another account.
/// So it is stamped LAZILY, by the first `Exercise` frame or combo frame carrying any of the keys, accepted or
/// refused ([`intent_version_needed`]: written before the core judges it). No `Snapshot` belt: a
/// leg-limit combo rests only on a paper book (no venue row takes one), a restart empties a paper
/// book, and a `Snapshot` keys each engine's orders by its own route key. A journal that never saw one
/// keeps its stamp and its bytes; the rollback line: move the journal aside once it is stamped 23.
///
/// 23 -> 24 (the attributes wave, decision 0267's 2.13) gave `vike_model::OrderReleasePrice` an
/// additive `venue` (a release's price condition read on ANOTHER venue, absent when `None`). The type
/// is `deny_unknown_fields`, so a v23 build cannot PARSE a record carrying the key (the boot restore
/// would restore nothing and move the directory aside): stamped LAZILY, by a `Submit` whose release
/// names a venue in a write-ahead frame ([`intent_version_needed`], so `append_command` and
/// `append_strategy_submit` stamp it), by a `MintedSubmit` carrying one (the belt in
/// `append_minted_submit`), and by a `Snapshot` whose held-for-release table holds one
/// ([`snap_version_needed`]). Independent of 23's step: a journal with a combo frame and no such
/// release stays 23, one with such a release is 24. A journal that never sees one keeps its stamp and
/// its bytes. The rollback line: cancel every HELD order whose release names a venue, then move the
/// journal aside once it is stamped 24.
///
/// 24 -> 25 (the order client id v2, decision 0270) gave the OLD `vike_model::OrderBracket` a NEW key,
/// `order_client_id`: the sender's group STEM, from which the core derives the three leg ids, so a
/// bracket resent after an unclear reply names the same legs. `OrderBracket` takes no
/// `deny_unknown_fields`, so a v24 build PARSES such a frame and drops the key, and its refold mints
/// three ids of its own: the legs the venue holds are not the ones it restores. So it is stamped
/// LAZILY, by the first bracket frame that names its stem, accepted or refused ([`intent_version_needed`]:
/// written before the core judges it, so `append_command` and `append_strategy_submit` stamp it). No
/// `Snapshot` belt: a `Snapshot` holds a bracket's legs under the ids already minted, never its stem.
/// Independent of the steps before it. A journal that never sees a stem keeps its stamp and its bytes;
/// the rollback line: move the journal aside once it is stamped 25.
///
/// 25 -> 26 (decision 0269's W3c) gave the OLD `vike_model::OrderListLeg` a NEW key, `trail`: a
/// trailing leg's distance, so an order list can hold a trailing exit live. `OrderListLeg` takes no
/// `deny_unknown_fields`, so a v25 build PARSES such a frame and drops the key, and its refold lowers a
/// trailing leg with no trail (refused by its terms rule, so the list is refused whole): the legs the
/// venue holds are not the ones it restores. So it is stamped LAZILY, by the first order-list frame
/// with a trailing leg, accepted or refused ([`intent_version_needed`]: written before the core judges
/// it, so `append_command` and `append_strategy_submit` stamp it). No `Snapshot` belt: a `Snapshot` holds a held
/// leg as its `OrderRequest`, whose `trail` a v25 reader knows. Independent of the steps
/// before it. A journal that never sees one keeps its stamp and its bytes; the rollback line: move the
/// journal aside once it is stamped 26.
///
/// 28 -> 29 bounded the `Snapshot` by what is live (`vike_execution::Engine::journal_snapshot`): its
/// registry leaves ENDED orders out (counted, `ended_orders_left_out`; their ids ride `recent_order_client_ids`), it
/// keeps only the newest closed-PnL rows (`vike_execution::JOURNAL_CLOSED_PNLS`), and its two trade-id sets keep a
/// window of days (`seen_trade_days`, `seen_fsm_trade_days`) and, once that window left an id out, a FLOOR
/// (`fill_floor_ms`). All of it is additive for the parser (no `deny_unknown_fields`). Only the floor changes
/// what a record MEANS to an older build, which would read the windowed list as the whole ledger and book a
/// re-delivery of a dropped fill again: so it is stamped LAZILY, by the first `Snapshot` that states a floor
/// ([`snap_version_needed`]). The rest needs no step: a v28 build never reads an ended order out of a `Snapshot`
/// for anything but the late-retry memory, which `recent_order_client_ids` now carries, and reads no closed-PnL row after
/// a restart. A journal whose ledgers never span the window keeps its stamp and its rollback; the rollback
/// line: move the journal aside once it is stamped 29.
///
/// 29 -> 30 ([`STORED_WORDS_VERSION`]) is the first step that is NOT additive, so the floor moves with it: the
/// owner's phase-4 rulings spelled the journal's stored words out in full, with no serde rename keeping the old
/// ones: the order client id keys (2026-11-11: `Snapshot.order_client_id_sequence`, `order_client_id_session`,
/// `recent_order_client_ids`, a mount row's `order_client_id_code`, `previous_order_client_id`, the
/// `order_client_id` of `GtdExpire`, of a contingency leg and of an emulated child) and the record and key words
/// (`Command`, `Snapshot`, `PortfolioSnapshot`, `OrderEmulation{Armed,Fire,Disarmed}`, `order_emulation_arms`,
/// `mount_attribution`, `arm_sequence`, `timestamp`, `sequence` and the rest); the old spellings are in
/// `CHANGELOG.md`. A v29 frame parsed under the new names would lose those keys (the order client id sequence,
/// the session, the late-retry memory), which is a silent mis-read, so this build REFUSES every segment below 30
/// and stamps every segment 30 from its first byte ([`BASE_VERSION`]): no record gates the step, because every
/// `Snapshot` carries renamed keys. The lazy steps 19..=29 above are history; their `*_VERSION` gates still name
/// what a record needs, and all of them are at or below the base now. The deploy moves the old journal aside
/// (`allow_journal_move`, after a copy), and an older build refuses a v30 journal.
///
/// The same step carries ONE new variant, [`JournalRecord::StrategyFileMounted`] (the strategy file a mount
/// opened and the sha256 of the bytes read, written once when the mount opens), and an additive
/// `strategy_file` on a `Snapshot`'s mount row (`JournalSnapshotMountAttribution::strategy_file`, absent while
/// `None`), the owner's 2026-11-13 ruling that 2b's record rides this step rather than one of its own.
pub(crate) const MIN_READABLE_VERSION: u32 = STORED_WORDS_VERSION;
/// The format version a segment is stamped with until a record only a newer one can read is
/// appended: a fresh segment, and a resumed older one (restamped UP to this, never past it).
/// [`VERSION`] is the newest this build can write; a segment is raised to it by
/// `Journal::append_command` when the record needs it and never lowered. Raise this, in the same
/// commit as the step it opens, only when that step is NOT gated on a record (a changed field of a
/// record every box writes), and say in the step's paragraph above why no record can gate it. It is
/// [`STORED_WORDS_VERSION`] with [`MIN_READABLE_VERSION`] (renamed keys ride every `Snapshot`).
pub(crate) const BASE_VERSION: u32 = STORED_WORDS_VERSION;
pub(crate) const HEADER: usize = 16; // magic u32 | version u32 | first_sequence u64

/// The version `Command::AdmitSymbol` first needed: a [`BASE_VERSION`] reader cannot parse the variant.
pub(crate) const ADMIT_SYMBOL_VERSION: u32 = 19;

/// The version an `AdmitSymbol` naming a quote currency or stating a margin rate first needed (and a
/// `Snapshot` holding the rate table): a v19 reader would fold the symbol at par.
pub(crate) const QUOTE_CURRENCY_VERSION: u32 = 20;

/// The version `Command::SetAccountTradingState` and `Command::HoldAccount` first needed: a v20 reader
/// cannot parse either variant.
pub(crate) const ACCOUNT_STATE_VERSION: u32 = 21;

/// The version an `OrderIntent::OrderList` and an `OUO` link first needed: a v21 reader cannot parse
/// the variant and folds an OUO leg as OCO.
pub(crate) const ORDER_LIST_VERSION: u32 = 22;

/// The version an `OrderIntent::Exercise`, a combo priced by its legs, a combo naming its account and
/// a combo naming its order client id first needed: a v22 reader cannot parse the variant, lowers a
/// leg-limit combo as a combo market order, routes a combo by its venue alone and mints its own id.
pub(crate) const LEG_LIMIT_EXERCISE_VERSION: u32 = 23;

/// The version a request whose release reads a price on another venue (`OrderReleasePrice::venue`) first
/// needed: a v23 reader cannot parse the key (`deny_unknown_fields`).
pub(crate) const RELEASE_VENUE_VERSION: u32 = 24;

/// The version a bracket naming its group stem (`OrderBracket::order_client_id`) first needed: a v24
/// reader drops the key and mints its own leg ids on replay.
pub(crate) const BRACKET_STEM_VERSION: u32 = 25;

/// The version an order list holding a trailing leg (`OrderListLeg::trail`) first needed: a v25 reader
/// drops the key and refolds the leg with no trail.
pub(crate) const TRAILING_LEG_VERSION: u32 = 26;

/// The version an EMULATED order first needed (decision 0275): a `Command::Emulated` step, and a `Snapshot`
/// holding an emulated parent (`Snapshot.emulated`). A v26 reader cannot parse the step, and would read the
/// `Snapshot` back without its parents, whose children it would then adopt as plain orders nobody works. A
/// node that never emulates never writes either, and keeps its rollback.
pub(crate) const EMULATED_VERSION: u32 = 27;

/// The version a shared close's timeout (`JournalRecord::SharedCloseTimeout`) first needed (decision 0271): a
/// build from before it cannot parse the variant, and a frame it cannot parse ends the segment as unreadable,
/// so the first marker raises the header to a step that build refuses instead. A node whose shared closes
/// never time out keeps its stamp. (A mount's closed-trade ledger in `Snapshot.closed_trades` needs no step: a
/// build from before the key ignores it.)
pub(crate) const SHARED_CLOSE_VERSION: u32 = 28;

/// The version a `Snapshot` whose fill ledger LEFT AN ID OUT first needed (`vike_execution::EngineSnapshot::fill_floor_ms`,
/// the journal `Snapshot` keeping a window of days of trade ids: `vike_execution::account::fill_window`). A v28 reader parses
/// it and reads the windowed list as the whole ledger, with no floor, so a venue re-delivering a fill the window
/// dropped would be booked a second time. Stamped LAZILY by [`snap_version_needed`]: a journal whose ledgers span
/// less than the window (every box younger than it, every quiet account) never writes a floor and keeps its
/// rollback.
pub(crate) const FILL_WINDOW_VERSION: u32 = 29;

/// The version a record carrying `req` needs for its release: [`RELEASE_VENUE_VERSION`] when its price
/// condition names a venue, [`BASE_VERSION`] otherwise. Three `Option` tests.
pub(crate) fn release_version_needed(req: &vike_model::OrderRequest) -> u32 {
    if req.release.and_then(|r| r.price).is_some_and(|c| c.venue.is_some()) {
        RELEASE_VENUE_VERSION
    } else {
        BASE_VERSION
    }
}

/// The version a segment must carry BEFORE `msg` is appended to it: [`ACCOUNT_STATE_VERSION`] for a
/// `SetAccountTradingState` or `HoldAccount`, [`QUOTE_CURRENCY_VERSION`] for an
/// `AdmitSymbol` that names a quote currency (a v19 reader would fold its symbol at par) or states
/// the symbol's own margin rate (a v19 reader would margin it at the engine-wide rate, which for a
/// 1x spot symbol on a leveraged engine UNDER-states the margin it needs),
/// [`ADMIT_SYMBOL_VERSION`] for any other `AdmitSymbol`, [`ORDER_LIST_VERSION`] for an order list and
/// [`LEG_LIMIT_EXERCISE_VERSION`] for an exercise or a combo with an account, an order client id or leg
/// limits, [`BRACKET_STEM_VERSION`] for a bracket naming its stem, [`TRAILING_LEG_VERSION`] for an
/// order list with a trailing leg ([`intent_version_needed`]), [`BASE_VERSION`] for the rest.
pub(crate) fn version_needed(msg: &vike_execution::Ingest) -> u32 {
    match msg {
        vike_execution::Ingest::Command(vike_execution::Command::AdmitSymbol(a)) => {
            if a.quote_currency.is_some() || a.initial_margin_requirement.is_some() {
                QUOTE_CURRENCY_VERSION
            } else {
                ADMIT_SYMBOL_VERSION
            }
        }
        vike_execution::Ingest::Command(
            vike_execution::Command::SetAccountTradingState { .. }
            | vike_execution::Command::HoldAccount { .. },
        ) => ACCOUNT_STATE_VERSION,
        vike_execution::Ingest::Command(vike_execution::Command::Order(intent)) => {
            intent_version_needed(intent)
        }
        vike_execution::Ingest::Command(vike_execution::Command::OrderOn(routed)) => {
            intent_version_needed(&routed.intent)
        }
        vike_execution::Ingest::Command(vike_execution::Command::Emulated(_)) => EMULATED_VERSION,
        _ => BASE_VERSION,
    }
}

/// The version a record carrying `intent` needs: [`ORDER_LIST_VERSION`] for an order list (written
/// before the core judges it, so a list it refuses stamps too: a v21 build cannot parse the frame),
/// [`LEG_LIMIT_EXERCISE_VERSION`] for an exercise and for a combo that names an account or a client
/// order id or whose legs carry a limit (the same way), [`RELEASE_VENUE_VERSION`] for a submit whose
/// release names a price venue (the same reason, a v23 build), [`BRACKET_STEM_VERSION`] for a bracket
/// naming its stem (a v24 build), [`TRAILING_LEG_VERSION`] for an order list with a trailing leg (a
/// v25 build), [`BASE_VERSION`] for every other intent. One discriminant test, three `Option` tests on
/// a submit, one on a bracket, one per leg of an order list.
pub(crate) fn intent_version_needed(intent: &vike_execution::OrderIntent) -> u32 {
    match intent {
        vike_execution::OrderIntent::Bracket(spec) if spec.order_client_id.is_some() => {
            BRACKET_STEM_VERSION
        }
        vike_execution::OrderIntent::OrderList(spec)
            if spec.legs.iter().any(|l| l.trail.is_some()) =>
        {
            TRAILING_LEG_VERSION
        }
        vike_execution::OrderIntent::OrderList(_) => ORDER_LIST_VERSION,
        vike_execution::OrderIntent::Exercise(_) => LEG_LIMIT_EXERCISE_VERSION,
        vike_execution::OrderIntent::Combo(spec)
            if spec.account.is_some()
                || spec.order_client_id.is_some()
                || spec.legs.iter().any(|l| l.limit.is_some()) =>
        {
            LEG_LIMIT_EXERCISE_VERSION
        }
        vike_execution::OrderIntent::Submit(req) => release_version_needed(req),
        _ => BASE_VERSION,
    }
}

/// The version a segment must carry BEFORE a `Snapshot` of `engines`, the contingency book
/// `contingencies` and the held-for-release table `held` is appended: [`RELEASE_VENUE_VERSION`] when
/// a held release names a price venue (a v23 build cannot parse it), [`ORDER_LIST_VERSION`] when the
/// book holds an `OUO` leg (a v21 build restores it as OCO), [`QUOTE_CURRENCY_VERSION`] when any engine
/// holds a foreign-quoted symbol (the table that makes its symbols' units the account's — without it a
/// v19 build restores their base units at par), [`BASE_VERSION`] otherwise.
///
/// The record that created either normally raised the stamp already and a roll carries it; this is
/// the belt for a `Snapshot` that outlives it in a segment that does not. [`FILL_WINDOW_VERSION`] comes first,
/// and no record but the `Snapshot` gates it: an engine whose fill ledger states a floor (it left an id out).
pub(crate) fn snap_version_needed(
    engines: &[vike_execution::EngineSnapshot],
    contingencies: &[crate::JournalSnapshotContingency],
    held: &[crate::OrderHeldRelease],
    emulated: &[crate::JournalSnapshotOrderEmulation],
) -> u32 {
    if engines.iter().any(|e| e.fill_floor_ms.is_some()) {
        FILL_WINDOW_VERSION
    } else if !emulated.is_empty() {
        EMULATED_VERSION
    } else if held.iter().any(|h| release_version_needed(&h.request) == RELEASE_VENUE_VERSION) {
        RELEASE_VENUE_VERSION
    } else if contingencies
        .iter()
        .any(|c| c.contingency_type == Some(vike_model::ContingencyType::Ouo))
    {
        ORDER_LIST_VERSION
    } else if engines.iter().any(|e| e.account.fx.is_some()) {
        QUOTE_CURRENCY_VERSION
    } else {
        BASE_VERSION
    }
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
