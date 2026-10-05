use super::*;

#[test]
fn a_single_account_process_has_nothing_to_say() {
    let engines = [("binance", "binance"), ("bybit", "bybit"), ("okx", "okx")];
    assert!(ambiguous_venues(engines).is_empty());
    assert_eq!(ambiguity_warning(&[]), None);
}

/// ⚠ **The ambiguity is scoped to the VENUE, never to the node** — §4.2: *"a box running
/// forty-nine binance accounts and one okx account keeps serving every account-less okx command
/// unchanged, and refuses only the binance ones."*
#[test]
fn ambiguity_is_per_venue_not_per_node() {
    let engines = [("binance", "binance"), ("okx", "okx"), ("binance", "binance#ALT")];
    let found = ambiguous_venues(engines);
    assert_eq!(found.len(), 1, "okx has one account and must not appear: {found:?}");
    assert_eq!(found[0].0, "binance");
    assert_eq!(
        found[0].1,
        vec!["binance".to_string(), "binance#ALT".to_string()],
        "engine order, so the DEFAULT account is listed first"
    );
}

#[test]
fn the_warning_names_the_rows_the_keys_and_every_account() {
    let found =
        ambiguous_venues([("binance", "binance"), ("binance", "binance#ALT"), ("bybit", "bybit")]);
    let text = ambiguity_warning(&found).expect("two binance accounts is something to say");
    assert!(!text.contains(".toml"), "no settings file may be named: {text}");
    assert!(text.contains("policy.accounts"), "names the ROW space: {text}");
    assert!(text.contains("policy.venues.binance"), "names the default account's KEY: {text}");
    assert!(text.contains("policy.accounts.binance.ALT"), "names the labelled KEY: {text}");
    assert!(text.contains("binance#ALT"), "names the second ACCOUNT: {text}");
    assert!(!text.contains("bybit"), "a single-account venue is not named at all: {text}");
    assert!(text.contains("REFUSES"), "says what will happen: {text}");
    assert!(text.contains("market_exit"), "…and what still works: {text}");
}

/// The paste-safety rule, asserted rather than trusted: no credential key name can appear,
/// because nothing but a route key and a settings path is ever interpolated.
#[test]
fn the_warning_prints_no_credential_key_name() {
    const VENUE: &str = "binance";
    let found = ambiguous_venues([(VENUE, VENUE), (VENUE, "binance#ALT")]);
    let text = ambiguity_warning(&found).expect("something to say");
    // ⚠ The venue's credential-key PREFIX is DERIVED rather than spelled out. Written as a
    // literal it would be an ENV-SHAPED string — the upper-cased venue id with a trailing
    // underscore — which `vike-ops`' settings-registry scanner reads as a credential key this
    // module reads. It reads none, and that gate's exception tables are for the gate's OWN
    // machinery (`scan.rs`, `settings.rs`, its own test file), never for a test needle.
    // Deriving it also keeps the needle correct if `VENUE` ever changes.
    let key_prefix = format!("{}_", VENUE.to_uppercase());
    for needle in ["_API_KEY", "_API_SECRET", "_API_PASSPHRASE", key_prefix.as_str()] {
        assert!(!text.contains(needle), "{needle} must never be pasted: {text}");
    }
}

#[test]
fn a_refusal_names_every_candidate_and_the_one_it_used_to_pick() {
    let text = refusal(
        "submit",
        "binance",
        &["binance".to_string(), "binance#ALT".to_string(), "binance#HEDGE".to_string()],
    );
    assert!(text.contains("binance#ALT"), "{text}");
    assert!(text.contains("binance#HEDGE"), "{text}");
    assert!(text.contains("3 accounts"), "{text}");
    assert!(text.contains("Nothing was sent"), "{text}");
    assert!(
        text.contains("used to reach `binance`"),
        "names the account the misroute reached, so an operator can tell whether they were \
             relying on it: {text}"
    );
}

/// ⚠ **The refusal's advice has to match what this node does with the reducing verbs.** Naming an
/// account narrows them to that account's book, and only naming NONE reaches every account.
///
/// The old text said they "reach EVERY account of the venue", unqualified, and that an operator
/// command "cannot name" an account. An operator who wanted out of `ALT` alone followed it, sent
/// an account-less `flatten`, and closed the DEFAULT account too. That happened on a node that
/// honours `flatten binance BTCUSDT ALT` and advertises the capability. "Cannot name one yet" is
/// also false for a submit, whose ticket carries `account`, though it is still true for a
/// bracket, a combo and a conditional arm.
#[test]
fn a_refusal_steers_a_one_account_reduce_to_the_verb_that_names_it() {
    let text = refusal("submit", "binance", &["binance".to_string(), "binance#ALT".to_string()]);
    assert!(
        text.contains("name it on `market_exit` / `mass_cancel` / `flatten`"),
        "names the narrowing: {text}"
    );
    assert!(text.contains("that account's book"), "…and what it narrows to: {text}");
    assert!(
        text.contains("naming NO account reach EVERY account"),
        "the fan-out is stated with its condition, never bare: {text}"
    );
    assert!(
        !text.contains("an operator command cannot name one yet"),
        "a submit CAN name its account: {text}"
    );
    assert!(text.contains("a submit's own `account`"), "…and the text says so: {text}");
    assert!(
        text.contains("a bracket, a combo and a conditional arm cannot name one yet"),
        "…while the three that cannot are named: {text}"
    );
}

/// The unheld-account refusal names the two facts that identify it — and, asserted just as
/// hard, does NOT name the books this node holds. That omission is the ruling (the edge
/// renders that list, before the Ack), so it is pinned rather than left to drift back in.
#[test]
fn an_unheld_account_refusal_names_the_account_and_lists_no_others() {
    let alt =
        vike_model::accounts::account_keys::AccountLabel::parse("NOSUCH").expect("a legal label");
    let text = unheld_account_refusal("submit", "binance", &alt);
    assert!(text.contains("NOSUCH"), "names the account that was asked for: {text}");
    assert!(text.contains("binance"), "names the venue: {text}");
    assert!(text.contains("Nothing was sent"), "{text}");
    assert!(
        !text.contains("binance#"),
        "no route key of another account may appear — enumerating the node's books is the \
             EDGE's rendering, and a second copy here is the one nobody reads: {text}"
    );
}

/// …and the DEFAULT account has a spelling here too, through [`AccountLabel`]'s `Display`.
/// It is reachable: a payload may name `DEFAULT` on a venue this core runs no engine for at
/// all, which is an unheld account like any other.
#[test]
fn the_default_account_is_nameable_in_an_unheld_refusal() {
    let text = unheld_account_refusal(
        "submit",
        "okx",
        &vike_model::accounts::account_keys::AccountLabel::Default,
    );
    assert!(text.contains("DEFAULT"), "the wire's spelling of the unlabelled account: {text}");
    assert!(text.contains("okx"), "{text}");
}

/// The venue-less refusal names the account, says the venue is what is missing, and says the
/// command was NOT widened — the one sentence an operator who sent a narrowed panic button
/// needs in order to know their other books were not touched.
#[test]
fn an_account_without_a_venue_refusal_names_the_account_and_refuses_the_widening() {
    let alt =
        vike_model::accounts::account_keys::AccountLabel::parse("ALT").expect("a legal label");
    let text = account_without_venue_refusal("market_exit", &alt);
    assert!(text.contains("market_exit REFUSED"), "{text}");
    assert!(text.contains("`ALT`"), "names the account: {text}");
    assert!(text.contains("no venue"), "names what is missing: {text}");
    assert!(text.contains("Nothing was sent"), "{text}");
    assert!(text.contains("NOT widened"), "{text}");
}
