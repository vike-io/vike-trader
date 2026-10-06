use super::*;
use crate::node::{account_admin_source, keys_for_account_capability};
use crate::summary::summary_line;
use crate::venue_arming::{DeadManActionToCore, deadman_absent_warning};
use vike_core::CoreSnapshot;
use vike_tradehub_client::proto::Scope;

// ---------------------------------------------------------------------------------------------
// THE ACCOUNT-ADMIN BARRIER (`account_admin_source`) — `docs/decisions/0065-accounts-are-
// managed-and-the-barrier-is-declared.md` §3c, the ONE site that decides whether this daemon
// can write credentials from the wire.
//
// ⚠ Every assertion below is about an ABSENCE, which is the whole of Part 1: `None` means the
// process holds no writer for a frame to reach, so the verb is refused *because there is
// nothing in the process to refuse with*. A test that only checked a refusal MESSAGE would be
// checking the wrong thing — the message is what a client sees when the capability is absent,
// and the capability's absence is what makes the message true.
// ---------------------------------------------------------------------------------------------

/// A node-key store holding all three keys, the shape `from_vars_with_admin` reads.
fn admin_keys() -> HashMap<String, String> {
    HashMap::from([
        (vike_tradehub_client::auth::OBSERVE_KEY_ENV.to_string(), "o".repeat(32)),
        (vike_tradehub_client::auth::CONTROL_KEY_ENV.to_string(), "c".repeat(32)),
        (vike_tradehub_client::auth::ADMIN_KEY_ENV.to_string(), "a".repeat(32)),
    ])
}

/// …and the same store WITHOUT the admin key — a box that has the ordinary node pair and has
/// never minted the third.
fn pair_only() -> HashMap<String, String> {
    HashMap::from([
        (vike_tradehub_client::auth::OBSERVE_KEY_ENV.to_string(), "o".repeat(32)),
        (vike_tradehub_client::auth::CONTROL_KEY_ENV.to_string(), "c".repeat(32)),
    ])
}

fn loopback_bind() -> Vec<std::net::SocketAddr> {
    vec!["127.0.0.1:7879".parse().expect("a literal loopback address parses")]
}

/// A bind the process CAN see is wide — the `0.0.0.0` wildcard, which `bind_exposure`
/// classifies as `Public` deliberately because it is the commonest way this surface gets
/// accidentally exposed.
fn wide_bind() -> Vec<std::net::SocketAddr> {
    vec!["0.0.0.0:7879".parse().expect("a literal wildcard address parses")]
}

/// The settings directory these tests hand in. It never has to EXIST: this function decides a
/// capability and opens no store — the store is opened per frame, by the server.
fn any_dir() -> &'static std::path::Path {
    std::path::Path::new("/nonexistent/settings")
}

/// ⚠ **THE BARRIER'S ONE CHECK.** `loopback` is the value whose assertion the process can
/// verify, so it is the one it verifies: a wide bind under that declaration arms NOTHING, and
/// the daemon keeps trading with the capability down rather than publishing a key-material
/// surface on an address anyone can reach.
///
/// This is the refusal `docs/decisions/0065` §3c Part 3 promised, landed where the process has
/// evidence for it rather than at a frame where it has none.
#[test]
fn a_wide_bind_refuses_the_account_capability_under_the_loopback_declaration() {
    assert!(
        account_admin_source(Some("loopback"), &wide_bind(), &admin_keys(), Some(any_dir()))
            .is_none(),
        "a `loopback` declaration over a non-loopback bind must arm NO account capability — \
             the declaration is the one assertion this process can check, and it is false here"
    );
}

/// ⚠ …and `tradehub_allow_public_bind` CANNOT waive it, which is the shape copied from
/// `vike_datahub_client::bind`'s unwaivable `RefuseUnauthenticated` arm.
///
/// The proof is STRUCTURAL rather than one more assertion about a flag: that flag is not an
/// input to this function at all, so there is no parameter through which consent to publish an
/// ORDER surface could become consent to publish a key-material one. Setting it in the one map
/// this function does read changes nothing, because nothing here looks for it. An operator who
/// genuinely wants the verbs on a wide bind says `contained`, which asserts a barrier OUTSIDE
/// the process rather than permission for there to be none.
#[test]
fn the_public_bind_opt_in_cannot_waive_the_loopback_refusal() {
    let mut keys = admin_keys();
    keys.insert("VIKE_TRADEHUB_ALLOW_PUBLIC_BIND".to_string(), "1".to_string());
    assert!(
        account_admin_source(Some("loopback"), &wide_bind(), &keys, Some(any_dir())).is_none(),
        "no value reachable from this function's inputs may waive the loopback check"
    );
}

/// The declaration the process CANNOT check arms regardless of the bind — and that is the
/// point of the third value rather than a hole in the second. `docs/decisions/0026` forbids
/// INFERRING a container's containment; it does not forbid an operator DECLARING it, and a
/// declared barrier is auditable in a way an inferred one is not.
#[test]
fn contained_arms_regardless_of_the_bind() {
    let armed =
        account_admin_source(Some("contained"), &wide_bind(), &admin_keys(), Some(any_dir()))
            .expect("`contained` is an assertion about a barrier outside this process");
    assert_eq!(armed.barrier, server::accounts::AccountBarrier::Contained);
    assert_eq!(armed.settings_dir, any_dir());
}

/// The ordinary armed case: declared `loopback`, bound on loopback, admin key present.
#[test]
fn a_loopback_declaration_over_a_loopback_bind_arms_the_capability() {
    let armed =
        account_admin_source(Some("loopback"), &loopback_bind(), &admin_keys(), Some(any_dir()))
            .expect("the one shape this process can verify, verified");
    assert_eq!(armed.barrier, server::accounts::AccountBarrier::Loopback);
}

/// ⚠ **A TYPO IS OFF, NOT AN ERROR AND NOT A GUESS.** `loopbak` must never arm a credential
/// surface, and the safe direction is the one where the capability does not exist. The daemon
/// logs the value it did not recognise, so an operator who typed one is told rather than left
/// believing the barrier is up.
#[test]
fn an_unrecognised_declaration_arms_nothing() {
    for typo in ["loopbak", "true", "1", "yes", "LOOPBACK", "on", "contained "] {
        let armed =
            account_admin_source(Some(typo), &loopback_bind(), &admin_keys(), Some(any_dir()));
        // ⚠ `"contained "` is in this list to pin the TRIM, not to refuse it: a trailing space
        // off a paste is not a typo, and it arms. Every other spelling here must not.
        if typo.trim() == "contained" {
            assert!(armed.is_some(), "a trailing space is trimmed, not refused");
        } else {
            assert!(armed.is_none(), "`{typo}` is not one of the two spellings");
        }
    }
}

/// Unset, blank and `off` are the DEFAULT, and the default is byte-identical to a binary
/// without the verb: no capability, no admin key read, no feature advertised.
#[test]
fn unset_blank_and_off_are_all_the_default() {
    for decl in [None, Some(""), Some("   "), Some("off")] {
        assert!(
            account_admin_source(decl, &loopback_bind(), &admin_keys(), Some(any_dir())).is_none(),
            "{decl:?} must leave this daemon byte-identical to one without the verb"
        );
    }
}

/// ⚠ **THE KEY IS THE SECOND GATE, and it is the one a settings write cannot forge.** A box
/// that declared the barrier and never minted an admin key arms nothing — which is what keeps
/// `docs/decisions/0065`'s named self-escalation path (a Control peer writing the declaration
/// into a settings file it can reach) from arming the capability.
#[test]
fn a_declaration_with_no_admin_key_arms_nothing() {
    for decl in ["loopback", "contained"] {
        assert!(
            account_admin_source(Some(decl), &loopback_bind(), &pair_only(), Some(any_dir()))
                .is_none(),
            "`{decl}` with no admin key in the node store must arm nothing"
        );
    }
    // …and a BLANK admin key is an absent one, exactly as a blank observe or control key is.
    let mut blank = pair_only();
    blank.insert(vike_tradehub_client::auth::ADMIN_KEY_ENV.to_string(), "   ".to_string());
    assert!(
        account_admin_source(Some("loopback"), &loopback_bind(), &blank, Some(any_dir())).is_none(),
        "a blank admin key is an absent one"
    );
}

/// A daemon whose BOOT resolved no project has no store to administer, and resolving one here
/// would be the `$VIKE_SETTINGS_DIR`-blind resolver's failure wearing a new surface.
#[test]
fn a_declaration_with_no_settings_directory_arms_nothing() {
    assert!(
        account_admin_source(Some("contained"), &loopback_bind(), &admin_keys(), None).is_none(),
        "no settings directory means no store to administer"
    );
}

/// ⚠ **THE ADMIN KEY IS NEVER LOADED WHEN THE CAPABILITY IS NOT ARMED**, which is the
/// key-ZEROING gate the control key already has, wearing the other polarity and one rung
/// stronger: the control gate zeroes a key it loaded, while this one never loads one at all.
/// So an `Admin` handshake against an undeclared box cannot verify REGARDLESS of what the
/// node-key store holds — `from_vars` reads two names and leaves the third empty.
#[test]
fn the_ordinary_node_key_read_leaves_the_admin_scope_absent() {
    let keys = vike_tradehub_client::auth::from_vars(&admin_keys())
        .expect("the observe/control pair is present");
    assert!(
        !keys.has(Scope::Account),
        "the two-name read must leave Scope::Account ABSENT even when the store holds an admin \
             key — a box that merely has one must not thereby arm the account surface"
    );
    let widened = vike_tradehub_client::auth::from_vars_with_admin(&admin_keys())
        .expect("the same pair, through the door that also reads the third name");
    assert!(widened.has(Scope::Account), "…and the explicit door is what grants it");
}

/// **Arming the account capability may not restore a control key the control gate ZEROED.**
///
/// The test above asserts only about `Scope::Account`, so it stayed green while the two steps
/// `start_observe_server` performs in sequence disagreed: step 2 zeroes the control key when
/// `flags.tradehub_control` is off, and step 3 used to call
/// `from_vars_with_admin(&node_vars)` — which is `from_vars` + `with_admin`, and `from_vars`
/// re-reads BOTH names out of the same map. The zeroed value was discarded rather than extended,
/// so a control-DISABLED box that armed accounts handed a Control peer back the key
/// `run_handshake`'s `!keys.has(Scope::Write)` gate had just refused it with — and
/// `Request::Preview`'s arm has no sink gate behind that check.
///
/// ⚠ It drives the PRODUCTION function [`keys_for_account_capability`], not a replay of it.
/// An earlier draft of this test re-spelled both steps inline and would have passed against the
/// defective daemon — the seam had to be extracted before the assertion could mean anything.
#[test]
fn arming_accounts_does_not_restore_a_control_key_the_control_gate_zeroed() {
    let vars = admin_keys();
    let loaded = vike_tradehub_client::auth::from_vars(&vars).expect("the pair is present");
    assert!(loaded.has(Scope::Write), "the store really does hold a control key");

    // Step 2, verbatim: control is OFF, so the control key is emptied.
    let zeroed =
        vike_tradehub_client::NodeKeys::new(loaded.key_for(Scope::Read).to_vec(), Vec::new());
    assert!(!zeroed.has(Scope::Write), "the control gate emptied it");

    // Step 3, through the daemon's own function.
    let armed = keys_for_account_capability(zeroed, true, &vars);

    assert!(
        !armed.has(Scope::Write),
        "arming the account capability RESTORED the control key the control gate had zeroed — \
             step 3 must EXTEND step 2's value, never re-read the node-key map"
    );
    assert!(armed.has(Scope::Account), "…while still granting the admin scope it was armed for");
}

// ---------------------------------------------------------------------------------------------
// The ready banner (`ready_mode_line`) — the string `docs/ops/tradehub-the CI box.md` and
// `deploy/vike-tradehub.service` both call the ONE authority on paper-vs-live.
// ---------------------------------------------------------------------------------------------

/// A `live_venues` ARMING RECORD, spelled the way `vike_mount::build_node` hands one over.
fn armed(venues: &[&str]) -> std::collections::HashSet<String> {
    venues.iter().map(|v| (*v).to_string()).collect()
}

/// A store that ANSWERED — the arm an ABSENT store also lands in, so every assertion below
/// that uses it is asserting the banner an unconfigured box prints.
fn readable() -> vike_bridge_core::credentials::StoreHealth {
    vike_bridge_core::credentials::StoreHealth::Readable
}

/// A store that EXISTS and would not OPEN, carrying `SecretsError`'s own Display the way
/// `load_workspace_secrets_from_env_checked` hands one over.
fn unreadable() -> vike_bridge_core::credentials::StoreHealth {
    vike_bridge_core::credentials::StoreHealth::Unreadable(
        "credential store /x/settings/db/vike.db could not be read: a writer of the settings \
             database was killed mid-write"
            .to_string(),
    )
}

/// **THE DEFECT, reproduced from the measurement that found it.**
///
/// The record below is verbatim what the CI box's shipped daemon logged on one startup, and the
/// banner it printed two lines later was `LIVE (venue=bybit)` — the single venue its profile
/// mounted. Nine venues held live authenticated exec sessions and the operator's one authority
/// on paper-vs-live named one of them.
///
/// The expectation is written out LONGHAND rather than derived from the input, so the test
/// cannot agree with a renderer that has the same bug (the declaration-pinning failure this
/// repo has been bitten by three times).
#[test]
fn the_banner_names_every_armed_venue_not_the_one_the_profile_mounts() {
    let record = armed(&[
        "hyperliquid",
        "deribit",
        "okx",
        "bybit",
        "alpaca",
        "aster",
        "binance",
        "ig",
        "oanda",
    ]);
    assert_eq!(
        ready_mode_line(true, &record, &readable()),
        "LIVE (venue=alpaca+aster+binance+bybit+deribit+hyperliquid+ig+oanda+okx)"
    );
    // ...and the shape the defect actually printed is now impossible from this record. Spelled
    // separately because a renderer that named only the FIRST armed venue would satisfy no
    // equality above but would still be the same class of lie.
    assert_ne!(ready_mode_line(true, &record, &readable()), "LIVE (venue=bybit)");
}

/// Sorted, so an operator diffing two startups of ONE binary never sees a reordering and reads
/// it as a change: `HashSet` iteration order is not a function of the contents alone.
///
/// Two records built by inserting the same venues in OPPOSITE orders must render identically —
/// and must render in the order written here, which is neither insertion order.
#[test]
fn the_banner_sorts_the_arming_record_rather_than_iterating_it() {
    let forwards = armed(&["okx", "binance", "aster"]);
    let backwards = armed(&["aster", "binance", "okx"]);
    assert_eq!(ready_mode_line(true, &forwards, &readable()), "LIVE (venue=aster+binance+okx)");
    assert_eq!(
        ready_mode_line(true, &forwards, &readable()),
        ready_mode_line(true, &backwards, &readable())
    );
}

/// The LIVE GATE ON with NOTHING ARMED — the empty-credential-store shape every
/// `venue_feed_splice_smoke` case runs the shipped binary in, and the shape a fresh deployment
/// has before its first key is added.
///
/// It must name the sentinel, and it must NOT collapse to `PAPER`: the gate being on is an
/// operator-visible fact independent of what armed (live feeds, real prices, the B11
/// live-account locks held, and one credential appearing arms real exec on the next start).
#[test]
fn an_armed_gate_with_nothing_armed_reads_none_and_not_paper() {
    let mode = ready_mode_line(true, &armed(&[]), &readable());
    assert_eq!(mode, "LIVE (venue=none)");
    assert_ne!(mode, "PAPER", "the live gate is ON — collapsing to PAPER would hide the arm");
    // Not a truncated `LIVE (venue=)`, which reads as a broken line rather than a statement.
    assert!(!mode.ends_with("venue=)"), "the empty set must render a WORD: {mode}");
}

/// The PAPER arm is byte-identical to the pre-fix daemon — the string, and nothing else.
///
/// Asserted against a NON-EMPTY record too: a paper mount builds no live client by any path, so
/// this pairing cannot occur, and the test exists to pin that the renderer answers from the GATE
/// on that arm rather than falling through to the venue rendering if it ever did.
#[test]
fn the_paper_arm_is_untouched() {
    assert_eq!(ready_mode_line(false, &armed(&[]), &readable()), "PAPER");
    assert_eq!(ready_mode_line(false, &armed(&["binance", "bybit"]), &readable()), "PAPER");
}

/// ⚠ **THE BUG, as a string comparison.** A box whose credential store exists and will not open
/// arms nothing, so the arming record is EMPTY — identical to a correctly-unarmed box's. Under
/// the pre-fix renderer both printed `LIVE (venue=none)`, and that was the whole of the
/// operator's ability to tell a live daemon that lost its keys from one that never had any.
///
/// Asserted as an INEQUALITY against the correctly-unarmed line rather than only as an equality
/// against the new one: an equality alone would still pass on a renderer that appended
/// something to BOTH arms, which would restore the indistinguishability it is meant to remove.
#[test]
fn an_unreadable_store_is_not_the_same_banner_as_a_correctly_unarmed_box() {
    let unarmed = ready_mode_line(true, &armed(&[]), &readable());
    let broken = ready_mode_line(true, &armed(&[]), &unreadable());
    assert_eq!(unarmed, "LIVE (venue=none)", "the correctly-unarmed box's banner is unchanged");
    assert_ne!(
        broken, unarmed,
        "a store that EXISTS and will not open printed the same banner as a box with no store \
             — which is the defect: every venue is on paper for a reason nobody can see"
    );
    assert_eq!(broken, "CREDENTIAL STORE UNREADABLE — LIVE (venue=none)");
}

/// The prefix is a PREFIX: the paper-vs-live half survives verbatim in both arms, so every
/// existing `grep PAPER` / `grep LIVE` and every runbook keeps answering.
#[test]
fn the_fault_is_announced_without_taking_the_paper_vs_live_answer_away() {
    let live = ready_mode_line(true, &armed(&["binance"]), &unreadable());
    let paper = ready_mode_line(false, &armed(&[]), &unreadable());
    assert!(live.starts_with(STORE_UNREADABLE_BANNER), "{live}");
    assert!(paper.starts_with(STORE_UNREADABLE_BANNER), "{paper}");
    assert!(live.ends_with("LIVE (venue=binance)"), "the live half must survive: {live}");
    assert!(paper.ends_with("PAPER"), "the paper half must survive: {paper}");
}

/// ⚠ **An ABSENT store is BYTE-IDENTICAL to before this parameter existed**, and that is a
/// requirement rather than a side effect: no store on the box is the ordinary unconfigured
/// state, the empty map it produces is a real measurement, and `StoreHealth::Readable` is the
/// arm it lands in. A daemon on a fresh box must print exactly what it printed yesterday.
#[test]
fn a_box_with_no_store_at_all_prints_exactly_what_it_always_did() {
    assert_eq!(ready_mode_line(false, &armed(&[]), &readable()), "PAPER");
    assert_eq!(ready_mode_line(true, &armed(&[]), &readable()), "LIVE (venue=none)");
    assert_eq!(
        ready_mode_line(true, &armed(&["okx", "bybit"]), &readable()),
        "LIVE (venue=bybit+okx)"
    );
}

/// The empty-set sentinel sits in a field whose every other value is a venue id, so it must not
/// be capable of being one.
///
/// `vike_model::VENUES` is DERIVED from the `crates/bridges/*` tree (its own roster test walks
/// the directory), so a future bridge crate named `none` reddens here rather than silently
/// making the banner ambiguous between "nothing armed" and "the `none` venue armed".
#[test]
fn banner_sentinel_is_not_a_venue_id() {
    assert!(
        !vike_model::VENUES.contains(&NO_VENUE_ARMED),
        "`{NO_VENUE_ARMED}` is now a venue id — the ready banner's empty-set sentinel must be \
             renamed to something the roster cannot contain"
    );
}

/// A seed per mount, venue-addressed the way `run` builds them.
fn seeds(venues: &[&str]) -> Vec<WireMountSeed> {
    venues
        .iter()
        .map(|v| WireMountSeed {
            strategy: "buy_hold".to_string(),
            params: format!("venue={v} symbol=X interval=1m :: size=1"),
            venue: (*v).to_string(),
            asset_class: Some("CryptoPerp".to_string()),
        })
        .collect()
}

/// The `StrategyStatus` row's `live` is a PER-VENUE fact, and the two ways it used to
/// over-claim are both asserted here rather than described.
///
/// One armed venue and two unarmed mounts in one daemon: the row set must SPLIT. Under the old
/// `live: flags.tradehub_live` all three read `true`, which is a claim that three mounts place
/// real orders when one does.
#[test]
fn a_mount_row_is_live_only_when_its_own_venue_armed() {
    let rows = wire_mount_rows(seeds(&["bybit", "okx", "oanda"]), &armed(&["bybit"]));
    let by_venue: Vec<bool> = rows.iter().map(|r| r.live).collect();
    assert_eq!(
        by_venue,
        vec![true, false, false],
        "only the armed venue's mount trades live; rows: {rows:?}"
    );
}

/// The `data_only = true` mount — the case the profile loader guarantees is REACHABLE, because
/// it refuses that key unless the live gate is on.
///
/// `withhold_venue_credentials` strips the venue's keys before `build_node`, so the venue never
/// enters the arming record even though its FEED is credentialed and live. The row must read
/// paper: its orders go to the paper book.
#[test]
fn a_data_only_mounts_row_reads_paper_even_though_its_feed_is_credentialed() {
    let rows = wire_mount_rows(seeds(&["oanda"]), &armed(&[]));
    assert!(!rows[0].live, "a withheld venue mounts paper exec, and the row must say so");
    // ...and the row is otherwise untouched — this fix changes ONE field.
    assert_eq!(rows[0].strategy, "buy_hold");
    assert_eq!(rows[0].params, "venue=oanda symbol=X interval=1m :: size=1");
}

/// A PAPER daemon hands over an empty record, so every row reads paper — byte-identical to the
/// pre-fix answer on that arm, where `flags.tradehub_live` was `false` for the same rows.
#[test]
fn every_row_of_a_paper_daemon_reads_paper() {
    let rows = wire_mount_rows(seeds(&["bybit", "oanda"]), &armed(&[]));
    assert!(rows.iter().all(|r| !r.live), "rows: {rows:?}");
}

// ---------------------------------------------------------------------------------------------
// The exec badge is per ACCOUNT
// ---------------------------------------------------------------------------------------------

/// One arming row, through the same type `vike_mount::venue_arming` produces.
fn arming_row(venue: &'static str, label: Option<&str>) -> vike_config::VenueArming {
    vike_config::VenueArming {
        venue,
        label: match label {
            None => vike_model::accounts::account_keys::AccountLabel::Default,
            Some(l) => {
                vike_model::accounts::account_keys::AccountLabel::parse(l).expect("a legal label")
            }
        },
        ceiling: vike_config::VenueMode::Live,
        effective: vike_config::VenueMode::Live,
        block: vike_config::ArmingBlock::None,
    }
}

/// **THE DEFECT.** The default `bybit` account has no credentials and mounts paper; a LABELLED
/// `bybit` account armed real exec in this same process. The venue-keyed badge answered
/// `live_venues.contains("bybit")` — false — and the daemon announced `exec = PAPER` beside a
/// live authenticated bybit session.
///
/// ⚠ The armed set is spelled through `VenueArming::route_key`, NOT as a `"bybit#ALT"` literal:
/// a fixture that hand-writes the key it is about to look up passes with the renderer broken,
/// which is the seeded-through-the-function-under-test failure this program has hit before.
#[test]
fn a_venue_whose_labelled_account_armed_does_not_announce_paper() {
    let rows = vec![arming_row("bybit", None), arming_row("bybit", Some("ALT"))];
    let live: std::collections::HashSet<String> = [rows[1].route_key()].into_iter().collect();

    let others = other_live_accounts("bybit", &rows, &live);
    assert_eq!(others, vec![rows[1].route_key()], "the ALT account's route key must be found");

    let announced =
        with_other_live_accounts(cex_arming(CexVenue::Bybit, false, false), "bybit", others);
    assert_eq!(announced.exec, EXEC_OTHER_ACCOUNT_LIVE);
    assert_ne!(announced.exec, EXEC_PAPER, "the badge must not read paper for a live venue");
    assert!(
        !announced.exec.starts_with(EXEC_PAPER),
        "…and must not answer an `exec=PAPER` grep either: {}",
        announced.exec
    );
    // The remedy still arms THIS mount, and now says what the paper verdict is about.
    let remedy = announced.remedy.expect("a paper mount still carries its remedy");
    assert!(remedy.contains("ALREADY LIVE"), "{remedy}");
    assert!(remedy.contains(&rows[1].route_key()), "the live account must be NAMED: {remedy}");
    assert!(remedy.contains("BYBIT_DEMO_API_KEY"), "the original remedy survives: {remedy}");
}

/// Both accounts armed: the badge still answers an `exec=LIVE` grep, and says there is more
/// than one book behind it.
#[test]
fn a_venue_with_two_armed_accounts_says_so_and_still_reads_live() {
    let rows = vec![arming_row("bybit", None), arming_row("bybit", Some("ALT"))];
    let live: std::collections::HashSet<String> =
        rows.iter().map(vike_config::VenueArming::route_key).collect();
    let announced = with_other_live_accounts(
        cex_arming(CexVenue::Bybit, true, true),
        "bybit",
        other_live_accounts("bybit", &rows, &live),
    );
    assert_eq!(announced.exec, EXEC_LIVE_MULTI_ACCOUNT);
    assert!(announced.exec.starts_with(EXEC_LIVE), "an `exec=LIVE` grep must still match");
    assert_eq!(announced.other_live, vec![rows[1].route_key()]);
    assert!(announced.remedy.is_none(), "a live mount carries no remedy");
}

/// **A single-account box is BYTE-IDENTICAL.** Every row is a default-account row, so
/// `other_live_accounts` is empty for every venue and `with_other_live_accounts` returns its
/// input field for field — asserted against the UNWRAPPED producer, both armed and not.
#[test]
fn a_single_account_box_announces_exactly_what_it_did_before() {
    let rows: Vec<vike_config::VenueArming> =
        ["binance", "bybit", "okx", "aster"].into_iter().map(|v| arming_row(v, None)).collect();
    for armed_venues in [vec![], vec!["bybit"], vec!["binance", "bybit", "okx", "aster"]] {
        let live: std::collections::HashSet<String> =
            armed_venues.iter().map(|v| (*v).to_string()).collect();
        for venue in [CexVenue::Binance, CexVenue::Bybit, CexVenue::Okx, CexVenue::Aster] {
            let slug = venue.slug();
            assert!(
                other_live_accounts(slug, &rows, &live).is_empty(),
                "{slug} has no second account on this box"
            );
            for mainnet in [false, true] {
                let exec_live = live.contains(slug);
                let bare = cex_arming(venue, mainnet, exec_live);
                let wrapped = with_other_live_accounts(
                    cex_arming(venue, mainnet, exec_live),
                    slug,
                    other_live_accounts(slug, &rows, &live),
                );
                assert_eq!(bare, wrapped, "{slug} mainnet={mainnet} must be untouched");
                assert_eq!(
                    wrapped.exec,
                    if exec_live { EXEC_LIVE } else { EXEC_PAPER },
                    "…and reads exactly the two badges it always read"
                );
            }
        }
    }
    // The credentialed-data producers take the same trip.
    for (bare, slug) in [
        (alpaca_arming(false), "alpaca"),
        (ctrader_arming(true), "ctrader"),
        (oanda_arming(false), "oanda"),
        (deribit_arming(true), "deribit"),
        (ig_arming(false), "ig"),
    ] {
        let exec = bare.exec;
        let wrapped = with_other_live_accounts(bare, slug, Vec::new());
        assert_eq!(wrapped.exec, exec, "{slug} must keep its badge with no second account");
        assert!(wrapped.other_live.is_empty());
    }
}

/// The route key of ANOTHER venue's labelled account never leaks into this venue's answer, and
/// a labelled account that did NOT arm is not reported as live.
#[test]
fn the_answer_is_scoped_to_the_venue_and_to_what_actually_armed() {
    let rows = vec![
        arming_row("bybit", Some("ALT")),
        arming_row("okx", Some("ALT")),
        arming_row("bybit", Some("HEDGE")),
    ];
    // Only okx#ALT armed.
    let live: std::collections::HashSet<String> = [rows[1].route_key()].into_iter().collect();
    assert!(
        other_live_accounts("bybit", &rows, &live).is_empty(),
        "okx's armed account must not appear under bybit"
    );
    assert_eq!(other_live_accounts("okx", &rows, &live), vec![rows[1].route_key()]);
}

/// `exec_badge` is total over its two inputs, and the four strings are distinct — so no state
/// can be mistaken for another by a grep.
#[test]
fn the_four_exec_badges_are_distinct() {
    let all = [
        exec_badge(false, false),
        exec_badge(false, true),
        exec_badge(true, false),
        exec_badge(true, true),
    ];
    let mut dedup = all.to_vec();
    dedup.sort_unstable();
    dedup.dedup();
    assert_eq!(dedup.len(), all.len(), "the four badges must be distinct: {all:?}");
}

// ---------------------------------------------------------------------------------------------
// The CEX (binance/bybit/okx) live arm.
// ---------------------------------------------------------------------------------------------

/// A `MakerMountConfig` for `venue` on the symbol `build_node` actually mounts it on — the same
/// lowering `DaemonProfile::to_mount_config` produces for a CEX profile.
fn cex_cfg(venue: CexVenue) -> MakerMountConfig {
    let symbol = wired_symbol_for(venue.slug()).expect("build_node mounts this venue");
    MakerMountConfig::crypto(venue.slug(), symbol, 0.5, 0.001)
}

/// **The slug table, pinned against an authority that is NOT `slug()` itself.**
///
/// ⚠ This is the missing gate, and its absence was MEASURED: on the CI box, rewriting
/// `CexVenue::Binance => "binance"` to `"bybit"` left the ENTIRE vike-tradehub suite GREEN.
/// Every other slug assertion here — `every_cex_venue_slug_names_a_wired_market`,
/// `cex_plan_accepts_the_wired_symbol`, the recon feed-status key — reads its expectation FROM
/// `slug()` and compares it against a set that contains all three strings, so no PERMUTATION of
/// them can fail: the declaration-pinning failure mode this repo has been bitten by three times.
///
/// The authority is each bridge crate's OWN public venue constant — the string that crate
/// stamps on the ticks its pump emits and that its exec path signs under. That makes this a
/// genuine cross-check rather than a hand copy of the match arms: `CexVenue::Binance` MUST be
/// the venue `vike_binance` is, because `CexVenue::Binance`'s feed arm calls
/// `vike_binance::market_data::spawn_binance_market_data` and nothing else.
///
/// The literal half is asserted too, because the bridge constant is itself mutable — the two
/// authorities would have to be changed together and in agreement to slip a wrong slug through.
/// The `match` is EXHAUSTIVE, so a fourth CEX venue is a compile error here rather than a row
/// somebody forgets.
#[test]
fn cex_venue_slugs_are_pinned_to_the_bridge_crates_own_venue_string() {
    for venue in CexVenue::ALL {
        let (bridge_const, literal) = match venue {
            // Homed at the CRATE ROOT rather than in `spot` since ruling 8's feeds/exec seam,
            // for the reason aster's note below gives: `data` (the keyless kline REST) names
            // it too, and `spot` is now behind that crate's `exec` feature.
            CexVenue::Binance => (vike_binance::VENUE, "binance"),
            CexVenue::Bybit => (vike_bybit::perp::VENUE, "bybit"),
            CexVenue::Okx => (vike_okx::perp::VENUE, "okx"),
            // Homed in `urls` (the feed-plane module) rather than an exec module — aster's
            // canonical id lives beside its host table so BOTH planes can name it
            // (`crates/bridges/aster/src/urls.rs`'s `VENUE`).
            CexVenue::Aster => (vike_aster::urls::VENUE, "aster"),
        };
        assert_eq!(
            venue.slug(),
            bridge_const,
            "{venue:?}'s slug must be the venue string its OWN bridge crate declares — the \
                 feed arm calls that crate's pump, while `make_engine` keys the ExecutionClient \
                 and the credential prefix on this slug, so a disagreement mounts one venue's \
                 book against another venue's account"
        );
        assert_eq!(
            venue.slug(),
            literal,
            "{venue:?}'s slug is pinned VERBATIM here as well as against the bridge constant, \
                 so that changing both in step is still a deliberate two-place edit"
        );
    }
}

/// Every CEX venue's slug must be a venue `build_node` actually mounts an engine for, and the
/// slug must be the string that table keys on. Cheap, but it is the join between the FEED half
/// (this file) and the EXEC half (`crate::wired_markets::WIRED_MARKETS`): a typo'd slug would give a feed
/// with no engine behind it, and every order would vanish at `accepts_symbol`.
///
/// ⚠ This one CANNOT see a permuted slug — every string it checks against contains all three.
/// `cex_venue_slugs_are_pinned_to_the_bridge_crates_own_venue_string` above is that gate.
#[test]
fn every_cex_venue_slug_names_a_wired_market() {
    for venue in CexVenue::ALL {
        let slug = venue.slug();
        assert!(
            wired_symbol_for(slug).is_some(),
            "{slug} has a live feed arm but `build_node` mounts no engine for it"
        );
        assert!(
            crate::config::LIVE_WIRED_VENUES.contains(&slug),
            "{slug} has a feed arm but is not advertised in LIVE_WIRED_VENUES"
        );
    }
}

/// The plan gate ACCEPTS the wired pair for each of the three venues, and CARRIES the resolved
/// ceiling-decided (decision 0095) network verdict through to the feed block.
///
/// The verdict has to travel in the plan: `vars` is moved into the `NodeConfig` before the feed
/// block runs, so the announcement cannot re-read the flag for itself — the same reason
/// `VenuePlan::Hyperliquid` carries its resolved `Network`. A plan that dropped it would leave
/// the mount unable to say which network it is about to trade on.
#[test]
fn cex_plan_accepts_the_wired_symbol_and_carries_the_network() {
    for venue in CexVenue::ALL {
        let cfg = cex_cfg(venue);
        for mainnet in [false, true] {
            match cex_plan(venue, &cfg, mainnet) {
                Ok(VenuePlan::Cex { venue: v, mainnet: m }) => {
                    assert_eq!(v, venue, "the plan must name the venue it was asked about");
                    assert_eq!(
                        m,
                        mainnet,
                        "{} must carry the resolved mainnet verdict to the announcement — \
                             `vars` is gone by then",
                        venue.slug()
                    );
                }
                other => {
                    panic!("{} on its own wired symbol must plan, got {other:?}", venue.slug())
                }
            }
        }
    }
}

/// **A foreign symbol is REFUSED, not silently mounted.**
///
/// `vike_mount::make_engine` wires no `extra_symbols`, so
/// `vike_exec::ExecutionEngine::accepts_symbol` is plain equality: a mount on the wrong symbol
/// keeps its feed, keeps quoting, and has every order AND every fill dropped with no log line
/// anywhere. That is the failure this gate converts into a startup error, so the error must name
/// it.
#[test]
fn cex_plan_refuses_a_foreign_symbol() {
    for venue in CexVenue::ALL {
        let mut cfg = cex_cfg(venue);
        let wired = cfg.token_id.clone();
        cfg.token_id = format!("{wired}-NOT-THE-MOUNTED-ONE");
        let err = cex_plan(venue, &cfg, false).expect_err("a foreign symbol must be refused");
        assert!(err.contains("SILENTLY DROPPED"), "names the real failure mode: {err}");
        assert!(err.contains(&wired), "names the symbol build_node mounts: {err}");
    }
}

/// **A non-positive or non-finite tick size is REFUSED.**
///
/// `cfg.tick_size` is handed straight to `spawn_*_market_data`, where it sizes the `L2Book`
/// price grid — a plain `f64` parameter with no validation on the venue side. A `0.0` (the
/// value a profile that never set `tick_size` would produce if the default ever changed) or a
/// NaN builds a degenerate grid that raises no error and yields no usable top-of-book, so the
/// maker simply never quotes. It is also the maker's OWN grid, so one check covers both.
#[test]
fn cex_plan_refuses_an_unusable_tick_size() {
    for bad in [0.0, -0.5, f64::NAN, f64::INFINITY] {
        let mut cfg = cex_cfg(CexVenue::Okx);
        cfg.tick_size = bad;
        let err = match cex_plan(CexVenue::Okx, &cfg, false) {
            Ok(plan) => panic!(
                "tick_size {bad} must be refused BEFORE a book is built on it, but the gate \
                     planned {plan:?}"
            ),
            Err(e) => e,
        };
        assert!(
            err.contains("tick_size"),
            "the refusal must name the field an operator has to fix: {err}"
        );
    }
    // ...and a good one still passes, so the guard is not simply always-on.
    let mut cfg = cex_cfg(CexVenue::Okx);
    cfg.tick_size = 0.1;
    assert!(cex_plan(CexVenue::Okx, &cfg, false).is_ok(), "a real okx tick size must pass");
}

/// A `LiveDataSink` that discards everything — enough to construct a real venue `Feeds`, which
/// connects nothing until `subscribe_*` is called, so the tests below build genuine feed objects
/// without touching the network.
struct NullSink;

impl LiveDataSink for NullSink {
    fn seed_bars(&self, _v: &str, _s: &str, _i: &str, _b: Vec<vike_model::Bar>) {}
    fn close_bar(&self, _v: &str, _s: &str, _i: &str, _b: vike_model::Bar) {}
    fn forming_bar(&self, _v: &str, _s: &str, _i: &str, _b: vike_model::Bar) {}
    fn mark_tick(&self, _v: &str, _s: &str, _p: f64, _t: i64) {}
    fn quote(&self, _v: &str, _s: &str, _q: vike_model::QuoteTick) {}
    fn trade(&self, _v: &str, _s: &str, _t: vike_model::TradeTick) {}
    fn book(&self, _v: &str, _s: &str, _b: Arc<vike_model::L2Book>) {}
}

fn cex_bars_for(venue: CexVenue) -> CexBars {
    let sink: Arc<dyn LiveDataSink> = Arc::new(NullSink);
    match venue {
        CexVenue::Binance => CexBars::Binance(vike_binance::market_feed::Feeds::new(sink, || {})),
        CexVenue::Bybit => CexBars::Bybit(vike_bybit::market_feed::Feeds::new(sink, || {})),
        CexVenue::Okx => CexBars::Okx(vike_okx::market_feed::Feeds::new(sink, || {})),
        // The same `with_env(.., Live)` construction as `wire_venue_feeds`' aster arm (a
        // `Feeds` connects nothing until `subscribe_*`, so this stays network-free).
        CexVenue::Aster => CexBars::Aster(vike_aster::market_feed::Feeds::with_env(
            sink,
            || {},
            vike_bridge_core::credentials::Environment::Live,
        )),
    }
}

/// **The feed-status health map carries EXACTLY the mounted CEX venue's own row.**
///
/// Two halves, and both are load-bearing in opposite directions:
///
/// - a row MUST exist, or that venue's reconcile pass reads a blanket `Healthy` and keeps
///   reconciling against a feed that is known to be down;
/// - and no OTHER venue may appear, because the health gate can only ever SUPPRESS a pass — a
///   spurious row silently stops an unrelated venue from reconciling at all, and a suppressed
///   pass can stay suppressed (`reconcile_config::health_from_feed_status`'s doc).
///
/// The keys must also be the exact strings `ReconManager::should_reconcile` looks up, which is
/// what ties `CexBars::slug` to `CexVenue::slug` and to `crate::wired_markets::WIRED_MARKETS`.
#[test]
fn the_cex_health_map_carries_only_the_mounted_venues_own_row() {
    for venue in CexVenue::ALL {
        let bars = cex_bars_for(venue);
        assert_eq!(bars.slug(), venue.slug(), "CexBars::slug must match CexVenue::slug");

        let feeds = LiveFeeds::Cex { ticks: None, bars };
        let map = feeds.recon_feed_statuses();
        assert_eq!(
            map.keys().collect::<Vec<_>>(),
            vec![venue.slug()],
            "exactly one row — this venue's own — must be health-gated, got {:?}",
            map.keys().collect::<Vec<_>>()
        );
        // The handle is live: it is the same `Arc` the feed will publish its status through, so
        // the gate reads a real string rather than a detached copy.
        let status = Arc::clone(map.get(venue.slug()).expect("its own row"));
        *status.lock().expect("status mutex") = "disconnected".to_string();
        assert_eq!(
            reconcile_config::health_from_feed_status(
                &feeds.recon_feed_statuses()[venue.slug()].lock().expect("status").clone()
            ),
            vike_core::ReconHealth::Degraded,
            "a disconnected feed must degrade THIS venue's reconcile health"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// The mount ANNOUNCEMENT (`cex_arming`) — the decisions the feed block makes about what to tell
// the operator. `live_mount` itself cannot be called from a test (it is a `main.rs` function
// that spawns a core, opens two sockets and reads the credential store), which is why these
// decisions were extracted; before that, NOTHING exercised the block and both defects below
// shipped inside it.
// ---------------------------------------------------------------------------------------------

/// **The LIVE announcement must name the NETWORK.** "EXEC IS LIVE" is a materially different
/// statement on demo than on mainnet, and both states are reachable from the same profile by one
/// environment variable. The old line said neither.
#[test]
fn a_live_cex_mount_announces_which_network_it_trades_on() {
    for venue in CexVenue::ALL {
        let demo = cex_arming(venue, false, true);
        assert_eq!(demo.exec, "LIVE");
        // Aster spells its non-mainnet tier the way its OWN credential naming does
        // (`ASTER_TESTNET_*` — `load_aster_credentials`), so the log matches the keys the
        // operator actually provisioned; the flag venues keep their `DEMO` spelling.
        let expected = if venue == CexVenue::Aster { "TESTNET" } else { "DEMO" };
        assert_eq!(demo.network, expected, "{} non-mainnet ⇒ {expected}", venue.slug());
        assert_eq!(demo.remedy, None, "a live mount has nothing to remedy");

        let main = cex_arming(venue, true, true);
        assert_eq!(main.exec, "LIVE");
        assert_eq!(
            main.network,
            "MAINNET",
            "{} mainnet with live creds is REAL MONEY and the mount must say so",
            venue.slug()
        );
        assert_ne!(
            demo.network, main.network,
            "the two networks must be DISTINGUISHABLE in the log — this is the whole point"
        );
    }
}

/// **The PAPER remedy must be ACTIONABLE IN THE STATE IT IS PRINTED IN.**
///
/// ⚠ The defect this pins: the old warning advised `{VENUE}_DEMO_API_KEY` /
/// `{VENUE}_DEMO_API_SECRET` unconditionally. `vike_mount::make_engine` chooses the credential
/// TIER from the ceiling before it looks anything up (decision 0095) — `if mainnet {
/// load_credentials_from(venue, Environment::Live, vars) } else { … Demo … }` — so in the
/// `live`-ceiling + no-LIVE-keys state, which is exactly the state an operator hits while going
/// live, the advice names a tier that is never consulted. Following it produces the identical
/// PAPER mount and the identical warning, with nothing to distinguish the second attempt from
/// the first. An instruction that cannot work is worse than no instruction.
#[test]
fn the_paper_remedy_names_the_tier_make_engine_will_actually_read() {
    for venue in CexVenue::ALL {
        if venue == CexVenue::Aster {
            // Aster has no `{VENUE}_MAINNET` flag and no `_API_KEY` shape, so this test's
            // whole flag-tier vocabulary does not apply — its remedy has its own gate below
            // (`the_aster_remedy_names_the_agent_wallet_keys_and_the_live_first_hazard`).
            continue;
        }
        let upper = venue.slug().to_uppercase();

        // DEMO state: the demo tier IS what `make_engine` reads, so advise it.
        let demo = cex_arming(venue, false, false);
        assert_eq!(demo.exec, "PAPER");
        assert_eq!(demo.network, "DEMO");
        let demo_remedy = demo.remedy.expect("a paper mount must say how to arm it");
        assert!(
            demo_remedy.contains(&format!("{upper}_DEMO_API_KEY")),
            "unset flag ⇒ the DEMO tier is the one that arms exec: {demo_remedy}"
        );
        assert!(
            !demo_remedy.contains(&format!("{upper}_LIVE_API_KEY")),
            "and it must not send an operator to add real-money keys: {demo_remedy}"
        );

        // MAINNET state: the demo tier is NOT consulted. Advising it is the bug.
        let main = cex_arming(venue, true, false);
        assert_eq!(main.exec, "PAPER");
        assert_eq!(main.network, "MAINNET");
        let main_remedy = main.remedy.expect("a paper mount must say how to arm it");
        assert!(
            main_remedy.contains(&format!("{upper}_LIVE_API_KEY")),
            "a `live` ceiling ⇒ `make_engine` loads the LIVE tier, so that is the tier to name: \
                 {main_remedy}"
        );
        assert!(
            !main_remedy.contains(&format!("{upper}_DEMO_API_KEY")),
            "⚠ THE DEFECT: advising the DEMO tier under a `live` ceiling is advice that arms \
                 NOTHING — `make_engine` never consults it in this state: {main_remedy}"
        );
        assert!(
            main_remedy
                .contains(&format!("vike-cli config set policy.venues.{} demo", venue.slug())),
            "the other way out — dropping back to demo — must be named too, because an \
                 operator who has no live keys yet wants that one: {main_remedy}"
        );

        // The two states must not print the same sentence: the whole failure was one string
        // serving both.
        assert_ne!(
            demo_remedy,
            main_remedy,
            "{} prints the SAME remedy in both states, which is the defect",
            venue.slug()
        );
    }
}

/// **Aster's PAPER remedy must speak ITS credential model, not the flag venues'.**
///
/// The generic remedy vocabulary is wrong for aster three separate ways, and each wrong word
/// sends an operator hunting a value that does not exist: there is no `ASTER_DEMO_*` tier
/// (the non-mainnet spelling is `TESTNET`), there is no `_API_KEY`/`_API_SECRET` shape (the
/// venue discontinued HMAC keys — the credential is an agent-wallet `_USER`/`_PRIVATE_KEY`
/// pair, `vike_aster::signing::load_aster_credentials`), and there is no `ASTER_MAINNET` flag
/// to set or unset. The one hazard the remedy MUST carry instead: `make_engine` resolves the
/// LIVE tier FIRST, so provisioning `ASTER_LIVE_*` arms REAL-MONEY MAINNET exec — the
/// credential tier IS the network choice, UNDER the ceiling: only a `live`
/// `policy.venues.aster` lets the LIVE pair be tried at all (decision 0095).
///
/// Only the `mainnet = false` paper state is asserted because it is the only reachable one:
/// aster's mainnet verdict is "LIVE creds present" ([`cex_mainnet_enabled`]), and present LIVE
/// creds make exec LIVE — `remedy = None`.
#[test]
fn the_aster_remedy_names_the_agent_wallet_keys_and_the_live_first_hazard() {
    let r = cex_arming(CexVenue::Aster, false, false).remedy.expect("paper");
    assert!(
        r.contains("ASTER_TESTNET_USER") && r.contains("ASTER_TESTNET_PRIVATE_KEY"),
        "the SAFE tier to advise is testnet, in the agent-wallet key shape: {r}"
    );
    assert!(
        r.contains("ASTER_LIVE_USER") && r.contains("REAL-MONEY MAINNET"),
        "…and it must say what the OTHER tier arms, because LIVE-first resolution makes \
             adding those keys a real-money decision: {r}"
    );
    assert!(
        r.contains("policy.venues.aster") && r.contains("`live` ceiling"),
        "…but ONLY under a `live` ceiling: below it `mountable_tier_for_account` deletes the \
             LIVE attempt outright and reads the testnet pair alone, so a remedy that says \
             unconditionally 'adding these arms MAINNET' names a hazard the ceiling already \
             closed and hides the one row that opens it: {r}"
    );
    assert!(
        !r.contains("_API_KEY") && !r.contains("_API_SECRET"),
        "aster has no HMAC key shape; naming one sends the operator after a value that does \
             not exist: {r}"
    );
    // `"DEMO"` and not an `ASTER_DEMO*` key spelling, deliberately twice over: it is the
    // STRONGER ban (no DEMO-tier vocabulary at all, not merely no one key), and a whole-literal
    // `ASTER_`-prefixed spelling here would read as an env key to the settings-registry
    // scanner's literal sweep (`vike_ops::scan::find_map_lookups`), demanding a registry row
    // for fixture data — the exact #1114 shape.
    assert!(
        !r.contains("DEMO") && !r.contains("UNSET ASTER_MAINNET"),
        "no DEMO tier and no MAINNET flag exist for aster — the flag venues' vocabulary is \
             exactly the unreachable advice this remedy exists to avoid: {r}"
    );
}

/// OKX v5 signs every request with a passphrase; binance and bybit use none (and aster's
/// agent-wallet model has no passphrase concept at all). The remedy must
/// name the keys that venue actually needs — no more (a key that venue has no use for sends an
/// operator hunting a value that does not exist) and no fewer (an OKX mount with key+secret and
/// no passphrase loads credentials, mounts LIVE, and then fails every signed call).
#[test]
fn the_remedy_names_the_passphrase_only_where_the_venue_signs_with_one() {
    for mainnet in [false, true] {
        let okx = cex_arming(CexVenue::Okx, mainnet, false).remedy.expect("paper");
        assert!(okx.contains("_API_PASSPHRASE"), "okx v5 requires a passphrase: {okx}");
        for venue in [CexVenue::Binance, CexVenue::Bybit, CexVenue::Aster] {
            let r = cex_arming(venue, mainnet, false).remedy.expect("paper");
            assert!(
                !r.contains("_API_PASSPHRASE"),
                "{} uses no passphrase; naming one sends the operator after a value that does \
                     not exist: {r}",
                venue.slug()
            );
        }
    }
}

/// **Every CEX venue drives BOTH maker verbs, bybit included.**
///
/// ⚠ The defect this pins: the mount used to log `quote_lane = "on_order_book"` for bybit and
/// `"on_quote_tick"` for the other two, on the claim that "bybit publishes no L1 quote lane at
/// all". That claim was FALSE. `spawn_bybit_market_data`'s `MdEvent::BookUpdated` arm sends
/// `ticks.book(..)` and then `quote_from_book(&book, symbol)` — a real `QuoteUpdate` on the same
/// core tick lane binance and okx use. The `venue_caps` row it cited (`live_data.quotes = false`)
/// describes the `DataClient::subscribe_quotes` seam, which this pump does not go through.
///
/// A false capability claim in a runtime log FIELD is the same class as a false `LIVE_CAPABLE`
/// row: an operator reads it as a measurement of the running system and debugs the wrong lane.
#[test]
fn every_cex_venue_announces_both_requote_lanes() {
    for venue in CexVenue::ALL {
        let arming = cex_arming(venue, false, true);
        assert!(
            arming.requote_lanes.contains("on_quote_tick"),
            "{} publishes a QuoteUpdate on the core tick lane, so it drives on_quote_tick: {}",
            venue.slug(),
            arming.requote_lanes
        );
        assert!(
            arming.requote_lanes.contains("on_order_book"),
            "{} publishes a BookUpdate too, so it drives on_order_book: {}",
            venue.slug(),
            arming.requote_lanes
        );
        // ⚠ The lanes are venue-INDEPENDENT. A per-venue lane string is exactly the shape the
        // false claim took, so equality across the roster is asserted rather than left implicit.
        assert_eq!(
            arming.requote_lanes,
            cex_arming(CexVenue::Binance, false, true).requote_lanes,
            "{} must announce the SAME lanes as binance — all three publish quote, trade and \
                 book",
            venue.slug()
        );
    }
}

/// The quote's PROVENANCE is still per-venue, and still worth logging — it is what an operator
/// checks when the quote lane is silent. bybit's is derived, so it is the one that can go quiet
/// while the book lane stays live (`quote_from_book` returns `None` on a one-sided book).
#[test]
fn the_quote_source_distinguishes_derived_from_native() {
    let bybit = cex_arming(CexVenue::Bybit, false, true).quote_source;
    assert!(
        bybit.contains("DERIVED") && bybit.contains("quote_from_book"),
        "bybit's quote is folded out of orderbook.50 by the pump: {bybit}"
    );
    for venue in [CexVenue::Binance, CexVenue::Okx, CexVenue::Aster] {
        let src = cex_arming(venue, false, true).quote_source;
        assert!(
            src.contains("native"),
            "{} decodes a native top-of-book channel: {src}",
            venue.slug()
        );
        assert_ne!(src, bybit, "provenance must still distinguish the venues");
    }
    // The fork and its template are the SAME channel on different host families — the field
    // must still say which one a silent quote lane should be debugged against.
    assert_ne!(
        cex_arming(CexVenue::Aster, false, true).quote_source,
        cex_arming(CexVenue::Binance, false, true).quote_source,
        "aster's row must be distinguishable from binance's"
    );
}

/// The hyperliquid and polymarket arms stay on the EMPTY map — byte-identically to before the
/// CEX arm existed. Pinned rather than assumed: the tempting "just collect every status handle
/// in scope" change would silently start suppressing reconcile passes on venues nobody assessed,
/// and it would look like a tidy-up in review.
#[test]
fn the_non_cex_arms_stay_on_the_empty_health_map() {
    let sink: Arc<dyn LiveDataSink> = Arc::new(NullSink);
    let hl = LiveFeeds::Hyperliquid(vike_hyperliquid::market_feed::Feeds::new(sink, || {}));
    assert!(
        hl.recon_feed_statuses().is_empty(),
        "hyperliquid keeps the exec-only-venue shape (every venue reads Healthy)"
    );
}

/// **The credential store is read ONCE per process, however many callers ask for it.**
///
/// A clean install found this as a doubled log line: with a 0644 store, the daemon printed the
/// identical `readable beyond its owner` WARN twice per start, because
/// `try_load_workspace_secrets_at` surfaces the store's permission finding on EVERY invocation
/// (deliberately — "no caller can forget to surface it") and four call sites each performed a
/// complete, independent load. A warning that repeats reads as two findings.
///
/// The doubled line was the symptom; the read was the defect. This asserts the fix at the
/// property, not at the log: after any call, [`CREDENTIALS`] is populated, so a later caller
/// takes the cached map instead of re-opening a plaintext file full of live venue secrets. Drop
/// the memoization and this fails — the `OnceLock` stays empty while the map still comes back.
///
/// ⚠ It reads the REAL store for this working directory, which under `cargo test` is
/// `crates/vike-tradehub/` — a crate directory with no `settings/` in it, so the load resolves
/// `Source::None` and an empty map. Nothing here asserts the CONTENTS, precisely so the test says
/// the same thing on a developer box, on CI, and on a production checkout.
#[test]
fn the_credential_store_is_read_once_however_many_callers_ask() {
    let first = workspace_credentials();
    assert!(
        CREDENTIALS.get().is_some(),
        "the store read must be memoized — an unmemoized `workspace_credentials` re-opens the \
             credential file, and re-emits its permission warning, once per caller"
    );
    let second = workspace_credentials();
    assert_eq!(first, second, "two callers must see the same credentials");
    assert!(
        std::ptr::eq(CREDENTIALS.get().unwrap(), CREDENTIALS.get().unwrap()),
        "one stored map, cloned per caller — not one load per caller"
    );
    // ⚠ ...and the VERDICT rides the same memo, which is the property that keeps the banner
    // honest: `workspace_credentials_checked` returns the pair from ONE open, so the map a
    // mount armed from and the health the ready banner renders can never describe two
    // different reads of two different stores.
    assert!(
        std::ptr::eq(credential_store_health(), &CREDENTIALS.get().unwrap().1),
        "the health must come from the memoized read, not from a second open"
    );
}

#[test]
fn summary_line_is_valid_json_with_expected_keys() {
    let snap = CoreSnapshot::empty("polymarket", "TOK");
    let line = summary_line(&snap, "TOK");
    let v: serde_json::Value =
        serde_json::from_str(&line).expect("the summary line must be valid JSON");
    assert_eq!(v["kind"], "summary");
    assert_eq!(v["orders"], 0);
    assert_eq!(v["working"], 0);
    assert_eq!(v["positions"], 0);
    assert_eq!(v["net_pos"], 0.0);
    assert!(v["fault"].is_null(), "no fault on a fresh snapshot");
    assert!(v["trading_state"].is_string());
}

/// The FIELD SET of the summary line is a STDOUT PROTOCOL surface — pinned verbatim, as a set,
/// so the scope fix below cannot quietly add or drop a key that something downstream parses.
/// A key added on purpose is one edited row here; a key added by accident is a red test.
///
/// ⚠ The set CHANGED, deliberately, on 2026-08-17: the single `equity` was replaced by
/// `equity_book`/`equity_wallet`/`wallet_venues`. `equity` was `Portfolio::equity_total`, the
/// sum of the daemon's own book-keeping and a venue's whole-account wallet — see
/// [`summary_line`]'s doc for the 62647.10600813 measured on the CI box. Dropping the NAME rather
/// than redefining it is the point: a consumer keyed on `.equity` gets `null` and breaks
/// loudly instead of silently reading a figure that means nothing.
///
/// ⚠ …and AGAIN on 2026-08-19 (the I10 rehearsal follow-up): `equity_book_mounted` +
/// `mounted_venues` were ADDED. Additive on purpose — every existing key keeps its name, its
/// meaning and its VALUE, so no `jq`/alerting consumer reads a number that changed under it;
/// the mounted-set scoping arrives as its own labelled pair instead
/// (`crate::summary::mounted_book_equity` carries the argument).
#[test]
fn the_summary_line_field_set_is_exactly_these_fifteen_keys() {
    let line = summary_line(&CoreSnapshot::empty("bybit", "BTCUSDT"), "BTCUSDT");
    let v: serde_json::Value = serde_json::from_str(&line).expect("valid JSON");
    let mut keys: Vec<&str> =
        v.as_object().expect("a JSON object").keys().map(|k| k.as_str()).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "equity_book",
            "equity_book_mounted",
            "equity_wallet",
            "fault",
            "fees",
            "kind",
            "mounted_venues",
            "net_pos",
            "orders",
            "positions",
            "realized_pnl",
            "seq",
            "trading_state",
            "wallet_venues",
            "working",
        ],
        "the summary line's field set is a protocol surface — changing it is a deliberate edit"
    );
    assert!(
        !v.as_object().expect("a JSON object").contains_key("equity"),
        "there must be no bare `equity` key: a venue wallet and a book-kept equity are \
             different quantities, and the name that used to carry their sum is retired rather \
             than redefined so a stale consumer fails loudly"
    );
}

/// One venue block, spelled out so the scope test below reads as data rather than a builder.
fn venue_block(
    venue: &str,
    realized: f64,
    fees: f64,
    positions: Vec<vike_core::PositionView>,
) -> vike_core::VenueBlock {
    vike_core::VenueBlock {
        venue: venue.to_string(),
        account: None,
        route_key: venue.to_string(),
        symbol: String::new(),
        extra_symbols: Vec::new(),
        mode: None,
        balance: 0.0,
        realized_pnl: realized,
        fees_paid: fees,
        funding_paid: 0.0,
        balance_mode: vike_exec::BalanceMode::Delta,
        equity: 0.0,
        unrealized: 0.0,
        missing_prices: 0,
        margin_used: 0.0,
        free_bp: 0.0,
        margin_ratio: 0.0,
        fee_schedule: None,
        trading_state: vike_exec::TradingState::Active,
        multipliers: Default::default(),
        multiplier_default: 1.0,
        positions,
    }
}

fn position(venue: &str, symbol: &str, size: f64) -> vike_core::PositionView {
    vike_core::PositionView {
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        position_side: "BOTH".to_string(),
        size,
        avg_px: 100.0,
        unrealized: 0.0,
        mark_source: None,
        leverage: 0.0,
        liq_price: 0.0,
        margin_mode: vike_model::MarginMode::Cross,
        isolated_margin: None,
    }
}

/// ⚠ **The the CI box shape, and the bug this test exists for.** `crate::wired_markets::WIRED_MARKETS` lists
/// binance first, so on a CEX node the PRIMARY engine is the binance PAPER engine — which has
/// traded nothing — while the mount that actually trades is bybit, a NON-primary engine.
/// `CoreSnapshot::build` binds `let acc = &engine.account` (the primary) into the scalar
/// `Portfolio::realized_pnl`/`fees_paid` and `CoreSnapshot::positions`, so a summary line that
/// read those four fields reported binance's silence: measured on the CI box as
/// `fees: 0.0, realized_pnl: 0.0, net_pos: 0.0, positions: 0` on the very minute the bybit mount
/// booked ten maker fills — while `equity` in the SAME line tracked those fills to eight decimal
/// places, which is how we know the engine saw everything and only the REPORT was wrong.
///
/// So this snapshot is built the way `build` builds one on the CI box: primary venue block EMPTY and
/// the primary-mirroring scalars left at 0.0, every traded number living in the SECOND venue
/// block. Every assertion below fails on the pre-fix code.
#[test]
fn the_summary_reports_a_non_primary_venues_fills_not_the_untraded_primarys_silence() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![
        venue_block("binance", 0.0, 0.0, vec![]),
        venue_block("bybit", 12.5, 0.75, vec![position("bybit", "BTCUSDT", -0.25)]),
    ];
    // The primary-mirroring scalars stay exactly as `build` leaves them for an untraded
    // primary — the point is that the line must NOT be reading them.
    assert_eq!(snap.portfolio.realized_pnl, 0.0);
    assert_eq!(snap.portfolio.fees_paid, 0.0);
    assert!(snap.positions.is_empty());

    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT"))
        .expect("the summary line must be valid JSON");
    assert_eq!(
        v["realized_pnl"], 12.5,
        "realized_pnl must be the CROSS-VENUE total; the primary engine traded nothing"
    );
    assert_eq!(v["fees"], 0.75, "fees must be the CROSS-VENUE total; the primary engine paid none");
    assert_eq!(
        v["positions"], 1,
        "positions must count every venue's rows, not just the primary's"
    );
    assert_eq!(
        v["net_pos"], -0.25,
        "net_pos must net the mount symbol across every venue, not read the primary's leg"
    );
}

/// The other half of "cross-venue": the four widened fields must SUM, not merely find the one
/// venue that happens to be non-empty. A per-venue-block fix that returned the first non-zero
/// row would pass the test above and fail this one.
#[test]
fn the_summary_totals_every_venue_rather_than_picking_one() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![
        venue_block("binance", 4.0, 0.25, vec![position("binance", "BTCUSDT", 2.0)]),
        venue_block("bybit", -1.5, 0.75, vec![position("bybit", "BTCUSDT", -0.5)]),
        venue_block("okx", 0.5, 0.5, vec![position("okx", "ETHUSDT", 3.0)]),
    ];
    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT"))
        .expect("the summary line must be valid JSON");
    assert_eq!(v["realized_pnl"], 3.0, "4.0 + -1.5 + 0.5");
    assert_eq!(v["fees"], 1.5, "0.25 + 0.75 + 0.5");
    assert_eq!(v["positions"], 3, "one row per venue, ETHUSDT included");
    assert_eq!(
        v["net_pos"], 1.5,
        "BTCUSDT nets +2.0 against -0.5 across venues; the ETHUSDT leg is a different symbol"
    );
}

/// ⚠ **The the CI box shape of 2026-08-17, and the bug this test exists for.** `VIKE_RECONCILE=1`
/// made bybit authoritative; `CoreThread::reconcile_reports` adopted the account's USDT
/// `walletBalance` — 53647, from a SHARED demo account carrying settlements on
/// AUCTIONUSDT/ONDOUSDT/ETHUSDT/WLDUSDT that this daemon never traded — while nine paper mounts
/// still contributed 1000 seed each. The headline `equity` jumped from ~10000 to
/// **62647.10600813**: a venue wallet the daemon does not own, plus paper seed cash, added
/// together and printed as one number.
///
/// The mount's OWN accounting on bybit is the 0.75 of realized PnL and the quarter-coin
/// position beside it — four orders of magnitude away from the wallet. So the report must
/// distinguish the two, and a reader must be able to tell WHICH is which from the key alone.
/// Every assertion below fails on the pre-fix line, which carried neither key.
#[test]
fn an_adopted_venue_wallet_is_never_added_to_book_kept_paper_seed() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    let mut venues =
        vec![vike_core::VenueBlock { equity: 1_000.0, ..venue_block("binance", 0.0, 0.0, vec![]) }];
    for v in ["okx", "hyperliquid", "aster", "deribit", "alpaca", "ctrader", "ig", "oanda"] {
        venues.push(vike_core::VenueBlock { equity: 1_000.0, ..venue_block(v, 0.0, 0.0, vec![]) });
    }
    // The one live mount: its cash is the venue's whole-account wallet, adopted verbatim.
    venues.push(vike_core::VenueBlock {
        balance: 53_647.10600813,
        balance_mode: vike_exec::BalanceMode::Authoritative,
        equity: 53_647.10600813,
        ..venue_block("bybit", 0.75, 0.25, vec![position("bybit", "BTCUSDT", -0.25)])
    });
    snap.portfolio.venues = venues;
    // The conflated figure the old line printed, kept here as the thing NOT to report.
    snap.portfolio.equity_total =
        vike_model::py_sum(snap.portfolio.venues.iter().map(|v| v.equity));
    assert_eq!(snap.portfolio.equity_total, 62_647.10600813, "the measured the CI box headline");

    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT"))
        .expect("the summary line must be valid JSON");

    assert_eq!(
        v["equity_book"], 9_000.0,
        "the book-kept half is the nine paper mounts' seed and nothing else"
    );
    assert_eq!(
        v["equity_wallet"], 53_647.10600813,
        "the venue-attested half is bybit's whole-account wallet, reported as its own quantity"
    );
    assert_eq!(
        v["wallet_venues"], "bybit",
        "and the line NAMES whose wallet it quoted, so the reader need not open a log"
    );
    // The whole point: no field on this line is the sum of the two.
    for (key, val) in v.as_object().expect("a JSON object") {
        if let Some(f) = val.as_f64() {
            assert_ne!(
                f, 62_647.10600813,
                "`{key}` is the conflated total — a venue wallet and a book-kept equity are \
                     different quantities and no field may add them"
            );
        }
    }
    // The mount's own accounting is still reported, unwidened and unswallowed by the wallet.
    assert_eq!(v["realized_pnl"], 0.75);
    assert_eq!(v["net_pos"], -0.25);
}

/// The other half: on a node where NOTHING has ever attested a balance, the wallet fields must
/// read empty rather than mirroring the book — otherwise a paper daemon reports its seed cash
/// twice, once under each name, and the split says nothing.
#[test]
fn a_pure_paper_node_reports_a_zero_wallet_and_names_no_venue() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![
        vike_core::VenueBlock { equity: 1_000.0, ..venue_block("binance", 0.0, 0.0, vec![]) },
        vike_core::VenueBlock { equity: 2_500.0, ..venue_block("okx", 0.0, 0.0, vec![]) },
    ];
    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT"))
        .expect("the summary line must be valid JSON");
    assert_eq!(v["equity_book"], 3_500.0);
    assert_eq!(v["equity_wallet"], 0.0, "no venue has attested a balance");
    assert_eq!(v["wallet_venues"], "", "so there is no wallet to name");
}

/// A `MountRowKind::Mount` row for `venue`/`symbol` — the snapshot half of "what this daemon
/// runs", which is what the mounted-set scoping reads.
fn mount_row(venue: &str, symbol: &str) -> vike_core::MountView {
    vike_core::MountView {
        kind: vike_core::MountRowKind::Mount,
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        interval: "1m".to_string(),
        ready: true,
        position: 0.0,
        realized_pnl: 0.0,
        unrealized_pnl: 0.0,
        notional: 0.0,
        budget: None,
        latched: false,
        params: None,
    }
}

/// ⚠ **The I10 REHEARSAL shape, and the observation this pair of keys exists for**
/// (`docs/ops/i10-rehearsal-2026-08-19.md`): two mounts seeded at 10k, ten default-build venue
/// engines each carrying that same seed, and a summary line reading `equity_book: 100000.0`.
///
/// Both halves are asserted, because the fix is that BOTH are reported: `equity_book` keeps
/// its whole-book value (a consumer that already reads it sees no change, and the
/// book/wallet partition still holds), while `equity_book_mounted` answers the question the
/// operator was actually asking — 20000.0 — and `mounted_venues` names the two it scoped to.
#[test]
fn the_summary_scopes_a_mounted_set_figure_beside_the_whole_book() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = [
        "binance",
        "bybit",
        "okx",
        "hyperliquid",
        "aster",
        "deribit",
        "alpaca",
        "ctrader",
        "ig",
        "oanda",
    ]
    .into_iter()
    .map(|v| vike_core::VenueBlock { equity: 10_000.0, ..venue_block(v, 0.0, 0.0, vec![]) })
    .collect();
    snap.mounts = vec![mount_row("binance", "BTCUSDT"), mount_row("bybit", "BTCUSDT")];

    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT"))
        .expect("the summary line must be valid JSON");
    assert_eq!(
        v["equity_book"], 100_000.0,
        "the whole-book figure is CORRECT and unchanged — ten engines seeded at 10k each"
    );
    assert_eq!(
        v["equity_book_mounted"], 20_000.0,
        "…and the scoped companion is the two MOUNTED venues' seeds, which is the number the \
             rehearsal's operator was reaching for"
    );
    assert_eq!(
        v["mounted_venues"], "binance,bybit",
        "the line NAMES what it scoped to, so the reader need not open a profile"
    );
}

/// The RESIDUAL row is not a mount. `CoreThread::mount_views` appends one
/// `MountRowKind::Residual` row (venue: the empty string) whenever any mount exists, so a
/// scoping that filtered on "has a venue row" rather than on KIND would silently widen the
/// set the day a residual row carried a venue — and would name an empty venue today.
#[test]
fn the_residual_row_is_not_treated_as_a_mount() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![
        vike_core::VenueBlock { equity: 1_000.0, ..venue_block("binance", 0.0, 0.0, vec![]) },
        vike_core::VenueBlock { equity: 2_500.0, ..venue_block("okx", 0.0, 0.0, vec![]) },
    ];
    snap.mounts = vec![
        mount_row("binance", "BTCUSDT"),
        vike_core::MountView { kind: vike_core::MountRowKind::Residual, ..mount_row("", "") },
    ];
    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT"))
        .expect("the summary line must be valid JSON");
    assert_eq!(v["equity_book_mounted"], 1_000.0, "only the binance mount is scoped in");
    assert_eq!(v["mounted_venues"], "binance", "the residual row names no venue");
}

/// A core that has mounted NOTHING scopes to nothing: `0.0` with an EMPTY name list, which is
/// how a reader tells "scoped to nothing" from "nothing to scope". `equity_book` still
/// reports the seed, so no capital goes unreported by the line as a whole.
#[test]
fn a_mountless_snapshot_scopes_to_zero_and_names_nobody() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues =
        vec![vike_core::VenueBlock { equity: 1_000.0, ..venue_block("binance", 0.0, 0.0, vec![]) }];
    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT"))
        .expect("the summary line must be valid JSON");
    assert_eq!(v["equity_book"], 1_000.0, "the whole book is still reported");
    assert_eq!(v["equity_book_mounted"], 0.0);
    assert_eq!(v["mounted_venues"], "");
}

/// A MOUNTED venue that has flipped `Authoritative` (reconcile adopted its wallet) is NAMED
/// but contributes NOTHING to the mounted BOOK figure — its equity lives in `equity_wallet`.
/// The scoped figure obeys the same partition `equity_book` does; a mounted-set number that
/// quietly pulled an adopted wallet back into a "book" key would re-commit the exact
/// conflation the 62647.10600813 split exists to prevent.
#[test]
fn a_mounted_venue_on_an_adopted_wallet_is_named_but_not_booked() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![
        vike_core::VenueBlock { equity: 10_000.0, ..venue_block("binance", 0.0, 0.0, vec![]) },
        vike_core::VenueBlock {
            balance: 53_647.10600813,
            balance_mode: vike_exec::BalanceMode::Authoritative,
            equity: 53_647.10600813,
            ..venue_block("bybit", 0.0, 0.0, vec![])
        },
    ];
    snap.mounts = vec![mount_row("binance", "BTCUSDT"), mount_row("bybit", "BTCUSDT")];
    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT"))
        .expect("the summary line must be valid JSON");
    assert_eq!(
        v["equity_book_mounted"], 10_000.0,
        "bybit's adopted wallet is NOT book-kept equity, mounted or otherwise"
    );
    assert_eq!(v["mounted_venues"], "binance,bybit", "but it IS a mount, and is named as one");
    assert_eq!(v["equity_wallet"], 53_647.10600813, "its equity is reported under the wallet");
    assert_eq!(v["wallet_venues"], "bybit");
}

// -- the stop path: what the stdio control channel does, and what it deliberately does not ----

/// Drive [`control_loop`] over a fixed script and report `(stop raised?, commands accepted)`.
fn drive(input: &str, is_tty: bool) -> (bool, usize) {
    let stop = AtomicBool::new(false);
    let mut sent = 0usize;
    control_loop(
        std::io::Cursor::new(input.as_bytes()),
        is_tty,
        &stop,
        |_cmd| sent += 1,
        || "{}".to_string(),
    );
    (stop.load(Ordering::SeqCst), sent)
}

/// ⚠ **The regression this daemon cannot afford.** Under systemd stdin is `/dev/null`, which
/// reads EOF the instant the daemon starts. If EOF meant "stop", the daemon would exit at
/// startup on every box, every start — so a non-tty EOF must leave the flag DOWN and the daemon
/// trading headless until a signal arrives. `tests/sigterm_stop.rs` proves the same property
/// against the real process; this pins the rule it rests on.
#[test]
fn a_non_tty_eof_does_not_stop_the_daemon() {
    assert_eq!(
        drive("", false),
        (false, 0),
        "a non-tty EOF must NOT raise the stop flag — systemd wires stdin to /dev/null, so this \
             would exit at startup on every service box"
    );
}

/// …and the mirror image, so the rule above is not bought by ignoring EOF entirely: Ctrl-D from
/// a human at a terminal IS an explicit stop.
#[test]
fn a_tty_eof_stops_the_daemon() {
    assert_eq!(drive("", true), (true, 0), "Ctrl-D on a TTY is a stop");
}

/// Every stop word raises the flag on either channel — the word is explicit, so whether the
/// channel is a terminal has nothing to add.
#[test]
fn every_stop_word_raises_the_flag_on_either_channel() {
    for word in ["shutdown", "quit", "exit"] {
        for is_tty in [true, false] {
            assert!(
                drive(&format!("{word}\n"), is_tty).0,
                "`{word}` must stop the daemon (is_tty={is_tty})"
            );
        }
    }
}

/// A JSON command is lowered and the channel keeps running — a stop is a WORD, never a side
/// effect of having been sent something.
#[test]
fn a_command_is_lowered_and_does_not_stop_the_daemon() {
    let req = vike_model::OrderRequest {
        client_order_id: "stdio-1".to_string(),
        venue: "polymarket".to_string(),
        symbol: "TOK".to_string(),
        side: 1,
        qty: 1.0,
        order_type: "limit".to_string(),
        price: Some(0.4),
        ..Default::default()
    };
    let json =
        serde_json::to_string(&Command::Order(vike_exec::OrderIntent::Submit(Box::new(req))))
            .expect("serialize an operator command");
    assert_eq!(
        drive(&format!("{json}\n"), false),
        (false, 1),
        "the command must reach the core lane, and must not be read as a stop"
    );
}

/// Garbage on the control channel is reported, never obeyed: a typo must not stop a daemon that
/// is holding a live book, and it must not be mistaken for a command either.
#[test]
fn junk_neither_stops_the_daemon_nor_reaches_the_core() {
    assert_eq!(drive("\n   \nnot json\nhalt\n", false), (false, 0));
}

/// EVERY shipped unit that starts this daemon, as `(repo-relative path, contents)`.
///
/// ⚠ **THERE IS ONE SINCE 2026-09-16, AND THERE WERE TWO — WHICH IS WHY THIS IS STILL A
/// TABLE.** The second row was the ONE-PROJECT-FOLDER unit the CI box actually ran, and for a while
/// only the first row was read: that file carried its own `TimeoutStopSec=` hand copy, its
/// comment claimed the test below checked it, and nothing did. The unit collapse deleted it —
/// a daemon has ONE unit file now, named without a suffix, and a box's real root is a
/// substitution rather than a second tracked file — so the class of defect this table was
/// widened for cannot currently exist.
///
/// It stays a TABLE rather than collapsing into a lone `include_str!` because the widening was
/// the fix and the shape is the memory of it: a SECOND unit added later must be added here by
/// hand, `include_str!` needing a literal path, and that is the intended cost.
const SHIPPED_UNITS: [(&str, &str); 1] =
    [("deploy/vike-tradehub.service", include_str!("../../../deploy/vike-tradehub.service"))];

/// `TimeoutStopSec=` from a unit's text, skipping COMMENTED lines — the unit discusses the
/// directive in prose right above setting it, so a naive prefix match on an untrimmed line
/// would read the commentary and pass on a unit that never sets the directive at all.
fn stop_timeout_secs(path: &str, unit: &str) -> u64 {
    unit.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| l.strip_prefix("TimeoutStopSec="))
        .unwrap_or_else(|| panic!("`{path}` must set TimeoutStopSec= explicitly"))
        .trim()
        .parse()
        .unwrap_or_else(|e| panic!("`{path}`: TimeoutStopSec= is a plain number of seconds: {e}"))
}

/// The teardown budget must fit inside EVERY shipped unit's `TimeoutStopSec=`, or systemd
/// SIGKILLs the daemon mid-teardown and the graceful stop buys nothing. Both numbers are READ
/// (the profile default from `DaemonSettings`, the timeout from each unit) rather than restated,
/// so the two cannot drift apart in a later edit.
#[test]
fn the_default_shutdown_deadline_fits_inside_the_units_stop_timeout() {
    let deadline = crate::config::DaemonSettings::default().shutdown_deadline_ms;
    for (path, unit) in SHIPPED_UNITS {
        let timeout = stop_timeout_secs(path, unit);
        // ⚠ ONE step of this daemon's stop is still OUTSIDE `run_with_deadline`: stopping the
        // observe publisher, which closes mailboxes and does not join the detached accept loop,
        // so it is bounded by `publish::POLL_INTERVAL`. STRICT `<` is what leaves room for it.
        //
        // ⚠ This comment used to name TWO such steps and call both "sub-second by
        // construction". That was FALSE of the second one: the summary-thread join sat here
        // with no timeout while the same thread delivers alerts INLINE through a
        // `WebhookSink<UreqTransport>` whose `timeout_global` is 10 s — so a single in-flight
        // alert could blow a `TimeoutStopSec=10` on its own, and this assertion was comparing
        // two numbers while excluding the term that broke the sum. The fix was to move that
        // join INTO `tasks`, where the deadline below actually covers it; the assertion is
        // unchanged, but it is now true. (The sibling recorder's version of this test made the
        // same class of mistake with a 12 s feed-stop prefix and still claimed the flush was
        // safe from SIGKILL. Say what is compared, so the next reader can check the claim
        // rather than inherit it.)
        assert!(
            deadline < timeout * 1_000,
            "the default [daemon] shutdown_deadline_ms ({deadline}) must be strictly under \
                 `{path}`'s TimeoutStopSec={timeout}s — otherwise SIGKILL wins the race and the \
                 teardown is cut in half. This bounds the hard-capped teardown, which now INCLUDES \
                 the summary-thread join; the publisher stop that precedes it is bounded by one \
                 poll interval and rides the difference."
        );
    }
}

/// The loser of a teardown claim waits on the SAME budget the winner's teardown runs under.
///
/// It is one number by construction — `deadline` is resolved once, above the claim, and passed
/// to both `StopSignal::await_teardown` and `run_with_deadline`. This asserts the property that
/// makes that safe: whichever thread ends the process, the stop still fits inside the unit's
/// `TimeoutStopSec=`, because both arms are bounded by the same profile deadline. A loser
/// waiting on a LARGER bound would let a second stop route hold the process open past SIGKILL;
/// one waiting on a smaller bound would exit while the winner was still cancelling.
#[test]
fn a_losing_claim_waits_within_the_same_stop_timeout_the_teardown_does() {
    let deadline = crate::config::DaemonSettings::default().shutdown_deadline_ms;
    for (path, unit) in SHIPPED_UNITS {
        let timeout = stop_timeout_secs(path, unit);
        // The wait the losing arm performs is `stop.await_teardown(deadline)` — the same value.
        assert!(
            deadline < timeout * 1_000,
            "a loser parked for the winner's budget ({deadline} ms) must still be released \
                 before `{path}`'s TimeoutStopSec={timeout}s, or a second stop route turns a \
                 graceful stop into a SIGKILL"
        );
    }
}

// -- alerting: where the rules file is looked for --------------------------------------------

/// The rules file resolves in exactly TWO places and nowhere else: `$VIKE_ALERTS` when it
/// names one, else `<state_dir>/alerts.json`. With NEITHER there is no path at all — which is
/// what makes [`maybe_mount_alerts`] log the OFF line instead of quietly reading a file
/// somewhere the operator was never told about.
///
/// The third assert is the one that bites: a resolver that reached for a directory of its own
/// — beside the executable, the working directory, a home — would answer `Some` there.
#[test]
fn the_alerts_file_resolves_only_from_the_override_or_the_state_directory() {
    let state = Path::new("/tmp/vike-state");
    assert_eq!(
        alerts_path_in(None, Some(state)),
        Some(state.join("alerts.json")),
        "no override ⇒ the state directory, joined with the library's own basename"
    );
    assert_eq!(
        alerts_path_in(Some("/etc/vike/rules.json"), Some(state)),
        Some(PathBuf::from("/etc/vike/rules.json")),
        "an explicit $VIKE_ALERTS names the file outright"
    );
    assert_eq!(
        alerts_path_in(None, None),
        None,
        "no override and no state directory ⇒ NO path, never one of this resolver's own \
             invention"
    );
    // A blank override is an unset one in every shell that produced it.
    for blank in [Some(""), Some("   ")] {
        assert_eq!(alerts_path_in(blank, Some(state)), Some(state.join("alerts.json")));
        assert_eq!(alerts_path_in(blank, None), None, "and blank cannot conjure one either");
    }
}

// -- settings / policy (settings-unification Phase 6c) ---------------------------------------

/// THE no-file property, at this daemon's own edge: with no `policy` rows on the machine the
/// loader yields `Policy::default()`, whose venue-facing projection is `MountPolicy::default()`.
/// Driven through the REAL loader (`load(None, &{})` is exactly "no home directory, no project
/// file, no environment") rather than asserting the default struct, so a default that stopped
/// being the no-file answer would fail here.
///
/// ⚠ This test's NAME used to be `..._leaves_every_venue_on_its_own_literal`, and that is no
/// longer what the default means. Every SCALAR field is still `None` — each venue arm keeps its
/// compiled-in literal — but `venues` defaults to `paper` for every venue, so a daemon with no
/// `policy.venues` row mounts ALL PAPER. Asserted here rather than left to the equality, because the
/// equality would hold just as well if the default flipped to `live` on both sides.
#[test]
fn an_absent_policy_file_is_the_mount_default_and_arms_no_venue() {
    let settings = vike_config::load(None, &HashMap::new()).unwrap();
    let mount = vike_mount::MountPolicy::from(&settings.policy);
    assert_eq!(mount, vike_mount::MountPolicy::default());
    assert_eq!(mount.market_slippage, None, "no file ⇒ each venue keeps its own literal");
    for venue in vike_model::VENUES {
        assert_eq!(
            mount.venue_mode(venue),
            vike_config::VenueMode::Paper,
            "{venue}: no policy.venues row must arm NOTHING — credential presence is no longer a gate \
                 that can act alone"
        );
    }
    // ⚠ This asserted `warnings.is_empty()`, and that emptiness was the register entry
    // `docs/ops/kill-switches.md` carried as "a missing settings directory is silently
    // uncapped": the VALUES above are right and the operator was told nothing about why they
    // are the compiled-in defaults. The values did not move; only the silence did.
    assert_eq!(
        settings.warnings,
        vec![vike_config::NO_SETTINGS_DIRECTORY_WARNING.to_string()],
        "a daemon that resolved no project must SAY its ceilings are defaults: {settings:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// The dead-man switch (M4) — the policy key REACHES `CoreConfig`. The trip behaviour itself is
// `crates/vike-core/src/runtime/tests/deadman.rs`'s job (`trips_after_timeout_and_cancels_all_
// open_orders`, `cancel_all_and_halt_engages_the_sentinel`); these pin only the fold from the
// file's two keys to the config the live mount hands the core, which is the half that did not
// exist before. Asserting the config is far more robust than racing a timer, and it is the
// whole of what this binary adds. The file→`Policy` half is `vike-config`'s own; `Policy`'s
// fields are public, so the variants are spelled as struct updates rather than round-tripped
// through a temp file that would prove the loader a second time.
// ---------------------------------------------------------------------------------------------

/// **The re-ruling, pinned at the seam:** a live mount that says NOTHING arms NO dead-man.
/// This test was `the_default_policy_arms_the_deadman_at_sixty_seconds_halting_on_the_
/// process_sentinel` for one morning and asserted the opposite; `vike_config::Policy::
/// deadman_timeout_ms` records why the ruling reversed (the switch observes silence, so an
/// armed default halted every session-bounded venue at every close). The absent key and the
/// explicit zero reach the core IDENTICALLY — `None`, no `DeadMan`, no timer — and are told
/// apart only by the warning, which the sibling test below pins.
#[test]
fn a_policy_that_says_nothing_arms_no_deadman() {
    assert!(
        deadman_config_from_policy(&vike_config::Policy::default()).is_none(),
        "an ABSENT key is OFF — nothing arms the silence-detector unless an operator writes it"
    );
}

/// The recommended line, WRITTEN, arms the switch at sixty seconds, halting, writing the
/// process's ONE HALT sentinel. Each of the four is asserted separately, because each is a
/// distinct way to be wrong — `None` (not armed), a different number (armed at the wrong
/// silence), `CancelAll` (trips and lets the strategy re-quote into a market it has not seen),
/// and a `None` halt file (engages the in-process gate but leaves the venue adapters'
/// cross-process check un-tripped).
#[test]
fn the_recommended_line_arms_the_deadman_at_sixty_seconds_halting_on_the_process_sentinel() {
    let policy = vike_config::Policy {
        deadman_timeout_ms: Some(vike_config::RECOMMENDED_DEADMAN_TIMEOUT_MS),
        ..vike_config::Policy::default()
    };
    let cfg = deadman_config_from_policy(&policy).expect("a written key ARMS the dead-man");
    assert_eq!(cfg.timeout, Duration::from_secs(60));
    assert_eq!(cfg.action, vike_core::DeadManAction::CancelAllAndHalt);
    assert!(cfg.action.engages_halt(), "the default action must engage HALT, not only cancel");
    // The resolver is memoized process-wide, so this is the path every venue's submit boundary
    // in this process checks and the one `touch HALT` in the runbook writes.
    assert_eq!(
        cfg.halt_file,
        Some(vike_bridge_core::halt::halt_path_from_env()),
        "the automatic trip and the manual kill switch must write ONE file"
    );
}

/// `deadman_timeout_ms = 0` is the EXPLICIT-off spelling, and it disables by ABSENCE: the core
/// gets `None`, builds no `DeadMan` and arms no timer — not a config with a zero inside it,
/// which the core would clamp to 1 ms and trip on the first quiet millisecond.
#[test]
fn deadman_timeout_zero_disarms_the_switch_by_absence() {
    let policy = vike_config::Policy {
        deadman_timeout_ms: Some(vike_config::DEADMAN_DISABLED_MS),
        ..vike_config::Policy::default()
    };
    assert!(deadman_config_from_policy(&policy).is_none(), "0 means OFF, and OFF means None");
}

/// The absent-key warning's DECISION, with no subscriber: a message for `None` only — not for
/// the explicit zero (the operator's recorded decision) and not for an armed value — and the
/// message names the file, the key, what the switch would do, the recommendation as a
/// paste-ready line, the zero that silences it, and the successor. The EMISSION (once, through
/// `tracing`) is `crates/vike-tradehub/tests/deadman_absent_warning.rs`'s job, in its own
/// binary, because a global-subscriber capture cannot share a test binary with tests that
/// drive `live_mount_with` under no subscriber.
///
/// ⚠ **This used to be a PAIR — one test per `Authority` arm — because the remedy used to
/// render differently depending on whether a box's settings files still answered.**
/// `docs/decisions/0086` deletes the files arm outright: there is one store now, so there is one
/// rendering, and this test carries what both halves used to prove.
#[test]
fn the_absent_key_warning_fires_for_none_alone_and_names_what_an_operator_needs() {
    let absent = vike_config::Policy::default();
    let msg = deadman_absent_warning(&absent).expect("an absent key WARNS whichever store answers");
    assert!(msg.contains("`policy.deadman_timeout_ms`"), "names the key: {msg}");
    assert!(msg.contains("cancel every resting"), "says what it would do: {msg}");
    assert!(msg.contains("engage HALT"), "…and that the default action halts: {msg}");
    assert!(msg.contains("observes SILENCE, not the connection"), "states the cost: {msg}");
    // The remedy, both halves, in the vocabulary of the store that answered.
    assert!(
        msg.contains("vike-cli config set policy.deadman_timeout_ms 60000"),
        "the arming line must be a command this box can run: {msg}"
    );
    assert!(
        msg.contains("vike-cli config set policy.deadman_timeout_ms 0"),
        "…and so must the one that records a decision AGAINST it: {msg}"
    );
    assert!(
        msg.contains("the settings database does not carry `policy.deadman_timeout_ms`"),
        "the headline names the store: {msg}"
    );
    // ⚠ The sentence this message must NOT contain any more. It said "this live mount has NO
    // automatic stop", which became FALSE the day the connection-state switch shipped armed by
    // default (M13) — an operator reading it would disable a real protection or write a key
    // they do not need. The successor is now named as SHIPPED, not as coming.
    assert!(
        !msg.contains("NO automatic stop"),
        "the link dead-man IS an automatic stop and is on by default: {msg}"
    );
    assert!(
        msg.contains("CONNECTION-state dead-man is armed by default"),
        "names what IS armed: {msg}"
    );
    assert!(msg.contains("link_deadman_grace_ms"), "names the key that governs it: {msg}");

    let decided = vike_config::Policy {
        deadman_timeout_ms: Some(vike_config::DEADMAN_DISABLED_MS),
        ..vike_config::Policy::default()
    };
    assert_eq!(deadman_absent_warning(&decided), None, "an explicit 0 is a decision");

    let armed =
        vike_config::Policy { deadman_timeout_ms: Some(5_000), ..vike_config::Policy::default() };
    assert_eq!(deadman_absent_warning(&armed), None, "an armed switch has nothing to warn");
}

/// The SIBLING half of the same warning: `link_deadman_grace_ms = 0`'s way back is a
/// `config set` of the default, because there is no `config unset` and therefore no line to
/// delete.
#[test]
fn the_link_grace_way_back_is_a_config_set_of_the_default() {
    let both_off = vike_config::Policy {
        link_deadman_grace_ms: Some(vike_config::LINK_DEADMAN_DISABLED_MS),
        ..vike_config::Policy::default()
    };
    let msg = deadman_absent_warning(&both_off).unwrap();
    assert!(msg.contains("Run `vike-cli config set policy.link_deadman_grace_ms 120000`"), "{msg}");
    assert!(msg.contains("AND NEITHER IS THE OTHER ONE"), "{msg}");
    assert!(msg.contains("NO automatic stop of any kind"), "{msg}");
}

// --- THE LINK DEAD-MAN's fold (M13) --------------------------------------------------------
// The FOUR inputs are the policy grace, `vike_model::link_deadman_default`, the venues this
// mount has and the LANES it subscribed for each; each test below moves ONE of them.

/// A mounted venue whose lanes DO carry a disconnect, for the fold tests — the disclosure half
/// is a separate question, tested against real [`VenuePlan`]s by
/// [`the_cex_arm_subscribes_no_lane_that_could_report_a_dead_link`] below.
fn seen(venue: &str) -> (String, MountLinkDisclosure) {
    (venue.to_string(), MountLinkDisclosure::Discloses { lane: "a test lane" })
}

/// The same venue mounted over lanes that carry nothing.
fn unseen(venue: &str) -> (String, MountLinkDisclosure) {
    (venue.to_string(), MountLinkDisclosure::Silent { why: "a test lane that discloses none" })
}

/// **The default, and the whole point of M13:** a policy that says NOTHING arms the link
/// switch on the venues the table says default ON — the exact opposite of the silence switch's
/// absent-key behaviour two tests up, and deliberately so.
#[test]
fn a_policy_that_says_nothing_arms_the_link_deadman_on_the_armed_venues() {
    let policy = vike_config::Policy::default();
    let venues = [seen("binance"), seen("oanda")];
    let cfg = link_deadman_config_from_policy(&policy, &venues)
        .expect("an ABSENT key ARMS the link dead-man — that is the M13 default");
    assert_eq!(cfg.grace, Duration::from_millis(vike_config::DEFAULT_LINK_DEADMAN_GRACE_MS));
    assert_eq!(cfg.action, vike_core::DeadManAction::CancelAllAndHalt, "shares deadman_action");
    assert_eq!(
        cfg.venues,
        ["binance".to_string()].into_iter().collect::<std::collections::BTreeSet<_>>(),
        "the FX venue is NOT armed — a weekend close must not be able to trip this switch"
    );
    assert_eq!(
        cfg.halt_file,
        Some(vike_bridge_core::halt::halt_path_from_env()),
        "both switches and the operator's hand must reach ONE sentinel"
    );
}

/// `link_deadman_grace_ms = 0` disarms it by ABSENCE, the sibling's idiom: the core gets
/// `None`, builds no latch and arms no timer.
#[test]
fn a_zero_link_grace_disarms_the_switch_by_absence() {
    let policy = vike_config::Policy {
        link_deadman_grace_ms: Some(vike_config::LINK_DEADMAN_DISABLED_MS),
        ..vike_config::Policy::default()
    };
    assert!(
        link_deadman_config_from_policy(&policy, &[seen("binance")]).is_none(),
        "0 means OFF, and OFF means None"
    );
}

/// ⚠ **A mount with no ARMED venue builds nothing.** An FX-only daemon must not carry a config
/// whose venue set is empty: that would arm a timer, contribute a waker cadence and forfeit
/// journal replay in order to watch nothing, which is the "a mechanism exists" claim
/// `docs/ops/kill-switches.md` opens by warning about.
#[test]
fn an_fx_only_mount_builds_no_link_deadman_at_all() {
    let policy = vike_config::Policy::default();
    let venues = [seen("oanda"), seen("ig"), seen("ibkr")];
    assert!(
        link_deadman_config_from_policy(&policy, &venues).is_none(),
        "no mounted venue defaults ON ⇒ no config, not an empty one"
    );
}

/// ⚠ **A venue the TABLE arms but this MOUNT cannot hear about is not armed either** — the
/// second half of the fold, and the one a venue-table-only version got wrong: it put binance
/// in the config's venue set (and printed an ARMED line for it) on a daemon that subscribes no
/// lane carrying a binance disconnect.
#[test]
fn an_armed_venue_whose_lanes_disclose_nothing_here_is_not_armed() {
    let policy = vike_config::Policy::default();
    assert!(
        link_deadman_config_from_policy(&policy, &[unseen("binance"), unseen("bybit")]).is_none(),
        "no venue can reach the latch on this mount ⇒ no config at all"
    );
    let cfg = link_deadman_config_from_policy(
        &policy,
        &[unseen("binance"), seen("polymarket"), unseen("okx")],
    )
    .expect("the one venue with a disclosing lane still arms");
    assert_eq!(
        cfg.venues,
        ["polymarket".to_string()].into_iter().collect::<std::collections::BTreeSet<_>>(),
        "only the venue this mount can actually hear a disconnect from"
    );
}

#[test]
fn the_cex_arm_subscribes_only_the_bar_verb_on_the_status_bearing_handle() {
    // **The CODE half of the bybit `recon_feed_statuses` condition.**
    //
    // That row keys on `CexBars::status()` — ONE `Arc<Mutex<String>>` shared by every lane the
    // bridge spawns on that handle. `LiveFeeds::recon_feed_statuses`' doc keeps the row on the
    // measured fact that this daemon drives exactly ONE lane on it, which is why the CI box's latch
    // was total and permanent rather than thrashing between lanes. That fact was a READING of
    // today's code; this makes it a gate.
    //
    // ⚠ It gates the CODE only. The PROFILE half — one interval, a spot symbol — stays an
    // operator fact: a second interval in the mount spawns a second `feed_main` through the
    // `intervals` loop below, and a `.P` symbol spawns `mark_main`. That is why condition (1)
    // of keeping the row (every lane, `mark_main` included, having a `SessionStatus::Live` arm)
    // is not optional, and why the doc says "the daemon runs one lane on this handle, and that
    // is gated" rather than "bybit's string is unambiguous".
    const FEEDS: &str = include_str!("feeds.rs");
    // Anchored on the kline lane's function (`wire_venue_feeds`' CEX arm calls it, then calls the
    // tick-pump function, which sits directly below it in the file).
    let arm_start = FEEDS.find("fn wire_cex_kline_lane(").expect(
        "the CEX kline lane's function head has moved — this scan is anchored on it and would \
             otherwise check nothing",
    );
    // Bound the scan at LANE 2's banner, where the tick pump (a DIFFERENT object, which never
    // touches `market_feed::Feeds::status`) takes over.
    let arm_end = FEEDS[arm_start..]
        .find("── LANE 2:")
        .expect("the CEX arm's lane-2 banner has moved — re-anchor this scan");
    let lane1 = &FEEDS[arm_start..arm_start + arm_end];
    assert!(
        lane1.contains("subscribe_bars"),
        "the CEX arm must still subscribe bars on the status-bearing handle, or this gate is \
             checking nothing"
    );
    for forbidden in ["subscribe_trades", "subscribe_depth", "subscribe_book"] {
        assert!(
            !lane1.contains(forbidden),
            "the CEX arm now calls `{forbidden}` on `CexBars` — a SECOND lane writing the one \
                 `Arc<Mutex<String>>` bybit's `recon_feed_statuses` row keys on. That row rests on \
                 this being a single-writer handle; either give every new lane a \
                 `SessionStatus::Live` arm and re-argue the row in \
                 `LiveFeeds::recon_feed_statuses`' doc, or withdraw the row."
        );
    }
}

/// ⚠ **The fact behind that filter, read off the REAL plans rather than asserted in prose.**
/// The CEX arm subscribes the kline lane and the tick pump; the kline lane discloses nothing,
/// the TICK PUMP discloses its transport state onto the core tick lane
/// (`crates/bridges/binance/src/family/depth.rs`'s `disclose_link` and its bybit/okx twins),
/// and the DOM depth lane — the venue's OTHER emitter — is deliberately not subscribed here.
/// Polymarket's `subscribe_book` reaches the core through a sink instead; deribit's bridge
/// emits nothing at all.
///
/// ⚠ **The panic arm is the point of the test and it has now flipped direction.** It used to
/// fire on `Discloses` (a depth subscription added without revisiting the decision); it fires
/// on `Silent` now, because losing the pump's disclosure would silently un-arm four venues on
/// every live daemon while `vike_model::link_deadman_default` still called them Armed —
/// exactly the false-promise state this whole fold exists to prevent, wearing the other face.
#[test]
fn the_cex_arm_subscribes_the_tick_pump_that_reports_a_dead_link() {
    for venue in [CexVenue::Binance, CexVenue::Bybit, CexVenue::Okx, CexVenue::Aster] {
        let plan = VenuePlan::Cex { venue, mainnet: true };
        match mount_link_disclosure(&plan) {
            MountLinkDisclosure::Discloses { lane } => {
                assert!(
                    lane.contains("tick pump"),
                    "the CEX arm's disclosure must name the lane it rests on: {lane}"
                );
                assert!(
                    lane.contains("NOT the kline lane"),
                    "…and must say which subscribed lane does NOT carry it, since that is the \
                         one a reader assumes: {lane}"
                );
            }
            MountLinkDisclosure::Silent { why } => panic!(
                "the CEX arm claims to disclose NOTHING ({why:?}) — if the tick pump's \
                     transport disclosure was removed or a subscription changed, then \
                     vike_model::link_deadman_default's Armed rows for binance/aster/bybit/okx are \
                     a promise this daemon cannot keep: fix the pump, or move those rows, and say \
                     so here"
            ),
        }
    }
    // …and the venue whose disclosure rides a SINK rather than the tick lane is unaffected.
    #[cfg(feature = "polymarket")]
    assert!(matches!(
        mount_link_disclosure(&VenuePlan::Polymarket),
        MountLinkDisclosure::Discloses { .. }
    ));
    assert!(
        matches!(mount_link_disclosure(&VenuePlan::Deribit), MountLinkDisclosure::Silent { .. }),
        "deribit's bridge calls stream_status nowhere"
    );
}

/// ⚠ **The end-to-end arming claim for the newly-covered venues, over the REAL plans and the
/// REAL table** — the join every other test here takes one leg of. A default `Policy`, a
/// CEX mount, and the four venues the table arms all end up in the config's venue set AND read
/// ARMED in the operator's startup lines. This is the test that would have been red on the day
/// the feature shipped.
#[test]
fn a_default_cex_mount_arms_the_link_deadman_and_says_so() {
    let policy = vike_config::Policy::default();
    let mounted: Vec<(String, MountLinkDisclosure)> =
        [CexVenue::Binance, CexVenue::Bybit, CexVenue::Okx, CexVenue::Aster]
            .into_iter()
            .map(|v| {
                let plan = VenuePlan::Cex { venue: v, mainnet: true };
                (v.slug().to_string(), mount_link_disclosure(&plan))
            })
            .collect();

    let cfg = link_deadman_config_from_policy(&policy, &mounted)
        .expect("a default CEX mount now builds the switch — that is the whole change");
    assert_eq!(
        cfg.venues,
        ["aster", "binance", "bybit", "okx"]
            .into_iter()
            .map(str::to_string)
            .collect::<std::collections::BTreeSet<_>>(),
        "every CEX venue the table arms is in the switch's venue set"
    );

    let lines = link_deadman_arming_report(&policy, &mounted);
    assert_eq!(lines.len(), 4, "one line per mounted venue");
    for line in &lines {
        assert!(line.contains("ARMED"), "an armed venue must READ armed: {line}");
        assert!(line.contains("120000 ms"), "…at the default grace: {line}");
        assert!(line.contains("tick pump"), "…naming the lane it rests on: {line}");
    }

    // ⚠ …and the venue-table half still refuses independently: an FX venue mounted through a
    // DISCLOSING lane stays off, because its market has sessions. Without this the test above
    // would pass on a fold that had quietly become "whatever the mount can hear".
    let with_fx = [mounted[0].clone(), seen("oanda")];
    let cfg = link_deadman_config_from_policy(&policy, &with_fx).expect("binance still arms");
    assert!(
        !cfg.venues.contains("oanda"),
        "a session-bounded venue must not arm however well this daemon hears it"
    );

    // ⚠ …and a venue whose REAL plan discloses nothing still gets its own off-reason printed
    // rather than being dropped from the report — asserted over the real `VenuePlan` rather
    // than the constructed `unseen()` the report test uses, since the whole point of that arm
    // is that it describes a real mount.
    let with_deribit =
        [mounted[0].clone(), ("deribit".to_string(), mount_link_disclosure(&VenuePlan::Deribit))];
    let lines = link_deadman_arming_report(&policy, &with_deribit);
    assert!(lines[1].contains("off for deribit"), "{}", lines[1]);
    assert!(!lines[1].contains("ARMED"), "a silent venue may not read as armed: {}", lines[1]);
    assert!(
        lines[1].contains("stream_status nowhere"),
        "…and must carry the RESIDUAL reason from its own table row, not the mount's: {}",
        lines[1]
    );
}

/// A written grace and the lighter action both arrive verbatim — the file edge has already
/// refused everything outside the bounds, so nothing is clamped here.
#[test]
fn a_written_link_grace_and_the_cancel_all_action_reach_the_core_config() {
    let policy = vike_config::Policy {
        link_deadman_grace_ms: Some(45_000),
        deadman_action: vike_config::DeadManActionSetting::CancelAll,
        ..vike_config::Policy::default()
    };
    let cfg =
        link_deadman_config_from_policy(&policy, &[seen("polymarket")]).expect("45 s arms it");
    assert_eq!(cfg.grace, Duration::from_secs(45));
    assert_eq!(cfg.action, vike_core::DeadManAction::CancelAll);
    assert!(!cfg.action.engages_halt());
}

/// The per-venue REPORT distinguishes the FOUR ways a venue can be off, by NAME — the reason
/// it is one line per venue rather than a count. Each arm is asserted on the fact an operator
/// would act on differently.
#[test]
fn the_arming_report_tells_the_four_off_reasons_apart() {
    let venues = [seen("polymarket"), seen("oanda"), seen("hyperliquid"), unseen("binance")];
    let lines = link_deadman_arming_report(&vike_config::Policy::default(), &venues);
    assert_eq!(lines.len(), 4, "one line per mounted venue");
    assert!(lines[0].contains("ARMED for polymarket at 120000 ms"), "{}", lines[0]);
    assert!(lines[1].contains("off for oanda"), "{}", lines[1]);
    assert!(lines[1].contains("weekend close"), "the SESSION reason, verbatim: {}", lines[1]);
    assert!(lines[2].contains("off for hyperliquid"), "{}", lines[2]);
    assert!(
        lines[2].contains("stream_status nowhere"),
        "the RESIDUAL reason — nothing in the policy rows can fix this one: {}",
        lines[2]
    );
    // ⚠ The MOUNT reason: the venue table arms binance, and this daemon still cannot hear it.
    // The line must not say ARMED — that was the false promise this case was added for.
    assert!(lines[3].contains("off for binance"), "{}", lines[3]);
    assert!(
        !lines[3].contains("ARMED"),
        "an unreachable venue may not read as armed: {}",
        lines[3]
    );
    assert!(
        lines[3].contains("THIS DAEMON subscribes no lane"),
        "says whose fault it is — the mount's, not the venue's: {}",
        lines[3]
    );

    // …and the operator's own off-switch is reported as ITS own reason, on every venue —
    // through the one store there is, and there is no `config unset` so the way back is a
    // `config set` of the default rather than "delete the line" (same class as the silence
    // dead-man's own remedy; `crates/vike-config/src/remedy.rs` carries the measurement).
    let off = vike_config::Policy {
        link_deadman_grace_ms: Some(vike_config::LINK_DEADMAN_DISABLED_MS),
        ..vike_config::Policy::default()
    };
    for line in link_deadman_arming_report(&off, &venues) {
        assert!(line.contains("`link_deadman_grace_ms = 0` in the settings database"), "{line}");
        assert!(
            line.contains("Run `vike-cli config set policy.link_deadman_grace_ms 120000`"),
            "{line}"
        );
        assert!(!line.contains("Delete the line"), "{line}");
        assert!(!line.contains("settings/policy.toml"), "{line}");
    }
}

/// **A run profile may not lower this ceiling either** — the twin of
/// `the_run_profile_cannot_touch_the_deadman`, and worth its own test because this switch is
/// ON by default: a `[guards]` table that could reach it would be able to DISARM a protection
/// the operator never had to ask for.
#[test]
fn the_run_profile_cannot_touch_the_link_deadman() {
    let profile = vike_core::RunProfile::from_toml_str(vike_core::run_profile::samples::LIVE_TOML)
        .expect("the shipped live sample parses");
    let mut cfg = vike_core::CoreConfig {
        submit_ack_timeout: Some(Duration::from_secs(7)),
        link_deadman: link_deadman_config_from_policy(
            &vike_config::Policy::default(),
            &[seen("binance")],
        ),
        ..vike_core::CoreConfig::default()
    };
    let _ = profile.apply_guards_and_sinks(&mut cfg);
    assert_eq!(
        cfg.submit_ack_timeout,
        Some(Duration::from_secs(30)),
        "the profile's [guards] must have been APPLIED for this test to prove anything"
    );
    let ldm = cfg.link_deadman.expect("the profile must not have disarmed the link dead-man");
    assert_eq!(
        ldm.grace,
        Duration::from_millis(vike_config::DEFAULT_LINK_DEADMAN_GRACE_MS),
        "…nor moved its grace"
    );
}

/// A configured timeout and the lighter action both arrive: the file's `"cancel_all"` becomes
/// the core's `CancelAll` (the mapping this crate owns because it is the one that sees both
/// types), and the milliseconds are carried verbatim — the file edge already refused what
/// would need clamping.
#[test]
fn a_configured_timeout_and_the_cancel_all_action_reach_the_core_config() {
    let policy = vike_config::Policy {
        deadman_timeout_ms: Some(5_000),
        deadman_action: vike_config::DeadManActionSetting::CancelAll,
        ..vike_config::Policy::default()
    };
    let cfg = deadman_config_from_policy(&policy).expect("5 s arms it");
    assert_eq!(cfg.timeout, Duration::from_secs(5));
    assert_eq!(cfg.action, vike_core::DeadManAction::CancelAll);
    assert!(!cfg.action.engages_halt(), "cancel_all pulls the book and leaves the state alone");
    // …and the mapping is total in the other direction too.
    assert_eq!(
        vike_config::DeadManActionSetting::CancelAllAndHalt.to_core(),
        vike_core::DeadManAction::CancelAllAndHalt
    );
}

/// **A run profile may not lower a policy ceiling.** `apply_guards_and_sinks` runs immediately
/// after the `CoreConfig` literal in `live_mount_with` and overwrites the guards it names; it
/// must have no way to reach `deadman`, or a `[guards]` table — a file with none of
/// the `policy` section's protections — could disarm the switch. Driven through the shipped LIVE
/// sample profile, which DOES name `submit_ack_timeout_ms`, and the assertion on that field is
/// what proves the profile was actually applied rather than ignored: a test where nothing
/// changed would be green for the wrong reason.
#[test]
fn the_run_profile_cannot_touch_the_deadman() {
    let profile = vike_core::RunProfile::from_toml_str(vike_core::run_profile::samples::LIVE_TOML)
        .expect("the shipped live sample parses");
    // A WRITTEN key: the default policy arms nothing now, and a test that started from `None`
    // could not tell "the profile disarmed it" from "it was never armed".
    let armed = vike_config::Policy {
        deadman_timeout_ms: Some(vike_config::RECOMMENDED_DEADMAN_TIMEOUT_MS),
        ..vike_config::Policy::default()
    };
    let mut cfg = vike_core::CoreConfig {
        // Deliberately NOT the sample's 30 s, so the overwrite below is observable.
        submit_ack_timeout: Some(Duration::from_secs(7)),
        deadman: deadman_config_from_policy(&armed),
        ..vike_core::CoreConfig::default()
    };
    let _ = profile.apply_guards_and_sinks(&mut cfg);
    assert_eq!(
        cfg.submit_ack_timeout,
        Some(Duration::from_secs(30)),
        "the profile's [guards] must have been APPLIED for this test to prove anything"
    );
    let dm = cfg.deadman.expect("the profile must not have disarmed the dead-man");
    assert_eq!(dm.timeout, Duration::from_secs(60), "…nor moved its timeout");
    assert_eq!(dm.action, vike_core::DeadManAction::CancelAllAndHalt, "…nor its action");
}

/// …and a policy that DOES name the band reaches the projection the live mount threads into
/// `make_engine`. Pure: the file→`Policy` half is `vike-config`'s own `tests/load.rs`; this pins
/// the daemon's hand-off, which is the half that did not exist before Phase 6c.
#[test]
fn a_configured_band_reaches_the_live_mounts_projection() {
    let policy =
        vike_config::Policy { market_slippage: Some(0.002), ..vike_config::Policy::default() };
    assert_eq!(vike_mount::MountPolicy::from(&policy).market_slippage, Some(0.002));
}

/// A loader warning is SURFACED, never swallowed. `main` emits exactly what
/// [`settings_warning_lines`] returns, so proving a warning survives that step proves the
/// daemon logs it — the failure this guards is the loader resolving something the operator did
/// not write while they believe their own file is in force.
///
/// ⚠ The warning is HAND-STUFFED, and stays so on purpose. The loader HAS a producer again
/// (`vike_config::NO_SETTINGS_DIRECTORY_WARNING`), and driving this through it would test the
/// producer rather than this daemon's half — the `settings: ` prefix and the verbatim text
/// reaching the log. `an_absent_policy_file_is_the_mount_default_and_arms_no_venue` above is
/// where the real producer is asserted at this daemon's edge; this stays a text that no
/// producer emits, so it cannot go green on a coincidence of wording.
#[test]
fn a_loader_warning_is_surfaced_not_swallowed() {
    let mut settings = vike_config::Settings::default();
    settings.warnings.push("preferences.something was resolved to 0.5".to_string());

    let lines = settings_warning_lines(&settings);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with("settings: "), "{}", lines[0]);
    assert!(lines[0].contains("preferences.something"), "names the key: {}", lines[0]);
    assert!(lines[0].contains("0.5"), "carried verbatim, not summarised: {}", lines[0]);
}

/// The quiet path stays quiet: nothing to resolve ⇒ nothing emitted, so a warn line in a
/// daemon's log always means something actually happened.
#[test]
fn a_clean_load_emits_no_settings_lines() {
    assert!(settings_warning_lines(&vike_config::Settings::default()).is_empty());
}

// -- reconcile-on-restart: the S2 default-ON gate and its refusal -------------------------

/// A `flags` settings row for one field, `true`.
fn flag_row(field: &str) -> vike_secrets::StoredSettings {
    vike_secrets::StoredSettings {
        settings: vec![vike_secrets::SettingRow {
            section: "flags".to_string(),
            key: field.to_string(),
            value: "true".to_string(),
        }],
        ..Default::default()
    }
}

/// The reconcile FORCE-ON flag reads the FLAGS ROW, and the environment still wins over it.
///
/// ⚠ **What this flag MEANS changed with S2, and the test name did not, deliberately** — it is
/// still "the reconcile gate reads the row" (a `flags.toml` FILE until `docs/decisions/0086`).
/// `flags.reconcile` is no longer the default answer: a mount that arms a live venue account
/// reconciles without it (see the composed test below). What it still does is force the driver
/// on where the armed-live probe reports nothing, and that is what a `flags` row has to keep
/// being able to do.
///
/// Driven through the REAL `vike_config::load_with_source` over a throwaway settings directory
/// rather than a hand-built `Flags`, because the thing being asserted is the LOADER's
/// precedence, and a hand-built value would assert nothing about it — `load`/`load_with_cli`
/// consult no rows at all any more (`vike_config::load`'s own doc), so this is the lowest-level
/// call that still can. The env keys come from `vike_config`'s own constants so a rename cannot
/// leave this test passing against a name nothing reads.
#[test]
fn the_reconcile_gate_reads_the_flags_row_and_the_env_still_wins() {
    let dir = tempfile::tempdir().expect("temp settings dir");
    let rows = flag_row("reconcile");
    let load_rows = |env: &HashMap<String, String>| {
        vike_config::load_with_source(
            Some(dir.path()),
            vike_config::StoreLayer::Rows { rows: &rows, adopted: None },
            env,
            &vike_config::CliOverrides::default(),
        )
        .unwrap()
    };

    // Neither flag set by default: the gate then rests entirely on the armed-live probe.
    let bare = vike_config::load(None, &HashMap::new()).unwrap();
    assert!(!bare.flags.reconcile, "unset ⇒ no FORCE-on");
    assert!(!bare.flags.reconcile_off, "unset ⇒ not refused — `false` is the guarded state");

    // The ROW arms it. This is the half that did nothing before the wiring.
    let from_row = load_rows(&HashMap::new());
    assert!(from_row.flags.reconcile, "a `flags.reconcile` row must arm the force-on gate");

    // …and the environment still outranks the row, in BOTH directions.
    let off = HashMap::from([(vike_config::flags::RECONCILE_ENV.to_string(), "0".to_string())]);
    let overridden = load_rows(&off);
    assert!(!overridden.flags.reconcile, "env must override a row `true`");

    let on = HashMap::from([(vike_config::flags::RECONCILE_ENV.to_string(), "1".to_string())]);
    assert!(vike_config::load(None, &on).unwrap().flags.reconcile);
}

/// The REFUSAL is reachable from both layers an operator has — a written `flags.reconcile_off`
/// row and a one-run exported variable — because a default-on behaviour that can only be
/// refused from a row cannot be refused at 3am, and one that can only be refused from the
/// environment cannot be refused durably on a box whose unit somebody else owns.
#[test]
fn the_reconcile_refusal_is_reachable_from_the_row_and_from_the_environment() {
    let dir = tempfile::tempdir().expect("temp settings dir");
    let rows = flag_row("reconcile_off");
    assert!(
        vike_config::load_with_source(
            Some(dir.path()),
            vike_config::StoreLayer::Rows { rows: &rows, adopted: None },
            &HashMap::new(),
            &vike_config::CliOverrides::default(),
        )
        .unwrap()
        .flags
        .reconcile_off
    );

    let env = HashMap::from([(vike_config::flags::RECONCILE_OFF_ENV.to_string(), "1".to_string())]);
    assert!(vike_config::load(None, &env).unwrap().flags.reconcile_off);

    // …and the environment can take a deployed refusal back off for one run.
    let back_on =
        HashMap::from([(vike_config::flags::RECONCILE_OFF_ENV.to_string(), "0".to_string())]);
    assert!(
        !vike_config::load_with_source(
            Some(dir.path()),
            vike_config::StoreLayer::Rows { rows: &rows, adopted: None },
            &back_on,
            &vike_config::CliOverrides::default(),
        )
        .unwrap()
        .flags
        .reconcile_off
    );
}

/// **THE S2 property at this daemon's own edge**, composed over the REAL probe this mount uses
/// (`vike_mount::armed_live_venues`) rather than a hand-picked count: a mount that arms a venue
/// account reconciles with NOTHING set, and a mount that arms none does not. The two halves
/// share one credential map and differ only in the arming CEILING, which is the lever an
/// operator actually has.
///
/// Both credential names are BUILT with `format!` fragments and no venue literal, per the note
/// on the credentialed-data tests below: the settings-registry literal sweep reads a whole
/// env-shaped literal as a read sighting and would demand a `SETTINGS` row for this crate.
#[test]
fn a_live_armed_mount_reconciles_with_nothing_set_and_a_paper_one_does_not() {
    let venue = "bybit";
    let prefix = venue.to_uppercase();
    let creds: HashMap<String, String> = HashMap::from([
        (format!("{prefix}_DEMO_API_KEY"), "k".to_string()),
        (format!("{prefix}_DEMO_API_SECRET"), "s".to_string()),
    ]);

    // PAPER ceiling — the shipped default for every venue. Credentials present and ignored.
    let paper = vike_mount::MountPolicy::default();
    let none_armed = vike_mount::armed_live_venues(
        crate::registry::REGISTRY,
        crate::wired_markets::WIRED_MARKETS,
        &creds,
        &paper,
    );
    assert!(none_armed.is_empty(), "a paper ceiling arms nothing: {none_armed:?}");
    assert!(
        !reconcile_config::reconcile_gate(false, false, none_armed.len()).enabled(),
        "a PAPER mount must build no reconcile driver — that is what keeps the default from \
             meaning `every process now talks to a venue`"
    );

    // …the same store under a ceiling that permits the demo tier.
    let armed_policy = vike_mount::MountPolicy {
        venues: vike_config::VenuePolicy::default().declare(venue, vike_config::VenueMode::Demo),
        ..Default::default()
    };
    let armed = vike_mount::armed_live_venues(
        crate::registry::REGISTRY,
        crate::wired_markets::WIRED_MARKETS,
        &creds,
        &armed_policy,
    );
    assert!(armed.contains(&venue.to_string()), "the ceiling must arm {venue}: {armed:?}");
    let gate = reconcile_config::reconcile_gate(false, false, armed.len());
    assert_eq!(gate, reconcile_config::ReconcileGate::LiveDefault);
    assert!(gate.enabled(), "an armed account reconciles with NOTHING set — the S2 default");

    // …and the operator can still refuse it without disarming the venue.
    assert!(!reconcile_config::reconcile_gate(false, true, armed.len()).enabled());
}

/// QUARANTINE-FIRST: with `VIKE_RECONCILE_POLICY` unset the daemon folds in `quarantine`, so
/// even a local-origin divergence (`MissingFill`) is HELD — a default-on live daemon auto-folds
/// NOTHING (CLAUDE.md's rule: `hybrid` auto-applies `PositionDrift` on an incomplete position
/// fetch, rewriting position size and booking realized PnL at the venue's price). Contrast the
/// `hybrid` reference default, which synthesizes it below.
///
/// ⚠ The fold itself now lives in `crate::reconcile_config::quarantine_first_default` (the
/// GUI's live mount needs the same pairing, and was running `hybrid`). This test stays HERE
/// because the claim it makes is about THIS DAEMON's effective policy — the thing an operator
/// reads off `docs/ops/tradehub-the CI box.md` — not about the helper.
#[test]
fn daemon_reconcile_policy_defaults_to_quarantine() {
    use vike_exec::recon::{DivergenceKind, ReconMode};
    let env = reconcile_config::quarantine_first_default(HashMap::new());
    let cfg = reconcile_config::build_recon_config(&env, HashMap::new());
    assert_eq!(cfg.policy.default, ReconMode::Quarantine);
    assert_eq!(cfg.policy.mode_for(DivergenceKind::MissingFill), ReconMode::Quarantine);
    assert!(
        crate::reconcile_config::auto_applied_kinds(&cfg.policy).is_empty(),
        "the daemon's own default must fold NOTHING without an operator"
    );
}

/// An operator who sets `VIKE_RECONCILE_POLICY=hybrid` is honored verbatim — the quarantine-first
/// default only fills an UNSET value.
#[test]
fn daemon_reconcile_policy_honors_explicit_override() {
    use vike_exec::recon::{DivergenceKind, ReconMode};
    let env = reconcile_config::quarantine_first_default(HashMap::from([(
        "VIKE_RECONCILE_POLICY".to_string(),
        "hybrid".to_string(),
    )]));
    let cfg = reconcile_config::build_recon_config(&env, HashMap::new());
    // hybrid auto-synthesizes a local-origin MissingFill (the quarantine default holds it).
    assert_eq!(cfg.policy.mode_for(DivergenceKind::MissingFill), ReconMode::Synthesize);
}

// ---------------------------------------------------------------------------------------------
// The CREDENTIALED-DATA arms (alpaca/ctrader, split-plane I9): the plan gates, the credential
// threading, the interval constraints, and the arming disclosures.
//
// ⚠ Every credential key below is BUILT with `format!` fragments, never spelled as a whole
// `ALPACA_`/`CTRADER_`-prefixed literal: the settings-registry literal sweep
// (`vike_ops::scan::find_map_lookups`) reads a whole env-shaped literal as a read sighting —
// the #1114 shape the aster remedy test above already documents.
// ---------------------------------------------------------------------------------------------

/// The SANDBOX trio, exactly as `vike_alpaca::load_alpaca_config_from(Demo, …)` looks it up.
fn alpaca_vars() -> HashMap<String, String> {
    let tier = vike_alpaca::alpaca_tier(vike_bridge_core::credentials::Environment::Demo);
    ["CLIENT_ID", "CLIENT_SECRET", "ACCOUNT_ID"]
        .iter()
        .map(|k| (format!("ALPACA_{tier}_{k}"), format!("test-{k}")))
        .collect()
}

/// The app pair + DEMO token pair, exactly as `CtraderConfig::from_vars(Demo, …)` looks them up.
fn ctrader_vars() -> HashMap<String, String> {
    let tier = "DEMO";
    let mut vars: HashMap<String, String> = ["CLIENT_ID", "CLIENT_SECRET"]
        .iter()
        .map(|k| (format!("CTRADER_{k}"), format!("app-{k}")))
        .collect();
    vars.insert(format!("CTRADER_{tier}_ACCESS_TOKEN"), "tok".to_string());
    vars.insert(format!("CTRADER_{tier}_REFRESH_TOKEN"), "refresh".to_string());
    vars
}

fn alpaca_cfg() -> MakerMountConfig {
    let symbol = wired_symbol_for("alpaca").expect("build_node mounts alpaca");
    let mut cfg = MakerMountConfig::crypto("alpaca", symbol, 0.01, 1.0);
    cfg.interval = "1m".to_string();
    cfg.interval_ms = 60_000;
    cfg
}

fn ctrader_cfg() -> MakerMountConfig {
    let symbol = wired_symbol_for("ctrader").expect("build_node mounts ctrader");
    let mut cfg = MakerMountConfig::crypto("ctrader", symbol, 0.00001, 1_000.0);
    cfg.interval = "1m".to_string();
    cfg.interval_ms = 60_000;
    cfg
}

/// The hyperliquid mount `venue_feed_plan`'s `("hyperliquid", _)` arm needs — `build_node` hardcodes
/// its market as `"BTC"` (see that arm's own guard), so every hyperliquid fixture in this file
/// shares this one config.
fn hyperliquid_btc_mount() -> MakerMountConfig {
    MakerMountConfig::crypto("hyperliquid", "BTC", 0.5, 0.001)
}

/// Decision 0095: the hyperliquid feed plan follows the ceiling, and a leftover variable in the map
/// decides nothing.
#[test]
fn the_hyperliquid_feed_plan_follows_the_ceiling_not_a_variable() {
    let cfg = hyperliquid_btc_mount();
    let stray = HashMap::from([(concat!("HYPERLIQUID", "_MAINNET").to_string(), "1".to_string())]);
    let policy_at = |mode| vike_mount::MountPolicy {
        venues: vike_config::VenuePolicy::default().declare("hyperliquid", mode),
        ..Default::default()
    };
    let live = policy_at(vike_config::VenueMode::Live);
    let demo = policy_at(vike_config::VenueMode::Demo);
    assert!(matches!(
        venue_feed_plan(&cfg, &HashMap::new(), &live).unwrap(),
        VenuePlan::Hyperliquid(vike_hyperliquid::config::Network::Mainnet)
    ));
    assert!(matches!(
        venue_feed_plan(&cfg, &stray, &demo).unwrap(),
        VenuePlan::Hyperliquid(vike_hyperliquid::config::Network::Testnet)
    ));
}

/// Every credentialed-data slug is joined to the exec plane the same way the CEX slugs are:
/// an engine row in `crate::wired_markets::WIRED_MARKETS` AND an advertised `LIVE_WIRED_VENUES` row.
#[test]
fn the_credentialed_data_venues_name_wired_markets_and_live_wired_rows() {
    for slug in ["alpaca", "ctrader", "oanda"] {
        assert!(
            wired_symbol_for(slug).is_some(),
            "{slug} has a live feed arm but `build_node` mounts no engine for it"
        );
        assert!(
            crate::config::LIVE_WIRED_VENUES.contains(&slug),
            "{slug} has a feed arm but is not advertised in LIVE_WIRED_VENUES"
        );
    }
}

/// **The alpaca plan CARRIES the resolved SANDBOX config** — the same reason
/// `VenuePlan::Cex` carries its mainnet verdict: `vars` is moved into the `NodeConfig` before
/// the feed block runs, so the DATA credentials must travel in the plan. The threading is
/// exec's own loader over exec's own tier, so the two planes cannot resolve differently.
#[test]
fn alpaca_plan_accepts_the_wired_pair_and_carries_the_sandbox_config() {
    match alpaca_plan(&alpaca_cfg(), &alpaca_vars()) {
        Ok(VenuePlan::Alpaca(config)) => {
            assert_eq!(config.account_id, "test-ACCOUNT_ID", "the pinned account travels");
            assert!(
                config.hosts.data_ws.contains("sandbox"),
                "Demo tier ⇒ SANDBOX data hosts (the exec side's own tier): {}",
                config.hosts.data_ws
            );
        }
        other => panic!("the wired alpaca pair with the trio present must plan, got {other:?}"),
    }
}

/// **Absent credentials REFUSE the alpaca mount** — the documented divergence from the exec
/// gate (which degrades to paper): there is no keyless alpaca stream, and a live mount without
/// a feed quotes into the void. The refusal must name the exact trio and the store.
#[test]
fn alpaca_plan_refuses_absent_credentials_naming_the_sandbox_trio() {
    let err = alpaca_plan(&alpaca_cfg(), &HashMap::new())
        .expect_err("no credentials must refuse the live mount, never mount feed-less");
    for needle in ["_CLIENT_ID", "_CLIENT_SECRET", "_ACCOUNT_ID", "SANDBOX", "vike-cli secrets set"]
    {
        assert!(err.contains(needle), "the refusal must name {needle}: {err}");
    }
    // A partial trio is the same refusal — the loader is all-or-nothing.
    let mut partial = alpaca_vars();
    partial.retain(|k, _| !k.ends_with("_ACCOUNT_ID"));
    assert!(alpaca_plan(&alpaca_cfg(), &partial).is_err(), "a partial trio must refuse too");
}

/// The alpaca-shaped per-mount refusals: a foreign symbol (the silent-drop hazard, same as
/// `cex_plan`) and a non-1m interval (the WS serves 1m bars only, and bars drive the live
/// watchdogs — a `5m` mount would run with both dead).
#[test]
fn alpaca_plan_refuses_a_foreign_symbol_and_a_non_1m_interval() {
    let mut foreign = alpaca_cfg();
    let wired = foreign.token_id.clone();
    foreign.token_id = format!("{wired}-NOT-THE-MOUNTED-ONE");
    let err = alpaca_plan(&foreign, &alpaca_vars()).expect_err("foreign symbol");
    assert!(err.contains("SILENTLY DROPPED"), "names the real failure mode: {err}");
    assert!(err.contains(&wired), "names the symbol build_node mounts: {err}");

    let mut five = alpaca_cfg();
    five.interval = "5m".to_string();
    five.interval_ms = 300_000;
    let err = alpaca_plan(&five, &alpaca_vars()).expect_err("non-1m interval");
    assert!(err.contains("1m"), "the refusal must name the one servable interval: {err}");

    let mut bad_tick = alpaca_cfg();
    bad_tick.tick_size = 0.0;
    assert!(
        alpaca_plan(&bad_tick, &alpaca_vars()).expect_err("degenerate tick").contains("tick_size"),
        "the tick refusal names the field"
    );
}

/// **The ctrader plan CARRIES the resolved DEMO config** (same vars-lifetime argument as
/// alpaca's), and the Demo tier resolves the demo HOST — the exec side's own pin.
#[test]
fn ctrader_plan_accepts_the_wired_pair_and_carries_the_demo_config() {
    match ctrader_plan(&ctrader_cfg(), &ctrader_vars()) {
        Ok(VenuePlan::Ctrader(config)) => {
            assert_eq!(
                config.host, "demo.ctraderapi.com",
                "Demo tier ⇒ the demo protobuf host, never live"
            );
            assert_eq!(config.port, vike_ctrader::config::CTRADER_PORT);
            assert_eq!(
                config.account_id, None,
                "no account id in the store ⇒ discovery at connect (never a live default — \
                     `conn.rs`'s NEVER-default-to-live rule)"
            );
        }
        other => {
            panic!("the wired ctrader pair with the tokens present must plan, got {other:?}")
        }
    }
}

/// **Absent credentials REFUSE the ctrader mount**, naming the app pair, the DEMO token pair
/// and the store — same divergence-from-exec argument as alpaca's refusal.
#[test]
fn ctrader_plan_refuses_absent_credentials_naming_the_token_set() {
    let err = ctrader_plan(&ctrader_cfg(), &HashMap::new())
        .expect_err("no credentials must refuse the live mount, never mount feed-less");
    for needle in
        ["_CLIENT_ID", "_CLIENT_SECRET", "_ACCESS_TOKEN", "_REFRESH_TOKEN", "vike-cli secrets set"]
    {
        assert!(err.contains(needle), "the refusal must name {needle}: {err}");
    }
    // The app pair alone (no token pair) is the same refusal — the loader is all-or-nothing.
    let mut app_only = ctrader_vars();
    app_only.retain(|k, _| !k.contains("TOKEN"));
    assert!(
        ctrader_plan(&ctrader_cfg(), &app_only).is_err(),
        "an app registration without an OAuth grant must refuse too"
    );
}

/// The ctrader foreign-symbol refusal — the same silent-drop hazard as every other venue.
#[test]
fn ctrader_plan_refuses_a_foreign_symbol() {
    let mut foreign = ctrader_cfg();
    let wired = foreign.token_id.clone();
    foreign.token_id = format!("{wired}-NOT-THE-MOUNTED-ONE");
    let err = ctrader_plan(&foreign, &ctrader_vars()).expect_err("foreign symbol");
    assert!(err.contains("SILENTLY DROPPED"), "names the real failure mode: {err}");
    assert!(err.contains(&wired), "names the symbol build_node mounts: {err}");
}

/// **Two ctrader mounts must agree on ONE synth interval** — they share one data socket and
/// one `MakerSink` bar synthesizer, so a second interval would silently never fire (the
/// polymarket same-token constraint, worn by the venue that synthesizes its bars).
#[test]
fn ctrader_mounts_must_agree_on_one_synth_interval() {
    let a = ctrader_cfg();
    let mut b = ctrader_cfg();
    b.interval = "5m".to_string();
    b.interval_ms = 300_000;
    let err = check_ctrader_intervals(&[&a, &b]).expect_err("two windows, one synth");
    assert!(
        err.contains("mounts[0]") && err.contains("mounts[1]"),
        "the refusal names both offending rows: {err}"
    );
    // Agreement passes, and other venues' rows never trip it.
    let c = ctrader_cfg();
    assert!(check_ctrader_intervals(&[&a, &c]).is_ok(), "one shared window is fine");
    let hl = hyperliquid_btc_mount();
    assert!(
        check_ctrader_intervals(&[&a, &hl]).is_ok(),
        "a non-ctrader row at any interval is not this gate's business"
    );
}

/// **The arming disclosures state each venue's fixed network and its quote-only requote lane.**
/// Neither venue has a mainnet flag — alpaca is SANDBOX-pinned, ctrader DEMO-pinned, both by
/// `make_engine`'s own tier resolution — and neither serves a book lane, so claiming the CEX
/// `on_order_book` verb here would be the false-lanes claim `CexVenue::quote_source` records.
#[test]
fn alpaca_and_ctrader_arming_disclose_fixed_network_and_quote_only_requote_lane() {
    let alpaca = alpaca_arming(true);
    assert_eq!(alpaca.exec, "LIVE");
    assert_eq!(alpaca.network, "SANDBOX", "alpaca's own tier word, not DEMO");
    assert_eq!(alpaca.requote_lanes, "on_quote_tick", "no book lane exists on this venue");
    assert_eq!(alpaca.remedy, None, "a live mount has nothing to remedy");

    let ctrader = ctrader_arming(true);
    assert_eq!(ctrader.exec, "LIVE");
    assert_eq!(ctrader.network, "DEMO");
    assert_eq!(ctrader.requote_lanes, "on_quote_tick", "no trade/book lane on this venue");
    assert_eq!(ctrader.remedy, None);
}

/// **The two PAPER remedies name their own (different) causes.** Alpaca's paper state should
/// be unreachable (the plan refused absent creds; `spawn` is infallible) — its remedy says
/// "report a bug", never "add a key". Ctrader's IS reachable — a failed SYNCHRONOUS exec
/// connect demotes to paper while the later data connect succeeds — and its remedy is a
/// RESTART, not a key. Neither may advise credentials: the plan gate already proved them
/// present, so key advice here would be unreachable-advice, the defect class
/// `CexArming::remedy` documents.
#[test]
fn the_credentialed_data_paper_remedies_name_their_actual_causes() {
    let alpaca = alpaca_arming(false).remedy.expect("paper must carry a remedy");
    assert!(
        alpaca.contains("bug"),
        "alpaca paper with resolved creds is a gate disagreement to report: {alpaca}"
    );
    let ctrader = ctrader_arming(false).remedy.expect("paper must carry a remedy");
    assert!(
        ctrader.contains("restart") || ctrader.contains("Restart"),
        "ctrader paper means the exec handshake failed; the remedy is a retry: {ctrader}"
    );
    assert!(
        ctrader.contains("SYNCHRONOUSLY"),
        "…and it must say WHY exec can be paper while this very feed is live: {ctrader}"
    );
}

/// **The data-only disclosure replaces ONLY the remedy** ([`data_only_arming`]): the venue
/// facts — network, requote lanes, quote source — stay whatever that venue's own
/// `*_arming(false)` says (never a second copy that can drift), while the remedy names the
/// DECLARATION, the mechanism, and the way back — and stops claiming a bug or a restart, the
/// two ordinary paper causes that are false on the declared path.
#[test]
fn the_data_only_disclosure_names_the_declaration_and_keeps_the_venue_facts() {
    for (venue, base) in [
        ("alpaca", alpaca_arming(false)),
        ("ctrader", ctrader_arming(false)),
        ("oanda", oanda_arming(false)),
        ("ig", ig_arming(false)),
    ] {
        let plain = match venue {
            "alpaca" => alpaca_arming(false),
            "ctrader" => ctrader_arming(false),
            "oanda" => oanda_arming(false),
            _ => ig_arming(false),
        };
        let armed = data_only_arming(base, venue);
        assert_eq!(armed.exec, "PAPER", "{venue}: the declared state IS paper");
        assert_eq!(armed.network, plain.network, "{venue}: network is the venue's own fact");
        assert_eq!(armed.requote_lanes, plain.requote_lanes, "{venue}: lanes untouched");
        assert_eq!(armed.quote_source, plain.quote_source, "{venue}: source untouched");
        let remedy = armed.remedy.expect("a declared data-only mount still discloses WHY");
        for needle in ["data_only = true", "WITHHELD", "BY DECLARATION", venue] {
            assert!(remedy.contains(needle), "{venue}: must carry {needle:?}: {remedy}");
        }
        // The two ORDINARY paper causes, each false on the declared path: the
        // alpaca/oanda/ig gate-disagreement claim and ctrader's handshake-retry advice.
        // (The remedy MAY say "not a bug" — that is the correction, not the claim.)
        for false_claim in ["bug to report", "restart", "Restart"] {
            assert!(
                !remedy.contains(false_claim),
                "{venue}: the declared path must not claim {false_claim:?}: {remedy}"
            );
        }
    }
}

/// **The withhold is the venue's whole `{VENUE}_` key family and nothing else**
/// ([`vike_mount::startup::withhold_venue_credentials`]): every prefixed key goes (exec cannot resolve any tier),
/// every foreign key stays (another venue's mount is untouched), and the count the
/// disclosure logs is the count removed. Every key is spelled through its venue's own naming
/// authority (`vike_oanda::oanda_env_var_names`, `vike_ig::ig_env_var_names` — the
/// `oanda_vars` idiom above) so a key-grid rename reddens here rather than silently testing
/// dead names — and neither a hardcoded `vars.get("…")` literal (which the settings-registry
/// scanner resolves into a demand for a false `SETTINGS` row) nor a
/// `vike_model::credential_keys` builder call (which enrols the whole crate as a
/// generated-key composition site) appears here.
#[test]
fn withhold_venue_credentials_strips_the_venue_prefix_and_nothing_else() {
    let (key_k, acct_k) =
        vike_oanda::oanda_env_var_names(vike_bridge_core::credentials::Environment::Demo);
    // A FOREIGN venue's key (ig, through its own naming authority) and a non-venue-prefixed
    // key: both must survive an oanda withhold untouched.
    let (foreign, _, _) =
        vike_ig::ig_env_var_names(vike_bridge_core::credentials::Environment::Demo);
    let unrelated = "OPERATOR_NOTE".to_string();
    let mut vars = HashMap::from([
        (key_k.clone(), "tok".to_string()),
        (acct_k.clone(), "acct".to_string()),
        (foreign.clone(), "other-venue".to_string()),
        (unrelated.clone(), "kept".to_string()),
    ]);
    let withheld = vike_mount::startup::withhold_venue_credentials(&mut vars, "oanda");
    assert_eq!(withheld, 2, "both oanda keys and only the oanda keys");
    assert!(!vars.contains_key(&key_k) && !vars.contains_key(&acct_k));
    assert!(
        vike_oanda::load_oanda_config_from(vike_bridge_core::credentials::Environment::Demo, &vars)
            .is_none(),
        "the practice loader oanda's mount reaches must now resolve ABSENCE — that absence IS \
             the paper gate the declaration rides"
    );
    assert_eq!(vars.get(&foreign).map(String::as_str), Some("other-venue"));
    assert_eq!(vars.get(&unrelated).map(String::as_str), Some("kept"));
}

/// **The credentialed-data venues contribute NO reconcile feed-status row** — the DECISION
/// documented on `LiveFeeds::recon_feed_statuses`: neither client exposes a status handle, and
/// the exec plane already classifies both as interval-only, never-health-blocked venues.
/// Constructed for real on the alpaca side (`AlpacaDataClient::new` is network-free — lazy
/// connections); ctrader's arm is the same literal `HashMap::new()` but its client cannot be
/// built without a live protobuf handshake, so its half rests on the same match arm this test
/// pins the shape of.
#[test]
fn the_alpaca_feed_contributes_no_recon_health_row() {
    let config = vike_alpaca::AlpacaConfig {
        client_id: "cid".to_string(),
        client_secret: "sec".to_string(),
        account_id: "acct".to_string(),
        env: vike_bridge_core::credentials::Environment::Demo,
        hosts: vike_alpaca::hosts_for(vike_bridge_core::credentials::Environment::Demo),
    };
    let client = vike_alpaca::AlpacaDataClient::new(config, Arc::new(NullSink), || {});
    let feeds = LiveFeeds::Alpaca(client);
    assert!(
        feeds.recon_feed_statuses().is_empty(),
        "no status handle exists on this seam — an invented row could only suppress passes"
    );
}

// ── OANDA: the third credentialed-data arm, and the only one with no socket at all ────────

/// The practice-tier token + account pair, exactly as `load_oanda_config_from(Demo, …)` looks
/// them up (`vike_oanda::oanda_env_var_names` is the naming authority; spelled through it so
/// a rename of the key grid reddens here rather than silently testing dead names).
fn oanda_vars() -> HashMap<String, String> {
    let (key_k, acct_k) =
        vike_oanda::oanda_env_var_names(vike_bridge_core::credentials::Environment::Demo);
    HashMap::from([(key_k, "tok-abc-123".to_string()), (acct_k, "101-004-1234567-001".to_string())])
}

fn oanda_cfg() -> MakerMountConfig {
    let symbol = wired_symbol_for("oanda").expect("build_node mounts oanda");
    let mut cfg = MakerMountConfig::crypto("oanda", symbol, 0.00001, 1_000.0);
    cfg.interval = "1m".to_string();
    cfg.interval_ms = 60_000;
    cfg
}

/// **The oanda plan CARRIES the resolved PRACTICE session** (the same vars-lifetime argument
/// as alpaca's and ctrader's), and the Demo tier resolves the fxPractice hosts on BOTH bases
/// — the REST one the candle poll fetches from and the STREAM one the pricing stream dials.
/// Both travel in the one config, so the two feed lanes cannot end up on different networks.
#[test]
fn oanda_plan_accepts_the_wired_pair_and_carries_the_practice_session() {
    match oanda_plan(&oanda_cfg(), &oanda_vars()) {
        Ok(VenuePlan::Oanda(config)) => {
            assert_eq!(config.account_id, "101-004-1234567-001", "the account travels");
            assert!(
                config.rest_base.contains("fxpractice")
                    && config.stream_base.contains("fxpractice"),
                "Demo tier ⇒ fxPractice on BOTH bases (the exec side's own pin): {} / {}",
                config.rest_base,
                config.stream_base
            );
            assert!(
                !format!("{config:?}").contains("tok-abc-123"),
                "the plan is `Debug`ged by the allow-list refusals — the bearer token must \
                     never be in that output"
            );
        }
        other => panic!("the wired oanda pair with the pair present must plan, got {other:?}"),
    }
}

/// **Absent credentials REFUSE the oanda mount** — the alpaca divergence-from-exec argument,
/// doubled: BOTH of this venue's feed lanes are Bearer-authed, so there is no keyless half to
/// fall back to either. The refusal must name the exact pair and the store.
#[test]
fn oanda_plan_refuses_absent_credentials_naming_the_token_and_account() {
    let err = oanda_plan(&oanda_cfg(), &HashMap::new())
        .expect_err("no credentials must refuse the live mount, never mount feed-less");
    for needle in ["_API_KEY", "_ACCOUNT_ID", "DEMO", "vike-cli secrets set"] {
        assert!(err.contains(needle), "the refusal must name {needle}: {err}");
    }
    // A token with no account id is the same refusal — the loader is all-or-nothing, and a
    // half-configured store is exactly how an operator arrives here.
    let mut partial = oanda_vars();
    partial.retain(|k, _| !k.ends_with("_ACCOUNT_ID"));
    assert!(oanda_plan(&oanda_cfg(), &partial).is_err(), "a lone token must refuse too");
}

/// The oanda-shaped per-mount refusals: a foreign symbol (the silent-drop hazard every venue
/// shares), a degenerate tick size, and — the one that is this venue's own — an interval the
/// candles endpoint has no GRANULARITY code for. The last is DERIVED from
/// `vike_oanda::granularity` in both directions here: a mappable interval must pass and an
/// unmappable one must fail, so the gate cannot drift from the table it reads.
#[test]
fn oanda_plan_refuses_a_foreign_symbol_and_an_unservable_interval() {
    let mut foreign = oanda_cfg();
    let wired = foreign.token_id.clone();
    foreign.token_id = format!("{wired}-NOT-THE-MOUNTED-ONE");
    let err = oanda_plan(&foreign, &oanda_vars()).expect_err("foreign symbol");
    assert!(err.contains("SILENTLY DROPPED"), "names the real failure mode: {err}");
    assert!(err.contains(&wired), "names the symbol build_node mounts: {err}");

    let mut bad_tick = oanda_cfg();
    bad_tick.tick_size = 0.0;
    assert!(
        oanda_plan(&bad_tick, &oanda_vars()).expect_err("degenerate tick").contains("tick_size"),
        "the tick refusal names the field"
    );

    // The venue's own refusal, both directions against the one table.
    let mut unservable = oanda_cfg();
    unservable.interval = "7m".to_string();
    unservable.interval_ms = 420_000;
    assert!(
        vike_oanda::granularity("7m").is_none(),
        "the fixture interval must genuinely be unmappable, or this proves nothing"
    );
    let err = oanda_plan(&unservable, &oanda_vars()).expect_err("unservable interval");
    assert!(err.contains("7m"), "the refusal must name the interval asked for: {err}");
    assert!(
        err.contains("granularity"),
        "…and the table that decided it, so the operator can look up what IS servable: {err}"
    );

    // …and a NON-1m interval the venue DOES serve must pass — this is not alpaca, whose WS
    // serves one bar width. A copy of alpaca's `interval != "1m"` refusal would fail here.
    let mut four_hour = oanda_cfg();
    four_hour.interval = "4h".to_string();
    four_hour.interval_ms = 14_400_000;
    assert!(
        vike_oanda::granularity("4h").is_some() && oanda_plan(&four_hour, &oanda_vars()).is_ok(),
        "oanda serves many granularities; only an UNMAPPABLE interval may be refused"
    );
}

/// **The oanda arming discloses the venue's own network word and its quote-only requote
/// lane.** There is no `OANDA_MAINNET` flag — `make_engine` resolves the Demo tier
/// unconditionally and `oanda_hosts` maps it to fxPractice — and the venue publishes neither a
/// book lane nor a trade tape, so claiming the CEX `on_order_book` verb would be the
/// false-lanes defect. The PAPER remedy is alpaca's "report a bug", not ctrader's "restart":
/// `OandaExecutionClient::spawn` is infallible at mount, so paper-with-resolved-credentials
/// can only be the two gates disagreeing over one map.
#[test]
fn oanda_arming_discloses_the_practice_network_and_a_quote_only_requote_lane() {
    let live = oanda_arming(true);
    assert_eq!(live.exec, "LIVE");
    assert_eq!(live.network, "PRACTICE", "the venue's own word for its non-live environment");
    assert_eq!(live.requote_lanes, "on_quote_tick", "no book lane and no trade tape here");
    assert_eq!(live.remedy, None, "a live mount has nothing to remedy");

    let paper = oanda_arming(false).remedy.expect("paper must carry a remedy");
    assert!(
        paper.contains("bug"),
        "oanda paper with resolved creds is a gate disagreement to report: {paper}"
    );
    assert!(
        !paper.to_lowercase().contains("add "),
        "…and it must NOT advise adding a key: `oanda_plan` already proved the pair present, \
             so key advice here would be the unreachable-advice defect `CexArming::remedy` \
             documents: {paper}"
    );
}

/// **The oanda feed contributes NO reconcile health row either — and this one is a genuine
/// DECISION rather than an absence.** Unlike `AlpacaDataClient`/`CtraderData`, this client DOES
/// expose a `status` handle of exactly the shape the CEX row keys on, so the row is withheld on
/// its merits: one last-writer-wins string is shared by the quote reader and every candle
/// poller, and a transient poll failure would read `Degraded` and suppress a reconcile pass
/// the bar lane has nothing to do with. Constructible for real here — `Feeds::new` is
/// network-free (threads are spawned per subscription, and this feed has none).
#[test]
fn the_oanda_feed_contributes_no_recon_health_row_despite_owning_a_status_handle() {
    let feeds = vike_oanda::market_feed::Feeds::new(Arc::new(NullSink), || {});
    assert!(
        !feeds.status.lock().expect("status").is_empty(),
        "the handle this test is ABOUT must exist and be readable, or the decision below is \
             about nothing"
    );
    let live = LiveFeeds::Oanda(Box::new(feeds));
    assert!(
        live.recon_feed_statuses().is_empty(),
        "the handle exists but is not per-lane evidence — see `recon_feed_statuses`' oanda \
             paragraph for what would earn the row"
    );
}

// ── DERIBIT: the widest feed arm, and the one with NO credential gate ─────────────────────

fn deribit_cfg() -> MakerMountConfig {
    let symbol = wired_symbol_for("deribit").expect("build_node mounts deribit");
    let mut cfg = MakerMountConfig::crypto("deribit", symbol, 0.5, 10.0);
    cfg.interval = "1m".to_string();
    cfg.interval_ms = 60_000;
    cfg
}

/// deribit is joined to the exec plane the same way every other live-wired slug is: an engine
/// row in `crate::wired_markets::WIRED_MARKETS` AND an advertised `LIVE_WIRED_VENUES` row.
#[test]
fn deribit_names_a_wired_market_and_a_live_wired_row() {
    assert!(
        wired_symbol_for("deribit").is_some(),
        "deribit has a live feed arm but `build_node` mounts no engine for it"
    );
    assert!(
        crate::config::LIVE_WIRED_VENUES.contains(&"deribit"),
        "deribit has a feed arm but is not advertised in LIVE_WIRED_VENUES"
    );
}

/// **The deribit plan takes NO credentials and refuses none** — the property that separates
/// this arm from its three credentialed-data neighbours, asserted in the only way that cannot
/// rot: the plan function takes no vars map at all, and `venue_feed_plan` reaches it with an
/// EMPTY one and still succeeds. Its feed is keyless public MAINNET, so absent keys are the
/// ordinary unconfigured state and leave a working feed over a paper book.
#[test]
fn deribit_plan_needs_no_credentials_and_mounts_on_an_empty_store() {
    match deribit_plan(&deribit_cfg()) {
        Ok(VenuePlan::Deribit) => {}
        other => panic!(
            "the wired deribit pair must plan with no credential map in sight, got {other:?}"
        ),
    }
    // …and the venue really is one `venue_feed_plan` reaches with an empty store, which is the
    // end-to-end statement of the same fact (the three credentialed arms cannot do this).
    assert!(
        // The all-paper default policy is inert here — this call exercises the deribit arm, which
        // never reads it.
        venue_feed_plan(&deribit_cfg(), &HashMap::new(), &vike_mount::MountPolicy::default())
            .is_ok(),
        "an empty credential map must still plan a deribit live mount — a keyless feed has no \
             refusal to make, and inventing one would be the failure this arm exists not to copy"
    );
}

/// The deribit-shaped per-mount refusals: a foreign symbol (the silent-drop hazard every venue
/// shares), a degenerate tick size, and — this venue's own — an interval the chart channel has
/// no RESOLUTION code for. The last is DERIVED from `vike_deribit::data::resolution_code` in
/// both directions here: a mappable interval must pass and an unmappable one must fail, so the
/// gate cannot drift from the table it reads.
#[test]
fn deribit_plan_refuses_a_foreign_symbol_and_an_unservable_interval() {
    let mut foreign = deribit_cfg();
    let wired = foreign.token_id.clone();
    foreign.token_id = format!("{wired}-NOT-THE-MOUNTED-ONE");
    let err = deribit_plan(&foreign).expect_err("foreign symbol");
    assert!(err.contains("SILENTLY DROPPED"), "names the real failure mode: {err}");
    assert!(err.contains(&wired), "names the symbol build_node mounts: {err}");

    let mut bad_tick = deribit_cfg();
    bad_tick.tick_size = 0.0;
    assert!(
        deribit_plan(&bad_tick).expect_err("degenerate tick").contains("tick_size"),
        "the tick refusal names the field"
    );

    // The venue's own refusal, both directions against the one table. `4h` is not an arbitrary
    // fixture: the deribit resolution enum genuinely skips it, which is exactly the kind of
    // gap that makes a hand-written list of intervals unsafe here.
    let mut unservable = deribit_cfg();
    unservable.interval = "4h".to_string();
    unservable.interval_ms = 14_400_000;
    assert!(
        vike_deribit::data::resolution_code("4h").is_err(),
        "the fixture interval must genuinely be unmappable, or this proves nothing"
    );
    let err = deribit_plan(&unservable).expect_err("unservable interval");
    assert!(err.contains("4h"), "the refusal must name the interval asked for: {err}");
    assert!(
        err.contains("resolution_code"),
        "…and the table that decided it, so the operator can look up what IS servable: {err}"
    );

    // …and a NON-1m interval the venue DOES serve must pass — this is not alpaca, whose WS
    // serves one bar width. A copy of alpaca's `interval != "1m"` refusal would fail here.
    let mut two_hour = deribit_cfg();
    two_hour.interval = "2h".to_string();
    two_hour.interval_ms = 7_200_000;
    assert!(
        vike_deribit::data::resolution_code("2h").is_ok() && deribit_plan(&two_hour).is_ok(),
        "deribit serves many resolutions; only an UNMAPPABLE interval may be refused"
    );
}

/// **The deribit arming discloses the TESTNET exec network, the MAINNET quote source, and the
/// full requote pair.** All three are facts rather than style: `make_engine` loads the DEMO
/// key tier and the bridge's authed sockets are hardcoded testnet while every public read is
/// hardcoded mainnet, and `venue_caps::DERIBIT` declares `book: true`, which this arm really
/// subscribes. The PAPER remedy is the CEX "add the keys" one — reachable, unlike the
/// credentialed arms' "report a bug" — and must NOT invent a `{VENUE}_MAINNET` flag, which
/// this venue does not have.
#[test]
fn deribit_arming_discloses_testnet_exec_over_mainnet_prices() {
    let live = deribit_arming(true);
    assert_eq!(live.exec, "LIVE");
    assert_eq!(live.network, "TESTNET", "every authed deribit socket is hardcoded testnet");
    assert_eq!(
        live.requote_lanes, "on_quote_tick + on_order_book",
        "this venue declares `book: true` and the arm subscribes the lossless book lane"
    );
    assert!(
        live.quote_source.contains("MAINNET"),
        "the feed reads a DIFFERENT network from exec — a disclosure that hid that would be a \
             half-truth: {}",
        live.quote_source
    );
    assert_eq!(live.remedy, None, "a live mount has nothing to remedy");

    // The caps row is the authority for the lane claim above, read rather than restated.
    let caps = vike_model::caps_for("deribit");
    assert!(caps.live_data.book, "the `on_order_book` half of the claim comes from this row");
    // The venue now serves the conflating DOM lane too (the datahub mounts it, 2026-10-04), which
    // is a fact about the VENUE: this arm still subscribes only the lossless book, because a depth
    // subscription emits `l2_snapshot` into sinks this daemon owns that drop it.
    assert!(caps.live_data.depth, "…and the venue serves the conflating DOM lane as well");

    let paper = deribit_arming(false).remedy.expect("paper must carry a remedy");
    assert!(
        paper.contains("_API_KEY") && paper.contains("_API_SECRET"),
        "the keyless-feed venue's remedy IS reachable and must name the exec keys: {paper}"
    );
    assert!(
        !paper.contains("MAINNET=1"),
        "deribit's network is never the ceiling (its bridge's mount binds testnet, and vike-mount's \
             `the_ceiling_alone_chooses_the_network_and_the_row_says_what_is_missing` pins it) — \
             advising a flag that is read nowhere is the unreachable-advice defect \
             `CexArming::remedy` documents: {paper}"
    );
}

/// **The deribit feed contributes NO reconcile health row** — the oanda decision, one lane
/// count worse. This client exposes a `status` handle of the CEX row's shape, but four lanes
/// share it last-writer-wins, and the book lane writes an error string on every deliberate
/// resync (a chain gap IS a session fault here), so a healthy feed would publish
/// `Degraded`-reading text as ordinary operation and suppress passes. Constructible for real —
/// `Feeds::new` is network-free.
#[test]
fn the_deribit_feed_contributes_no_recon_health_row_despite_owning_a_status_handle() {
    let feeds = vike_deribit::market_feed::Feeds::new(Arc::new(NullSink), || {});
    assert!(
        !feeds.status.lock().expect("status").is_empty(),
        "the handle this test is ABOUT must exist and be readable, or the decision below is \
             about nothing"
    );
    let live = LiveFeeds::Deribit(Box::new(feeds));
    assert!(
        live.recon_feed_statuses().is_empty(),
        "the handle exists but is not per-lane evidence — see `recon_feed_statuses`' deribit \
             paragraph for what would earn the row"
    );
}

// ── IG: the narrowest feed arm — two verbs, and the absence is structural ─────────────────

/// The DEMO trio, exactly as `load_ig_config_from(Demo, …)` looks it up
/// (`vike_ig::ig_env_var_names` is the naming authority; spelled through it so a rename of the
/// key grid reddens here rather than silently testing dead names).
fn ig_vars() -> HashMap<String, String> {
    let (key_k, id_k, pw_k) =
        vike_ig::ig_env_var_names(vike_bridge_core::credentials::Environment::Demo);
    HashMap::from([
        (key_k, "key-xyz".to_string()),
        (id_k, "myuser".to_string()),
        (pw_k, "s3cr3t".to_string()),
    ])
}

fn ig_cfg() -> MakerMountConfig {
    let symbol = wired_symbol_for("ig").expect("build_node mounts ig");
    let mut cfg = MakerMountConfig::crypto("ig", symbol, 0.1, 1.0);
    cfg.interval = "1m".to_string();
    cfg.interval_ms = 60_000;
    cfg
}

/// ig is joined to the exec plane the same way every other live-wired slug is: an engine row
/// in `crate::wired_markets::WIRED_MARKETS` AND an advertised `LIVE_WIRED_VENUES` row.
#[test]
fn ig_names_a_wired_market_and_a_live_wired_row() {
    assert!(
        wired_symbol_for("ig").is_some(),
        "ig has a live feed arm but `build_node` mounts no engine for it"
    );
    assert!(
        crate::config::LIVE_WIRED_VENUES.contains(&"ig"),
        "ig has a feed arm but is not advertised in LIVE_WIRED_VENUES"
    );
}

/// **The ig plan CARRIES the resolved DEMO login** (the same vars-lifetime argument as
/// alpaca's, ctrader's and oanda's — `vars` is moved into the `NodeConfig` before the feed
/// block runs), and the Demo tier resolves the DEMO dealing gateway, which is the base every
/// per-subscription `IgSession::login` will use.
#[test]
fn ig_plan_accepts_the_wired_pair_and_carries_the_demo_session() {
    match ig_plan(&ig_cfg(), &ig_vars()) {
        Ok(VenuePlan::Ig(config)) => {
            assert_eq!(
                config.rest_base,
                vike_ig::ig_rest_base(vike_bridge_core::credentials::Environment::Demo),
                "the Demo tier must resolve the DEMO gateway, read from the venue's own \
                     resolver rather than restated here"
            );
            let dbg = format!("{config:?}");
            for secret in ["key-xyz", "myuser", "s3cr3t"] {
                assert!(
                    !dbg.contains(secret),
                    "the plan is `Debug`ged by the allow-list refusals — no IG secret may be \
                         in that output, and {secret} was: {dbg}"
                );
            }
        }
        other => panic!("the wired ig pair with the trio present must plan, got {other:?}"),
    }
}

/// **Absent credentials REFUSE the ig mount** — the alpaca divergence-from-exec argument:
/// every Lightstreamer subscription opens its own IG login, so there is no keyless half to
/// fall back to. The refusal must name the exact trio and the store. A PARTIAL trio refuses
/// too — the loader is all-or-nothing, and a half-filled store is exactly how an operator
/// arrives here.
#[test]
fn ig_plan_refuses_absent_credentials_naming_the_whole_trio() {
    let err = ig_plan(&ig_cfg(), &HashMap::new())
        .expect_err("no credentials must refuse the live mount, never mount feed-less");
    for needle in ["_API_KEY", "_IDENTIFIER", "_PASSWORD", "DEMO", "vike-cli secrets set"] {
        assert!(err.contains(needle), "the refusal must name {needle}: {err}");
    }
    let mut partial = ig_vars();
    partial.retain(|k, _| !k.ends_with("_PASSWORD"));
    assert!(ig_plan(&ig_cfg(), &partial).is_err(), "a trio missing its password must refuse");
}

/// The ig-shaped per-mount refusals: a foreign symbol (the silent-drop hazard every venue
/// shares), a degenerate tick size, and — this venue's own — an interval Lightstreamer has no
/// chart SCALE for. The last is DERIVED from `vike_ig::market_data::ig_scale` in both
/// directions here: a streamable interval must pass and an unstreamable one must fail, so the
/// gate cannot drift from the table it reads.
#[test]
fn ig_plan_refuses_a_foreign_symbol_and_an_unstreamable_interval() {
    let mut foreign = ig_cfg();
    let wired = foreign.token_id.clone();
    foreign.token_id = format!("{wired}.NOT.THE.MOUNTED.ONE");
    let err = ig_plan(&foreign, &ig_vars()).expect_err("foreign symbol");
    assert!(err.contains("SILENTLY DROPPED"), "names the real failure mode: {err}");
    assert!(err.contains(&wired), "names the epic build_node mounts: {err}");

    let mut bad_tick = ig_cfg();
    bad_tick.tick_size = 0.0;
    assert!(
        ig_plan(&bad_tick, &ig_vars()).expect_err("degenerate tick").contains("tick_size"),
        "the tick refusal names the field"
    );

    // The venue's own refusal, both directions against the one table. `15m` is a width the
    // crate's REST history ladder serves and the STREAMING set does not, which is precisely
    // why the gate reads `ig_scale` rather than "does IG have candles at all".
    let mut unstreamable = ig_cfg();
    unstreamable.interval = "15m".to_string();
    unstreamable.interval_ms = 900_000;
    assert!(
        vike_ig::market_data::ig_scale("15m").is_none(),
        "the fixture interval must genuinely be unstreamable, or this proves nothing"
    );
    let err = ig_plan(&unstreamable, &ig_vars()).expect_err("unstreamable interval");
    assert!(err.contains("15m"), "the refusal must name the interval asked for: {err}");
    assert!(
        err.contains("ig_scale"),
        "…and the table that decided it, so the operator can look up what IS streamed: {err}"
    );

    // …and a NON-1m interval the venue DOES stream must pass — this is not alpaca, whose WS
    // serves one bar width. A copy of alpaca's `interval != "1m"` refusal would fail here.
    let mut five_minute = ig_cfg();
    five_minute.interval = "5m".to_string();
    five_minute.interval_ms = 300_000;
    assert!(
        vike_ig::market_data::ig_scale("5m").is_some() && ig_plan(&five_minute, &ig_vars()).is_ok(),
        "ig streams several scales; only an UNSTREAMABLE interval may be refused"
    );
}

/// **The ig arming discloses the DEMO gateway and a QUOTE-ONLY requote lane, and the second
/// half is structural.** `venue_caps::IG` declares no trade tape and no book because a DEALER
/// venue publishes neither — the same fact that keeps IG DEFERRED in
/// `market_data_conformance.rs` — so claiming the CEX `on_order_book` verb would be the
/// false-lanes defect. The PAPER remedy is alpaca's "report a bug", not ctrader's "restart":
/// `IgExecutionClient::spawn` is infallible at mount, so paper-with-resolved-credentials can
/// only be the two gates disagreeing over one map.
#[test]
fn ig_arming_discloses_the_demo_gateway_and_a_quote_only_requote_lane() {
    let live = ig_arming(true);
    assert_eq!(live.exec, "LIVE");
    assert_eq!(live.network, "DEMO", "the tier and the gateway are the same word here");
    assert_eq!(live.requote_lanes, "on_quote_tick", "no trade tape and no ladder on a dealer");
    assert_eq!(live.remedy, None, "a live mount has nothing to remedy");

    // The caps row is the authority for the lane claim above, read rather than restated — and
    // it is what makes the two absences structural rather than unwired.
    let caps = vike_model::caps_for("ig");
    assert!(caps.live_data.quotes && caps.live_data.bars, "the two verbs this arm subscribes");
    assert!(
        !caps.live_data.trades && !caps.live_data.book && !caps.live_data.depth,
        "…and the three IG structurally cannot serve; widening the arm past this row would \
             turn a venue fact into a mount error"
    );

    let paper = ig_arming(false).remedy.expect("paper must carry a remedy");
    assert!(
        paper.contains("bug"),
        "ig paper with resolved creds is a gate disagreement to report: {paper}"
    );
    assert!(
        !paper.to_lowercase().contains("add "),
        "…and it must NOT advise adding a key: `ig_plan` already proved the trio present, so \
             key advice here would be the unreachable-advice defect `CexArming::remedy` \
             documents: {paper}"
    );
}

/// **The ig feed contributes NO reconcile health row** — the oanda decision once more, and the
/// closed-market case makes it sharpest: a status handle of the CEX row's shape exists, shared
/// last-writer-wins across subscription threads that each hold their own IG session, and FX
/// closes every weekend, so a gate keyed on it would suppress passes on quiet rather than on
/// failure. Constructible for real — `Feeds::new` is network-free (threads are spawned per
/// subscription, and this feed has none).
#[test]
fn the_ig_feed_contributes_no_recon_health_row_despite_owning_a_status_handle() {
    let config =
        vike_ig::load_ig_config_from(vike_bridge_core::credentials::Environment::Demo, &ig_vars())
            .expect("the fixture trio resolves");
    let feeds = vike_ig::market_feed::Feeds::new(Arc::new(NullSink), || {}, config);
    assert!(
        !feeds.status.lock().expect("status").is_empty(),
        "the handle this test is ABOUT must exist and be readable, or the decision below is \
             about nothing"
    );
    let live = LiveFeeds::Ig(feeds);
    assert!(
        live.recon_feed_statuses().is_empty(),
        "the handle exists but is not per-lane evidence — see `recon_feed_statuses`' ig \
             paragraph for what would earn the row"
    );
}

// ---------------------------------------------------------------------------------------------
// `parse_args_from` — the LIVE DAEMON's whole argument surface.
//
// It had no test at all until this section, because `parse_args` read `std::env::args()` and so
// could not be driven. Everything below is the SAME parser the shipped daemon runs; only the
// argv SOURCE is injected.
// ---------------------------------------------------------------------------------------------

/// An argv stream as `parse_args_from` takes it — already `argv[0]`-stripped.
fn argv(v: &[&str]) -> std::vec::IntoIter<String> {
    v.iter().map(|s| (*s).to_string()).collect::<Vec<String>>().into_iter()
}

/// The parsed `Args`, or a panic naming what came back instead — every happy-path case here
/// expects a run rather than help or a version.
fn run_args(v: &[&str]) -> Args {
    match parse_args_from(argv(v)) {
        Ok(Parsed::Args(a)) => a,
        other => panic!("expected a run from {v:?}, got {other:?}"),
    }
}

/// ⚠ **`--config` is RETIRED (0086) and OPTIONAL**: it no longer names anything this binary
/// reads — the ACTIVE daemon-profile ROW does — so a bare invocation, and one that names only
/// `--profile`, must both parse cleanly. `run` is what refuses a bare invocation now, and only
/// when no active daemon-profile row exists either; the PARSER's job ends at "did this argv make
/// sense", which a `--config`-less one always does.
///
/// This test used to be named for the opposite claim (`--config` REQUIRED, its absence refused
/// here) — the exact regression a reader tracing this history should see stated rather than
/// silently swept.
#[test]
fn config_is_optional_and_retired() {
    let bare = run_args(&[]);
    assert_eq!(bare.config_path, None, "a bare invocation must parse — nothing is required");
    assert_eq!(bare.profile_path, None);

    let profile_only = run_args(&["--profile", "run.toml"]);
    assert_eq!(profile_only.config_path, None);
    assert_eq!(profile_only.profile_path.as_deref(), Some("run.toml"));

    // A GIVEN `--config` still parses — and its value is carried, so `run` can warn that it was
    // ignored — it is simply never required.
    assert_eq!(run_args(&["--config", "d.toml"]).config_path.as_deref(), Some("d.toml"));
}

/// Both flags, in BOTH spellings each — `--flag value` and `--flag=value`. The `=` form is what a
/// systemd `ExecStart=` line tends to carry, and this is the only parser in this sweep that
/// accepts it.
#[test]
fn both_flags_parse_in_both_spellings() {
    for spelling in [
        &["--config", "d.toml", "--profile", "r.toml"][..],
        &["--config=d.toml", "--profile=r.toml"][..],
        &["--config=d.toml", "--profile", "r.toml"][..],
        &["--config", "d.toml", "--profile=r.toml"][..],
    ] {
        let a = run_args(spelling);
        assert_eq!(a.config_path.as_deref(), Some("d.toml"), "{spelling:?}");
        assert_eq!(a.profile_path.as_deref(), Some("r.toml"), "{spelling:?}");
    }
    // …and `--profile` is genuinely OPTIONAL: absent it falls through to $VIKE_RUN_PROFILE.
    assert_eq!(run_args(&["--config", "d.toml"]).profile_path, None);
}

/// A value containing `=` survives BOTH spellings — `split_once` takes the FIRST `=` only, so a
/// path like `/etc/vike/a=b.toml` is not truncated either way. Worth pinning because it is the
/// one place the `=` spelling could silently corrupt an operator's path.
#[test]
fn a_value_containing_an_equals_sign_is_not_truncated() {
    assert_eq!(
        run_args(&["--config", "/etc/a=b.toml"]).config_path.as_deref(),
        Some("/etc/a=b.toml")
    );
    assert_eq!(run_args(&["--config=/etc/a=b.toml"]).config_path.as_deref(), Some("/etc/a=b.toml"));
}

/// **`--help` and `--version` are SUCCESSES, not errors** — the regression this file's `Parsed`
/// enum exists to prevent (a non-zero `--help` breaks `set -e`, packaging smoke tests and every
/// wrapper that checks a status). Both spellings of each, and `--help` reached AFTER other
/// arguments, which is how a person actually asks for it.
#[test]
fn help_and_version_are_outcomes_not_errors() {
    for flag in ["-h", "--help"] {
        assert!(matches!(parse_args_from(argv(&[flag])), Ok(Parsed::Help)), "{flag}");
    }
    for flag in ["-V", "--version"] {
        assert!(matches!(parse_args_from(argv(&[flag])), Ok(Parsed::Version)), "{flag}");
    }
    // Asked for after a --config: help still wins, and this is no longer proving anything about
    // a required-flag check — there is none any more — only that `--help` short-circuits
    // regardless of what came before it.
    assert!(matches!(parse_args_from(argv(&["--config", "d.toml", "--help"])), Ok(Parsed::Help)));
    assert!(matches!(parse_args_from(argv(&["--help", "--config"])), Ok(Parsed::Help)));
    // `-V`, never `-v`: lowercase `-v` is verbosity everywhere else on the box, so it stays an
    // unknown argument here rather than silently printing a version.
    assert!(parse_args_from(argv(&["-v"])).is_err(), "-v must not be --version");
}

/// A typo'd flag is REJECTED and NAMED, not ignored — including the `=` spelling of one, which
/// is the form a `systemd` unit line takes.
#[test]
fn an_unknown_argument_is_rejected_and_named() {
    for bad in ["--conf", "--config-path", "--Config", "-c", "d.toml"] {
        let e = parse_args_from(argv(&[bad, "d.toml"])).expect_err("a typo must not run");
        assert!(e.contains(bad), "the error names the offending argument: {e}");
    }
    let inline = parse_args_from(argv(&["--conf=d.toml"])).expect_err("the = spelling too");
    assert!(inline.contains("--conf"), "{inline}");
}

/// A trailing valued flag is an ERROR rather than a silently-defaulted one — the hole every
/// `vike-backfill` bin has and this daemon does not, because both arms `ok_or` instead of
/// letting `it.next()`'s `None` fall through.
#[test]
fn a_trailing_valued_flag_is_an_error_in_both_arms() {
    for flag in ["--config", "--profile"] {
        let e = parse_args_from(argv(&[flag])).expect_err("a trailing valued flag");
        assert!(e.contains(flag), "{e}");
    }
    // …and one with a value already given still errors on the trailing one.
    let e = parse_args_from(argv(&["--config", "d.toml", "--profile"])).expect_err("trailing");
    assert!(e.contains("--profile"), "{e}");
}

/// **A FINDING, now refused — and the point is that the two SPELLINGS agree.** `--config=` with
/// nothing after the `=` used to be ACCEPTED and to yield an EMPTY config path: the inline
/// branch took `split_once`'s right half verbatim and no arm checked it for emptiness. A
/// `systemd` unit whose `ExecStart` interpolates an unset shell variable produces exactly that
/// line. The daemon then failed opening `""`, so it did not trade on a default — but the
/// required-flag check it was supposed to trip had already passed, and the diagnostic an
/// operator got was a file-open error rather than "you gave no --config". The space spelling
/// did NOT have the hole, so the same flag behaved differently depending on how it was written.
///
/// All four ways of writing "no value" are now the same refusal, and each error names the flag:
/// no value at all, an empty inline value, an empty quoted argument (`--config ""`, the OTHER
/// shape an unset `$VIKE_CONFIG` takes), and a whitespace-only one.
#[test]
fn an_empty_value_is_refused_in_both_spellings_exactly_like_a_missing_one() {
    for line in
        [&["--config"][..], &["--config="][..], &["--config", ""][..], &["--config", "   "][..]]
    {
        let e = parse_args_from(argv(line)).expect_err("an empty config path is no config path");
        assert!(e.contains("--config"), "the error names the flag for {line:?}: {e}");
    }
    for line in [
        &["--config", "d.toml", "--profile"][..],
        &["--config", "d.toml", "--profile="][..],
        &["--config", "d.toml", "--profile", ""][..],
    ] {
        let e = parse_args_from(argv(line)).expect_err("…and the optional flag the same way");
        assert!(e.contains("--profile"), "{line:?}: {e}");
    }
    // OMITTING `--profile` is still how you say "no run profile" — it falls through to
    // $VIKE_RUN_PROFILE. Refusing an EMPTY value must not have made the flag mandatory.
    assert_eq!(run_args(&["--config", "d.toml"]).profile_path, None);
}

/// **A FINDING, pinned.** A repeated flag takes the LAST value, silently — so a unit file that
/// gained a second `--config` line (a merge, an override drop-in) runs the daemon on the second
/// profile with nothing said about the first. Pinned because the direction is what an operator
/// appending an override depends on, and because "silently" is the part worth knowing.
#[test]
fn a_repeated_flag_takes_the_last_value_silently() {
    assert_eq!(
        run_args(&["--config", "a.toml", "--config", "b.toml"]).config_path.as_deref(),
        Some("b.toml")
    );
    assert_eq!(
        run_args(&["--config=a.toml", "--config=b.toml"]).config_path.as_deref(),
        Some("b.toml")
    );
}

/// **A FINDING, now refused — and the diagnostic is about the right token.** In the SPACE
/// spelling a valued flag used to consume whatever followed, including another flag:
/// `--config --profile r.toml` yielded the config path `"--profile"` and then died on `r.toml`
/// as an unknown argument, so the message named a token the operator had written correctly.
/// The TRAILING case had nothing left to trip over and parsed CLEANLY with a config path of
/// `--profile`, after which the daemon failed opening a file by that name.
///
/// Both now name the unfed flag AND the flag that would have been eaten, and neither mentions
/// the innocent trailing value.
#[test]
fn a_swallowed_flag_is_refused_and_the_diagnostic_names_both_flags() {
    let e = parse_args_from(argv(&["--config", "--profile", "r.toml"]))
        .expect_err("a flag is not a config path");
    assert!(e.contains("--config") && e.contains("--profile"), "both are named: {e}");
    assert!(!e.contains("r.toml"), "…and the message is no longer about the value: {e}");
    // The trailing case, which used to parse cleanly.
    let tail =
        parse_args_from(argv(&["--config", "--profile"])).expect_err("no longer a config path");
    assert!(tail.contains("--config") && tail.contains("--profile"), "{tail}");
    // The `=` spelling of the same mistake is refused too, so neither form has a hole.
    let inline = parse_args_from(argv(&["--config=--profile"])).expect_err("inline too");
    assert!(inline.contains("--config") && inline.contains("--profile"), "{inline}");
}

// ---------------------------------------------------------------------------------------------
// THE PRECEDENCE SUITE — "the environment beats the file", as a test rather than a doc comment
//
// Every key the unread-settings sweep wired is read by a library several frames below this
// binary, out of a map this binary FILLS. So "which source wins" stopped being a property of
// one `env::var` call and became a property of a fold — and a fold is exactly the kind of thing
// a doc comment can claim and the code can contradict. `docs/decisions/0054` states the
// constraint (one image, many containers, configured by `Environment=` lines); the CI box's live
// reconcile policy and balance-seed flag sit in a root-owned systemd drop-in, and a file value
// that beat those would silently change how a live trading daemon folds position divergences.
// ---------------------------------------------------------------------------------------------

/// The `flags` KEY for a folded variable, from the registry rather than from a second
/// hand-written mapping — `vike_config::flags::FLAG_REGISTRY` is the authority that the field
/// and the variable belong together, and `crates/vike-config/tests/flag_registry.rs` gates it
/// both ways.
fn flags_key_for(var: &str) -> &'static str {
    vike_config::flags::FLAG_REGISTRY
        .iter()
        .find(|m| m.env == var)
        .unwrap_or_else(|| panic!("{var} is folded but has no FLAG_REGISTRY row"))
        .field
}

/// A settings tree and a `flags` settings row for one key. `docs/decisions/0086`: there is no
/// `flags.toml` for it to be a line of any more — `vike_config::load`/`load_with_cli` consult no
/// rows at all, so every caller below drives `load_with_source` with the row directly.
fn settings_dir_with_flag(
    key: &str,
    value: bool,
) -> (tempfile::TempDir, vike_secrets::StoredSettings) {
    let dir = tempfile::tempdir().expect("a temp settings dir");
    let rows = vike_secrets::StoredSettings {
        settings: vec![vike_secrets::SettingRow {
            section: "flags".to_string(),
            key: key.to_string(),
            value: value.to_string(),
        }],
        ..Default::default()
    };
    (dir, rows)
}

/// [`vike_config::load_with_source`] with `rows` as the settings database's answer — the one
/// call that still consults them.
fn load_flag_row(
    dir: &tempfile::TempDir,
    rows: &vike_secrets::StoredSettings,
    env: &HashMap<String, String>,
) -> vike_config::Settings {
    vike_config::load_with_source(
        Some(dir.path()),
        vike_config::StoreLayer::Rows { rows, adopted: None },
        env,
        &vike_config::CliOverrides::default(),
    )
    .unwrap_or_else(|e| panic!("the settings must load: {e}"))
}

/// The resolved value of ONE folded row, read back out of the same table the fold iterates.
fn resolved_row(flags: vike_config::Flags, var: &str) -> bool {
    folded_flag_rows(flags)
        .into_iter()
        .find(|(n, _, _)| *n == var)
        .map(|(_, on, _)| on)
        .expect("the row is in the table it came from")
}

/// **THE BLOCKER'S REGRESSION TEST, and the MAJOR'S, table-driven over every folded key.**
///
/// For each key: its `flags` row says `true`, the process environment says `"0"`, and the
/// credential store — the third source nobody modelled — says `"1"`. The environment must win.
///
/// It drives the REAL `vike_config::load`, so the file-then-env half is the shipped resolution
/// and not a re-implementation, and then the REAL fold. Every key is `FoldTier::Resolved`: the
/// value is OVERWRITTEN, so the map carries `"0"` and the reader downstream sees the
/// environment's answer — including for `flags.preflight_skip`, a SAFETY OVERRIDE, where a store
/// line standing would be widening one. (It asserted a second `FoldTier` variant,
/// `CredentialStoreFirst` — the fold mode that left a credential line standing — for the two
/// Polymarket gates until decision 0095's review closed it.) A flag whose variable decision
/// 0095 retired has no environment layer and is skipped below —
/// `a_retired_flag_variable_is_refused_not_folded` and
/// `no_credential_store_line_survives_the_fold_for_any_key` cover those.
#[test]
fn a_process_env_value_beats_a_file_value_for_every_wired_key() {
    for (var, _, FoldTier::Resolved) in folded_flag_rows(vike_config::Flags::default()) {
        // Decision 0095: a flag whose variable was RETIRED has no environment value to lose to — the
        // variable refuses startup instead; `a_retired_flag_variable_is_refused_not_folded` pins it.
        if !vike_config::flags::FLAG_REGISTRY.iter().any(|m| m.env == var && m.reads_env()) {
            continue;
        }
        let key = flags_key_for(var);
        let (dir, rows) = settings_dir_with_flag(key, true);
        let env = HashMap::from([(var.to_string(), "0".to_string())]);
        let settings = load_flag_row(&dir, &rows, &env);

        // Layer one: the environment beat the row inside `vike_config::load_with_source`.
        assert!(!resolved_row(settings.flags, var), "{var}=0 must beat a `{key} = true` row");

        // Layer two: the fold did not hand it back. `vars` carries the hostile credential line.
        let mut vars = HashMap::from([(var.to_string(), "1".to_string())]);
        fold_flags_into_vars(settings.flags, &mut vars);
        assert_eq!(
            vars.get(var).map(String::as_str),
            Some("0"),
            "{var}: an exported `0` must survive the fold — a `1` here is a credential file \
             outranking the environment on a key whose reader never had that tier"
        );
    }
}

/// Decision 0095: a folded flag whose variable was RETIRED has NO environment layer — a set
/// variable refuses startup instead of folding, and the row alone reaches the map. Derived from the
/// fold's own table and `FlagMeta::reads_env`, so a flag retired later is covered the day it is.
#[test]
fn a_retired_flag_variable_is_refused_not_folded() {
    let retired: Vec<&str> = folded_flag_rows(vike_config::Flags::default())
        .into_iter()
        .map(|(var, _, _)| var)
        .filter(|var| {
            !vike_config::flags::FLAG_REGISTRY.iter().any(|m| m.env == *var && m.reads_env())
        })
        .collect();
    // Anti-vacuity: the Polymarket gates were the first retired, so an empty set means the
    // derivation broke, not that nothing is retired.
    assert!(retired.contains(&vike_config::flags::POLY_EXEC_ENV), "{retired:?}");
    for var in retired {
        let env = HashMap::from([(var.to_string(), "1".to_string())]);
        assert!(vike_config::refuse_removed_env(&env).is_err(), "{var}=1 must refuse startup");
        let key = flags_key_for(var);
        let (dir, rows) = settings_dir_with_flag(key, false);
        let settings = load_flag_row(&dir, &rows, &env);
        assert!(!resolved_row(settings.flags, var), "{var}=1 must not arm `{key}`");
    }
}

/// The wiring still WORKS — the half a "make the environment win" change could break by
/// over-correcting. Nothing exported, its `flags` row says `true`, and every folded key reaches
/// the map as `"1"`.
#[test]
fn a_file_value_reaches_the_map_when_the_environment_is_silent() {
    for (var, _, _) in folded_flag_rows(vike_config::Flags::default()) {
        let key = flags_key_for(var);
        let (dir, rows) = settings_dir_with_flag(key, true);
        let settings = load_flag_row(&dir, &rows, &HashMap::new());
        let mut vars = HashMap::new();
        fold_flags_into_vars(settings.flags, &mut vars);
        assert_eq!(
            vars.get(var).map(String::as_str),
            Some("1"),
            "{var}: a `{key} = true` row must reach the map — that IS the wiring"
        );
    }
}

/// The one SAFETY OVERRIDE that keeps an environment layer — `flags.preflight_skip` — carried to
/// the map: exported `=0` plus a `true` row must resolve to REFUSE. (The withdraw override lost its
/// environment layer to decision 0095; `the_withdraw_override_is_the_row_alone` is its test.)
///
/// It asserts the exact string, because `vike_mount::startup` lives in a crate this one does not
/// depend on directly — that side of the chain is proved in
/// `crates/vike-mount/src/preflight_skip_precedence_tests.rs`'s
/// `the_preflight_is_not_skipped_when_the_environment_says_zero`, which takes this `"0"` as
/// its input.
#[test]
fn a_safety_override_refuses_when_the_environment_says_zero_and_the_file_says_true() {
    let var = vike_config::flags::PREFLIGHT_SKIP_ENV;
    let key = flags_key_for(var);
    let (dir, rows) = settings_dir_with_flag(key, true);
    let env = HashMap::from([(var.to_string(), "0".to_string())]);
    let settings = load_flag_row(&dir, &rows, &env);
    // The hostile credential line again: this is the shape that armed a live-money gate.
    let mut vars = HashMap::from([(var.to_string(), "1".to_string())]);
    fold_flags_into_vars(settings.flags, &mut vars);
    assert_eq!(
        vars.get(var).map(String::as_str),
        Some("0"),
        "{var} must be disarmed in the map the reader consults"
    );
}

/// Decision 0095: the withdraw override has no environment layer — `flags.allow_withdraw_keys` is
/// its only source. The fold writes the row's answer OVER whatever the credential store carried
/// (`FoldTier::Resolved`), so a stale store line cannot arm it, an exported variable changes
/// nothing here (it refuses startup instead), and the mount-path reader sees exactly the row.
#[test]
fn the_withdraw_override_is_the_row_alone() {
    let var = vike_config::flags::ALLOW_WITHDRAW_KEYS_ENV;
    let exported = HashMap::from([(var.to_string(), "0".to_string())]);
    assert!(vike_config::refuse_removed_env(&exported).is_err(), "{var} must refuse startup");
    for row in [true, false] {
        let (dir, rows) = settings_dir_with_flag(flags_key_for(var), row);
        let settings = load_flag_row(&dir, &rows, &exported);
        let mut vars = HashMap::from([(var.to_string(), "1".to_string())]);
        fold_flags_into_vars(settings.flags, &mut vars);
        assert_eq!(
            vike_bridge_core::key_permissions::allow_withdraw_keys(&vars),
            row,
            "row = {row}: the fold must carry the row, never the store's `1` or the export's `0`"
        );
    }
}

/// **The resolved flag is the SOLE source of every folded key — a credential-store line of ANY
/// spelling neither survives the fold nor changes what it writes.**
///
/// This replaced `the_credential_store_tier_is_exactly_the_refused_arming_set`, which held that
/// exactly the keys whose store line is "refused at startup" may keep the store as a tier. That
/// pairing was the bug (decision 0095's review): the refusal's value grammar is narrower than the
/// readers', so a `1 x` row armed real-money Polymarket exec over `flags.poly_exec = false` without
/// tripping it. The property worth holding is the one that does not depend on the refusal at all —
/// every hostile spelling, over both a false and a true flag, ends up as the flag's own value.
#[test]
fn no_credential_store_line_survives_the_fold_for_any_key() {
    let all_on = vike_config::Flags {
        poly_exec: true,
        poly_reconcile: true,
        hyperliquid_hip3: true,
        record_properties: true,
        allow_withdraw_keys: true,
        preflight_skip: true,
        ..Default::default()
    };
    for flags in [vike_config::Flags::default(), all_on] {
        for (var, resolved, FoldTier::Resolved) in folded_flag_rows(flags) {
            for hostile in ["1", "0", "1 x", "1 # arm it", "true", "garbage", ""] {
                let mut vars = HashMap::from([(var.to_string(), hostile.to_string())]);
                fold_flags_into_vars(flags, &mut vars);
                assert_eq!(
                    vars.get(var).map(String::as_str),
                    Some(flag_wire_value(resolved).as_str()),
                    "{var}: a credential line `{hostile}` must not survive the fold — the \
                     resolved flag ({resolved}) is the only thing that may reach the reader"
                );
            }
        }
    }
}

/// **A credential row can neither ARM nor VETO the two Polymarket gates** — the exposure decision
/// 0095's review found: `POLY_EXEC=1 x` passes `vike_config::refuse_credential_file_arming` (its
/// grammar is "the text before `#`, trimmed, is exactly `1`") yet the reader takes the first token,
/// so the row armed real-money exec over `flags.poly_exec = false`; and a `POLY_EXEC=0` row
/// silently vetoed a true flag. With `FoldTier::Resolved` the map carries the flag's value whatever
/// the store held.
#[test]
fn a_credential_row_can_neither_arm_nor_veto_the_polymarket_gates() {
    use vike_config::flags::{POLY_EXEC_ENV, POLY_RECONCILE_ENV};
    let only = |var: &str, on: bool| match var {
        POLY_EXEC_ENV => vike_config::Flags { poly_exec: on, ..Default::default() },
        POLY_RECONCILE_ENV => vike_config::Flags { poly_reconcile: on, ..Default::default() },
        other => panic!("not a Polymarket gate: {other}"),
    };
    for var in [POLY_EXEC_ENV, POLY_RECONCILE_ENV] {
        // Flag OFF, and a row that ARMS in the reader: the map must say off.
        for arming in ["1", "1 x", "1 # arm it"] {
            let mut vars = HashMap::from([(var.to_string(), arming.to_string())]);
            fold_flags_into_vars(only(var, false), &mut vars);
            assert_eq!(
                vars.get(var).map(String::as_str),
                Some("0"),
                "{var}: a credential row `{arming}` armed a gate whose flag is off"
            );
        }
        // Flag ON, and a row that would VETO: the flag arms it.
        let mut vars = HashMap::from([(var.to_string(), "0".to_string())]);
        fold_flags_into_vars(only(var, true), &mut vars);
        assert_eq!(
            vars.get(var).map(String::as_str),
            Some("1"),
            "{var}: a credential row `0` vetoed a flag that is on"
        );
    }
}

/// The same, through the REAL readers the mount consults — and the sanity half that keeps the test
/// honest: the raw `1 x` row DOES arm the reader and DOES slip the boot refusal, so this fails if
/// the exposure it closes ever stops being real (and the fix stops being the thing under test).
#[cfg(feature = "polymarket")]
#[test]
fn the_polymarket_readers_see_the_flag_and_not_the_credential_row() {
    use vike_config::flags::{POLY_EXEC_ENV, POLY_RECONCILE_ENV};
    let sneaky = HashMap::from([
        (POLY_EXEC_ENV.to_string(), "1 x".to_string()),
        (POLY_RECONCILE_ENV.to_string(), "1 x".to_string()),
    ]);
    assert!(vike_polymarket::poly_exec_enabled(&sneaky), "the raw row arms the exec gate");
    assert!(vike_polymarket::poly_reconcile_enabled(&sneaky), "…and the reconcile gate");
    assert!(
        vike_config::refuse_credential_file_arming(&sneaky).is_ok(),
        "…while the boot refusal's narrower grammar lets it through — the hole"
    );

    let mut vars = sneaky.clone();
    fold_flags_into_vars(vike_config::Flags::default(), &mut vars);
    assert!(!vike_polymarket::poly_exec_enabled(&vars), "a false flag disarms the exec gate");
    assert!(!vike_polymarket::poly_reconcile_enabled(&vars), "…and the reconcile gate");

    let mut vars = HashMap::from([
        (POLY_EXEC_ENV.to_string(), "0".to_string()),
        (POLY_RECONCILE_ENV.to_string(), "0".to_string()),
    ]);
    let on = vike_config::Flags { poly_exec: true, poly_reconcile: true, ..Default::default() };
    fold_flags_into_vars(on, &mut vars);
    assert!(vike_polymarket::poly_exec_enabled(&vars), "a true flag arms it over a `0` row");
    assert!(vike_polymarket::poly_reconcile_enabled(&vars), "…and the reconcile gate");
}

/// **The `data_only` disclosure, argued rather than assumed.** The fold runs BEFORE
/// `withhold_venue_credentials`, which strips `vars` by `{VENUE}_` prefix and reports the count
/// in the operator-facing warning — so a folded key whose NAME began with an eligible venue's
/// prefix would be counted as one more withheld CREDENTIAL than the store ever held.
///
/// None does, today: the eligible set is `crate::config::DATA_ONLY_VENUES`
/// (alpaca/ctrader/ig/oanda — a `data_only` hyperliquid or polymarket mount is refused at
/// profile load), and no folded key starts with any of those prefixes. This pins it from BOTH
/// tables, so either a new folded key or a new eligible venue reddens here instead of quietly
/// making a disclosure line wrong.
#[test]
fn no_folded_flag_key_collides_with_a_data_only_venue_prefix() {
    for (var, _, _) in folded_flag_rows(vike_config::Flags::default()) {
        for venue in crate::config::DATA_ONLY_VENUES {
            let prefix = format!("{}_", venue.to_uppercase());
            assert!(
                !var.starts_with(&prefix),
                "{var} starts with `{prefix}`, so a `data_only = true` {venue} mount would \
                     strip it and COUNT it as a withheld credential in the startup disclosure — \
                     fold it after the withhold, or rename the key"
            );
        }
    }
}

/// The reconcile family's own precedence, which is a property of the BASE MAP rather than of a
/// tier: `daemon_recon_env_from` starts from the process env, so an exported value is already
/// present and the `or_insert` cannot replace it.
#[test]
fn the_process_env_beats_the_resolved_flag_in_the_reconcile_family() {
    let on = vike_config::Flags {
        reconcile_balance: true,
        reconcile_generate_missing: true,
        ..vike_config::Flags::default()
    };
    for var in [
        vike_config::flags::RECONCILE_BALANCE_ENV,
        vike_config::flags::RECONCILE_GENERATE_MISSING_ENV,
    ] {
        // The unit file says `0` and the resolved flag says `true` — which cannot actually
        // happen through `vike_config::load` (the env layer would have made it false), and is
        // asserted anyway: this function must not be the place that could re-widen it.
        let exported = HashMap::from([(var.to_string(), "0".to_string())]);
        let env = daemon_recon_env_from(on, exported);
        assert_eq!(
            env.get(var).map(String::as_str),
            Some("0"),
            "{var}: an `Environment=` line in a systemd drop-in must survive the fold"
        );
        // ...and with nothing exported, the file's `true` reaches the family's one map.
        let from_file = daemon_recon_env_from(on, HashMap::new());
        assert_eq!(from_file.get(var).map(String::as_str), Some("1"), "{var}: the wiring works");
    }
}

/// `config.journal_dir`, the one wired key that is not a flag: an exported `VIKE_JOURNAL_DIR`
/// beats `config.journal_dir`, and the setting's value lands only where the variable is absent.
#[test]
fn the_process_env_beats_config_journal_dir() {
    let from_file = std::path::Path::new("/srv/from-config-toml");
    let exported = HashMap::from([(
        vike_config::config::JOURNAL_DIR_ENV.to_string(),
        "/srv/from-the-unit-file".to_string(),
    )]);
    let vars = journal_vars_from(Some(from_file), exported);
    assert_eq!(
        vars.get(vike_config::config::JOURNAL_DIR_ENV).map(String::as_str),
        Some("/srv/from-the-unit-file"),
        "an `Environment=VIKE_JOURNAL_DIR=...` line beats config.journal_dir"
    );
    // ...and the file reaches an otherwise-silent environment, which is the wiring itself.
    let vars = journal_vars_from(Some(from_file), HashMap::new());
    assert_eq!(
        vars.get(vike_config::config::JOURNAL_DIR_ENV).map(String::as_str),
        Some("/srv/from-config-toml")
    );
    // No key at all when neither source answers — `journal_config_from` then returns `None`,
    // the byte-identical WAL-off default.
    let vars = journal_vars_from(None, HashMap::new());
    assert!(!vars.contains_key(vike_config::config::JOURNAL_DIR_ENV));
}
