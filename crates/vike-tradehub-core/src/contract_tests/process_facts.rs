//! The process-wide facts each mount is handed, and labelled accounts spoken for though unmounted.
use super::*;

/// **Every start is handed the process-wide HALT sentinel's path as DATA** (decision 0099): the
/// one place a bridge's path comes from the memoized resolver, so live exec clients watch exactly
/// the file the paper books and the daemon's startup advisory name — not a second sentinel.
#[test]
fn process_facts_hand_every_start_the_process_wide_sentinel_path() {
    let facts = crate::contract::adapter_process_facts();
    assert!(!facts.halt_path.as_os_str().is_empty(), "an empty sentinel path watches nothing");
    assert_eq!(
        facts.halt_path,
        crate::halt::halt_path_from_env(),
        "the mount's sentinel must be the process's one resolved sentinel"
    );
    // …and the PROBE's view resolves nothing: the resolver memoizes and logs, which a question
    // must not do from a root that never declared a project.
    assert!(crate::contract::process_facts().halt_path.as_os_str().is_empty());
    // The directories are the same either way.
    let probe = crate::contract::process_facts();
    assert_eq!((&facts.state_dir, &facts.bin_dir), (&probe.state_dir, &probe.bin_dir));
}

// ---- an account the fan-out never starts is still spoken for ----
// `make_engine_accounts` starts an account only when it runs (`accounts_to_run`), so one that
// resolved paper for want of keys never reaches `start`, where the "live-tier key set is stored
// and unused" line is said. The fold asks `VenueAdapter::report_not_running_account` for exactly
// those — the DEFAULT account included since decision 0156, which no longer starts a paper engine
// to hear its bridge out: not an account that reached `start` (its `start` speaks), not a `paper`
// tier (silent by design). It returns nothing, so the start set is held unchanged. Planted:
// the property is the FOLD's; each bridge's `adapter_tests.rs` holds what it says.

/// `resolve` scripted per label; records each account it speaks for with its `AdapterInputs::tier`.
struct SpeaksForUnmounted {
    spoken_for: Mutex<Vec<(String, AccountTier)>>,
}

impl VenueAdapter for SpeaksForUnmounted {
    fn venue(&self) -> &'static str {
        "alpaca"
    }
    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration { addresses_accounts: true, ..PLANTED_DECLARATION }
    }
    fn resolve(&self, inputs: &AdapterInputs<'_>) -> Resolution {
        match inputs.account.text() {
            Some("ARMED") => Resolution::Armed {
                tier: AccountTier::Demo,
                held_below_live: Some(HeldBelowLive::DemoOnlyArm),
            },
            Some("UNWIRED" | "LIVECAP") => Resolution::Paper(PaperCause::LiveTierNotWired),
            Some("NOSDK") => Resolution::Paper(PaperCause::SdkAbsent),
            _ => Resolution::Paper(PaperCause::NoCredentials),
        }
    }
    fn start(&self, _req: AdapterRequest<'_>) -> AdapterOutcome {
        AdapterOutcome::paper()
    }
    fn report_not_running_account(&self, inputs: &AdapterInputs<'_>) {
        self.spoken_for.lock().expect("lock").push((inputs.account.to_string(), inputs.tier));
    }
}

static SPEAKS: SpeaksForUnmounted = SpeaksForUnmounted { spoken_for: Mutex::new(Vec::new()) };
static SPEAKS_REG: [VenueRow; 1] = [VenueRow::Adapter(&SPEAKS)];

/// One fan-out over the planted venue; returns the labels it started (the default account first)
/// and every account it did NOT run, with why.
fn fan_out(policy: &crate::AdapterPolicy) -> (Vec<String>, Vec<(String, ArmingBlock)>) {
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let (vars, risk) = (HashMap::new(), budget());
    let mut env = crate::AdapterEnv::new(&SPEAKS_REG, &vars, &tx, &mut live);
    env.risk_profile = Some(&risk);
    env.policy = Some(policy);
    let mounted = crate::make_engine_accounts(&mut env, "alpaca", &[], &[])
        .unwrap_or_else(|e| panic!("a planted fan-out never refuses: {e}"));
    assert!(live.is_empty(), "nothing here may mount live: {live:?}");
    let running = mounted.running.into_iter().map(|(l, _)| l.to_string()).collect();
    let stopped =
        mounted.not_running.into_iter().map(|nr| (nr.account.to_string(), nr.block)).collect();
    (running, stopped)
}

#[test]
fn only_an_account_that_is_never_started_is_spoken_for() {
    let label = |text: &str| AccountLabel::parse(text).expect("a legal label");
    // Each account's own row states its tier: the bridge is handed that tier.
    let policy = crate::AdapterPolicy::default()
        .with_account("alpaca", &AccountLabel::Default, AccountTier::Live)
        .with_account("alpaca", &label("UNWIRED"), AccountTier::Demo)
        .with_account("alpaca", &label("NOCREDS"), AccountTier::Demo)
        .with_account("alpaca", &label("NOSDK"), AccountTier::Demo)
        .with_account("alpaca", &label("LIVECAP"), AccountTier::Live)
        .with_account("alpaca", &label("ARMED"), AccountTier::Demo)
        .with_account("alpaca", &label("DISARMED"), AccountTier::Paper);
    SPEAKS.spoken_for.lock().expect("lock").clear();
    let (mounted, mut stopped) = fan_out(&policy);

    // Exactly the accounts that RUN (decision 0156): the `paper` row's simulator. ARMED armed and
    // reached `start`, whose planted answer built no client, so it does not run either; the
    // DEFAULT account, whose bridge resolved paper, no longer starts a paper engine by itself.
    assert_eq!(mounted, ["DISARMED"], "only the account that runs at its row's tier");
    stopped.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        stopped,
        [
            ("ARMED".to_string(), ArmingBlock::AdapterFailed),
            ("DEFAULT".to_string(), ArmingBlock::NoCredentials),
            ("LIVECAP".to_string(), ArmingBlock::LiveTierNotWired),
            ("NOCREDS".to_string(), ArmingBlock::NoCredentials),
            ("NOSDK".to_string(), ArmingBlock::SdkAbsent),
            ("UNWIRED".to_string(), ArmingBlock::LiveTierNotWired),
        ],
        "every other account is NOT RUNNING with its reason"
    );

    let mut spoken: Vec<(String, AccountTier)> = SPEAKS.spoken_for.lock().expect("lock").clone();
    spoken.sort();
    assert_eq!(
        spoken,
        [
            // Each account's own tier is what the bridge is handed.
            ("DEFAULT".to_string(), AccountTier::Live),
            ("LIVECAP".to_string(), AccountTier::Live),
            ("NOCREDS".to_string(), AccountTier::Demo),
            ("NOSDK".to_string(), AccountTier::Demo),
            ("UNWIRED".to_string(), AccountTier::Demo),
        ],
        "exactly the accounts that resolved paper for want of keys (or of the shim), at a tier \
         above `paper` — the default account included, not ARMED (it reached `start`, which \
         speaks), not DISARMED (its tier is `paper`)"
    );

    // …and once per start per account: a second fan-out is a second start.
    SPEAKS.spoken_for.lock().expect("lock").clear();
    fan_out(&policy);
    assert_eq!(SPEAKS.spoken_for.lock().expect("lock").len(), 5, "once per account per fan-out");
}
