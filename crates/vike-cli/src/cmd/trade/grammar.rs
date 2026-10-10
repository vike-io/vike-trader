//! The REPL line GRAMMAR (pure): the `--reason` split, the lifecycle verbs, the order/state words.

use serde_json::{Value, json};
use vike_tradehub_client::wire::WireTradingState;

use crate::cmd::verbs::Verb;

use super::STATE_MIGRATION_HINT;
use super::grammar_orders::{parse_modify, parse_submit};

// ---- the verb grammar (PURE — no network, fully testable) ------------------------------------
//
// The [`Verb`] enum itself (and its `is_write` / `to_wire_command`) lives in [`crate::cmd::verbs`]
// — the ONE args→WireCommand construction site shared with the `mcp` write tools — and is
// re-exported above. Only the REPL line GRAMMAR stays here, for BOTH grammars: the order verbs
// ([`parse_line`]) and the node-lifecycle three ([`parse_lifecycle`]), which used to build their
// own wire commands here and no longer do.

/// The `--reason` tail marker: everything after it on the line is the operator rationale.
const REASON_FLAG: &str = "--reason";

/// Split a REPL line into `(command, reason)` at the first `--reason` TOKEN — run BEFORE
/// [`parse_line`], so the verb grammar never has to know the rationale exists (and a rationale
/// containing a word like `--qty` can never be mistaken for a flag).
///
/// The rationale is taken VERBATIM to end of line, so a multi-word "why" needs no quoting; it is
/// trimmed, and ONE optional layer of surrounding `"`/`'` quotes is removed for the habitual typist.
/// `--reason` with nothing after it (or only whitespace) yields `None` — an empty rationale is no
/// rationale. `--reason` matches only as a whole whitespace-delimited token, so a symbol or coid that
/// merely CONTAINS the text is not a split point. No `--reason` at all ⇒ `(line, None)`, byte-for-byte
/// what the parser saw before this existed. PURE.
pub(crate) fn split_reason(line: &str) -> (&str, Option<String>) {
    let (command, tail) = split_tail(line, REASON_FLAG);
    let reason = tail.map(unquote).map(str::trim).filter(|r| !r.is_empty()).map(str::to_string);
    (command, reason)
}

/// Split `line` at the first whole-token occurrence of `flag`: everything BEFORE it (with trailing
/// whitespace trimmed) and everything AFTER it (trimmed), or `None` when the flag is not present.
///
/// The shared mechanic behind BOTH rest-of-line flags — [`split_reason`]'s `--reason` and
/// [`parse_mount`]'s `--params` — and it exists as one function because they are the same rule:
/// take the remainder verbatim so a multi-word value (a sentence, a JSON object with spaces in it)
/// needs no quoting, and match the flag only as a whole whitespace-delimited token so a coid or a
/// symbol that merely CONTAINS the text is not a split point.
///
/// ⚠ It distinguishes ABSENT (`None`) from PRESENT-BUT-EMPTY (`Some("")`), which its two callers
/// answer differently and must: an empty `--reason` is no rationale (there is nothing to record),
/// while an empty `--params` is a typo — the operator asked for a params object and supplied none,
/// and silently mounting with `{}` would start a strategy on defaults they never chose. PURE.
pub(super) fn split_tail<'a>(line: &'a str, flag: &str) -> (&'a str, Option<&'a str>) {
    let mut cursor = 0usize;
    let at = loop {
        let Some(hit) = line[cursor..].find(flag) else { return (line, None) };
        let at = cursor + hit;
        let after = &line[at + flag.len()..];
        let is_token = (at == 0 || line[..at].ends_with(char::is_whitespace))
            && (after.is_empty() || after.starts_with(char::is_whitespace));
        if is_token {
            break at;
        }
        cursor = at + flag.len();
    };
    (line[..at].trim_end(), Some(line[at + flag.len()..].trim()))
}

/// Strip ONE layer of matching surrounding `"` or `'` quotes, if present. Byte indexing is safe: both
/// quote characters are single-byte, so slicing just inside them lands on char boundaries.
pub(super) fn unquote(s: &str) -> &str {
    let b = s.as_bytes();
    let quoted = b.len() >= 2 && b[0] == b[b.len() - 1] && (b[0] == b'"' || b[0] == b'\'');
    if quoted { &s[1..s.len() - 1] } else { s }
}

/// Parse one REPL line, routing on its leading token: the strategy-lifecycle grammar
/// ([`parse_lifecycle`]) if it claims the verb, the order grammar ([`parse_line`]) otherwise. The
/// ONE entry the REPL loop calls, so a line still has exactly one grammar even though the file
/// holds two. PURE.
///
/// ⚠ It used to return a `Parsed` union — `Parsed::Order(Verb)` beside a
/// `Parsed::Lifecycle(WireCommand)` — because the lifecycle three were built here rather than in
/// [`crate::cmd::verbs`]. Both grammars produce a [`Verb`] now, so the union is gone and there is
/// one write path from here down; see this module's doc for what happened to the second builder.
///
/// The line handed here has already had any `--reason` tail split off by [`split_reason`].
pub(crate) fn parse_repl_line(line: &str) -> Result<Verb, String> {
    let Some((verb, rest)) = take_token(line) else {
        return Err("empty line".to_string());
    };
    match parse_lifecycle(verb, rest) {
        Some(parsed) => parsed,
        None => parse_line(line),
    }
}

/// The strategy-LIFECYCLE grammar — `mount` / `unmount` / `set-setting` — parsed to the [`Verb`]
/// each names. `None` when `verb` is not one of them, which is the signal [`parse_repl_line`] falls
/// through to the order vocabulary on.
///
/// `rest` is the remainder of the line VERBATIM, not a token slice, because two of these verbs take
/// a rest-of-line value (`mount --params`, `set-setting`'s value) that whitespace tokenization
/// would mangle — a JSON object and a TOML array both contain spaces a person will type. That is
/// the whole of what stays REPL-specific here: the grammar. The [`WireCommand`] each one becomes is
/// built by [`Verb::to_wire_command`], the same site the `mcp` write tools resolve through.
pub(super) fn parse_lifecycle(verb: &str, rest: &str) -> Option<Result<Verb, String>> {
    match verb.to_ascii_lowercase().as_str() {
        "mount" => Some(parse_mount(rest)),
        "unmount" => Some(parse_unmount(rest)),
        // `set` is sugar the way `panic`/`pos`/`snap` are, and it reads naturally at a prompt. It
        // is a whole-token match, so it collides with nothing — including the REMOVED `state`,
        // whose migration hint [`parse_line`] still answers by name.
        "set-setting" | "set" => Some(parse_set_setting(rest)),
        _ => None,
    }
}

/// The `--params` marker on `mount`: like `--reason`, everything after it is taken to end of line.
pub(super) const PARAMS_FLAG: &str = "--params";

/// `mount <venue> <symbol> <interval> (--name <strategy> | --rhai <path>) [--id <mount-id>]
/// [--params <json to end of line>]`
///
/// Adds a strategy to the RUNNING node's core with no restart ([`Verb::MountStrategy`], which
/// becomes `WireCommand::MountStrategy` at the shared construction site).
///
/// **`--name` XOR `--rhai`.** The wire contract is an exclusive choice — the profile `[strategy]`
/// vocabulary verbatim, a registry name or a Rhai script path — so both-or-neither is refused HERE,
/// naming both spellings, rather than spending a round trip on a refusal the grammar already knew.
/// ⚠ `--rhai` is a path on the **NODE's** filesystem, not this machine's: the node opens it, and a
/// path that exists here proves nothing about there.
///
/// **`--account` names WHICH ACCOUNT of the venue the mount trades and reads**, and the three
/// states are distinct: omitted names none, `DEFAULT` names the venue's unlabelled account
/// deliberately, a label names that account. On a venue this node runs TWO engines of, omitting it
/// is REFUSED by the node rather than resolved to the default — which is the whole point, since the
/// default account's route key IS the bare venue, so the silent answer would look correct. The
/// refusal names the spellings that core answers to. Requires the node to advertise
/// `FEATURE_MOUNT_ACCOUNT`; against an older node a NAMED account is refused here, unsent.
///
/// **`--params` runs to end of line** and must be a JSON OBJECT — the `[strategy.params]` table, in
/// the same delegate-don't-mirror form `WireCommand::MountStrategy` carries, so the wire never
/// re-declares a strategy's knobs and neither does this parser. Rest-of-line rather than one token
/// because `{"gamma": 0.0008}` is what a person types and whitespace tokenization would split it
/// into three. It therefore comes LAST among the mount flags (a trailing `--reason` is still fine —
/// that tail is split off before this parser ever runs). Absent ⇒ `{}`, an empty table; PRESENT and
/// empty is a typo and is refused, because mounting on a strategy's defaults nobody chose is not
/// what the operator asked for.
fn parse_mount(rest: &str) -> Result<Verb, String> {
    const USAGE: &str = "usage: mount <venue> <symbol> <interval> (--name <strategy> | --rhai \
                         <path-on-the-node>) [--account <LABEL|DEFAULT>] [--id <mount-id>] \
                         [--params <json to end of line>]";

    let (head, params_text) = split_tail(rest, PARAMS_FLAG);
    let params = match params_text {
        None => json!({}),
        Some("") => {
            return Err(format!(
                "mount: {PARAMS_FLAG} needs a JSON object — drop the flag entirely for an empty \
                 params table\n{USAGE}"
            ));
        }
        Some(text) => {
            let value: Value = serde_json::from_str(text)
                .map_err(|e| format!("mount: {PARAMS_FLAG} is not JSON ({e}): {text}\n{USAGE}"))?;
            if !value.is_object() {
                return Err(format!(
                    "mount: {PARAMS_FLAG} must be a JSON OBJECT (the `[strategy.params]` table), \
                     got {value}\n{USAGE}"
                ));
            }
            value
        }
    };

    let (mut name, mut rhai, mut id, mut account): (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = (None, None, None, None);
    let mut positional: Vec<&str> = Vec::new();
    let tokens: Vec<&str> = head.split_whitespace().collect();
    let mut i = 0usize;
    while i < tokens.len() {
        let tok = tokens[i];
        // Both spellings, the same pair `submit --coid` accepts: `--flag value` and `--flag=value`.
        let (flag, inline) = match tok.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v)),
            _ => (tok, None),
        };
        match flag {
            "--name" | "--rhai" | "--id" | "--account" => {
                let value = match inline {
                    Some(v) => {
                        i += 1;
                        v
                    }
                    None => {
                        let v = *tokens
                            .get(i + 1)
                            .ok_or_else(|| format!("mount: {flag} needs a value\n{USAGE}"))?;
                        i += 2;
                        v
                    }
                };
                // A value that is itself a flag means the operator's value went missing and the
                // NEXT flag was eaten as one — the failure `submit --coid has space` taught.
                if value.is_empty() || value.starts_with("--") {
                    return Err(format!("mount: {flag} needs a value, got {value:?}\n{USAGE}"));
                }
                let slot = match flag {
                    "--name" => &mut name,
                    "--rhai" => &mut rhai,
                    "--account" => &mut account,
                    _ => &mut id,
                };
                if slot.is_some() {
                    return Err(format!("mount: {flag} given twice\n{USAGE}"));
                }
                *slot = Some(value.to_string());
            }
            other if other.starts_with("--") => {
                return Err(format!("mount: unexpected flag {other:?}\n{USAGE}"));
            }
            _ => {
                positional.push(tok);
                i += 1;
            }
        }
    }

    if positional.len() != 3 {
        return Err(USAGE.to_string());
    }
    match (&name, &rhai) {
        (Some(_), Some(_)) => {
            return Err(format!(
                "mount: --name and --rhai are EXCLUSIVE — a mount's strategy source is either a \
                 registry name or a Rhai script path, never both\n{USAGE}"
            ));
        }
        (None, None) => {
            return Err(format!(
                "mount: a strategy source is required — pass --name <strategy> (a registry name) \
                 or --rhai <path> (a script on the NODE's filesystem)\n{USAGE}"
            ));
        }
        _ => {}
    }

    // Refused HERE, in the REPL's own vocabulary, rather than a round trip away — but read with
    // `parse_wire_account`, the one authority on the grammar, so this edge adds a message and not a
    // second set of rules. ⚠ It admits `DEFAULT`, which a `policy.accounts` row refuses: on the
    // wire that spelling is how an operator says "the unlabelled account, deliberately" as
    // distinct from saying nothing, and at two engines of one venue those are different answers.
    if let Some(a) = &account
        && let Err(e) = vike_model::accounts::account_keys::parse_wire_account(a)
    {
        return Err(format!(
            "mount: --account {a:?} — {e}. Drop the flag to name no account, or pass DEFAULT to \
             name the venue's unlabelled account deliberately\n{USAGE}"
        ));
    }

    Ok(Verb::MountStrategy {
        venue: positional[0].to_string(),
        account,
        symbol: positional[1].to_string(),
        interval: positional[2].to_string(),
        // `None` lets the node derive `{venue}__{symbol}__{interval}`. Deliberately NOT derived
        // here: the derivation is the node's rule, and a client copy of it would be a second one to
        // keep in step for no gain — the node accepts the absence and answers with its own answer.
        controller_id: id,
        name,
        rhai,
        params,
    })
}

/// `unmount <mount-id>` — remove one mount from the running core ([`Verb::UnmountStrategy`]).
///
/// One token, and a second one is an error rather than being ignored: a mount id carries no spaces,
/// so extra words mean the operator meant something this verb does not do — and the thing they most
/// plausibly meant (naming the mount by `<venue> <symbol> <interval>`) would need this client to
/// re-implement the node's own id derivation. `help` states where the id comes from instead.
fn parse_unmount(rest: &str) -> Result<Verb, String> {
    const USAGE: &str = "usage: unmount <mount-id>";
    let mut tokens = rest.split_whitespace();
    let id = tokens.next().ok_or(USAGE)?;
    if tokens.next().is_some() {
        return Err(format!(
            "unmount: a mount id is ONE token (the explicit --id given at mount time, or the \
             node-derived {{venue}}__{{symbol}}__{{interval}})\n{USAGE}"
        ));
    }
    Ok(Verb::UnmountStrategy { controller_id: id.to_string() })
}

/// The suffix the retired `set-setting <file> …` token carried (`policy.toml`), stripped before the
/// section-word test so a line still spelled the old way gets the by-name refusal
/// ([`parse_set_setting`]) rather than a write of the key `policy.toml`.
const RETIRED_TOKEN_SUFFIX: &str = ".toml";

/// `set-setting <full.dotted.key> <value to end of line>` — write ONE row of the node's settings
/// database, named by its key ([`Verb::SetSetting`]; `docs/decisions/0086`). The same two
/// positionals `vike-cli config set` takes, for the same reason: the key's first segment already
/// names its section.
///
/// The value runs to END OF LINE and is taken VERBATIM. Both halves of that are deliberate: a TOML
/// value can contain spaces (`["a", "b"]`, a prose string), and unlike `--reason` it is NOT
/// unquoted, because quotes are meaningful to the node's TOML parse — stripping them would turn the
/// string `"250"` into the integer `250`, silently changing the type of the key being set.
///
/// ⚠ **The retired `<file>` token is refused BY NAME.** The grammar was
/// `set-setting <file> <key> <value>` until a write became one row named by its key; a line still
/// spelled that way would otherwise parse that token as the key and the real key as the start of
/// the value. A first token that is a bare section word (`vike_config::SettingsSection::parse`), or
/// one with the `.toml` suffix that token used to carry ([`RETIRED_TOKEN_SUFFIX`]), is that
/// retired token: no settings key is either one (a key names a field INSIDE a section), so the
/// refusal can never catch a real write.
///
/// Nothing here validates the KEY. The node holds the loader and refuses an unknown key with its
/// own message; a second copy of that rule on this side would be one more thing to keep in step,
/// and it would still not be the one that decides. What goes on the wire — `file` derived from the
/// key, `confirm` empty — is [`Verb::to_wire_command`]'s to decide.
pub(super) fn parse_set_setting(rest: &str) -> Result<Verb, String> {
    const USAGE: &str = "usage: set-setting <full.dotted.key> <value to end of line>";
    let (key, value) = take_token(rest).ok_or(USAGE)?;
    let token = key.strip_suffix(RETIRED_TOKEN_SUFFIX).unwrap_or(key);
    if vike_config::SettingsSection::parse(token).is_some() {
        return Err(format!(
            "set-setting takes no file any more — a write is one row named by its key, and the \
             key's first segment is its section (docs/decisions/0086). Drop {key:?}: \
             set-setting <full.dotted.key> <value to end of line>, e.g. `set-setting \
             policy.max_notional_per_order 250`\n{USAGE}"
        ));
    }
    // Trailing whitespace is stripped and surrounding quotes are NOT (see the doc above) — the two
    // are different acts: one removes what the terminal added, the other would change the type.
    let value = value.trim_end();
    if value.is_empty() {
        return Err(format!(
            "set-setting: no value — a key set to nothing is not a write anybody meant\n{USAGE}"
        ));
    }
    Ok(Verb::SetSetting { key: key.to_string(), value: value.to_string() })
}

/// Split off the first whitespace-delimited token and return it with the (left-trimmed) remainder,
/// or `None` when there is no token at all. The half-tokenized read the two rest-of-line verbs need:
/// their leading fields are tokens, their last field is not.
fn take_token(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    if s.is_empty() {
        return None;
    }
    match s.find(char::is_whitespace) {
        Some(at) => Some((&s[..at], s[at..].trim_start())),
        None => Some((s, "")),
    }
}

/// Parse one REPL line into a [`Verb`] — the ORDER/state vocabulary. Terse, whitespace-tokenized.
/// Malformed input is a clean `Err(String)`. PURE — no network, no I/O — so the whole grammar is
/// unit-testable.
///
/// Reached through [`parse_repl_line`], which routes the strategy-lifecycle verbs elsewhere first.
/// The line handed here has already had any `--reason` tail split off by [`split_reason`].
pub(crate) fn parse_line(line: &str) -> Result<Verb, String> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let (verb, rest) = tokens.split_first().ok_or("empty line")?;
    match verb.to_ascii_lowercase().as_str() {
        "submit" | "buy" | "sell" => parse_submit(verb, rest),
        "cancel" => {
            let coid = rest.first().ok_or("usage: cancel <coid>")?;
            if rest.len() > 1 {
                return Err("usage: cancel <coid>".to_string());
            }
            Ok(Verb::Cancel((*coid).to_string()))
        }
        "modify" => parse_modify(rest),
        "flatten" => {
            if rest.len() != 2 {
                return Err("usage: flatten <venue> <symbol>".to_string());
            }
            // ⚠ `account: None` — this REPL grammar takes no book selector; task 7 of the
            // trade-CLI-plane widened `Verb::Flatten` with the field for the one-shot
            // `crate::cmd::trade::position`'s `flatten`, which DOES take one. Lifting the REPL onto
            // the same selector grammar is future work, not a gap this line's arity covers.
            Ok(Verb::Flatten {
                venue: rest[0].to_string(),
                symbol: rest[1].to_string(),
                account: None,
            })
        }
        "market-exit" | "panic" => {
            if rest.len() > 1 {
                return Err("usage: market-exit [venue]".to_string());
            }
            // Same note as `flatten` above.
            Ok(Verb::MarketExit { venue: rest.first().map(|s| s.to_string()), account: None })
        }
        "mass-cancel" => {
            if rest.len() > 2 {
                return Err("usage: mass-cancel [venue] [symbol]".to_string());
            }
            // Same note as `flatten` above.
            Ok(Verb::MassCancel {
                venue: rest.first().map(|s| s.to_string()),
                symbol: rest.get(1).map(|s| s.to_string()),
                account: None,
            })
        }
        // ⚠ THREE WORDS, NOT ONE WITH AN ARGUMENT. `state` used to be this arm — no token meant
        // READ, a token meant WRITE — so `state halted` halted a live daemon by adding a word to a
        // read. Ruling 17 removed it: `status` reads (the mode AND the mount registry, in one
        // output), `halt` and `resume` each write and each says so in its own name. Every one of
        // them REFUSES a trailing token, which is what keeps the old spelling from being half-alive:
        // `status halted` is a usage error naming the two verbs that write, never a halt.
        "status" => {
            if rest.is_empty() {
                Ok(Verb::Status)
            } else {
                Err("usage: status  (it takes no argument — `halt` and `resume` are the verbs \
                     that CHANGE the mode)"
                    .to_string())
            }
        }
        "halt" => {
            if rest.is_empty() {
                Ok(Verb::SetState(WireTradingState::Halted))
            } else {
                Err("usage: halt  (it takes no argument)".to_string())
            }
        }
        "resume" => {
            if rest.is_empty() {
                Ok(Verb::SetState(WireTradingState::Active))
            } else {
                Err("usage: resume  (it takes no argument)".to_string())
            }
        }
        "orders" => {
            if rest.len() > 1 {
                return Err("usage: orders [symbol]".to_string());
            }
            Ok(Verb::Orders(rest.first().map(|s| s.to_string())))
        }
        "positions" | "pos" => {
            if rest.len() > 1 {
                return Err("usage: positions [venue]".to_string());
            }
            Ok(Verb::Positions(rest.first().map(|s| s.to_string())))
        }
        "equity" | "balance" => Ok(Verb::Equity),
        "snapshot" | "snap" => Ok(Verb::Snapshot),
        "recent" => match rest.first() {
            None => Ok(Verb::Recent(None)),
            Some(n) => {
                let n: usize = n.parse().map_err(|_| format!("recent: not a count: {n:?}"))?;
                Ok(Verb::Recent(Some(n)))
            }
        },
        "help" | "?" => Ok(Verb::Help),
        "quit" | "exit" | "q" => Ok(Verb::Quit),
        // ⚠ THE MIGRATION HINT, and it is deliberately an `Err` rather than an alias. `state` is
        // removed, not renamed — the no-shims rule applied to a grammar, and a shim that still
        // worked would keep alive the exact line (`state halted`) ruling 17 exists to delete. But
        // this prompt is where the removed word gets typed under pressure, off a runbook somebody
        // wrote in 2026, at the moment they most need the daemon to stop: an operator who reads
        // `unknown command: "state"` and has to go and find a page has been made slower by a safety
        // fix. So the refusal REFUSES and then names both replacements, in the order they are
        // needed. It sits here rather than in the arms above because there is no `state` arm any
        // more and there must not be one — this is the unknown-verb catch-all, answering one word
        // by name. [`STATE_MIGRATION_HINT`] is the sentence; `run` prints the same one for the
        // one-shot spelling, so the two surfaces cannot come to say different things.
        "state" => Err(STATE_MIGRATION_HINT.to_string()),
        other => Err(format!("unknown command: {other:?}")),
    }
}
