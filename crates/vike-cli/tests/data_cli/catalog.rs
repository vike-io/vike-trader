//! `data catalog`: the group's help, the capability matrix and the recorded instrument grid.

use std::net::TcpListener;

use vike_model::AssetClass;

use super::support::{
    PREMISE_ATTEMPTS, is_listening, spawn_class_probe_datahub, spawn_seeded_datahub,
};
use super::*;

/// The group ANSWERS now, and its help is the only place its four verbs are named.
///
/// ⚠ This is also the case that would catch `catalog` being routed back to the unbuilt-group stub:
/// that path exits on the USAGE rung with "designed but not built yet", and this asserts the
/// opposite of both halves.
///
/// ⚠ **The verb check is over the ROW, not over the word, and that is a correction.** It asserted
/// `text.contains(verb)` and could not fail for the reason it names: every verb name also occurs
/// in the page's own PROSE (`--venue V   ls/refresh:`, "`data catalog venues` is the roster",
/// "an instrument `ls` lists"), so deleting a verb's whole usage block left this green with the
/// verb undiscoverable from `--help` — measured in the unit suite beside
/// `crates/vike-cli/src/cmd/data/catalog/grammar.rs`'s `usage`, where `ls` occurred 9 times and `refresh`
/// twice outside their own blocks. A usage BLOCK is the only line that opens at column 2 with the
/// verb and carries text after it.
#[test]
fn the_catalog_group_answers_and_its_help_names_every_verb() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let out = run(scratch.path(), &["data", "catalog", "--help"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    for verb in ["ls", "show", "refresh", "venues"] {
        let head = format!("  {verb} ");
        let has_block = text.lines().any(|l| match l.strip_prefix(&head) {
            Some(rest) => !rest.trim().is_empty(),
            None => false,
        });
        assert!(has_block, "`data catalog --help` must carry `{verb}`'s own block: {text}");
    }
    assert!(!text.contains("designed but not built"), "this group is built: {text}");

    // ...and a bare group names its roster rather than doing something by default.
    let bare = run(scratch.path(), &["data", "catalog"]);
    assert_eq!(bare.status.code(), Some(2), "a verb is REQUIRED: {bare:?}");
    let err = stderr(&bare);
    for verb in ["ls", "show", "refresh", "venues"] {
        assert!(err.contains(verb), "the refusal must name `{verb}`: {err}");
    }
    assert!(!err.contains("designed but not built"), "{err}");
}

/// **§8.1's load-bearing requirement, as a spawned process: the matrix answers with NO datahub.**
///
/// A box that cannot reach one is exactly the box whose operator needs to know what this build can
/// do, so an unreachable server is REPORTED and the verb still exits 0 with every roster venue on
/// stdout. The roster is read from `vike_model::VENUES` here rather than typed, for the same reason
/// the module derives it: a venue added to the tree must appear with no edit to this file.
///
/// ⚠ The address is a port this test BOUND and then released, and the premise is MEASURED on both
/// sides of the run — the mechanism (and the incident) [`an_unreachable_datahub_is_the_connect_rung`]
/// carries. Here it guards only the "NOT REACHED" half: the exit code and the venue rows are
/// asserted unconditionally, because `venues` owes 0 and a full local matrix whoever is on that
/// port.
#[test]
fn the_capability_matrix_answers_with_no_datahub_reachable() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let mut rerolled: Vec<String> = Vec::new();

    for _ in 0..PREMISE_ATTEMPTS {
        let addr = {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
            listener.local_addr().expect("resolve assigned port")
        };
        let text = addr.to_string();
        if is_listening(&addr) {
            rerolled.push(format!("{text} was taken before the run"));
            continue;
        }
        let out = run(scratch.path(), &["data", "catalog", "venues", "--addr", &text]);
        let body = stdout(&out);
        // Unconditional: whoever holds that port, this verb owes a local matrix and a zero.
        assert!(out.status.success(), "`venues` must not fail on an absent datahub: {out:?}");
        for venue in vike_model::VENUES {
            assert!(body.contains(venue), "every roster venue must be rendered: {body}");
        }
        if is_listening(&addr) {
            rerolled.push(format!("{text} was taken while the run happened"));
            continue;
        }
        // Both probes refused, so the address was closed across the whole run and the server half
        // of the rendering is the CLI's own answer to an absent datahub.
        assert!(body.contains("NOT REACHED"), "the missing half must be SAID: {body}");
        assert!(
            body.contains("unasked, which is not the same as unserved"),
            "an unasked column may not be reported as a refusal: {body}"
        );
        assert!(!body.contains("not served"), "nothing may claim that server refused: {body}");
        return;
    }

    panic!(
        "the premise never held: {PREMISE_ATTEMPTS} freshly-bound ephemeral ports were each taken \
         by another process before this case could prove anything about them — {rerolled:?}"
    );
}

/// The matrix against a REAL datahub keeps the two sources APART, which is the whole of §8.1.
///
/// The seeded server is `vike_datahub::serve`, which mounts no catalog lane and no market-data hub
/// — so it advertises neither `venue_catalog` nor any `md_venue=` entry. That is not a degenerate
/// fixture, it is the commonest deployment: a data daemon serving store reads. What it proves is
/// that the build's columns survive a server that serves none of them, which is the failure mode an
/// operator cannot otherwise see.
#[test]
fn the_capability_matrix_separates_this_build_from_the_server_that_answered() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();
    let out = run(scratch.path(), &["data", "catalog", "venues", "--addr", &addr, "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out))
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {}", stdout(&out)));

    assert_eq!(doc["group"], "catalog");
    assert_eq!(doc["verb"], "venues");
    // The BUILD half: every roster venue, from this binary's own tables.
    let venues = doc["build"]["venues"].as_array().expect("the build's rows");
    assert_eq!(venues.len(), vike_model::VENUES.len());
    // The SERVER half: a real handshake, kept in its own object. ⚠ `state` is a THREE-token field
    // where this read `reachable`, a boolean — a server that answers the handshake and then
    // REFUSES the connection (a PROTO_VERSION skew, a key it will not take) was reached, and
    // `"reachable": false` said the opposite. See `ServerView::state`.
    assert_eq!(doc["server"]["state"], "answered");
    assert!(doc["server"]["features"].is_array(), "a reachable server carries its features: {doc}");
    assert_eq!(
        doc["server"]["serves_venue_catalog"], false,
        "this fixture mounts no catalog lane, and the document must say so rather than omit it"
    );
    // ...and the DIFFERENCE, named rather than folded into either half. This server advertises no
    // `md_venue=` entry at all, so every venue this build declares a live feed for is skew.
    let declared = doc["skew"]["declared_here_unserved_there"].as_array().expect("the skew");
    assert!(!declared.is_empty(), "this build declares live feeds nobody here serves: {doc}");
    // The anti-vacuity control: the build's own live-lane column is UNTOUCHED by that skew, which
    // is the merge §8.1 forbids. A venue named in the skew still carries its lanes above it.
    let skewed = declared[0].as_str().expect("a venue slug");
    let row = venues
        .iter()
        .find(|v| v["venue"] == skewed)
        .unwrap_or_else(|| panic!("{skewed} must have a build row: {doc}"));
    assert!(
        !row["live_lanes"].as_array().expect("lanes").is_empty(),
        "the server's `no` must not empty this build's column: {row}"
    );

    // ...and the same run in TABLE mode, for the sentence the document does not carry.
    //
    // ⚠ That sentence used to end `a `data realtime watch` on one of them is refused by the
    // server, not by this binary`. When it was struck, both halves were false because that group
    // had no route at all — it was refused on this binary's own usage rung. `data realtime watch`
    // SHIPS now, so the first half is true and the second is still false: this fixture's datahub
    // advertises no market-data plane, so `DatahubClient::md_subscribe` refuses on the capability
    // LOCALLY, "nothing was sent", and the refusal the sentence attributed to the server never
    // reaches it. The one verb whose product is keeping "which side said no" legible would have
    // been naming the wrong side. This is the case that catches it coming back, end to end.
    let table = run(scratch.path(), &["data", "catalog", "venues", "--addr", &addr]);
    assert!(table.status.success(), "{}", stderr(&table));
    let body = stdout(&table);
    assert!(body.contains("SKEW"), "this fixture serves no md_venue, so there IS a skew: {body}");
    assert!(
        !body.contains("data realtime"),
        "the skew line may not attribute a refusal to a side that did not make it: {body}"
    );
}

/// `ls` against a datahub that mounts no catalog lane says WHICH side refused, and does it without
/// sending anything.
///
/// ⚠ The rung is the run-failure one rather than connect-class: the socket was fine and the
/// capability was missing, which is a served answer. `crates/vike-cli/src/cmd/data/shared.rs`'s `connect`
/// is where that split is made and this is the case that would catch it moving.
#[test]
fn a_listing_against_a_server_with_no_catalog_lane_names_the_missing_capability() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();
    let out =
        run(scratch.path(), &["data", "catalog", "ls", "--venue", "binance", "--addr", &addr]);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "a server that ANSWERED is the run-failure rung: {err}");
    assert!(err.contains("venue_catalog"), "the refusal names the missing capability: {err}");
    assert_eq!(stdout(&out), "", "nothing was printed as though a venue had been listed");
}

/// **`show` reads the STORE, end to end** — a venue producer's recorded grid travels
/// store → `properties_as_of` → wire → the operator — and an instrument with no recorded grid is
/// the EMPTY rung rather than a zeroed one.
///
/// ⚠ The pairing is the point. `okx/BTC-USDT-SWAP` in [`spawn_class_probe_datahub`] carries TWO
/// properties rows and the LATER one names a class, so a `show` that took the first would render
/// `unclassified` and this case would redden — which is what makes the as-of instant assertable
/// from outside the process. `binance/BTCUSDT` in the same fixture has bars and no properties row
/// at all, so it is the absence, and the two must not share an exit code.
#[test]
fn show_reads_the_recorded_grid_and_an_unrecorded_instrument_is_the_empty_rung() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_class_probe_datahub().to_string();

    let args = ["data", "catalog", "show", "okx:BTC-USDT-SWAP", "--addr", &addr, "--json"];
    let out = run(scratch.path(), &args);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out))
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {}", stdout(&out)));
    assert_eq!(doc["recorded"], true);
    assert_eq!(doc["venue"], "okx");
    assert_eq!(doc["symbol"], "BTC-USDT-SWAP");
    assert_eq!(
        doc["asset_class"],
        AssetClass::CryptoPerp.sql_word(),
        "the LATER row's class, as the model's own stored word: {doc}"
    );
    assert!(
        doc["source"].as_str().unwrap_or_default().contains("kind=properties"),
        "the document must name the source it read, not imply a venue query: {doc}"
    );

    // The absence. A venue-recorded grid and NO grid must not share an exit code, or a pipeline
    // reads "this instrument has a zero tick size" for "nobody has ever recorded it".
    let missing =
        run(scratch.path(), &["data", "catalog", "show", "binance:BTCUSDT", "--addr", &addr]);
    assert_eq!(missing.status.code(), Some(7), "nothing was evaluated: {}", stderr(&missing));
    let err = stderr(&missing);
    assert!(err.contains("RECORDER"), "the absence is a store fact, and says so: {err}");
    assert!(err.contains("data catalog ls"), "…and names the verb that asks the VENUE: {err}");
}

/// A command line this group can refuse locally exits on the USAGE rung, before a socket is opened
/// — so the diagnostic comes from the binary the operator typed rather than from a timeout.
///
/// ⚠ **The VENUE-SHAPE rows are the ones that were missing, and their absence hid a real defect.**
/// This table carried no case for the `--venue` VALUE, and until
/// `crates/vike-cli/src/cmd/data/catalog/grammar.rs`'s `parse` called
/// `vike_datahub_client::catalog::validate_catalog_venue`, none of `--venue BINANCE`, `--venue
/// bin@nce` or an empty `--venue` was refused locally at all: each opened a connection, and what
/// the operator was told depended on who was on the port — the CONNECT rung (3, which a wrapper
/// retries) with no datahub up, or the `does not advertise venue_catalog` message against a server
/// with no catalog lane. Neither ever mentioned the slug. These rows are the doc's own claim,
/// applied to the one flag value this group can judge for itself.
///
/// ⚠ The empty spellings are written as `""` on purpose: they are what a shell variable that
/// expanded to nothing produces, which is the case an operator cannot see in their own scrollback.
#[test]
fn a_bad_catalog_command_line_is_the_usage_rung() {
    let scratch = tempfile::tempdir().expect("tempdir");
    for (args, needle) in [
        (vec!["data", "catalog", "frobnicate"], "unknown `data catalog` verb"),
        (vec!["data", "catalog", "ls"], "--venue"),
        (vec!["data", "catalog", "show"], "VENUE:SYMBOL"),
        (vec!["data", "catalog", "show", "binance:BTCUSDT:1h"], "INTERVAL"),
        (vec!["data", "catalog", "venues", "--venue", "binance"], "roster"),
        (vec!["data", "catalog", "ls", "--venue", "binance", "--class", "perp"], "unknown"),
        // ⚠ The needle was `"not built"` until `export --addr --format csv` shipped. `csv` is
        // WRITTEN now — by a verb, to a file — so the refusal here is about this verb printing
        // rather than about the workspace lacking a writer, and the needle follows the fact.
        (vec!["data", "catalog", "venues", "--format", "csv"], "FILE format"),
        // The venue SHAPE, judged here rather than at the far end of a socket.
        (vec!["data", "catalog", "ls", "--venue", "BINANCE"], "lowercase letters and digits"),
        (vec!["data", "catalog", "refresh", "--venue", "bin@nce"], "outside the permitted set"),
        (vec!["data", "catalog", "ls", "--venue", ""], "names no venue"),
        // ...and the third narrowing/rendering flag that took an empty value in silence.
        (vec!["data", "catalog", "ls", "--venue", "binance", "--search", ""], "EMPTY"),
    ] {
        let out = run(scratch.path(), &args);
        let err = stderr(&out);
        assert_eq!(out.status.code(), Some(2), "{args:?} is the USAGE rung: {err}");
        assert!(err.contains(needle), "{args:?} must say `{needle}`: {err}");
        assert_eq!(stdout(&out), "", "{args:?} wrote a document for a run that never happened");
    }
}
