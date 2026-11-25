//! The rows proven ON THEIR OWN SEAM. An exec lane that is its own loop or a native session
//! (`VenueConnectionDriver::OwnLoop`, `VenueConnectionDriver::NativeSession`) has no `run_user` stream for the scenarios
//! to script, so a cell is proven where the venue's OWN suite drives its REAL code over a seam of its own (a fake venue,
//! a fake sidecar, a loopback peer, the gatewayhub's supervision suite), by the tests it names. CI runs each in its own
//! crate; this harness holds the citations (`every_cell_proven_on_its_seam_names_a_test_that_exists` reads each file at
//! run time, so a proof that is renamed or deleted reddens here instead of leaving its cell claiming it) and prints
//! them in `coverage_matrix`.
//!
//! A cell is [`SessionCell::Proven`] only by tests that pin the scenario's property on that venue's real code, and its
//! words say what they do not reach. A cell no test proves is [`SessionCell::Deferred`] with the shape that has no seam,
//! or the defect it found; a scenario a row does not list is DEFERRED too ([`SessionRow::cell`]). S7 is the row's
//! recovery kind's to decide, as on a covered row. A venue with no row here stays DEFERRED whole, with its shape.

use vike_venues::venue_connection::VenueConnectionRecovery;

use super::Scenario;

/// One test: its file from the repository root, and its name.
pub(super) struct Proof {
    pub file: &'static str,
    pub test: &'static str,
}

/// A scenario on a row proven on its own seam.
pub(super) enum SessionCell {
    /// Pinned by the tests `by`; `pins` says what they hold, and what they do not reach.
    Proven { by: &'static [Proof], pins: &'static str },
    /// The row's shape has nothing for the scenario to test, with why.
    NotApplicable(&'static str),
    /// It applies and no test proves it on this seam, with the shape that has none or the defect found.
    Deferred(&'static str),
}

/// A venue's exec row proven on its own seam.
pub(super) struct SessionRow {
    pub venue: &'static str,
    /// The seam its proofs drive.
    pub seam: &'static str,
    pub cells: &'static [(Scenario, SessionCell)],
}

static UNLISTED: SessionCell = SessionCell::Deferred("no test proves it on this seam");
static NO_HISTORY: SessionCell =
    SessionCell::NotApplicable("its recovery reads no venue history, so no history can lag");

impl SessionRow {
    /// What `scenario` is on this row, whose recovery kind is `recovery`.
    pub(super) fn cell(
        &self,
        scenario: Scenario,
        recovery: VenueConnectionRecovery,
    ) -> &'static SessionCell {
        if scenario == Scenario::LaggingHistory
            && recovery != VenueConnectionRecovery::HistoryWindow
        {
            return &NO_HISTORY;
        }
        self.cells
            .iter()
            .find(|(listed, _)| *listed == scenario)
            .map_or(&UNLISTED, |(_, cell)| cell)
    }
}

/// The rows. Extending: a venue whose own suite gains a seam adds its row here, naming the tests.
pub(super) const ROWS: &[SessionRow] = &[IG, CTRADER, DUKASCOPY, FXCM, IBKR];

const IG_RETRY: &str = "crates/bridges/ig/src/execution/user_data/retry_tests.rs";
const IG_STREAM: &str = "crates/bridges/ig/src/execution/user_data_tests.rs";
const EXEC_HARNESS: &str = "crates/vike-bridge-core/tests/bridge_conformance.rs";
/// The exec harness' reconnect row: a fill the shared pump's replay delivers again folds once, by trade id.
const EXEC_HARNESS_REPLAY: Proof = Proof {
    file: EXEC_HARNESS,
    test: "reconnect_mid_order_no_duplicate_or_lost_terminal_all_covered_venues",
};

/// IG's Lightstreamer trade stream.
const IG: SessionRow = SessionRow {
    venue: "ig",
    seam: "the trade stream's pure retry plan (`crates/bridges/ig/src/execution/user_data/retry.rs`'s `RetryState`), its \
           line decoder (`crates/bridges/ig/src/execution/user_data.rs`'s `events_for_line`) and its agent against \
           loopback peers; the TLCP transport itself (`crates/bridges/ig/src/execution/user_data.rs`'s `run_once`) is \
           verifiable only against a live IG server",
    cells: &[
        (
            Scenario::Drop,
            SessionCell::Proven {
                by: &[
                    Proof {
                        file: IG_RETRY,
                        test: "a_session_that_ran_and_dropped_is_still_logged_every_time",
                    },
                    Proof {
                        file: IG_RETRY,
                        test: "the_reconnect_bounds_are_the_execution_rows_one_and_sixty_seconds",
                    },
                    Proof {
                        file: IG_STREAM,
                        test: "trade_stream_request_bodies_pin_the_wire_bytes",
                    },
                ],
                pins: "a session that streamed and dropped is warned about every time and dialled again after the \
                       row's first wait (1 s), spending no login; every session subscribes with `LS_snapshot=true`, \
                       so the reopen re-delivers the trade state",
            },
        ),
        (
            Scenario::ServerClose,
            SessionCell::Proven {
                by: &[Proof {
                    file: IG_RETRY,
                    test: "a_clean_session_resets_the_streak_the_backoff_and_the_heartbeat",
                }],
                pins: "a session the server ended (`LOOP` or `END`, which `run_once` answers as a session that ran) \
                       says nothing and dials again after the row's first wait, every streak reset; reading the \
                       `LOOP` or `END` line is the transport's, which no offline test drives",
            },
        ),
        (
            Scenario::Expiry,
            SessionCell::Proven {
                by: &[
                    Proof {
                        file: IG_RETRY,
                        test: "only_the_permanent_refused_path_backs_off_and_only_it_stops_re_logging_in",
                    },
                    Proof {
                        file: IG_RETRY,
                        test: "the_refused_backoff_doubles_from_the_flat_delay_and_capabilities_at_the_ceiling",
                    },
                ],
                pins: "a refused handshake (`CONERR`: the CST/XST pair expired) logs in again before the next dial, at \
                       most `MAX_REFUSED_HANDSHAKES` times at the row's first wait; past that it spends no login and \
                       backs off doubling to the row's 60 s ceiling, never a hot loop",
            },
        ),
        (
            Scenario::Stall,
            SessionCell::Proven {
                by: &[
                    Proof {
                        file: IG_STREAM,
                        test: "the_trade_stream_agent_carries_the_execution_rows_idle_bound",
                    },
                    Proof {
                        file: IG_STREAM,
                        test: "the_trade_stream_agent_gives_up_on_a_host_that_never_answers",
                    },
                ],
                pins: "every read of the stream waits at most the row's `idle_threshold` (30 s, on the built agent), \
                       and a peer that takes the request and says nothing is given up on at that bound (a 500 ms \
                       agent); the server's `PROBE` lines keep a healthy read short of it",
            },
        ),
        (
            Scenario::GapFill,
            SessionCell::Proven {
                by: &[
                    Proof {
                        file: IG_STREAM,
                        test: "trade_stream_request_bodies_pin_the_wire_bytes",
                    },
                    Proof {
                        file: IG_STREAM,
                        test: "a_position_opened_is_the_fill_pair_stamped_with_its_own_utc_timestamp",
                    },
                ],
                pins: "every session subscribes with `LS_snapshot=true`, and a position OPENED in what it delivers \
                       decodes to the fill pair keyed on its `dealId` and stamped with the venue's own time; what \
                       IG's snapshot holds after a real gap is unmeasured",
            },
        ),
        (
            Scenario::BusyCommands,
            SessionCell::NotApplicable(
                "its exec thread runs its own command loop (`crates/bridges/ig/src/execution/mod.rs`'s `run`) with no \
                 gap-sentinel: a working order's later fill rides the trade stream and its snapshot",
            ),
        ),
        (
            Scenario::DoubleDelivery,
            SessionCell::Proven {
                by: &[
                    Proof {
                        file: IG_STREAM,
                        test: "a_position_opened_is_the_fill_pair_stamped_with_its_own_utc_timestamp",
                    },
                    EXEC_HARNESS_REPLAY,
                ],
                pins: "a streamed fill and its snapshot re-delivery are one `OPU`, keyed on the position's `dealId`, \
                       and the exec harness folds a fill of IG's real stream decoder delivered twice once",
            },
        ),
        (
            Scenario::Keepalive,
            SessionCell::NotApplicable(
                "the server's `PROBE` lines are the keepalive, on the interval its `CONOK` announces; the lane sends \
                 nothing of its own, and its read bound (S5) is its watch",
            ),
        ),
        (
            Scenario::DialBound,
            SessionCell::Proven {
                by: &[
                    Proof {
                        file: IG_STREAM,
                        test: "the_trade_stream_agent_carries_the_execution_rows_idle_bound",
                    },
                    Proof {
                        file: IG_STREAM,
                        test: "the_trade_stream_agent_gives_up_on_a_host_that_never_answers",
                    },
                ],
                pins: "the agent's connect window is the row's `dial_timeout` (10 s), and a peer that accepts and \
                       never answers is cut at the read bound",
            },
        ),
    ],
};

const CT_CONNECTION: &str = "crates/bridges/ctrader/src/transport/connection/connection_tests.rs";
const CT_RECONNECT: &str = "crates/bridges/ctrader/tests/offline/connection_reconnect.rs";
const CT_LAPSE: &str = "crates/bridges/ctrader/tests/offline/session_lapse.rs";

/// cTrader's protobuf session actor.
const CTRADER: SessionRow = SessionRow {
    venue: "ctrader",
    seam: "`FakeCtrader` (`crates/bridges/ctrader/src/test_support/fake.rs`), a scripted cTrader server on loopback TCP \
           that the REAL session actor dials, authorizes and reconnects to; production timings are pinned beside it, \
           and a test narrows only the idle bound and the heartbeat",
    cells: &[
        (
            Scenario::Drop,
            SessionCell::Proven {
                by: &[
                    Proof { file: CT_CONNECTION, test: "the_reconnects_waits_are_pinned_verbatim" },
                    Proof {
                        file: CT_RECONNECT,
                        test: "actor_reconnects_and_reauths_after_a_dropped_socket",
                    },
                    Proof {
                        file: CT_RECONNECT,
                        test: "reconcile_rebuilds_order_client_id_order_map_on_reconnect",
                    },
                ],
                pins: "a dropped socket is dialled again on the row's ladder (1 s doubling to the 5 s ceiling, each \
                       step spread under it, real waits), the whole two-stage handshake runs again, and the new session re-reads the venue's \
                       pending orders and rebuilds the order map",
            },
        ),
        (
            Scenario::ServerClose,
            SessionCell::Proven {
                by: &[Proof {
                    file: CT_LAPSE,
                    test: "an_account_disconnect_event_opens_a_new_session_and_asks_for_the_streams_again",
                }],
                pins: "raw TCP carries no close code: the venue ends a session by `ProtoOAAccountDisconnectEvent`, \
                       which opens a new session and asks for the streams again",
            },
        ),
        (
            Scenario::Expiry,
            SessionCell::Proven {
                by: &[
                    Proof {
                        file: CT_LAPSE,
                        test: "a_token_refused_at_the_new_session_is_renewed_there_and_the_socket_comes_back",
                    },
                    Proof {
                        file: CT_LAPSE,
                        test: "a_refused_re_auth_spends_no_refresh_token_in_a_loop",
                    },
                    Proof {
                        file: "crates/bridges/ctrader/tests/offline/connection_retry.rs",
                        test: "permanent_auth_rejection_fails_fast_without_burning_retries",
                    },
                ],
                pins: "a lapsed access token is renewed once by the reconnect and the socket comes back, a refused \
                       re-auth spends no refresh token in a loop, and a permanent refusal of the first connect fails \
                       at once",
            },
        ),
        (
            Scenario::Stall,
            SessionCell::Proven {
                by: &[
                    Proof {
                        file: CT_RECONNECT,
                        test: "a_blackholed_socket_is_called_dead_at_the_read_idle_deadline",
                    },
                    Proof { file: CT_RECONNECT, test: "a_quiet_socket_that_answers_is_left_alone" },
                ],
                pins: "a socket the venue answers nothing on is called dead at the idle bound and dialled again, and a \
                       quiet socket that answers the actor's question is left alone (the bound narrowed to 600 ms; \
                       production's is the row's 30 s, three heartbeats, held by a compile-time assert beside \
                       `READ_IDLE_AFTER`)",
            },
        ),
        (
            Scenario::GapFill,
            SessionCell::Proven {
                by: &[
                    Proof {
                        file: CT_RECONNECT,
                        test: "a_market_order_that_filled_while_nobody_listened_books_its_fill",
                    },
                    Proof {
                        file: CT_RECONNECT,
                        test: "execution_event_not_dropped_during_reconcile",
                    },
                ],
                pins: "an order whose answer the dead socket held is asked about on the new session and books the \
                       venue's own fill under its deal id, and a fill that lands during the reconnect's reconcile is \
                       kept; a RESTING order's gap fill is only warned about (the row's `why`), so it is reconcile's",
            },
        ),
        (
            Scenario::BusyCommands,
            SessionCell::NotApplicable(
                "its commands ride the session actor's queue \
                 (`crates/bridges/ctrader/src/transport/connection/actor.rs`'s `actor_loop`) with no gap-sentinel: the \
                 reopened session asks about what the dead socket left unanswered",
            ),
        ),
        (
            Scenario::DoubleDelivery,
            SessionCell::Proven {
                by: &[
                    Proof {
                        file: "crates/bridges/ctrader/tests/offline/execution_mapper.rs",
                        test: "order_filled_dual_publishes_fill_then_order_filled",
                    },
                    Proof {
                        file: CT_RECONNECT,
                        test: "a_market_order_that_filled_while_nobody_listened_books_its_fill",
                    },
                ],
                pins: "the live execution event and the new session's deal read both key a fill on the venue's deal \
                       id, the engine's trade-id dedup key",
            },
        ),
        (
            Scenario::Keepalive,
            SessionCell::Proven {
                by: &[
                    Proof {
                        file: CT_CONNECTION,
                        test: "the_connection_runs_on_the_declared_timings",
                    },
                    Proof {
                        file: CT_CONNECTION,
                        test: "the_short_read_does_not_quicken_the_heartbeat_or_the_probe",
                    },
                ],
                pins: "a `HEARTBEAT_EVENT` at the row's `keepalive_every` (10 s) and the account probe on its own \
                       clock, neither quickened by the exec actor's short read (narrowed to 200 ms and 300 ms)",
            },
        ),
        (
            Scenario::DialBound,
            SessionCell::Proven {
                by: &[
                    Proof {
                        file: "crates/bridges/ctrader/tests/offline/connection_retry.rs",
                        test: "a_peer_that_takes_the_dial_and_says_nothing_is_cut_at_the_rows_reply_window",
                    },
                    Proof {
                        file: CT_CONNECTION,
                        test: "the_connection_runs_on_the_declared_timings",
                    },
                ],
                pins: "the REAL first connect (`connect_and_auth`, plaintext as `FakeCtrader` speaks), dialled at \
                       `vike_bridge_core::silent_peer`'s peer, is cut at the row's `response_timeout` (15 s, real \
                       time), the wait for the handshake's first reply: a loopback peer takes the SYN at once, so the \
                       row's 10 s `dial_timeout`, pinned beside it, bounds only a TCP connect no offline peer can \
                       black-hole; the TLS wrap production adds is not driven",
            },
        ),
    ],
};

const DK_RESTART: &str = "crates/bridges/dukascopy/tests/sidecar/dukascopy_restart.rs";
const DK_EXEC: &str = "crates/bridges/dukascopy/tests/sidecar/dukascopy_exec.rs";

/// Dukascopy's JForex sidecar.
const DUKASCOPY: SessionRow = SessionRow {
    venue: "dukascopy",
    seam: "the scripted fake sidecar (`crates/bridges/dukascopy/src/bin/fake_jforex_bridge.rs`), run as a real child \
           process over the JSON-lines stdio the JForex sidecar speaks, under the client's REAL supervisor; the JForex \
           half (the reconnect inside a live sidecar and its replay) has no CI seam",
    cells: &[
        (
            Scenario::Drop,
            SessionCell::Proven {
                by: &[
                    Proof {
                        file: DK_RESTART,
                        test: "the_production_policy_brings_a_dead_sidecar_back_within_its_first_wait",
                    },
                    Proof {
                        file: DK_RESTART,
                        test: "a_sidecar_that_dies_idle_is_restarted_by_itself_and_a_later_submit_is_filled",
                    },
                ],
                pins: "a sidecar that dies mid-session (JForex ended its session) is started again by the client \
                       within the row's first wait (5 s, the production policy, end to end) and fills the next order; \
                       JForex's own reconnect inside a live sidecar is driven by no CI test \
                       (`crates/bridges/dukascopy/jforex-bridge/src/main/java/vike/jforex/Bridge.java`'s `onDisconnect`)",
            },
        ),
        (
            Scenario::ServerClose,
            SessionCell::Proven {
                by: &[Proof {
                    file: DK_RESTART,
                    test: "the_clients_sidecar_state_is_on_the_cell_the_gatewayhub_lane_reads",
                }],
                pins: "a session JForex ends (`SERVER_SHUTDOWN` ends the sidecar) reads `Down` in the sidecar's own \
                       words from the death to the next sidecar's `ready`, then `Up`",
            },
        ),
        (
            Scenario::Expiry,
            SessionCell::Proven {
                by: &[
                    Proof {
                        file: DK_RESTART,
                        test: "refused_restarts_back_off_within_their_bounds_and_the_sidecar_comes_back",
                    },
                    Proof {
                        file: DK_RESTART,
                        test: "a_sidecar_that_keeps_dying_right_after_ready_is_backed_off_like_a_refused_login",
                    },
                ],
                pins: "a refused login backs the restarts off doubling to the ceiling, never spinning, and the sidecar \
                       comes back with no hand step once a login is allowed; a session ended right after `ready` is \
                       backed off the same way",
            },
        ),
        (
            Scenario::Stall,
            SessionCell::Proven {
                by: &[
                    Proof {
                        file: DK_RESTART,
                        test: "a_sidecar_silent_past_the_idle_limit_is_killed_and_restarted",
                    },
                    Proof {
                        file: DK_RESTART,
                        test: "silence_before_ready_never_trips_the_idle_limit",
                    },
                ],
                pins: "a sidecar that wrote no line for the row's `idle_threshold` (210 s; a millisecond lane in the \
                       test), counted from its `ready`, is killed as wedged, reads `Down` in the limit's words, is \
                       reaped and started again on the ladder, and serves the next order; a slow login's silence before \
                       `ready` never trips it; a real wedged JVM is not driven",
            },
        ),
        (
            Scenario::GapFill,
            SessionCell::Deferred(
                "the gap's fills come back by JForex's replay inside the sidecar, which no Java test and no mode of \
                 the fake sidecar scripts \
                 (`crates/bridges/dukascopy/jforex-bridge/src/main/java/vike/jforex/Bridge.java`'s `replayAfterReconnect`)",
            ),
        ),
        (
            Scenario::BusyCommands,
            SessionCell::NotApplicable(
                "its commands go straight to the sidecar's stdin \
                 (`crates/bridges/dukascopy/src/execution/mod.rs`'s `ExecutionClientDukascopy`), with no gap-sentinel",
            ),
        ),
        (
            Scenario::DoubleDelivery,
            SessionCell::Deferred(
                "the same replay: whether JForex's re-scan names a fill the stream already carried by the same trade \
                 id is unproven offline",
            ),
        ),
        (
            Scenario::Keepalive,
            SessionCell::Proven {
                by: &[
                    Proof {
                        file: DK_RESTART,
                        test: "the_probe_keeps_a_quiet_sidecar_alive_with_the_nodes_pass_off",
                    },
                    Proof {
                        file: "crates/bridges/dukascopy/src/reconcile/query.rs",
                        test: "a_probes_answer_never_answers_an_open_question",
                    },
                ],
                pins: "the supervisor's own `recon` question, asked under `PROBE_ID` once the sidecar has been silent \
                       for the row's `keepalive_every` (60 s), keeps a quiet sidecar with no reconcile pass up past \
                       three idle limits, and its answer never answers a pass's question; JForex keeps its own session",
            },
        ),
        (
            Scenario::DialBound,
            SessionCell::Proven {
                by: &[
                    Proof {
                        file: DK_EXEC,
                        test: "a_hung_first_start_is_killed_and_the_respawn_becomes_the_client",
                    },
                    Proof {
                        file: DK_EXEC,
                        test: "a_login_that_always_hangs_gives_up_after_the_attempts_and_leaves_no_sidecar",
                    },
                    Proof {
                        file: "crates/bridges/dukascopy/src/execution/mod.rs",
                        test: "the_ready_timeout_is_the_execution_rows_three_hundred_seconds",
                    },
                ],
                pins: "a start that never says `ready` is cut at the per-attempt wait and reaped before the next, a \
                       login that always hangs gives up after its attempts with no sidecar left, and the whole start \
                       is bounded by the row's `handshake_timeout` (300 s)",
            },
        ),
    ],
};

const FX_PROBE: &str = "crates/bridges/fxcm/src/account/venue_link_tests.rs";
const FX_SUPERVISED: &str = "crates/vike-gatewayhub/tests/trade/fxcm_reconnect.rs";

/// FXCM's ForexConnect session.
const FXCM: SessionRow = SessionRow {
    venue: "fxcm",
    seam: "the gatewayhub's supervision suite (`crates/vike-gatewayhub/tests/trade/fxcm_reconnect.rs`): the REAL \
           `FxcmSessionProbe` over a status cell written as the exec thread writes it \
           (`crates/bridges/fxcm/src/account/venue_link.rs`'s `from_status`), under the REAL supervisor on an injected \
           FX clock; the ForexConnect SDK is loaded at run time and no CI run links it",
    cells: &[
        (
            Scenario::Drop,
            SessionCell::Proven {
                by: &[
                    Proof {
                        file: FX_PROBE,
                        test: "only_a_final_sdk_state_reports_the_session_lost",
                    },
                    Proof {
                        file: FX_SUPERVISED,
                        test: "a_session_lost_on_a_weekday_is_rebuilt_in_a_new_epoch_and_reconnecting_never_is",
                    },
                ],
                pins: "a dropped session is the SDK's to reconnect: `Connecting` and its own `Reconnecting` never take \
                       the adapter down, so the supervisor never races the SDK; the SDK's reconnect itself is the demo \
                       smoke's",
            },
        ),
        (
            Scenario::ServerClose,
            SessionCell::Proven {
                by: &[
                    Proof {
                        file: FX_PROBE,
                        test: "only_a_final_sdk_state_reports_the_session_lost",
                    },
                    Proof {
                        file: FX_SUPERVISED,
                        test: "a_session_lost_on_a_weekday_is_rebuilt_in_a_new_epoch_and_reconnecting_never_is",
                    },
                ],
                pins: "a session the SDK gives up on (`SessionLost`, `Disconnected`, `Disconnecting`) is a loss: the \
                       adapter is torn down, the lease ended at the loss, and the session rebuilt in a new epoch with \
                       a reconcile owed",
            },
        ),
        (
            Scenario::Expiry,
            SessionCell::Proven {
                by: &[
                    Proof {
                        file: FX_PROBE,
                        test: "a_failed_login_is_down_and_a_refused_credential_says_it_is_not_retried",
                    },
                    Proof {
                        file: FX_SUPERVISED,
                        test: "a_refused_start_after_the_open_backs_off_from_the_first_wait",
                    },
                ],
                pins: "a refused credential is down in words that say nothing logs in again, and a rebuild the venue \
                       refuses backs off from the supervisor's first wait, doubling",
            },
        ),
        (
            Scenario::Stall,
            SessionCell::NotApplicable(
                "the SDK owns the session's liveness: vike sets no liveness bound of its own \
                 (`crates/vike-venues/src/venue_timings.rs`'s `FXCM`)",
            ),
        ),
        (
            Scenario::GapFill,
            SessionCell::Deferred(
                "SDK-gated: what the ForexConnect tables re-surface after a reconnect or a rebuild is proven by the \
                 demo smoke at the FX open, not by CI; the rebuilt session is held until the bridge says it learned \
                 the venue (`fxcm_reconnect.rs`'s `a_bridge_that_announces_its_learning_is_not_held_and_one_that_never_does_is`)",
            ),
        ),
        (
            Scenario::BusyCommands,
            SessionCell::NotApplicable(
                "its exec thread runs its own ticking loop (`crates/bridges/fxcm/src/execution/mod.rs`'s `run`) with no \
                 gap-sentinel",
            ),
        ),
        (
            Scenario::DoubleDelivery,
            SessionCell::Proven {
                by: &[EXEC_HARNESS_REPLAY],
                pins: "the exec harness folds a fill of fxcm's real mapper delivered twice once (trade-id dedup); what \
                       the SDK delivers again after a reconnect is the demo smoke's",
            },
        ),
        (Scenario::Keepalive, SessionCell::NotApplicable("the SDK keeps its own session alive")),
        (
            Scenario::DialBound,
            SessionCell::NotApplicable("the SDK dials: no socket of ours to bound"),
        ),
    ],
};

const IB_SUPERVISED: &str = "crates/vike-gatewayhub/tests/trade/ibkr_reconnect.rs";
const IB_SOCKET: &str = "crates/bridges/vike-ibkr/src/transport/socket_tests.rs";
const IB_LINK: &str = "crates/bridges/vike-ibkr/src/transport/link_watch_tests.rs";
const IB_LIFECYCLE: &str = "crates/bridges/vike-ibkr/tests/ibkr_lifecycle.rs";

/// Interactive Brokers on its socket backend (IB Gateway over `ibapi`), the table's `ibkr` row.
const IBKR: SessionRow = SessionRow {
    venue: "ibkr",
    seam: "the gatewayhub's supervision suite (`crates/vike-gatewayhub/tests/trade/ibkr_reconnect.rs`): a fake gateway \
           socket probed by the REAL `IbkrGateway` watch under the REAL supervisor, and the bridge's own suites over \
           a scripted `ibapi` session; IB Gateway itself is the demo's",
    cells: &[
        (
            Scenario::Drop,
            SessionCell::Proven {
                by: &[
                    Proof {
                        file: IB_SUPERVISED,
                        test: "a_closed_gateway_ends_the_lease_at_the_loss_and_the_returning_one_rebuilds_it_in_a_new_epoch",
                    },
                    Proof {
                        file: IB_SOCKET,
                        test: "a_terminal_stream_error_is_a_stream_death_and_ends_the_pump",
                    },
                ],
                pins: "a gateway that goes away (its daily logoff) takes the adapter down and ends the lease at the \
                       loss, and the returning gateway rebuilds it in a new epoch with a reconcile owed; a stream that \
                       fails is a stream death, never a silent pump",
            },
        ),
        (
            Scenario::ServerClose,
            SessionCell::Proven {
                by: &[
                    Proof {
                        file: IB_SOCKET,
                        test: "a_clean_end_of_the_stream_is_a_stream_death_too",
                    },
                    Proof {
                        file: IB_SUPERVISED,
                        test: "the_bridge_reporting_its_link_down_takes_the_session_down_at_once",
                    },
                ],
                pins: "a clean end of the stream is a stream death too, and the bridge's report of it takes the \
                       session down at once, ahead of the probe",
            },
        ),
        (
            Scenario::Expiry,
            SessionCell::Proven {
                by: &[Proof {
                    file: IB_SUPERVISED,
                    test: "a_restore_the_venue_refuses_is_retried_with_a_growing_wait",
                }],
                pins: "a rebuild the gateway refuses is retried on a growing wait, never a hot loop",
            },
        ),
        (
            Scenario::Stall,
            SessionCell::Proven {
                by: &[
                    Proof {
                        file: IB_LINK,
                        test: "a_server_link_broken_past_the_threshold_with_no_restore_is_lost",
                    },
                    Proof {
                        file: IB_LINK,
                        test: "an_unconnected_socket_and_an_unanswered_snapshot_are_lost_too",
                    },
                    Proof {
                        file: IB_LINK,
                        test: "an_outage_that_heals_by_itself_inside_the_normal_range_is_never_lost",
                    },
                ],
                pins: "a gateway whose server link stays broken, whose socket is not connected or whose snapshot goes \
                       unanswered for `LOST_AFTER` (the row's `lost_after`, 300 s) is called lost, and an outage that \
                       heals inside it never is",
            },
        ),
        (
            Scenario::GapFill,
            SessionCell::Proven {
                by: &[
                    Proof {
                        file: IB_SUPERVISED,
                        test: "a_stock_fill_the_rebuilt_bridge_replays_is_booked_under_the_symbol_the_gatewayhub_taught",
                    },
                    Proof {
                        file: IB_LIFECYCLE,
                        test: "the_executions_replay_waits_for_the_end_of_the_open_orders",
                    },
                ],
                pins: "the rebuilt session asks for the open orders and then the day's executions, and a fill it \
                       replays is booked under the symbol the order was sent under",
            },
        ),
        (
            Scenario::BusyCommands,
            SessionCell::NotApplicable(
                "its exec thread runs its own loop (`crates/bridges/vike-ibkr/src/execution/mod.rs`'s `run_loop`) with no \
                 gap-sentinel: a gap is the rebuilt session's executions replay",
            ),
        ),
        (
            Scenario::DoubleDelivery,
            SessionCell::Proven {
                by: &[
                    Proof {
                        file: "crates/bridges/vike-ibkr/src/execution/event_mapper_tests.rs",
                        test: "replaying_a_fully_emitted_pair_emits_nothing_in_either_arrival_order",
                    },
                    Proof {
                        file: IB_LIFECYCLE,
                        test: "reconnect_replay_emits_a_fill_whose_commission_was_lost",
                    },
                ],
                pins: "a replayed execution already emitted (its execution and commission pair) emits nothing in \
                       either arrival order, and one whose commission the gap lost is emitted once by the replay",
            },
        ),
        (
            Scenario::Keepalive,
            SessionCell::NotApplicable("the `ibapi` client keeps the gateway socket itself"),
        ),
        (
            Scenario::DialBound,
            SessionCell::Proven {
                by: &[Proof {
                    file: IB_SOCKET,
                    test: "the_execution_connect_gives_up_on_a_silent_gateway",
                }],
                pins: "the exec connect against a gateway that never answers the handshake gives up within its bound \
                       with a typed connect error",
            },
        ),
    ],
};
