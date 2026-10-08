//! The credential fence: the MCP surface can neither advertise nor reach a credential write.

use super::*;
use crate::cmd::mcp::instructions::INSTRUCTIONS_NO_CREDENTIAL;

/// **THE MCP SURFACE ADVERTISES NO CREDENTIAL WRITER, AND CONTAINS NO CALL INTO ONE.**
///
/// `docs/decisions/0036-credentials-are-read-only-from-the-cli-and-the-mcp-surface.md` is the
/// record, and its fourth reason is the one this test holds: `mcp` is a verb of the SAME binary
/// as `secrets`, one dispatch arm away from any CLI writer, so the only way to keep a credential
/// write out of the agent surface for certain is to keep it out of this file. That mattered in
/// the abstract while `vike-cli` had no writer at all; since `vike-cli secrets set` exists it is
/// a live property, and the record's reopen clause names an MCP tool as one of the four things
/// that would re-decide the whole verdict.
///
/// The comparison is against Hummingbot's MCP server, whose `setup_connector` tool takes an
/// AGENT-SUPPLIED credentials dict and writes it, gated only by a `confirm_override` flag for a
/// connector that already exists — so a NEW key is written with no gate at all. A credential
/// write is a larger grant than an order, and this server already makes order preview mandatory.
///
/// ⚠ FOUR evidences, because each alone is defeatable. A tool NAMED `rotate_secret` with an
/// innocuous description passes a description-only check; a tool called `configure` that
/// happened to call the writer passes a name-only one; a free-text `instructions` sentence
/// naming a write verb reaches the model without being a tool at all; and — the one
/// `docs/decisions/0065` §2 measured — a tool that builds an account-administration WIRE frame
/// reaches a credential write on the DAEMON's box while calling no writer in this process, so
/// every function-name needle here stays green. So: no advertised tool may pair a credential
/// word with a write word, AND this file may not call the writer at all, AND every `secrets`
/// subcommand the instructions name must be a READ, AND this file may not name the account
/// wire types.
///
/// ⚠ **And a FIFTH evidence lives in its own test, deliberately:**
/// `the_account_verbs_are_unreachable_through_a_helper_that_names_no_wire_type`. The fourth
/// above keys on this file NAMING a wire type, and argues that a frame cannot be built without
/// naming its request type — true of the frame, false of the CALLER. A client helper taking
/// primitives builds the frame inside ITSELF, exactly as `set_setting` already does for
/// settings, and this file reaches the verb naming nothing. That evidence is separate rather
/// than a fifth leg here so the two can be MEASURED apart: plant such a helper and this test
/// stays green while that one reddens, which is `docs/decisions/0065` §2's finding rather than
/// an assertion about it.
///
/// ⚠ **What it does NOT walk: the PROMPT texts.** The subject/act scan reads `tools_spec` only,
/// and the prompt bodies (`arm_a_venue`) plus a tool error string (`missing_key`) do pair a
/// credential word with a store word — they would trip this test if it were pointed at them,
/// which is why the scope is stated rather than left to be inferred from a green run. It is a
/// deliberate bound and not an oversight: a prompt EXECUTES nothing, and the second evidence
/// below — this file calls no writer — already holds for the whole file, prompts included. The
/// residual is a prompt that TELLS an agent to reach a credential by another route (a shell
/// tool, say), which `0036`'s reopen clause explicitly covers ("including one that merely calls
/// a shell") and which no text scan of this file could catch anyway.
#[test]
fn the_mcp_surface_advertises_no_credential_writer() {
    // The subject words, and the act words. Substring matching on purpose — `secrets`,
    // `credential` and `rotate_secret` all have to be caught, and a tool that talks about a
    // credential without proposing to change one (there are none today) would still be flagged
    // and would then be a deliberate decision rather than a drift.
    const SUBJECT: [&str; 4] = ["secret", "credential", "api key", "api_key"];
    const ACT: [&str; 6] = ["set", "write", "rotate", "store", "save", "configure"];

    let spec = tools_spec();
    for tool in spec.as_array().expect("tools_spec is an array") {
        let name = tool["name"].as_str().expect("every tool is named").to_lowercase();
        let description = tool["description"].as_str().unwrap_or_default().to_lowercase();
        for text in [&name, &description] {
            let subject = SUBJECT.iter().find(|s| text.contains(**s));
            let act = ACT.iter().find(|a| text.contains(**a));
            if let (Some(s), Some(a)) = (subject, act) {
                panic!(
                    "tool `{name}` pairs `{s}` with `{a}`: the MCP surface may advertise no \
                         credential write. See docs/decisions/0036 — an MCP tool is one of the \
                         four things its reopen clause says re-decides the record from the top."
                );
            }
        }
    }

    // …and the ROUTING half: this file calls neither the workspace's one upsert nor its
    // journalled wrapper, so no tool can reach a credential write by any name at all.
    //
    // ⚠ Both needles are `concat!`ed rather than spelled, and that is not decoration: this test
    // reads its OWN file, so a whole spelling written here as test DATA would report itself as a
    // call site and the assertion could never pass. It is the same self-scanning trap
    // `crates/vike-model/src/scan.rs`'s `find_calls` documents for the gate that walks it.
    //
    // ⚠ "THIS FILE" is the whole MODULE since `cmd/mcp.rs` became a directory (code-layout phase 2,
    // task 9): the parent plus the seven children that took its code. A scan of the parent alone
    // would have stayed green while every moved line left its reach. They are `include_str!`ed
    // rather than walked for the reason the floor below gives — a path that stops existing is a
    // compile error here, which is the right failure direction for a fence.
    const THIS_FILE: &str = concat!(
        include_str!("../../mcp.rs"),
        include_str!("../backtest_tools.rs"),
        include_str!("../config.rs"),
        include_str!("../data_tools.rs"),
        include_str!("../instructions.rs"),
        include_str!("../node_reads.rs"),
        include_str!("../node_writes.rs"),
        include_str!("../offline_tools.rs"),
        include_str!("../preview.rs"),
        include_str!("../prompts.rs"),
        include_str!("../protocol.rs"),
        include_str!("../resources.rs"),
        include_str!("../scope.rs"),
        include_str!("../tool_schemas.rs"),
        include_str!("../venue_gate.rs"),
    );
    const UPSERT: &str = concat!("save_", "credentials(");
    const JOURNALLED: &str = concat!("save_", "credentials_journalled(");
    // ⚠ THE THIRD NEEDLE, since `docs/decisions/0054`'s credential half: the Backend-aware
    // upsert that routes a write to the settings DATABASE or to the file. It is the name every
    // production writer in this workspace now calls, so a refusal keyed on the two above alone
    // would have left the MCP surface able to reach a credential write by the only spelling
    // anybody uses. The three are independent matches — `credentials_to_store(` is not
    // `credentials(` — so this adds a needle rather than widening one.
    const ROUTED: &str = concat!("save_", "credentials_to_store(");
    // ⚠ THE FOURTH NEEDLE, and it is not an upsert at all — it is the CREATE.
    // `vike_secrets::migrate` builds the settings database and fills it from both credential
    // files, which is a strictly larger act than replacing a named key, and none of the three
    // needles above matches it. Without this line `crates/vike-cli/src/cmd/mcp.rs` could call
    // the migrator inside a tool handler and this half stayed green, while
    // `docs/decisions/0036`'s FOURTH amendment asserted that this very test held the property
    // "exactly as for the three writers before it". It did not; it does now.
    //
    // The surface was never unguarded — `crates/vike-ops/tests/settings_secrets/credential_writer_gate.rs` keys
    // `migrate` tree-wide and `mcp.rs` is not in its `WRITER_CALLERS` — but a record that names
    // a test for a property that test does not hold is the defect, whichever other gate happens
    // to cover it.
    const MIGRATOR: &str = concat!("migr", "ate(");
    for writer in [UPSERT, JOURNALLED, ROUTED, MIGRATOR] {
        assert!(
            !THIS_FILE.contains(writer),
            "cmd/mcp.rs (or a child of it) calls `{writer}` — the MCP surface is read-only about \
             credentials"
        );
    }
    // Non-vacuity: the needle really does find a call when one is present, so a rename of the
    // writer cannot turn this half silently green. `crates/vike-ops/tests/
    // credential_writer_gate.rs` is the tree-wide version of the same question, with a kill
    // proof that plants a real call.
    //
    // ⚠ Keyed on `ROUTED`, and the day it moved is worth recording: `secrets set` called
    // `save_credentials` directly until the store acquired a database home, at which point it
    // moved onto the Backend-aware upsert and `secrets.rs` stopped containing the old needle at
    // all. This floor caught that — it went red while the two refusals above stayed green,
    // which is precisely the "a fix breaks its NEIGHBOUR" shape it exists for. Point it at the
    // writer the CLI actually calls, never at whichever needle happens to still match.
    //
    // ⚠ It names `secrets/set.rs` and not `secrets.rs` since `cmd/secrets.rs` was split by verb
    // (code-layout phase 2, task 10): the writer call lives in the child that holds `secrets set`.
    assert!(
        include_str!("../../secrets/set.rs").contains(ROUTED),
        "the needle must match the CLI's one writer call site, or this proves nothing"
    );
    // ⚠ THE FIFTH NEEDLE, for `secrets copy-node-keys` (2026-10-08). Its writer call is `ROUTED`
    // above, but a tool could reach the VERB without spelling the writer — by calling the verb's
    // own entry, which then copies every node key of another project into this one. So the module
    // and its entry are needles too: no MCP file may name either. Non-vacuity: the dispatcher that
    // DOES route to it contains the needle.
    const NODE_KEY_COPY: &str = concat!("copy_node", "_keys");
    assert!(
        !THIS_FILE.contains(NODE_KEY_COPY),
        "cmd/mcp.rs (or a child of it) names the node-key copy verb — the MCP surface may reach \
         no credential writer, and that verb writes every node key a service authenticates with"
    );
    assert!(
        include_str!("../../secrets.rs").contains(NODE_KEY_COPY),
        "the copy needle must match the `secrets` dispatcher's route to it, or this proves nothing"
    );
    // ⚠ THE SIXTH NEEDLE, for `secrets ibc-start` / `ibkr-cp-login` (2026-10-08) — the first verbs
    // that hand a credential VALUE onward rather than a name: one launches IBC with the IB Gateway's
    // login on its argv, the other runs a child with the pair in its environment.
    // They write no store row, so no writer needle above can see them, and a tool that called their
    // entry would put a login where an agent can reach it. No MCP file may name the module.
    // Non-vacuity: the dispatcher that DOES route to it contains the needle.
    const GATEWAY_LOGIN: &str = concat!("ibkr", "_login");
    assert!(
        !THIS_FILE.contains(GATEWAY_LOGIN),
        "cmd/mcp.rs (or a child of it) names the IBKR gateway-login readers — the MCP surface may \
         reach no credential VALUE, and those verbs hand one to a file or a child process"
    );
    assert!(
        include_str!("../../secrets.rs").contains(GATEWAY_LOGIN),
        "the gateway-login needle must match the `secrets` dispatcher's route to it, or this \
         proves nothing"
    );

    // …and the FOURTH half — placed here beside the ROUTING half it extends rather than after
    // the instructions half below, because it asks the same question that one does: can this
    // file REACH a credential write? `docs/decisions/0065-accounts-are-managed-and-the-
    // barrier-is-declared.md` §2 owes it by name: **no tool may CONSTRUCT the account-
    // administration wire verbs.**
    //
    // ⚠ The three halves above are blind to it, and 0065 measured that rather than assuming
    // it: *"All three stay green while an agent gains this capability."* The reason is that
    // this surface is ALREADY a first-class node-wire client — `Server::execute` special-cases
    // `WireCommand::SetSetting` and routes it to `execute_settings_write`, which opens its own
    // connection and calls a CLIENT HELPER. So a new command variant is reachable the moment a
    // tool builds one, and no needle keyed on a WRITER's function name can see it: the
    // credential never passes through `save_credentials_to_store` in THIS process at all — it
    // goes on a wire and the DAEMON writes it. `crates/vike-ops/tests/
    // credential_writer_gate.rs` is equally blind for the same reason, and its `mcp.rs`-shaped
    // hole is this assertion.
    //
    // Three things keep the surfaces apart and this is one of them; the other two are the
    // SCOPE (those verbs require `Scope::Account`, a third key, and `vike-cli` holds no verb
    // that presents one) and the ADVERTISEMENT (`FEATURE_ACCOUNT_VERBS` is withheld by
    // `served_features` unless the daemon's own barrier declaration armed the capability).
    // Neither of those is a property of THIS file, which is why this one is.
    //
    // ⚠ Needles `concat!`ed for the self-scanning reason above, and keyed on the TYPES rather
    // than on a helper name: a helper can be renamed or inlined, but a frame cannot be built
    // without naming the request type that goes in it.
    const ACCOUNT_REQ: &str = concat!("Account", "Request");
    const ACCOUNT_VERB: &str = concat!("Account", "Verb");
    const ACCOUNT_LIST: &str = concat!("WireAccount", "List");
    const ACCOUNT_WRITTEN: &str = concat!("WireAccount", "Written");
    for needle in [ACCOUNT_REQ, ACCOUNT_VERB, ACCOUNT_LIST, ACCOUNT_WRITTEN] {
        assert!(
            !THIS_FILE.contains(needle),
            "cmd/mcp.rs (or a child of it) names `{needle}` — the MCP surface may not reach the account-\
                 administration verbs. They carry a credential VALUE and write the settings \
                 database on the DAEMON's box, which is a grant larger than an order; \
                 docs/decisions/0036 reason 4 is what an agent holding it would have to argue \
                 against, and docs/decisions/0065 §2 names this assertion as the half of that \
                 fence a wire verb needs."
        );
    }
    // ⚠ **Non-vacuity, and the first attempt at it FAILED THIS VERY ASSERTION** — which is
    // worth recording, because it is the same self-scanning trap the routing half above
    // documents, one level further in. That attempt was a
    // `std::any::type_name::<…>()` turbofish per needle: a stronger floor in principle (it
    // asks the COMPILER for the spelling), and impossible in practice, because naming the type
    // in this file is precisely what the loop above forbids. The needles went red against
    // their own proof.
    //
    // The floor reads the DEFINING FILE instead. It puts no forbidden spelling in this file —
    // an `include_str!` path is not a type name — and it still fails loudly if a needle stops
    // naming anything, which is how a scan like this otherwise dies quietly. A MOVE of that
    // file is a compile error here rather than a silent pass, which is the right failure
    // direction for a fence.
    const WIRE_TYPES: &str = include_str!("../../../../../vike-tradehub-client/src/wire.rs");
    for needle in [ACCOUNT_REQ, ACCOUNT_VERB, ACCOUNT_LIST, ACCOUNT_WRITTEN] {
        assert!(
            WIRE_TYPES.contains(needle),
            "the needle `{needle}` no longer names anything in the wire crate — re-key it, or \
                 this half of the fence is scanning for a name nothing has"
        );
    }

    // …and the THIRD half, added with the `instructions` field: a second free-text channel that
    // reaches the model, which the two scans above cannot see. `instructions` is not a tool, so
    // `0036`'s reopen clause ("an MCP TOOL that writes, or reaches, a credential") does not
    // cover it — which is exactly why it needs its own line here rather than an assumption.
    //
    // ⚠ The SUBJECT×ACT pairing above CANNOT be reused on this text, and the attempt is
    // instructive: `<project>/settings/secrets.env` pairs the subject `secret` with the act
    // `set` — inside the word `settings` — so the crude rule would refuse the surface for
    // naming the store's real path. Substring matching is right for a tool NAME and a one-line
    // description written to a house style; it is wrong for prose. The rule here is the precise
    // one instead: every `secrets` subcommand this text names must be a READ.
    const SECRETS_READ_VERBS: [&str; 2] = ["list", "path"];
    for text in [
        instructions(&ToolAccess::full()),
        instructions(&ToolAccess::new(Profile::ReadOnly, Vec::new())),
    ] {
        for command in backticked_commands(&text) {
            let mut words = command.split_whitespace();
            if words.next() != Some("vike-cli") || words.next() != Some("secrets") {
                continue;
            }
            let sub = words.next().unwrap_or_default();
            assert!(
                SECRETS_READ_VERBS.contains(&sub),
                "the MCP instructions name `vike-cli secrets {sub}`, which is not one of the \
                     READ subcommands {SECRETS_READ_VERBS:?}. The instructions are text an agent \
                     acts on, and this surface directs the operator to READ a credential and never \
                     to write one — see docs/decisions/0036, whose four reasons a credential write \
                     reachable from an agent's context would have to argue against."
            );
        }
        // …and the POSITIVE half, because an omission is what an agent fills in with a guess:
        // the text must SAY this server cannot reach a credential at all.
        assert!(
            text.contains(INSTRUCTIONS_NO_CREDENTIAL),
            "the MCP instructions must state outright that this server reaches no credential \
                 — an omission is what an agent fills in with a guess, and the store's path reads \
                 as an invitation without it"
        );
    }
}

/// A `#[cfg(test)]` helper, not a build system: every `.rs` file under each repo-relative
/// root, read OFF DISK so that a file added tomorrow is walked without anybody remembering to
/// list it. Roots resolve from `CARGO_MANIFEST_DIR` rather than the working directory, because
/// `cargo test -p vike-cli` sets the CWD to the crate directory and a workspace run does not.
fn rs_sources_under(roots: &[&str]) -> Vec<(String, String)> {
    let crates_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/vike-cli has a parent")
        .to_path_buf();
    let mut out = Vec::new();
    let mut stack: Vec<std::path::PathBuf> = roots.iter().map(|r| crates_dir.join(r)).collect();
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("this scan must be able to read {}: {e}", dir.display()));
        for entry in entries {
            let path = entry.expect("a readable directory entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
                    panic!("this scan must be able to read {}: {e}", path.display())
                });
                let rel = path.strip_prefix(&crates_dir).unwrap_or(&path).display().to_string();
                out.push((rel.replace('\\', "/"), text));
            }
        }
    }
    out
}

/// **THE FIFTH EVIDENCE: NO HELPER LETS THIS SURFACE REACH THE ACCOUNT VERBS.**
///
/// A SEPARATE `#[test]`, and that is a measurement rather than a style: the four evidences in
/// `the_mcp_surface_advertises_no_credential_writer` have to be able to stay GREEN while this
/// one goes RED. That is the whole finding of `docs/decisions/0065-accounts-are-managed-and-
/// the-barrier-is-declared.md` §2 — plant the helper it warns about and the fence above does
/// not move — and a fifth leg folded into that test would have hidden the finding behind one
/// test name.
///
/// ⚠ **The hole this closes is inside the FOURTH evidence's own argument.** That one keys on
/// the wire TYPE NAMES appearing in this file, and states why: *"a helper can be renamed or
/// inlined, but a frame cannot be built without naming the request type that goes in it."*
/// True of the frame; false of the CALLER, which is the half that decides what an agent can
/// reach. `crates/vike-tradehub-client/src/remote_control.rs`'s `set_setting` is the standing
/// proof, and this file already calls it: a public helper taking `(addr, control_key, file,
/// key, value, confirm, reason)` — every one a primitive — which builds its command INSIDE
/// itself, so `Server::execute_settings_write` reaches a node-side settings write while naming
/// no wire type at all. An account twin written to that same template, say
/// `set_account_credential(addr, admin_key, key, value, confirm)`, is callable from a tool
/// handler with all four needles above still absent from this file — and the credential is
/// written on the DAEMON's box, so `crates/vike-ops/tests/settings_secrets/credential_writer_gate.rs` stays
/// green through it as well.
///
/// So this evidence follows the frame to where it MUST be built rather than to where it is
/// called, and two facts make that a scan instead of a call graph: a helper that reaches the
/// node has to DIAL it, and a helper that sends an account frame has to NAME one of its types.
/// **No file may do both.** The partition is total today and needs no exception list —
/// `wire.rs`, `proto.rs` and `lib.rs` name the types and dial nothing, while `handshake.rs`,
/// `remote_control.rs`, `remote_handle.rs` and `liveness.rs` dial and name nothing — so a
/// helper added to any of them lands on both sides at once. The roots are walked ON DISK at
/// run time rather than `include_str!`ed for the one shape an `include_str!` list cannot see:
/// a NEW file, which is exactly where somebody adding an account client would put it.
///
/// ⚠ **Scope, stated rather than left to be inferred from a green run.** Two roots: the client
/// crate — the only crate in this tree holding these types AND the handshake — and
/// `vike-cli`'s own `src/`, the crate this file is composed into, where an in-process helper
/// would live. A helper in a THIRD crate that opened its own socket and spoke the protocol by
/// hand would escape, and nothing here would see it. That is a declared residual, not a claim
/// — and it is
/// `docs/decisions/0036-credentials-are-read-only-from-the-cli-and-the-mcp-surface.md`'s
/// reopen clause firing somewhere no text scan reaches.
#[test]
fn the_account_verbs_are_unreachable_through_a_helper_that_names_no_wire_type() {
    // Split for the self-scanning reason the fourth evidence documents, one level further in:
    // this test walks its OWN file off disk, so a whole spelling written here as test data
    // would report `mcp.rs` as naming an account type and the assertion could never pass.
    const NAMES: [&str; 5] = [
        concat!("Account", "Request"),
        concat!("Account", "Verb"),
        concat!("WireAccount", "List"),
        concat!("WireAccount", "Written"),
        concat!("Request::", "Account"),
    ];
    // The DIAL needles. The client crate's handshake is `pub(crate)` and is its one door onto
    // a node, so every helper there goes through it; the raw connect is named beside it
    // because a new file could open its own socket instead of borrowing that door.
    const DIALS: [&str; 2] = [concat!("node_", "handshake("), concat!("TcpStream", "::connect")];

    let sources = rs_sources_under(&["vike-tradehub-client/src", "vike-cli/src"]);

    // Non-vacuity, three floors, because a scan like this otherwise dies quietly: the walk
    // found a tree, the DIAL needles match something in it, and the NAME needles match
    // something in it. Without the last two, a rename on either side turns the whole evidence
    // green while the capability it guards is wide open.
    assert!(
        sources.len() > 40,
        "the walk found only {} source files — it is not reading the tree",
        sources.len()
    );
    assert!(
        sources.iter().any(|(_, text)| DIALS.iter().any(|d| text.contains(d))),
        "no walked file dials a node, so the needles {DIALS:?} name nothing and this evidence \
             would pass whatever a helper did"
    );
    assert!(
        sources.iter().any(|(_, text)| NAMES.iter().any(|n| text.contains(n))),
        "no walked file names an account wire type — re-key the needles, or this evidence is \
             scanning for names nothing has"
    );

    for (path, text) in &sources {
        let Some(dial) = DIALS.iter().find(|d| text.contains(**d)) else { continue };
        let Some(name) = NAMES.iter().find(|n| text.contains(**n)) else { continue };
        panic!(
            "{path} both dials a node (`{dial}`) and names `{name}`, so it holds — or can \
                 hold — a client helper that reaches the account-administration verbs. The MCP \
                 surface is a first-class client of this same wire from this same binary and can \
                 call such a helper with PRIMITIVES, naming no wire type: \
                 `the_mcp_surface_advertises_no_credential_writer` stays green through that, and \
                 so does crates/vike-ops/tests/settings_secrets/credential_writer_gate.rs, because the credential \
                 is written on the DAEMON's box and passes no writer in this process. Those verbs \
                 carry a credential VALUE, which docs/decisions/0036 reason 4 says re-decides that \
                 record from the top, and docs/decisions/0065 §2 is the measurement that this \
                 shape defeats every needle above it. If the helper is genuinely wanted, the fence \
                 has to move onto something the MCP surface cannot call at all — a decision, \
                 argued, and never a needle re-keyed until it is green again."
        );
    }
}

/// **AND THE FLOOR UNDER THE SCOPE CLAIM, which until now was held by nothing at all.**
///
/// `the_mcp_surface_advertises_no_credential_writer`'s fourth evidence names three things that
/// keep this surface off the account plane, and says outright that two of them are "not a
/// property of THIS file": the SCOPE — those verbs want `Scope::Account`, and `vike-cli` holds
/// no verb that presents one — and the ADVERTISEMENT. The advertisement has a test on the
/// daemon's side. The scope had none: it was true by ABSENCE, and adding an `admin` accessor
/// beside `observe` and `control` on `crates/vike-cli/src/cmd/nodekeys.rs`'s `NodeKeyring`
/// would have reddened nothing in this tree. A fence with a second leg that can be removed
/// without a test moving is a fence with one leg, so this is that leg.
#[test]
fn the_cli_keyring_cannot_present_the_admin_scope() {
    // The keyring this binary hands every node-dialling verb, the MCP server included. Two
    // accessors, and the third scope deliberately has none.
    const KEYRING: &str = include_str!("../../nodekeys.rs");
    const ADMIN_ACCESSOR: &str = concat!("fn ", "admin");
    assert!(
        !KEYRING.contains(ADMIN_ACCESSOR),
        "cmd/nodekeys.rs declares `{ADMIN_ACCESSOR}` — the CLI's keyring may not present the \
             admin scope. It is one of the two things the fourth evidence of \
             `the_mcp_surface_advertises_no_credential_writer` names as keeping the agent surface \
             off the account-administration verbs, and the MCP server is handed this same keyring."
    );
    // Non-vacuity: the shape really does match the accessors that ARE there, so a refactor of
    // the keyring cannot turn the refusal above silently green.
    for present in [concat!("fn ", "observe"), concat!("fn ", "control")] {
        assert!(
            KEYRING.contains(present),
            "the needle shape no longer matches this keyring's real accessors — re-key it, or \
                 the refusal above is scanning for a spelling nothing uses"
        );
    }

    // …and the CONSTRUCTOR half, because an accessor is not the only door: a `NodeKeys` gains
    // an admin key from exactly one function, whose one caller in this workspace is the DAEMON
    // reading its own store. No file of this crate may call it.
    const ADMIN_KEYS: &str = concat!("from_vars_", "with_admin(");
    for (path, text) in rs_sources_under(&["vike-cli/src"]) {
        assert!(
            !text.contains(ADMIN_KEYS),
            "{path} calls `{ADMIN_KEYS}` — this crate composes the MCP surface, and an admin \
                 key held in THIS process is what the account-administration verbs authenticate \
                 against. docs/decisions/0065 §2's scope split is the claim this refusal holds."
        );
    }
    assert!(
        include_str!("../../../../../vike-tradehub-client/src/auth.rs").contains(ADMIN_KEYS),
        "the needle `{ADMIN_KEYS}` no longer names the admin-key constructor — re-key it, or \
             this half is scanning for a name nothing has"
    );
}

/// Rust CODE with `//` comments removed — a doc mention is prose, not a reach.
///
/// ⚠ **Every other evidence in this family scans whole files, comments included, and that is a
/// measured defect rather than a nuance.** An adversarial review of the fifth evidence went red
/// against a DOC COMMENT the reviewer had just written, and `crates/vike-cli/src/cmd/mcp.rs`
/// sits on the dialing side of that test's partition today ONLY because its module doc mentions
/// `TcpStream::connect`. A fence that a sentence can trip is a fence that teaches authors to
/// stop writing sentences. Quote-aware so a `//` inside a string literal is not mistaken for a
/// comment.
fn rust_code_of(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_string = false;
    let mut prev_slash = false;
    for (i, b) in bytes.iter().enumerate() {
        match b {
            b'"' => {
                in_string = !in_string;
                prev_slash = false;
            }
            b'/' if !in_string => {
                if prev_slash {
                    return &line[..i - 1];
                }
                prev_slash = true;
            }
            _ => prev_slash = false,
        }
    }
    line
}

/// The sites that legitimately name the admin scope, each with the reason it is not a reach.
///
/// ⚠ **There are none in these two crates now, and that is the intended end state.** The two
/// sites that used to be pinned here — the ping report's `Scope -> &str` and the client crate's
/// twin of it beside the handshake — were exhaustive `match` arms turning a scope into a WORD
/// (a label authorises nothing). They were the same body twice, and it now lives once, as
/// `Scope::name` in `vike-node-proto`, which this scan does not read: neither crate names
/// `Scope::Account` in code any more.
///
/// A site is not absorbed by adding a row. The refusal below says what to do instead, and says
/// it because an exception list that grows on contact is the shape of gate this tree has watched
/// rot elsewhere.
const SCOPE_LABEL_SITES: [&str; 0] = [];

/// **The sixth evidence, and it follows the AUTHORIZATION rather than a type name.**
///
/// ⚠ **This exists because an adversarial review defeated the fifth twice, with compiling
/// code, while every other evidence here stayed green.** Both evasions are recorded because the
/// shape of each is the argument:
///
/// * **A JSON literal.** This wire is length-prefixed `serde_json`, so a frame is a STRING —
///   `serde_json::from_value(json!({"Account": {"verb": {"SetCredential": …}}}))` builds a
///   `Request` naming no account type at all, and was proved byte-equal to the typed frame.
///   The fourth evidence's *"a frame cannot be built without naming the request type that goes
///   in it"* is true of the FRAME and false of the CALLER.
/// * **A split across the partition's own poles.** The fifth evidence reasons that no file both
///   dials and names. True — and *"no file may do both"* is not *"no pair of files may"*. A
///   constructor in `proto.rs` (names, never dials) plus a primitives-only sender in
///   `remote_control.rs` (dials, names nothing) is idiomatic, compiles, and passes everything.
///
/// **What both must do, and cannot delegate, is PRESENT THE SCOPE.** An account verb is refused
/// by `crates/vike-tradehub/src/server/accounts.rs`'s `account_admission` under anything but
/// `Scope::Account`, so a reach that works has `Scope::Account` in the file that authenticates. That
/// is a property of the authorization rather than of a spelling, which is why this evidence
/// keys on it.
///
/// ⚠ **It still does not hold the CLASS, and saying so is the point.** `Scope::Account` is itself
/// a name: `Scope` derives `Deserialize`, so a JSON literal or an integer tag reaches the same
/// variant naming nothing. **No text scan can hold this property, because the wire is data and
/// a scan reads code.** What would hold it is structural and is a decision rather than a test —
/// `docs/decisions/0036`'s *What would reopen this* clause is where it belongs, and the
/// candidates measured during that review were: make the account plane unreachable from this
/// binary at the CRATE graph; seal the frame type so a literal cannot be deserialized into it
/// from outside; or move the barrier onto the DAEMON, which is the only side that sees what it
/// is actually being asked to do. Until one is taken, this family is ADVISORY — it raises the
/// cost of an accident and stops none of the three evasions above on purpose.
#[test]
fn a_dialing_file_does_not_present_the_admin_scope() {
    const ADMIN_SCOPE: &str = concat!("Scope", "::Account");
    let mut offenders = Vec::new();
    let mut seen_labels: Vec<&str> = Vec::new();
    for (path, text) in rs_sources_under(&["vike-cli/src", "vike-tradehub-client/src"]) {
        let reaches: Vec<&str> =
            text.lines().map(rust_code_of).filter(|code| code.contains(ADMIN_SCOPE)).collect();
        if reaches.is_empty() {
            continue;
        }
        if let Some(site) = SCOPE_LABEL_SITES.iter().find(|s| path.ends_with(*s)) {
            seen_labels.push(site);
            continue;
        }
        offenders.push(format!("  {path}: {}", reaches.join(" | ")));
    }
    for site in SCOPE_LABEL_SITES {
        assert!(
            seen_labels.contains(&site),
            "the pinned label site {site} no longer names `{ADMIN_SCOPE}` in CODE — either it \
                 moved, or `rust_code_of` has started eating real lines. Either way this test is \
                 scanning for something nothing has, which is the failure mode it exists to avoid \
                 in others."
        );
    }
    assert!(
        offenders.is_empty(),
        "these files present `{ADMIN_SCOPE}` in code, and an account verb is refused under \
             every other scope — so a reach that WORKS has it here:\n{}\n\nIf this is a legitimate \
             new client, it is not this test that decides: docs/decisions/0036 fences the MCP \
             surface from credential writes and `docs/decisions/0065` §2 owes the scope split. Add \
             a pinned site with the argument, or take one of the structural answers this test's \
             own doc names — do not widen the needle.",
        offenders.join("\n")
    );
}
