//! The process-wide facts each mount is handed, and labelled accounts spoken for though unmounted.
use super::*;

/// **Every mount is handed the process-wide HALT sentinel's path as DATA** (decision 0099): the
/// one place a bridge's path comes from the memoized resolver, so live exec clients watch exactly
/// the file the paper books and the daemon's startup advisory name — not a second sentinel.
#[test]
fn process_facts_hand_every_mount_the_process_wide_sentinel_path() {
    let facts = crate::contract::mount_process_facts();
    assert!(!facts.halt_path.as_os_str().is_empty(), "an empty sentinel path watches nothing");
    assert_eq!(
        facts.halt_path,
        vike_bridge_core::halt::halt_path_from_env(),
        "the mount's sentinel must be the process's one resolved sentinel"
    );
    // …and the PROBE's view resolves nothing: the resolver memoizes and logs, which a question
    // must not do from a root that never declared a project.
    assert!(crate::contract::process_facts().halt_path.as_os_str().is_empty());
    // The directories are the same either way.
    let probe = crate::contract::process_facts();
    assert_eq!((&facts.state_dir, &facts.bin_dir), (&probe.state_dir, &probe.bin_dir));
}

// ---- a LABELLED account the fan-out never mounts is still spoken for ----
// `make_engine_accounts` mounts a labelled account only when it armed (`accounts_to_mount`), so
// one that resolved paper for want of keys never reaches `mount`, where the "live-tier key set is
// stored and unused" line is said. The fold asks `VenueMount::report_unmounted_account` for
// exactly those: not the default account or an armed one (their `mount` speaks), not a `paper`
// ceiling (silent by design). It returns nothing, so the mount set is held unchanged. Planted:
// the property is the FOLD's; each bridge's `mount_tests.rs` holds what it says.

/// `resolve` scripted per label; records each account it speaks for with its `live_permitted`.
struct SpeaksForUnmounted {
    spoken_for: Mutex<Vec<(String, bool)>>,
}

impl VenueMount for SpeaksForUnmounted {
    fn venue(&self) -> &'static str {
        "alpaca"
    }
    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration { addresses_accounts: true, ..PLANTED_DECLARATION }
    }
    fn resolve(&self, inputs: &MountInputs<'_>) -> Resolution {
        match inputs.account.text() {
            Some("ARMED") => Resolution::Armed {
                tier: Tier::Demo,
                held_below_live: Some(HeldBelowLive::DemoOnlyArm),
            },
            Some("UNWIRED" | "LIVECAP") => Resolution::Paper(PaperCause::LiveTierNotWired),
            Some("NOSDK") => Resolution::Paper(PaperCause::SdkAbsent),
            _ => Resolution::Paper(PaperCause::NoCredentials),
        }
    }
    fn mount(&self, _req: MountRequest<'_>) -> MountOutcome {
        MountOutcome::paper()
    }
    fn report_unmounted_account(&self, inputs: &MountInputs<'_>) {
        self.spoken_for
            .lock()
            .expect("lock")
            .push((inputs.account.to_string(), inputs.live_permitted));
    }
}

static SPEAKS: SpeaksForUnmounted = SpeaksForUnmounted { spoken_for: Mutex::new(Vec::new()) };
static SPEAKS_REG: [VenueRow; 1] = [VenueRow::Mount(&SPEAKS)];

/// One fan-out over the planted venue; returns the labels it mounted (the default account first).
fn fan_out(policy: &crate::MountPolicy) -> Vec<String> {
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let (vars, risk) = (HashMap::new(), budget());
    let mut env = crate::MountEnv::new(&SPEAKS_REG, &vars, &tx, &mut live);
    env.risk_profile = Some(&risk);
    env.policy = Some(policy);
    let mounted = crate::make_engine_accounts(&mut env, "alpaca", &[], &[])
        .expect("a planted paper mount never refuses");
    assert!(live.is_empty(), "nothing here may mount live: {live:?}");
    mounted.into_iter().map(|(l, _)| l.to_string()).collect()
}

#[test]
fn only_a_labelled_account_that_is_never_mounted_is_spoken_for() {
    let label = |text: &str| AccountLabel::parse(text).expect("a legal label");
    // The venue line is `live`, so an account line of `live` is permitted and one of `demo` is capped.
    let policy = crate::MountPolicy {
        venues: VenuePolicy::default()
            .declare("alpaca", VenueMode::Live)
            .declare_account("alpaca", &label("UNWIRED"), VenueMode::Demo)
            .declare_account("alpaca", &label("NOCREDS"), VenueMode::Demo)
            .declare_account("alpaca", &label("NOSDK"), VenueMode::Demo)
            .declare_account("alpaca", &label("LIVECAP"), VenueMode::Live)
            .declare_account("alpaca", &label("ARMED"), VenueMode::Demo)
            .declare_account("alpaca", &label("DISARMED"), VenueMode::Paper),
        ..Default::default()
    };
    SPEAKS.spoken_for.lock().expect("lock").clear();
    let mounted = fan_out(&policy);

    // Exactly `accounts_to_mount`'s selection: the default account and the one that armed.
    assert_eq!(mounted, ["DEFAULT", "ARMED"], "the mount set is unchanged");

    let mut spoken: Vec<(String, bool)> = SPEAKS.spoken_for.lock().expect("lock").clone();
    spoken.sort();
    assert_eq!(
        spoken,
        [
            // A `live` ceiling hands the bridge `live_permitted = true`; a `demo` one, false.
            ("LIVECAP".to_string(), true),
            ("NOCREDS".to_string(), false),
            ("NOSDK".to_string(), false),
            ("UNWIRED".to_string(), false),
        ],
        "exactly the labelled accounts that resolved paper for want of keys (or of the shim), under \
         a ceiling above `paper` — not the default account (its `mount` speaks), not ARMED \
         (mounted), not DISARMED (its ceiling is `paper`)"
    );

    // …and once per start per account: a second fan-out is a second start.
    SPEAKS.spoken_for.lock().expect("lock").clear();
    fan_out(&policy);
    assert_eq!(SPEAKS.spoken_for.lock().expect("lock").len(), 4, "once per account per fan-out");
}
