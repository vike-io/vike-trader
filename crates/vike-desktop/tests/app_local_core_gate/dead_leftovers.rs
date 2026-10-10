//! ⚠ **The local core's always-`None` / always-empty LEFTOVERS stay deleted.** When the desktop
//! lost its local trading core (rulings 1 and 2, #1727) a set of slots outlived it: fields bound
//! only to `None` or to an empty collection, the teardown steps that took them, the arm-table
//! variant that named the deleted composition, and a picker flag nothing could set to `false`. Each
//! read as live wiring while doing nothing, in a crate no CI test executes. They were deleted in
//! one cut; this pins that none comes back under its old name. The BACKFILL TAIL followed on
//! 2026-10-10: three receivers built with their sender already dropped (`bf_rx`, `bf_done_rx`,
//! `poly_resolve_rx`), the staging only one of them could fill (`bf_pending`, its cap and trim) and
//! a paging-floor map no feed wrote (`earliest_live_ids`) — every drain over them answered
//! `Disconnected` on its first call, so none could ever run its body.
//!
//! The rule is the NAME, scanned as an identifier token over the same comment-free GUI code
//! [`super::the_gui_names_nothing_from_vike_core`] scans, plus three struct bodies for the two
//! leftovers whose bare word is too common to ban everywhere (`recorder`: the options chain
//! recorder is live; `available`: `ui.available_width()` is egui's).

use super::{code, fn_body, gui_sources};

/// The deleted names, as identifier tokens, each with what it was. A token match, so a longer
/// identifier that merely CONTAINS one (`armed_live_venues`, `chain_recorder`) is not a hit.
const DEAD_TOKENS: [(&str, &str); 17] = [
    ("materializer", "`App::materializer`, a journal materializer nothing spawned"),
    ("MaterializerHandle", "its type — the GUI's only reason to link vike-journal"),
    ("RecorderHandleTy", "the uninhabited type `App::recorder` was declared as"),
    ("live_locks", "the live-account lock guards of a mount the desktop does not run"),
    ("_live_locks", "`App::_live_locks`, the same guards, always empty"),
    ("forwarder_stop", "the drain-stop of a live-event forwarder nothing spawns"),
    (
        "live_venues",
        "`App::live_venues`, the armed-LIVE set of exec clients the desktop has none of",
    ),
    ("LocalCore", "`split_plane::AppMode::LocalCore`, the deleted local-core composition"),
    ("app_mode", "`split_plane::app_mode`, whose only row a launch reached was a constant"),
    // The backfill tail (2026-10-10): the sender-less receivers `dead_receiver` built after the
    // local market-data plane left, and the staging only `bf_rx` could ever fill.
    ("bf_rx", "`App::bf_rx`, the aggTrades-backfill batch lane, whose sender was dropped at birth"),
    ("bf_done_rx", "`App::bf_done_rx`, the backfill workers' exit-report lane, sender-less too"),
    ("bf_pending", "`CoreSyncState::bf_pending`, staging only `bf_rx` batches could fill"),
    ("BF_PENDING_MAX_TICKS", "the cap on that staging"),
    ("trim_pending_backfill", "the trim that enforced the cap"),
    ("earliest_live_ids", "`App::earliest_live_ids`, a paging floor map no feed wrote"),
    ("poly_resolve_rx", "`App::poly_resolve_rx`, the cockpit's Gamma-resolver lane, sender-less"),
    ("dead_receiver", "the helper that built those receivers with their sender already dropped"),
];

/// `(struct head, field token, what it was)`: a field no struct of that name may carry again.
const DEAD_FIELDS: [(&str, &str, &str); 3] = [
    (
        "struct App {",
        "recorder",
        "`App::recorder`, a tick-recorder handle that could only be `None`",
    ),
    ("struct ObserverMount {", "recorder", "the same handle, on its way into `App`"),
    ("pub struct BackendPicker<'a> {", "available", "`BackendPicker::available`, always `true`"),
];

/// Whether `line` holds `token` as a whole identifier (no identifier character on either side).
fn has_token(line: &str, token: &str) -> bool {
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    line.match_indices(token).any(|(start, _)| {
        let before = line[..start].chars().next_back();
        let after = line[start + token.len()..].chars().next();
        !before.is_some_and(is_ident) && !after.is_some_and(is_ident)
    })
}

/// THE PREDICATE, over `(repo-relative path, source)` pairs. `Err` lists every site.
fn gui_holds_no_dead_leftovers(sources: &[(String, String)]) -> Result<(), String> {
    let mut sites: Vec<String> = Vec::new();
    for (path, src) in sources {
        let code = code(src);
        for line in code.lines() {
            for (token, what) in DEAD_TOKENS {
                if has_token(line, token) {
                    sites.push(format!("  {path}: `{token}` ({what}): {}", line.trim()));
                }
            }
        }
        for (head, field, what) in DEAD_FIELDS {
            if fn_body(&code, head).is_some_and(|body| body.lines().any(|l| has_token(l, field))) {
                sites.push(format!("  {path}: `{head}` carries `{field}` ({what})"));
            }
        }
    }
    if sites.is_empty() {
        return Ok(());
    }
    Err(format!("{} dead-leftover sites\n{}", sites.len(), sites.join("\n")))
}

/// ⚠ **No always-`None` / always-empty leftover of the local core is back.** A slot nothing fills
/// is not neutral here: it reads as wiring to the next person who touches the shell, and the
/// teardown, dispatch and arming code that consulted these slots did nothing for a month.
#[test]
fn the_local_cores_dead_leftovers_stay_deleted() {
    let sources = gui_sources();
    // Non-vacuity: each struct head is still found, so a rename cannot turn a field check off.
    for (head, _, _) in DEAD_FIELDS {
        assert!(
            sources.iter().any(|(_, s)| fn_body(&code(s), head).is_some()),
            "no GUI file declares `{head}` any more — re-point this gate at the renamed struct"
        );
    }
    if let Err(why) = gui_holds_no_dead_leftovers(&sources) {
        panic!(
            "the GUI holds a leftover of the deleted local core: {why}\n\n  \
             The desktop has no local core, no exec client, no recorder and no journal to \
             materialize. A slot only ever `None`/empty, or a flag only ever one value, is dead \
             code wearing wiring's clothes: delete it rather than re-adding it."
        );
    }
}

/// The predicate refuses each planted leftover and accepts the shipped look-alikes, so "this gate
/// would catch it" is shown rather than asserted.
#[test]
fn the_dead_leftover_gate_can_actually_fail() {
    let one = |src: &str| gui_holds_no_dead_leftovers(&[("x.rs".to_string(), src.to_string())]);
    let shipped = concat!(
        "struct App {\n    feeds: Feeds,\n    shutdown: Arc<AtomicBool>,\n}\n",
        "struct ObserverMount {\n    books: Arc<BookStore>,\n}\n",
        "pub struct BackendPicker<'a> {\n    pub backends: &'a BackendsFile,\n    pub reported: Option<R>,\n}\n",
        "let n = vike_mount::armed_live_venues(&vars);\n",
        "let rec = provider.chain_recorder();\n",
        "let w = ui.available_width();\n",
        "let r = provider.chain_recorder().expect(\"recorder reached the options provider\");\n",
        "fn no_recorder_is_the_default() {}\n",
        "let mode = AppMode::ObserveOnly;\n",
        "// App::materializer and live_venues in a full-line comment are prose\n",
        "let ids = binance_feed.earliest_live_id_floor();\n",
        "let bf_spawned: HashSet<String> = HashSet::new();\n",
    );
    assert_eq!(one(shipped), Ok(()), "the shipped shapes");

    for (planted, what) in [
        ("    materializer: Option<vike_journal::materialize::MaterializerHandle>,\n", "a field"),
        ("let m: Option<MaterializerHandle> = None;\n", "a binding of the type"),
        ("enum RecorderHandleTy {}\n", "the uninhabited stand-in"),
        ("let live_locks = Vec::new();\n", "a lock vector"),
        ("    _live_locks: live_locks,\n", "the field init"),
        ("self.forwarder_stop.store(true, Relaxed);\n", "a teardown store"),
        ("venue_is_live: app.live_venues.contains(v),\n", "the seed read"),
        ("    LocalCore,\n", "the variant"),
        ("let m = split_plane::app_mode(false, true);\n", "the arm table"),
        ("let n = 1; // live_venues\n", "a trailing comment (`code` keeps it)"),
        ("    bf_rx: Receiver<(String, Vec<TradeTick>)>,\n", "the batch lane field"),
        ("for report in app.bf_done_rx.try_iter() {}\n", "the exit-report drain"),
        ("    bf_pending: &mut self.bf_pending,\n", "the staging slot"),
        ("let cap = core_sync::BF_PENDING_MAX_TICKS;\n", "the staging cap"),
        ("trim_pending_backfill(held, cap);\n", "the trim"),
        ("slots.earliest_live_ids.lock().unwrap().clear();\n", "the paging-floor teardown"),
        ("while let Ok(r) = app.poly_resolve_rx.try_recv() {}\n", "the Gamma drain"),
        ("let rx = dead_receiver::<u8>();\n", "the sender-less constructor"),
    ] {
        assert!(one(&format!("{shipped}{planted}")).is_err(), "{what} passed: {planted}");
    }
    for (struct_with_field, what) in [
        ("struct App {\n    recorder: Option<R>,\n}\n", "`App::recorder`"),
        ("struct ObserverMount {\n    recorder: Option<R>,\n}\n", "`ObserverMount::recorder`"),
        ("pub struct BackendPicker<'a> {\n    pub available: bool,\n}\n", "`available`"),
    ] {
        assert!(one(struct_with_field).is_err(), "{what} passed");
    }
    let err = one("let a = live_venues;\nlet b = forwarder_stop;\n").unwrap_err();
    assert!(err.starts_with("2 dead-leftover sites\n"), "the count line: {err}");
}
