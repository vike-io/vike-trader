// C ABI shim over the FXCM ForexConnect C++ SDK, for calling from Rust.
// Self-contained: implements the session-status + response listeners inline.
//
// Portable across Windows and Linux (x86_64). The wait/signal and ref-count primitives are
// std-only (`std::condition_variable`/`std::mutex`/`std::atomic`) — NOT `windows.h`
// `CreateEvent`/`WaitForSingleObject`/`O2GAtomic` — so there is no `#ifdef` around threading.
// The ForexConnect C++ API surface is byte-identical across the two platforms (headers differ
// only by CRLF-vs-LF), so everything between the primitives is unchanged. The single remaining
// platform `#ifdef` is the export decoration below (a linkage attribute, not threading):
// `__declspec(dllexport)` (Win) vs `__attribute__((visibility("default")))` (ELF).
#define _CRT_SECURE_NO_WARNINGS
#include <string>
#include <cstring>
#include <cstdio>
#include <cmath>
#include <queue>
#include <set>
#include <mutex>
#include <atomic>
#include <condition_variable>
#include <chrono>
#include <thread>
#include "forexconnect/ForexConnect.h"

// C-ABI export decoration — the only platform split left. On Windows the exports must carry
// `__declspec(dllexport)` (as the original shim did); on ELF (Linux) default visibility is the
// portable equivalent. Threading below is fully portable std, so no other `#ifdef` remains.
#ifdef _WIN32
    #define FC_EXPORT __declspec(dllexport)
#else
    #define FC_EXPORT __attribute__((visibility("default")))
#endif

// Login (and logout) wait, and the default for the reconcile reads, which run on their OWN session.
#define WAIT_MS 30000

// ---------------- the exec thread's time budget for ONE order command ----------------
// The Rust exec thread blocks inside these calls, and the core's stuck-order watchdog counts from the
// submit: `guards.submit_ack_timeout_ms` defaults to 30000 and the backstop reject falls at
// `ack + 2 * submit_ack_confirm_grace_ms` = 60 s (`vike-core`'s `sweep_stuck_orders`; fxcm's `confirm`
// answers at ack time, so the budget below must be spent BEFORE the 30 s confirm). A placement used
// to wait WAIT_MS for the reply and then refresh the Orders table for up to WAIT_MS more: ~60 s with
// no answer, and a phantom `OrderRejected` for an order that was live. Every wait on the order path is
// now bounded on its own, and the worst case of one placement is their SUM:
//   quote refresh (pips entry only) + pre-send snapshot (limit only) + reply wait + lookup
//   = 2500 + 2500 + 10000 + 5000 = 20000 ms, 10 s under the default ack timeout.
// The FIRST order on an instrument the session has not yet seen is the one exception: the exec thread may first
// spend `fc_set_subscription`'s budget (8 s, `crates/bridges/fxcm/src/subscription.rs`'s `EXEC_BUDGET_MS`; 0 s when
// the login-time snapshot already reads `T`, which is the ordinary case) before the placement, so that order's worst
// case is 20000 + 8000 = 28000 ms: only 2 s under the default ack timeout, held by a `const` assert in
// `crates/bridges/fxcm/src/exec.rs`. A DEFINITE refusal of the subscription ends the order without a placement.
#define ORDER_WAIT_MS       10000  // the reply to a CreateOrder / EditOrder / DeleteOrder request
#define PRESEND_REFRESH_MS   2500  // a refresh BEFORE the request is sent (quote, order-id snapshot)
#define LOOKUP_REFRESH_MS    5000  // the post-send Orders lookup
#define ECHO_REFRESH_MS      5000  // the venue-rate echo read (a slow demo must not stall a queued Cancel)
// The Offers table read of the conversion facts (`fc_offers`). It runs on the RECONCILE session's thread, never the
// exec thread's, but a stuck refresh would still hold that thread (and so a reconcile pass) for the whole of
// WAIT_MS, and the door waits on the answer: a quote older than this is not worth waiting for.
#define FACTS_REFRESH_MS     8000
// `fc_set_subscription` (the instrument subscription): each stage is bounded on its own AND by the caller's total
// budget (`budget_ms`: the exec thread passes 8 s so a first order on an unsubscribed pair still fits under the ack
// timeout, the reconcile session 25 s). Measured 2026-10-26 on the demo login: the Offers lookup ~0.2 s, FXCM's
// answer 0.19-0.49 s after the request, the row reading the new status at once in 11 of 12 cases and one second
// later in the twelfth.
#define SUBSCRIBE_LOOKUP_MS      2500   // the Offers read that finds the row and its current status
#define SUBSCRIBE_REPLY_MS       8000   // the answer to the SetSubscriptionStatus request
#define SUBSCRIBE_SETTLE_STEP_MS  250   // the pause between two reads of the row while it settles
#define SUBSCRIBE_SETTLE_READ_MS 2000   // one such read

// ---------------- the shim's own ABI version ----------------
// `dlsym` proves a NAME exists; it cannot see that an existing name's MEANING changed. This integer
// can. Bump it when an entry point's contract moves in a way a resolver could not detect, and gate
// the new behaviour on it in `crates/bridges/fxcm/src/loader.rs`.
//
// ⚠ A shim that PREDATES this symbol is version 0 — absence is the signal, which is why the Rust
// side binds it optionally and defaults to 0 rather than refusing the load. the CI box has such a shim
// installed right now (`/var/lib/vike/lib/libfcshim.so`), so that path is the modal
// case for the first release after this change, not an edge.
//
// 1 = `fc_login_ex` exists: a login failure carries the SDK's own words and a class.
// 2 = `fc_place_entry_at` and `fc_edit_order` exist: a resting LIMIT entry can be placed at a
//     REQUESTED rate (`fc_place_entry` only takes a pip distance from the quote), and a resting
//     entry's rate can be amended in place (`EditOrder`). Both are bound OPTIONALLY by the loader
//     and only when this number is >= 2, so an older shim degrades to a named refusal.
#define FC_ABI_VERSION 2

// ---------------- fc_login_ex failure classes ----------------
// What the SHIM OBSERVED about a failed login — never a reading of what the venue MEANT. The text
// FXCM sends is passed through verbatim and classified by nobody here; telling a dead account from
// a bad connection name is a reading of that string and belongs on the Rust side, where it can be
// fixture-tested, if it is ever wanted at all.
#define FC_LOGIN_OK               0  // connected; there is no failure to describe
#define FC_LOGIN_REPORTED         1  // onLoginFailed fired — `err_out` holds the SDK's own text
#define FC_LOGIN_DISCONNECTED     2  // reached Disconnected with no onLoginFailed: no text exists
#define FC_LOGIN_NO_ANSWER        3  // no session-status callback at all inside WAIT_MS
#define FC_LOGIN_SDK_INIT_FAILED  4  // createSession() returned null — the SDK did not initialise
#define FC_LOGIN_TRADING_SESSION  5  // several trading sessions offered; this shim selects none

// ---------------- post-send outcome codes ----------------
// The request WAS SENT and the shim cannot say whether the venue took it. These are distinct from
// -7/-8, which `findOffer`/`refreshTable` also return BEFORE anything is sent (a definite "nothing
// reached the venue"), so the Rust side can tell a refusal from an unknown outcome by the code alone.
// A caller must NEVER turn either into a refusal: the order may rest (or the amend may have applied).
#define FC_SENT_NO_ANSWER        (-11)  // no response inside ORDER_WAIT_MS (and, for a placement, not found in Orders)
#define FC_SENT_NO_ORDER_ID      (-12)  // a response with neither an error nor an order id (same lookup)
// Only from the Orders lookup (`fc_find_order`, and inside a placement, where it is reported as the
// unknown outcome above): more than one row fits what was sent, so none of them is claimed as ours.
#define FC_LOOKUP_AMBIGUOUS      (-13)
// `fc_set_subscription` only: the instrument is not tradable and its `O2GInstrumentType` is none of the nine the SDK names
// (1..=9), so the bridge does not subscribe it. It was "not a regular forex instrument" (and so every metal, index and CFD)
// until the classes were admitted; a shim of that age still returns it for them, and the Rust side words both.
#define FC_UNKNOWN_INSTRUMENT_TYPE (-14)
// `fc_close_trade` only: the venue does not allow a market CLOSE order for the trade's instrument on this account
// (`IO2GPermissionChecker::canCreateMarketCloseOrder`). The SDK's own ClosePosition sample closes such an account with
// an opposite market OPEN instead; this shim does not, and says so.
#define FC_CLOSE_NOT_PERMITTED   (-15)
// `fc_place*` and `fc_set_subscription`: the ACCOUNT may not create this kind of order on the instrument. The login's
// permission checker (`canCreateEntryOrder` for a limit, `canCreateMarketOpenOrder` for a market order) reads DISABLED,
// and FXCM answers an order on such an instrument with `ORA-20102: Access Violation` (MEASURED on the demo login for
// USD/HKD, 2026-10-26: the one forex row of 48 with the permission off, though it reads subscription `T`). Nothing is
// sent; the words name the permission. Only DISABLED refuses: a permission that reads HIDDEN or unknown is the venue's to judge.
#define FC_ORDER_NOT_PERMITTED   (-16)
// `fc_place_exit` only: the venue does not allow a stop or limit order for the trade's instrument on this account
// (`IO2GPermissionChecker::canCreateStopLimitOrder`).
// ⚠ Every code below -10 is defined HERE and nowhere else, one value per name: `tests/fxcm_shim_return_codes.rs` fails when two
// share a value, or when a bare `return -NN;` below -10 appears (the Rust side maps a code to words, so two meanings of one code
// would read as the wrong refusal). `server/fxcm-exotic-pairs` owns -16 (`FC_ORDER_NOT_PERMITTED`), hence -17 here.
#define FC_EXIT_NOT_PERMITTED    (-17)

// After a non-connected wake, how long to keep looking for the SDK's TEXT.
//
// ⚠ Normally zero cost. The SDK's own sample clients print `Login error: …` BEFORE
// `status::disconnected`, so by the time `fc_login_ex` observes the wake the text is already
// recorded and this loop exits on its first check. It exists for the REVERSE ordering, which is
// unmeasured: a `Disconnected` that raced ahead of `onLoginFailed` would otherwise be reported as
// class 2 with no words at all — a fix that looks like it worked. It can only ever run on a login
// that has ALREADY failed.
#define LOGIN_TEXT_GRACE_MS 250

// ---------------- portable auto-reset event ----------------
// Replaces the Win32 auto-reset `CreateEvent(0, FALSE, FALSE, 0)` the shim relied on, with the
// SAME semantics on every platform: signal is a sticky flag consumed by exactly one waiter, and a
// signal delivered BEFORE a wait is NOT lost (the flag persists until a `wait` observes it). Like a
// Win32 auto-reset event, repeated `set()`s before a wait coalesce into a single wake. The
// predicate on `wait_for` (not a bare `notify`) is what makes the pre-signal safe against a lost
// wakeup, matching the EventQueue's existing `std::mutex` style in this file.
class AutoResetEvent {
    std::mutex m;
    std::condition_variable cv;
    bool signaled = false;
public:
    // Signal. Wakes one waiter (or is remembered until the next `wait`). Mirrors Win32 SetEvent.
    void set() {
        std::lock_guard<std::mutex> g(m);
        signaled = true;
        cv.notify_one();
    }
    // Clear a pending signal without waiting. Mirrors Win32 ResetEvent.
    void reset() {
        std::lock_guard<std::mutex> g(m);
        signaled = false;
    }
    // Wait up to `ms` for a signal; returns true if signaled (consuming it, auto-reset), false on
    // timeout. Mirrors `WaitForSingleObject(evt, ms) == WAIT_OBJECT_0`.
    bool wait(unsigned ms) {
        std::unique_lock<std::mutex> lk(m);
        bool ok = cv.wait_for(lk, std::chrono::milliseconds(ms), [this] { return signaled; });
        if (ok) signaled = false;
        return ok;
    }
};

// ---------------- session status listener ----------------
//
// ⚠ **`onLoginFailed`'s parameter was NAMED and DISCARDED here, and that one line was the whole
// defect.** The SDK hands this callback the text its own sample clients print in under a second
// ("Login error: User or connection doesn't exist." on a dead account; an ORA-499 naming the whole
// host-discovery request on a bad connection name), and the shim threw it away — so every login
// failure reached Rust as one indistinguishable null return, and two sessions (2026-08-19 and
// 2026-09-22) each spent hours re-deriving "the demo account is dead" from a mute timeout.
class StatusListener : public IO2GSessionStatus {
    std::atomic<unsigned int> mRef; AutoResetEvent mEvt; bool mConnected; IO2GSession* mSession;
    // ⚠ `mErrM` is NOT redundant with the AutoResetEvent's own mutex. That one gives a
    // happens-before edge on the SIGNALLED path only; the grace poll in `fc_login_ex` reads this
    // string while no event has been consumed, so the callback thread may still be inside
    // `onLoginFailed` writing it. `mClass` is atomic for the same reason — it is what the poll
    // spins on.
    std::mutex mErrM; std::string mError; std::atomic<int> mClass;
public:
    StatusListener(IO2GSession* s): mRef(1), mConnected(false), mSession(s), mClass(FC_LOGIN_OK) {}
    ~StatusListener() {}
    long addRef() { return (long)++mRef; }
    long release() { long r = (long)--mRef; if (r==0) delete this; return r; }
    void reset() {
        mConnected = false; mEvt.reset(); mClass = FC_LOGIN_OK;
        std::lock_guard<std::mutex> g(mErrM); mError.clear();
    }
    bool wait() { return mEvt.wait(WAIT_MS); }
    bool isConnected() { return mConnected; }
    // What this listener has OBSERVED so far — one of the FC_LOGIN_* codes above.
    int failureClass() { return mClass.load(); }
    // The SDK's own words, copied out under the lock. Empty unless the class is FC_LOGIN_REPORTED.
    std::string errorCopy() { std::lock_guard<std::mutex> g(mErrM); return mError; }
    void onSessionStatusChanged(O2GSessionStatus status) {
        if (status == IO2GSessionStatus::Connected) {
            mConnected = true; mClass = FC_LOGIN_OK; mEvt.set();
        } else if (status == IO2GSessionStatus::Disconnected) {
            mConnected = false;
            // ⚠ A CAS, never a plain store: `onLoginFailed` fires FIRST in every ordering measured
            // against the SDK's sample clients, and a store here would overwrite its class 1 — and
            // with it the operator's only account of WHY — with a contentless class 2.
            int expected = FC_LOGIN_OK;
            mClass.compare_exchange_strong(expected, FC_LOGIN_DISCONNECTED);
            mEvt.set();
        } else if (status == IO2GSessionStatus::TradingSessionRequested) {
            // ⚠ NEW ARM, and it is a mute-failure fix rather than a refactor. This status reaches
            // here on an account offering several trading sessions, and the SDK then waits for a
            // `setTradingSession` this shim never sends. Matching neither branch above, it set
            // nothing and signalled nothing — so that login HUNG for the full WAIT_MS and returned
            // null with no account of itself, which is an instance of the very defect this change
            // exists to close. Naming it costs three lines and answers in milliseconds.
            mConnected = false; mClass = FC_LOGIN_TRADING_SESSION; mEvt.set();
        }
    }
    void onLoginFailed(const char* error) {
        mConnected = false;
        { std::lock_guard<std::mutex> g(mErrM); mError = error ? error : ""; }
        mClass = FC_LOGIN_REPORTED;
        mEvt.set();
    }
};

// ---------------- response listener (captures created order id) ----------------
class RespListener : public IO2GResponseListener {
    std::atomic<unsigned int> mRef; AutoResetEvent mEvt; IO2GSession* mSession;
    // ⚠ The SDK calls these callbacks on ITS thread while the caller thread reads `orderId()`/`error()`
    // and (re)arms the id, so every access to the three strings goes through `mM`. The caller arms
    // `setRequestID` BEFORE `subscribeResponse` (see the call sites), so no callback can see an
    // unarmed listener. Accessors return COPIES: a `c_str()` into a string a callback may rewrite
    // would dangle.
    std::mutex mM; std::string mReqId, mOrderId, mError;
public:
    RespListener(IO2GSession* s): mRef(1), mSession(s) { s->addRef(); }
    ~RespListener() { mSession->release(); }
    long addRef() { return (long)++mRef; }
    long release() { long r = (long)--mRef; if (r==0) delete this; return r; }
    void setRequestID(const char* id) {
        { std::lock_guard<std::mutex> g(mM); mReqId = id; mOrderId.clear(); mError.clear(); }
        mEvt.reset();
    }
    bool wait() { return mEvt.wait(ORDER_WAIT_MS); }
    bool wait(unsigned ms) { return mEvt.wait(ms); }
    std::string orderId() { std::lock_guard<std::mutex> g(mM); return mOrderId; }
    std::string error() { std::lock_guard<std::mutex> g(mM); return mError; }
    void onRequestCompleted(const char* requestId, IO2GResponse* response) {
        if (!requestId) return;
        bool mine; { std::lock_guard<std::mutex> g(mM); mine = (mReqId == requestId); }
        if (response && mine) {
            if (response->getType() != CreateOrderResponse) mEvt.set();
        }
    }
    void onRequestFailed(const char* requestId, const char* error) {
        if (!requestId) return;
        bool mine = false;
        { std::lock_guard<std::mutex> g(mM);
          if (mReqId == requestId) { mError = error ? error : "request failed"; mine = true; } }
        if (mine) mEvt.set();
    }
    void onTablesUpdates(IO2GResponse* data) {
        if (!data) return;
        O2G2Ptr<IO2GResponseReaderFactory> f = mSession->getResponseReaderFactory();
        if (!f) return;
        O2G2Ptr<IO2GTablesUpdatesReader> r = f->createTablesUpdatesReader(data);
        if (!r) return;
        for (int i = 0; i < r->size(); ++i) {
            if (r->getUpdateTable(i) == Orders && r->getUpdateType(i) == Insert) {
                O2G2Ptr<IO2GOrderRow> o = r->getOrderRow(i);
                bool mine = false;
                // ⚠ `getRequestID()` can be NULL (a row the shim did not send): `std::string == NULL` is UB.
                const char* rid = o ? o->getRequestID() : 0;
                if (rid) { std::lock_guard<std::mutex> g(mM);
                    if (mReqId == rid) { mOrderId = o->getOrderID(); mine = true; } }
                if (mine) mEvt.set();
            }
        }
    }
};

// ---------------- async order-event lane (audit A3 fill/terminal stream) ----------------
// A thread-safe FIFO of JSON event envelopes. The persistent EventListener (below) pushes from the
// ForexConnect callback thread; `fc_poll_event` drains from the Rust exec thread.
struct EventQueue {
    std::mutex m;
    std::queue<std::string> q;
    void push(const std::string& s) { std::lock_guard<std::mutex> g(m); q.push(s); }
    bool pop(std::string& out) {
        std::lock_guard<std::mutex> g(m);
        if (q.empty()) return false;
        out = q.front(); q.pop(); return true;
    }
};

// Minimal JSON string escape (order ids / instruments are ASCII, but be safe against quotes).
static std::string jesc(const char* s) {
    std::string o; if (!s) return o;
    for (const char* p = s; *p; ++p) { if (*p=='"' || *p=='\\') o.push_back('\\'); o.push_back(*p); }
    return o;
}

// Resolve the human instrument symbol ("EUR/USD") for a trade's offer id. The IO2GTradeRow surfaced
// by createTablesUpdatesReader is the NonTableManager BASE row (getTradeRow returns IO2GTradeRow*),
// which carries getOfferID() but NOT getInstrument() — getInstrument lives only on the table-manager
// IO2GTradeTableRow. So we map the offer id back to its instrument through the login-rules Offers
// snapshot, the same source findOffer() reads, mirroring how the SDK's NonTableManager samples
// resolve a trade's instrument via its offer. Returns "" when the offer id is unknown.
// ⚠ This stays on the LOGIN-TIME snapshot on purpose: the offer-id -> instrument NAME does not move,
// and the persistent EventListener calls it on an SDK callback thread, where a refresh-table request
// (which waits for a callback) must never be issued — see "fresh table reads" below.
static std::string instrumentForOfferId(IO2GSession* s, const char* offerId) {
    if (!offerId || !*offerId) return "";
    O2G2Ptr<IO2GLoginRules> rules = s->getLoginRules();
    if (!rules) return "";
    O2G2Ptr<IO2GResponse> resp = rules->getTableRefreshResponse(Offers);
    if (!resp) return "";
    O2G2Ptr<IO2GResponseReaderFactory> rf = s->getResponseReaderFactory();
    if (!rf) return "";
    O2G2Ptr<IO2GOffersTableResponseReader> rd = rf->createOffersTableReader(resp);
    if (!rd) return "";
    for (int i = 0; i < rd->size(); ++i) {
        O2G2Ptr<IO2GOfferRow> o = rd->getRow(i);
        if (o && strcmp(o->getOfferID(), offerId) == 0)
            return o->getInstrument();
    }
    return "";
}

// An instrument's name with its case and its slashes taken out (`XAU/USD` and `xauusd` are `XAUUSD`, `USOil` and `USOIL` are
// `USOIL`): the engine's spelling of a symbol, which the Rust side hands over upper-cased and, for six letters, slashed
// (`COPPER` arrives as `COP/PER`; the venue's row is `Copper`).
static std::string canonInstr(const char* s) {
    std::string out;
    for (; s && *s; ++s) {
        if (*s == '/') continue;
        out += (*s >= 'a' && *s <= 'z') ? (char)(*s - 'a' + 'A') : *s;
    }
    return out;
}

// The VENUE's own spelling of the instrument a caller named: the login-time Offers row whose canonical name is the same
// (a local read, no round trip; the snapshot holds every row), else the name as given. Every SDK call that takes an
// instrument name wants the venue's spelling (`getBaseUnitSize`, the permission checker, the Offers lookups), and a
// name that is not the venue's own reads as 0 or `unknown` there, not as an error.
static std::string venueName(IO2GSession* s, const char* instr) {
    if (!instr) return "";
    const std::string want = canonInstr(instr);
    O2G2Ptr<IO2GLoginRules> rules = s->getLoginRules();
    if (!rules) return instr;
    O2G2Ptr<IO2GResponse> resp = rules->getTableRefreshResponse(Offers);
    if (!resp) return instr;
    O2G2Ptr<IO2GResponseReaderFactory> rf = s->getResponseReaderFactory();
    if (!rf) return instr;
    O2G2Ptr<IO2GOffersTableResponseReader> rd = rf->createOffersTableReader(resp);
    if (!rd) return instr;
    for (int i = 0; i < rd->size(); ++i) {
        O2G2Ptr<IO2GOfferRow> o = rd->getRow(i);
        if (o && o->getInstrument() && canonInstr(o->getInstrument()) == want) return o->getInstrument();
    }
    return instr;
}

// The ONE place a FILL envelope's `snprintf` literal is spelled, for a trade that OPENS (Trades Insert) and for a
// trade that CLOSES (ClosedTrades Insert) alike: the two cannot drift apart, and
// `crates/bridges/fxcm/tests/fxcm_shim_envelope_grammar.rs` reads this literal against its committed fixture.
// `remainTradeId` is the closed row's `TradeIDRemain`: the NEW id the rest of a PARTIALLY closed trade comes back under
// (MEASURED 2026-10-26); empty for an opening fill and for a close of a whole trade.
static void pushFill(EventQueue* q, const char* orderId, const char* tradeId, const char* instrument,
        const char* buySell, int amount, double rate, double commission, const char* remainTradeId,
        double rollover, double grossPl) {
    char buf[768];
    snprintf(buf, sizeof(buf),
        "{\"kind\":\"fill\",\"order_id\":\"%s\",\"trade_id\":\"%s\",\"instrument\":\"%s\","
        "\"side\":\"%s\",\"amount\":%d,\"rate\":%.6f,\"commission\":%.6f,\"remain_trade_id\":\"%s\","
        "\"rollover\":%.6f,\"gross_pl\":%.6f,\"ts\":0}",
        jesc(orderId).c_str(), jesc(tradeId).c_str(), jesc(instrument).c_str(), jesc(buySell).c_str(),
        amount, rate, commission, jesc(remainTradeId).c_str(), rollover, grossPl);
    q->push(buf);
}

// Persistent response listener subscribed for the whole session. Turns Trades(Insert) into a
// "fill" envelope, ClosedTrades(Insert) into the CLOSING fill envelope and Orders(Delete) into a "canceled"
// envelope, enqueued for `fc_poll_event`.
// Audit A3: after a reconnect ForexConnect re-delivers the current Trades/Orders tables, so the
// gap fills re-surface here; the Rust core dedups by trade_id.
// The IO2GTradeRow accessors below are the base-row surface (getTradeID/getOpenOrderID/getOfferID/
// getBuySell/getAmount/getOpenRate/getCommission); the instrument is NOT a base-row field, so it is
// resolved from the trade's offer id via instrumentForOfferId() above.
//
// ⚠ A trade closed by id (`fc_close_trade`) never appears as a Trades Insert. The SDK reports the close as a
// ClosedTrades Insert keyed by the closing order (`getCloseOrderID()`, the way its own ClosePosition sample's
// OrderMonitor correlates one), and this listener used to ignore that table: a correct close reached the venue and the
// core never heard of it. The closing fill carries its OWN trade id, `<trade id>-close-<closing order id>`: the core
// drops a fill whose trade_id it has folded already (`Account::apply_fill`, and the FSM's own set), and the opening
// fill of that trade carries the bare trade id, so a close under the bare id would be dropped as a replay of the open.
//
// A trade closed by its own trade-bound STOP or LIMIT (`fc_place_exit`) arrives the same way, MEASURED 2026-10-26: the
// ClosedTrades row's `CloseOrderID` is the EXIT's order id (so the fill routes to the exit's client order id), its
// `CloseRate` the exit's rate, and the venue deletes the sibling exit (Orders Delete, status `C`) in the same batch: that
// is the `canceled` envelope below, which routes to the sibling's client order id. The exit's own Orders Update (`E`) and
// Delete (`F`) are an execution, not a cancel, and are dropped as every executed order's are.
class EventListener : public IO2GResponseListener {
    std::atomic<unsigned int> mRef; IO2GSession* mSession; EventQueue* mQ;
public:
    EventListener(IO2GSession* s, EventQueue* q): mRef(1), mSession(s), mQ(q) { s->addRef(); }
    ~EventListener() { mSession->release(); }
    long addRef() { return (long)++mRef; }
    long release() { long r = (long)--mRef; if (r==0) delete this; return r; }
    void onRequestCompleted(const char*, IO2GResponse*) {}
    void onRequestFailed(const char*, const char*) {}
    void onTablesUpdates(IO2GResponse* data) {
        if (!data) return;
        O2G2Ptr<IO2GResponseReaderFactory> f = mSession->getResponseReaderFactory();
        if (!f) return;
        O2G2Ptr<IO2GTablesUpdatesReader> r = f->createTablesUpdatesReader(data);
        if (!r) return;
        char buf[512];
        for (int i = 0; i < r->size(); ++i) {
            if (r->getUpdateTable(i) == Trades && r->getUpdateType(i) == Insert) {
                O2G2Ptr<IO2GTradeRow> t = r->getTradeRow(i);
                if (!t) continue;
                // ⚠ A PARTIAL close SPLITS its trade, and the remainder arrives as a Trades Insert of its own:
                // MEASURED on the demo (2026-10-26), a close of 1000 of trade 186982939 (B 2000) was answered by a
                // ClosedTrades row for 186982939 (B 1000, `TradeIDRemain` = 186983022), a Trades Insert of 186983022
                // (B 1000, the SAME open order and open time, `TradeIDOrigin` = 186982939) and the delete of the old
                // row. That insert is no execution: reporting it as a fill would book the remainder AGAIN, under an
                // id the core has not folded and under the opening order's route. A trade that opens by an order
                // has no origin (every OM trade measured: empty), so a non-empty origin is exactly the split.
                const char* origin = t->getTradeIDOrigin();
                if (origin && *origin) continue;
                std::string instrument = instrumentForOfferId(mSession, t->getOfferID());
                // `rollover`, `gross_pl` 0.0: a trade that has just opened has held nothing overnight and
                // realized nothing (the Rust side books a rollover only off a CLOSING fill).
                pushFill(mQ, t->getOpenOrderID(), t->getTradeID(), instrument.c_str(), t->getBuySell(),
                    t->getAmount(), t->getOpenRate(), t->getCommission(), "", 0.0, 0.0);
            } else if (r->getUpdateTable(i) == ClosedTrades && r->getUpdateType(i) == Insert) {
                O2G2Ptr<IO2GClosedTradeRow> c = r->getClosedTradeRow(i);
                if (!c) continue;
                std::string instrument = instrumentForOfferId(mSession, c->getOfferID());
                // The row's BuySell is the side of the trade that was closed; the closing leg is the other way.
                const char* was = c->getBuySell();
                const char* side = (was && *was == 'B') ? "S" : "B";
                const char* closeOrder = c->getCloseOrderID() ? c->getCloseOrderID() : "";
                std::string tradeId = std::string(c->getTradeID() ? c->getTradeID() : "") + "-close-" + closeOrder;
                // ⚠ commission: the CLOSED row's own figure, raw (account currency, positive = a charge: the SDK's
                // "amount of funds subtracted from the account balance"). It is NOT what the closing fill books: whether
                // it already contains the commission the OPENING fill carried is unmeasured (the demo charges none), so
                // the Rust side (`crates/bridges/fxcm/src/commission.rs`) books `this figure - the open fill's share`,
                // never below zero. Realized PnL is from the prices; the venue's balance is the truth.
                // ⚠ `TradeIDRemain` is the NEW id a PARTIAL close leaves the rest of the trade under (the remainder's Trades
                // Insert is skipped above: it is no execution), so the Rust side can follow the trade — the exits the venue
                // keeps on it, and the reconcile report that must read the remainder as the trade the core already folded.
                // Empty for a close of the whole trade.
                const char* remain = c->getTradeIDRemain();
                // ⚠ rollover: the CLOSED row's own `getRolloverInterest()`, raw — "the cumulative amount of funds ADDED to
                // the account balance for holding a position opened overnight", in the account currency, positive or
                // negative, 0.0 when the position was not held overnight (the SDK reference). It is the realized rollover
                // of the closed amount; `crates/bridges/fxcm/src/rollover.rs` books it once per closing fill as funding.
                // `gross_pl` is the same row's `getGrossPL()`: not booked. ⚠ There is NO `net_pl`: `getNetPL()` exists only on
                // the Table Manager's `IO2GClosedTradeTableRow`, never on the `IO2GClosedTradeRow` this listener reads (the
                // line that called it did not COMPILE against the staged SDK, 2026-10-30), and a net computed here from
                // these same figures could not check the rollover's sign (it would confirm any sign it was built with).
                pushFill(mQ, closeOrder, tradeId.c_str(), instrument.c_str(), side,
                    c->getAmount(), c->getCloseRate(), c->getCommission(), remain ? remain : "",
                    c->getRolloverInterest(), c->getGrossPL());
            } else if (r->getUpdateTable(i) == Orders && r->getUpdateType(i) == Delete) {
                O2G2Ptr<IO2GOrderRow> o = r->getOrderRow(i);
                if (!o) continue;
                // An Orders(Delete) fires for BOTH a cancel AND an order leaving the working set
                // because it EXECUTED — a filled market order deletes its order row too. getStatus()
                // discriminates, per the SDK's OrderMonitor::onOrderDeleted: 'C' = canceled,
                // 'R' = rejected, anything else = executed. An executed delete is NOT a cancel (its
                // fill already arrived on the Trades(Insert) above), so it is dropped here — else
                // every market fill would emit a spurious OrderCanceled ahead of its own fill.
                const char* st = o->getStatus();
                char s = (st && *st) ? *st : 0;
                if (s == 'C') {
                    snprintf(buf, sizeof(buf),
                        "{\"kind\":\"canceled\",\"order_id\":\"%s\",\"ts\":0}", jesc(o->getOrderID()).c_str());
                    mQ->push(buf);
                } else if (s == 'R') {
                    snprintf(buf, sizeof(buf),
                        "{\"kind\":\"rejected\",\"order_id\":\"%s\",\"reason\":\"rejected\",\"ts\":0}",
                        jesc(o->getOrderID()).c_str());
                    mQ->push(buf);
                }
                // else: executed (filled) — no terminal event here; the fill came via Trades(Insert).
            }
        }
    }
};

struct Handle { IO2GSession* session; StatusListener* status; EventQueue events; EventListener* evl; };

// ---------------- fresh table reads (refresh-table requests) ----------------
//
// ⚠ **`getLoginRules()->getTableRefreshResponse(T)` IS NOT A CURRENT READ, and for Orders and Trades
// it is not a read at all.** The SDK documents it as the table "loaded during login"
// (`IO2GLoginRules::isTableLoadedByDefault` / `getTableRefreshResponse`). MEASURED on the demo login
// (2026-10-13): `isTableLoadedByDefault` is true for Offers and Accounts and FALSE for Orders and
// Trades, so `getTableRefreshResponse(Orders)` and `(Trades)` return NULL in EVERY session — also one
// that logged in after the order/trade existed — and Offers/Accounts are frozen at login (a quote
// that stood still while the market moved ~22 updates a second; a balance that stayed 49999.87 while
// the account row said 49999.80). This file used to read all four that way and turn the NULL into
// "[]", so `fc_orders`/`fc_trades` reported "nothing there" forever and the reconcile client was
// blind: zero orders, zero fills, a flat position, and a restore planner that reads "empty" as
// "cancelled".
//
// The SDK's own way to a CURRENT table (its PrintTable and GetOffers samples) is a refresh-table
// request: `createRefreshTableRequest(T)` (all accounts) or `createRefreshTableRequestByAccount(T,
// account)`, sent with `sendRequest`, answered on `onRequestCompleted` with the table as it stands.
// `refreshTable` below is that, with a bounded wait of its own (`timeoutMs`).
//
// ⚠ Never call `refreshTable` from an SDK CALLBACK thread (`onTablesUpdates`, `onRequestCompleted`):
// it blocks until a callback delivers the response. That is why `instrumentForOfferId` — which the
// persistent EventListener runs on the callback thread — still reads the login-time Offers snapshot:
// it needs only the offer-id -> instrument NAME, which does not move.

// One-shot listener for ONE refresh-table request: keeps the matching response (with its own
// reference) or the SDK's failure text, and wakes the waiter. A fresh instance per request, so no
// state survives from one read to the next.
class RefreshListener : public IO2GResponseListener {
    std::atomic<unsigned int> mRef; AutoResetEvent mEvt; IO2GSession* mSession;
    std::mutex mM; std::string mReqId, mError; IO2GResponse* mResp;
public:
    RefreshListener(IO2GSession* s): mRef(1), mSession(s), mResp(0) { s->addRef(); }
    ~RefreshListener() { if (mResp) mResp->release(); mSession->release(); }
    long addRef() { return (long)++mRef; }
    long release() { long r = (long)--mRef; if (r==0) delete this; return r; }
    void arm(const char* id) {
        { std::lock_guard<std::mutex> g(mM); mReqId = id ? id : ""; mError.clear(); }
        mEvt.reset();
    }
    bool wait(unsigned ms) { return mEvt.wait(ms); }
    std::string error() { std::lock_guard<std::mutex> g(mM); return mError; }
    // Hand the kept response (and its reference) to the caller; null if there is none.
    IO2GResponse* take() { std::lock_guard<std::mutex> g(mM); IO2GResponse* r = mResp; mResp = 0; return r; }
    void onRequestCompleted(const char* requestId, IO2GResponse* response) {
        if (!requestId || !response) return;
        {
            std::lock_guard<std::mutex> g(mM);
            if (mReqId != requestId) return;
            response->addRef();
            if (mResp) mResp->release();
            mResp = response;
        }
        mEvt.set();
    }
    void onRequestFailed(const char* requestId, const char* error) {
        if (!requestId) return;
        {
            std::lock_guard<std::mutex> g(mM);
            if (mReqId != requestId) return;
            mError = (error && *error) ? error : "request failed";
        }
        mEvt.set();
    }
    void onTablesUpdates(IO2GResponse*) {}
};

// Send ONE refresh-table request for `table` (`account` null = every account on the login) and wait
// for ITS response. Returns 0 with the response in `out`, or a negative code with the SDK's words
// (or ours) in `err` — NEVER an empty table standing in for a failed read. The codes are the ones
// `fc_place` already uses where the meaning is the same: -4 no factory, -5 the request could not be
// built, -6 the venue failed the request, -8 no answer inside `timeoutMs`.
//
// ⚠ Every caller on the ORDER path passes its own short bound (see the budget above ORDER_WAIT_MS);
// only the reconcile reads, which run on their own session, keep the WAIT_MS default.
static int refreshTable(IO2GSession* s, O2GTable table, const char* account,
        O2G2Ptr<IO2GResponse>& out, std::string& err, unsigned timeoutMs = WAIT_MS) {
    O2G2Ptr<IO2GRequestFactory> rf = s->getRequestFactory();
    if (!rf) { err = "no request factory"; return -4; }
    O2G2Ptr<IO2GRequest> req = account
        ? rf->createRefreshTableRequestByAccount(table, account)
        : rf->createRefreshTableRequest(table);
    if (!req) {
        const char* e = rf->getLastError();
        err = (e && *e) ? e : "the refresh-table request could not be created";
        return -5;
    }
    RefreshListener* rl = new RefreshListener(s);
    s->subscribeResponse(rl);
    rl->arm(req->getRequestID());
    s->sendRequest(req);
    int rc = 0;
    if (rl->wait(timeoutMs)) {
        std::string failure = rl->error();
        IO2GResponse* r = rl->take();
        if (!failure.empty()) { err = failure; rc = -6; if (r) r->release(); }
        else if (!r) { err = "the refresh-table request completed with no response"; rc = -6; }
        else out = r;  // O2G2Ptr::operator=(T*) adopts the reference `take` handed over
    } else { err = "timeout waiting for the refresh-table response"; rc = -8; }
    s->unsubscribeResponse(rl); rl->release();
    return rc;
}

// ⚠ NOT made fresh, on purpose: the account's IDENTITY (which row order flow targets) comes from the
// login-time Accounts snapshot. `firstAccount` is `fc_place`'s account and `fc_base_unit_size`'s, so
// reading it afresh would turn the hazard recorded in decision 0150 (a login with two eligible
// accounts moves its flow to the second when the first enters a margin call) from a next-login event
// into a MID-SESSION one, on the live order path. The reconcile reads below take this same row's id,
// so they report on exactly the account orders go to — and read its CURRENT state by id.
static IO2GAccountRow* firstAccount(IO2GSession* s) {
    O2G2Ptr<IO2GLoginRules> rules = s->getLoginRules();
    if (!rules) return 0;
    O2G2Ptr<IO2GResponse> resp = rules->getTableRefreshResponse(Accounts);
    if (!resp) return 0;
    O2G2Ptr<IO2GResponseReaderFactory> rf = s->getResponseReaderFactory();
    if (!rf) return 0;
    O2G2Ptr<IO2GAccountsTableResponseReader> rd = rf->createAccountsTableReader(resp);
    for (int i = 0; i < rd->size(); ++i) {
        O2G2Ptr<IO2GAccountRow> a = rd->getRow(i);
        if (a && strcmp(a->getMarginCallFlag(),"N")==0 &&
            (strcmp(a->getAccountKind(),"32")==0 || strcmp(a->getAccountKind(),"36")==0))
            return a.Detach();
    }
    return 0;
}
// An Offers row held TOGETHER with the response and reader it was read from, released in reverse
// (row, reader, response). The login-rules response outlived `findOffer` because the login rules
// keep their own reference; a refresh response has no other owner, so the caller must keep it alive
// for as long as it reads the row.
struct OfferHold {
    O2G2Ptr<IO2GResponse> resp;
    O2G2Ptr<IO2GOffersTableResponseReader> rd;
    O2G2Ptr<IO2GOfferRow> row;
    IO2GOfferRow* operator->() const { return row; }
};

// Find the SUBSCRIBED Offers row for `instr`. Returns 0 and fills `out`, -3 when there is no such row
// (the "offer not found" the order path has always reported), or a refresh failure code from
// `refreshTable` with its words in `err`.
//
// `fresh` picks where the row comes from. true = a refresh-table request, i.e. the quote AS IT STANDS
// NOW; false = the login-time snapshot, exactly what this function always read. Only a quote that
// is going to be PRICED OFF needs to be current (a LIMIT entry's rate, `fc_place` below), and a refresh
// can fail where the snapshot cannot, so the market path keeps the snapshot: it uses the offer's id
// alone, which does not move, and gains nothing from a round trip that could only add a failure.
//
// `status` is set to the status of the row when the instrument IS listed but is not `T` (empty when it is not
// listed at all), so the caller can say what the venue holds instead of a bare "not found".
static int findOfferOnce(IO2GSession* s, const char* instr, bool fresh, OfferHold& out,
        std::string& err, std::string& status) {
    status.clear();
    if (fresh) {
        int rc = refreshTable(s, Offers, 0, out.resp, err, PRESEND_REFRESH_MS);
        if (rc != 0) return rc;
    } else {
        O2G2Ptr<IO2GLoginRules> rules = s->getLoginRules();
        if (!rules) return -3;
        out.resp = rules->getTableRefreshResponse(Offers);
        if (!out.resp) return -3;
    }
    O2G2Ptr<IO2GResponseReaderFactory> rf = s->getResponseReaderFactory();
    if (!rf) { err = "no response reader factory"; return fresh ? -4 : -3; }
    out.rd = rf->createOffersTableReader(out.resp);
    if (!out.rd) { err = "the response is not an Offers table"; return fresh ? -7 : -3; }
    for (int i = 0; i < out.rd->size(); ++i) {
        O2G2Ptr<IO2GOfferRow> o = out.rd->getRow(i);
        if (!o || strcmp(instr, o->getInstrument()) != 0) continue;
        const char* st = o->getSubscriptionStatus();
        if (st && strcmp(st, "T") == 0) {
            out.row = o.Detach();
            return 0;
        }
        status = (st && *st) ? st : "?";
    }
    return -3;
}
// An instrument whose row is listed but not `T` is looked up AGAIN in a fresh table before it is refused: the
// login-time snapshot is kept current by the venue's pushed updates (measured 2026-10-26: a second session saw
// another session's subscription at once), but a push still in flight must not turn an order into a refusal.
// A row that is still not `T` is -3 with the status in `err` (the bridge subscribes a symbol itself, `fc_set_subscription`).
static int findOffer(IO2GSession* s, const char* instr, bool fresh, OfferHold& out,
        std::string& err) {
    std::string status;
    int rc = findOfferOnce(s, instr, fresh, out, err, status);
    if (rc == -3 && !status.empty() && !fresh) {
        OfferHold again;
        std::string err2;
        int rc2 = findOfferOnce(s, instr, true, again, err2, status);
        if (rc2 == 0) { out.resp = again.resp; out.rd = again.rd; out.row = again.row; return 0; }
        if (rc2 != -3) { err = err2; return rc2; }
    }
    if (rc == -3 && !status.empty())
        err = "the offer is listed but its subscription status is `" + status + "`, not `T` (tradable)";
    return rc;
}
static void cpy(char* dst, int len, const char* src) {
    if (!dst || len <= 0) return; strncpy(dst, src ? src : "", len-1); dst[len-1]=0;
}
// True when the login's permission checker reads DISABLED for creating an order of this kind on `instr` (`market`:
// `canCreateMarketOpenOrder`, else `canCreateEntryOrder`). A local read of the login rules: no round trip, no refresh.
// Hidden, unknown and an unreadable checker are NOT a refusal (the venue judges those; `canCreateNetStopLimitOrder` read
// hidden and the venue still accepted the order).
static bool orderCreationDisabled(IO2GSession* s, const char* instr, bool market) {
    O2G2Ptr<IO2GLoginRules> rules = s->getLoginRules();
    if (!rules) return false;
    O2G2Ptr<IO2GPermissionChecker> perm = rules->getPermissionChecker();
    if (!perm) return false;
    return (market ? perm->canCreateMarketOpenOrder(instr) : perm->canCreateEntryOrder(instr)) == PermissionDisabled;
}

// A REQUESTED rate, checked against the instrument's price grid (`10^-digits`, the Offers row's
// `getDigits()`), and returned snapped onto it. Used by `fc_place_entry_at` and `fc_edit_order`.
//
// ⚠ REFUSES an off-grid rate; it never ROUNDS one. A price the caller did not choose is the defect
// this venue's adapter has already produced twice (a fixed pip distance, a rounded lot count), so a
// rate with more decimals than the instrument carries is an error naming the grid, and the caller
// re-sends one it picked. The only change made is to absorb binary-float noise (the RiskGate's tick
// rounding leaves values such as 0.09000000000000001): a request within 1e-6 of a tick is that tick.
// Returns 0 with the snapped rate in `out`, else -10 with the reason in `err`.
static int snapRate(IO2GOfferRow* offer, double rate, double& out, std::string& err) {
    if (!std::isfinite(rate) || rate <= 0.0) { err = "the requested rate is not a positive finite number"; return -10; }
    int digits = offer->getDigits();
    if (digits < 0 || digits > 8) { err = "the venue reports an unusable price precision for this instrument"; return -10; }
    double scale = std::pow(10.0, digits);
    double ticks = rate * scale;
    double nearest = std::nearbyint(ticks);
    if (std::fabs(ticks - nearest) > 1e-6) {
        char b[160];
        snprintf(b, sizeof(b), "the requested rate %.10g is off this instrument's price grid (%d decimals)", rate, digits);
        err = b;
        return -10;
    }
    out = nearest / scale;
    return 0;
}

// Copy a fully-built JSON snapshot into the caller's buffer and return the FULL length needed
// (bytes, excluding the NUL) — the grow-and-retry protocol the reconcile table readers share. When
// the buffer is too small the return is >= out_len; the Rust side (sys.rs `snapshot`) re-allocates
// to the returned size and calls once more, so an arbitrarily large table is never truncated into
// malformed JSON. Mirrors the snprintf "how many chars would have been written" convention.
static int emit_json(const std::string& json, char* out, int out_len) {
    int needed = (int)json.size();
    if (out && out_len > 0) {
        int n = needed < out_len - 1 ? needed : out_len - 1;
        memcpy(out, json.data(), (size_t)n);
        out[n] = 0;
    }
    return needed;
}

// Refresh `table` for the tradable account — the account order flow targets (`firstAccount`), so the
// reconcile reads report on exactly the book orders go to. 0 and the response in `resp`, else the
// failure code with its reason in `err`; -2 when no account is eligible. Used by fc_orders/fc_trades.
static int refreshForAccount(IO2GSession* s, O2GTable table, O2G2Ptr<IO2GResponse>& resp,
        std::string& err) {
    O2G2Ptr<IO2GAccountRow> account = firstAccount(s);
    if (!account) { err = "no account"; return -2; }
    return refreshTable(s, table, account->getAccountID(), resp, err);
}

extern "C" {

// The shim's ABI version — see FC_ABI_VERSION above. Takes nothing and touches nothing, so it is
// safe to call before any session exists; the Rust loader calls it once, at load.
FC_EXPORT int fc_abi_version(void) { return FC_ABI_VERSION; }

// Log in, and on FAILURE say what was observed and what the SDK said about it.
//
// Returns the session handle, or null. On null: `*class_out` is one of the FC_LOGIN_* codes and
// `err_out` holds a message — the SDK's OWN TEXT verbatim for FC_LOGIN_REPORTED, and a description
// of what the shim saw for every other class. Both out-params are optional (null / `err_len <= 0`
// are tolerated by `cpy`), which is what lets `fc_login` below be a plain forward.
//
// ⚠ This is a NEW NAME rather than a wider `fc_login`, and that is the ABI argument rather than a
// style preference. `dlsym` resolves by name and checks no signature — `crates/bridges/fxcm/src/loader.rs`
// says so at its `bind!` macro — so widening `fc_login` in place would let a binary built from this
// tree push seven arguments at the four-argument `fc_login` in the shim the CI box has INSTALLED today.
// On SysV x86-64 that does not even crash: the extra arguments land in registers the old body
// ignores, the caller then reads an error buffer the callee never wrote, and a login failure comes
// back reporting NO reason at all — worse than the message it replaced, on the one venue where the
// whole point was to stop losing the reason.
FC_EXPORT void* fc_login_ex(const char* user, const char* pw, const char* url, const char* conn,
        char* err_out, int err_len, int* class_out) {
    if (class_out) *class_out = FC_LOGIN_NO_ANSWER;
    cpy(err_out, err_len, "");

    IO2GSession* s = CO2GTransport::createSession();
    // ⚠ This check is new, and without it the next line was a null dereference: `createSession()`
    // returning null went straight into `new StatusListener(s)` and `s->subscribeSessionStatus(sl)`.
    // A box whose SDK failed to initialise SIGSEGV'd inside a venue mount instead of saying so.
    if (!s) {
        if (class_out) *class_out = FC_LOGIN_SDK_INIT_FAILED;
        cpy(err_out, err_len,
            "CO2GTransport::createSession() returned null: the ForexConnect libraries are present "
            "(this shim loaded) but the SDK did not initialise on this box");
        return 0;
    }

    StatusListener* sl = new StatusListener(s);
    s->subscribeSessionStatus(sl);
    sl->reset();
    s->login(user, pw, url, conn);
    bool signaled = sl->wait();
    if (!(signaled && sl->isConnected())) {
        int cls = signaled ? sl->failureClass() : FC_LOGIN_NO_ANSWER;
        // THE GRACE — see LOGIN_TEXT_GRACE_MS. Only on an already-failed login, and only while the
        // words have not arrived; it exits on its first check in every ordering measured so far.
        if (signaled && cls != FC_LOGIN_REPORTED) {
            for (int waited = 0; waited < LOGIN_TEXT_GRACE_MS; waited += 10) {
                if (sl->failureClass() == FC_LOGIN_REPORTED) break;
                std::this_thread::sleep_for(std::chrono::milliseconds(10));
            }
            cls = sl->failureClass();
        }
        // A wake this shim cannot account for is NOT reported as a success. Reaching here with
        // FC_LOGIN_OK means something signalled the event and no arm claimed it.
        if (cls == FC_LOGIN_OK) cls = FC_LOGIN_NO_ANSWER;

        if (cls == FC_LOGIN_REPORTED) {
            // VERBATIM. The shim parses nothing: this string is the only account of the failure
            // that exists, and every classification of it would be a guess at an undocumented,
            // unversioned message set.
            cpy(err_out, err_len, sl->errorCopy().c_str());
        } else if (cls == FC_LOGIN_DISCONNECTED) {
            cpy(err_out, err_len,
                "the ForexConnect session went Disconnected without the SDK naming a reason");
        } else if (cls == FC_LOGIN_TRADING_SESSION) {
            cpy(err_out, err_len,
                "the login offers several trading sessions (IO2GSessionStatus::"
                "TradingSessionRequested) and this shim selects none, so it can never complete");
        } else {
            cpy(err_out, err_len,
                "no ForexConnect session-status callback arrived inside the shim's wait window");
        }
        if (class_out) *class_out = cls;
        s->unsubscribeSessionStatus(sl); sl->release(); s->release();
        return 0;
    }

    if (class_out) *class_out = FC_LOGIN_OK;
    Handle* h = new Handle(); h->session = s; h->status = sl;
    // Persistent async event listener: captures fills/cancels for fc_poll_event (A3 fill lane).
    h->evl = new EventListener(s, &h->events);
    s->subscribeResponse(h->evl);
    return h;
}

// THE ABI ANCHOR. Four arguments, frozen, forever — an OLD binary resolves this name in a NEW shim
// and must get exactly what it has always got. ONE body, so the two can never answer differently.
FC_EXPORT void* fc_login(const char* user, const char* pw, const char* url, const char* conn) {
    return fc_login_ex(user, pw, url, conn, 0, 0, 0);
}

// Drain ONE pending async order event as JSON. Returns 1 (event copied to out), 0 (queue empty),
// or <0 on a bad handle. Thread-safe (the queue is mutex-guarded) — called from the Rust exec
// thread between commands, independent of the ForexConnect callback thread that fills the queue.
FC_EXPORT int fc_poll_event(void* hp, char* out, int out_len) {
    Handle* h = (Handle*)hp; if (!h) return -1;
    std::string ev;
    if (!h->events.pop(ev)) return 0;
    cpy(out, out_len, ev.c_str());
    return 1;
}

// The instrument's BASE UNIT SIZE on the tradable account — i.e. exactly the multiplier `fc_place`
// below applies to the `lots` it is handed (`amount = baseUnit * lots`).
//
// It is exported so the Rust side can stop sizing in LOTS. `qty` on an `OrderRequest` is base units
// everywhere else in this workspace, and the `amount` that comes back on a fill is base units too —
// so for as long as this number lived only inside `fc_place`, a caller sending the units it holds
// everywhere else placed that many LOTS: `qty: 10000` on EUR/USD was ten thousand lots, ten
// million units. `crates/bridges/fxcm/src/event_mapper.rs`'s `lots_for` divides by this figure now
// and REFUSES a size that is not an exact multiple, which makes submit and fill the same unit and
// makes an unplaceable size a visible reject rather than a silently different order.
//
// Read-only, and it reuses the two lookups `fc_place` already does (`firstAccount` + the login
// rules' trading-settings provider), so the number the Rust side divides by is by construction the
// number the placement multiplies by. A non-positive result is an ERROR rather than a default: a
// zero would make every division degenerate and a negative one would flip the side.
FC_EXPORT int fc_base_unit_size(void* hp, const char* instrument, int* out,
        char* err_out, int err_len) {
    Handle* h = (Handle*)hp; if (!h) return -1;
    const std::string venue_name = venueName(h->session, instrument);
    instrument = venue_name.c_str();
    O2G2Ptr<IO2GAccountRow> account = firstAccount(h->session);
    if (!account) { cpy(err_out,err_len,"no account"); return -2; }
    O2G2Ptr<IO2GLoginRules> rules = h->session->getLoginRules();
    if (!rules) { cpy(err_out,err_len,"no login rules"); return -4; }
    O2G2Ptr<IO2GTradingSettingsProvider> tsp = rules->getTradingSettingsProvider();
    if (!tsp) { cpy(err_out,err_len,"no trading settings provider"); return -4; }
    int baseUnit = tsp->getBaseUnitSize(instrument, account);
    if (baseUnit <= 0) { cpy(err_out,err_len,"non-positive base unit size"); return -9; }
    if (out) *out = baseUnit;
    return 0;
}

// The tradable account's id and its CURRENT balance. The account is the one order flow targets
// (`firstAccount`, from the login snapshot — see the note there); its balance is read from a
// refresh-table request for the Accounts table, matched BY ID, because the login-time row is frozen
// at login (it kept reporting 49999.87 while the account stood at 49999.80). Returns 0, or a
// negative code: -2 no tradable account, -4/-5/-6/-8 a refresh failure (`refreshTable`), -7 the
// refreshed table does not hold the account. On a failure `acct_out` holds the reason as text.
FC_EXPORT int fc_account(void* hp, char* acct_out, int acct_len, double* bal_out) {
    Handle* h = (Handle*)hp; if (!h) return -1;
    O2G2Ptr<IO2GAccountRow> a = firstAccount(h->session);
    if (!a) { cpy(acct_out, acct_len, "no account"); return -2; }
    double balance = 0.0;
    if (bal_out) {
        O2G2Ptr<IO2GResponse> resp; std::string err;
        int rc = refreshTable(h->session, Accounts, 0, resp, err);
        if (rc != 0) { cpy(acct_out, acct_len, err.c_str()); return rc; }
        O2G2Ptr<IO2GResponseReaderFactory> rf = h->session->getResponseReaderFactory();
        if (!rf) { cpy(acct_out, acct_len, "no response reader factory"); return -4; }
        O2G2Ptr<IO2GAccountsTableResponseReader> rd = rf->createAccountsTableReader(resp);
        if (!rd) { cpy(acct_out, acct_len, "the response is not an Accounts table"); return -7; }
        bool found = false;
        for (int i = 0; i < rd->size() && !found; ++i) {
            O2G2Ptr<IO2GAccountRow> row = rd->getRow(i);
            if (row && strcmp(row->getAccountID(), a->getAccountID()) == 0) {
                balance = row->getBalance(); found = true;
            }
        }
        if (!found) { cpy(acct_out, acct_len, "the refreshed Accounts table does not hold the account"); return -7; }
    }
    cpy(acct_out, acct_len, a->getAccountID());
    if (bal_out) *bal_out = balance;
    return 0;
}

// ---------------- reconcile table snapshots (read-only) ----------------
// The Orders/Trades table snapshots the `ReconClient` (recon_client.rs) diffs against local state.
// Each is a FRESH refresh-table request (`refreshTable`) for the tradable account — the account order
// flow targets, `firstAccount` — enumerated with the response-reader factory. It is NOT the
// login-rules table-refresh response: Orders and Trades are never loaded at login (see "fresh table
// reads" above), so that path returned NULL, and this file turned NULL into "[]" for every session.
// The row's human instrument is resolved from its offer id via instrumentForOfferId() (the base
// IO2GOrderRow/IO2GTradeRow carry getOfferID but NOT getInstrument — the same limitation the async
// EventListener already works around). Each returns a JSON array string via emit_json's
// grow-and-retry length protocol — a table that does not fit makes the Rust side call again, which
// is a second refresh, and it keeps going until the table fits.
//
// ⚠ A read that FAILED is a NEGATIVE return, never "[]". The reconcile parsers read an empty trades
// snapshot as a FLAT POSITION and an empty orders snapshot as "nothing resting" (the restore planner
// reads that as "cancelled"), so an empty table must mean the venue SAID the table is empty. On a
// negative return `out` holds the reason as text (the Rust side shows it; a caller that ignores it
// loses nothing). Codes: -1 no handle, -2 no tradable account, -4 no factory, -5 the request could
// not be built, -6 the venue failed it, -7 the response is not the table asked for, -8 no answer
// inside WAIT_MS.

// Snapshot the current Orders table (resting entry/limit/stop working orders) as a JSON array.
FC_EXPORT int fc_orders(void* hp, char* out, int out_len) {
    Handle* h = (Handle*)hp; if (!h) return -1;
    O2G2Ptr<IO2GResponse> resp; std::string err;
    int rc = refreshForAccount(h->session, Orders, resp, err);
    if (rc != 0) { cpy(out, out_len, err.c_str()); return rc; }
    O2G2Ptr<IO2GResponseReaderFactory> rf = h->session->getResponseReaderFactory();
    if (!rf) { cpy(out, out_len, "no response reader factory"); return -4; }
    O2G2Ptr<IO2GOrdersTableResponseReader> rd = rf->createOrdersTableReader(resp);
    if (!rd) { cpy(out, out_len, "the response is not an Orders table"); return -7; }
    std::string json = "[";
    char buf[512];
    for (int i = 0; i < rd->size(); ++i) {
        O2G2Ptr<IO2GOrderRow> o = rd->getRow(i);
        if (!o) continue;
        std::string instr = instrumentForOfferId(h->session, o->getOfferID());
        snprintf(buf, sizeof(buf),
            "%s{\"order_id\":\"%s\",\"instrument\":\"%s\",\"buysell\":\"%s\","
            "\"amount\":%d,\"type\":\"%s\",\"status\":\"%s\"}",
            (json.size() > 1 ? "," : ""),
            jesc(o->getOrderID()).c_str(), jesc(instr.c_str()).c_str(),
            jesc(o->getBuySell()).c_str(), o->getAmount(),
            jesc(o->getType()).c_str(), jesc(o->getStatus()).c_str());
        json += buf;
    }
    json += "]";
    return emit_json(json, out, out_len);
}

// Snapshot the current Trades table (open positions) as a JSON array. Reconcile derives BOTH the
// net position report AND the per-open-trade fill report from this one snapshot: each row's
// trade_id is the SAME id the async fill lane (EventListener, Trades(Insert)) reports, so the
// reconcile engine's trade_id dedup lines up and no execution is booked twice.
FC_EXPORT int fc_trades(void* hp, char* out, int out_len) {
    Handle* h = (Handle*)hp; if (!h) return -1;
    O2G2Ptr<IO2GResponse> resp; std::string err;
    int rc = refreshForAccount(h->session, Trades, resp, err);
    if (rc != 0) { cpy(out, out_len, err.c_str()); return rc; }
    O2G2Ptr<IO2GResponseReaderFactory> rf = h->session->getResponseReaderFactory();
    if (!rf) { cpy(out, out_len, "no response reader factory"); return -4; }
    O2G2Ptr<IO2GTradesTableResponseReader> rd = rf->createTradesTableReader(resp);
    if (!rd) { cpy(out, out_len, "the response is not a Trades table"); return -7; }
    std::string json = "[";
    char buf[512];
    for (int i = 0; i < rd->size(); ++i) {
        O2G2Ptr<IO2GTradeRow> t = rd->getRow(i);
        if (!t) continue;
        std::string instr = instrumentForOfferId(h->session, t->getOfferID());
        // `trade_id_origin` is the trade this one was SPLIT from by a partial close (empty for a trade an order opened):
        // the reconcile fill report reads the remainder as the trade the core already folded.
        snprintf(buf, sizeof(buf),
            "%s{\"trade_id\":\"%s\",\"order_id\":\"%s\",\"instrument\":\"%s\","
            "\"buysell\":\"%s\",\"amount\":%d,\"open_rate\":%.6f,\"commission\":%.6f,"
            "\"trade_id_origin\":\"%s\"}",
            (json.size() > 1 ? "," : ""),
            jesc(t->getTradeID()).c_str(), jesc(t->getOpenOrderID()).c_str(),
            jesc(instr.c_str()).c_str(), jesc(t->getBuySell()).c_str(),
            t->getAmount(), t->getOpenRate(), t->getCommission(),
            jesc(t->getTradeIDOrigin()).c_str());
        json += buf;
    }
    json += "]";
    return emit_json(json, out, out_len);
}

// ---------------- the two reads decision 0183 (multi-currency) needs ----------------
// Both are OPTIONAL exports: the loader binds them by NAME alone and no ABI bump goes with them (no
// existing entry point changed meaning), so a shim installed before them degrades to a named answer on
// the Rust side, never to a refusal of the venue. Both are READ-ONLY, take the reconcile protocol
// (`emit_json`'s grow-and-retry length, a NEGATIVE return carrying its reason in `out`) and are called on
// the RECONCILE session's thread, never from an SDK callback (`fc_offers` waits for one).
//
// ⚠ UNVERIFIED-WITHOUT-A-SESSION: they are written against the SDK headers and documentation of this tree's
// `vendor/fcsdk`, and read documented accessors. They were TYPE-CHECKED against the SDK's headers on the CI box
// (`g++ -std=c++11 -Wall -Wextra -fsyntax-only`, 2026-10-19: no diagnostic in these two functions), but never
// linked into a shim and never run: no demo login has executed them. What the account's currency and an
// Offers row actually hold on the demo is proven only by a demo run (`tests/fxcm_currency_smoke.rs`,
// read-only).

// The login's SYSTEM PROPERTIES as one flat JSON object of strings. This is where the ACCOUNT'S
// CURRENCY lives: the Accounts table has NO currency column (`IO2GAccountRow` carries a balance "in the
// account currency" and never names it), but the SDK documents the system property `BASE_CRNCY` ("the
// base currency of the system ... the currency of the account", e.g. `USD`) as "available immediately
// after login". The shim dumps every property and parses NONE of them: the Rust side picks the one it
// wants, so a property this shim never heard of costs nothing. It is a login-time read of the login
// rules, not a request, so it cannot time out; -4 means the login rules hold no such response, -7 that
// the response is not a system-properties one.
FC_EXPORT int fc_system_properties(void* hp, char* out, int out_len) {
    Handle* h = (Handle*)hp; if (!h) return -1;
    O2G2Ptr<IO2GLoginRules> rules = h->session->getLoginRules();
    if (!rules) { cpy(out, out_len, "no login rules"); return -4; }
    O2G2Ptr<IO2GResponse> resp = rules->getSystemPropertiesResponse();
    if (!resp) { cpy(out, out_len, "the login rules hold no system properties response"); return -4; }
    O2G2Ptr<IO2GResponseReaderFactory> rf = h->session->getResponseReaderFactory();
    if (!rf) { cpy(out, out_len, "no response reader factory"); return -4; }
    O2G2Ptr<IO2GSystemPropertiesReader> rd = rf->createSystemPropertiesReader(resp);
    if (!rd) { cpy(out, out_len, "the response is not a system properties response"); return -7; }
    std::string json = "{";
    for (int i = 0; i < rd->size(); ++i) {
        const char* value = 0;
        const char* name = rd->getProperty(i, value);
        if (!name || !*name) continue;
        if (json.size() > 1) json += ",";
        json += "\""; json += jesc(name); json += "\":\""; json += jesc(value); json += "\"";
    }
    json += "}";
    return emit_json(json, out, out_len);
}

// A finite double, else 0.0 (which the Rust side refuses as a price): `snprintf` writes `nan` / `inf`
// for the others, and those are not JSON, so one bad row would make the whole table unreadable.
static double finite_or_zero(double v) { return std::isfinite(v) ? v : 0.0; }

// How OLD an Offers row's quote is, in milliseconds, or false when the venue gave no usable time. The row's
// `getTime()` (the last price update) and the session's `getServerTime()` are both `DATE`s (OLE days since
// 1899-12-30, as a double); only their DIFFERENCE is used, so the time zone cancels PROVIDED the two share one.
//
// ⚠ UNVERIFIED-WITHOUT-A-SESSION: that they share one is read off the headers (`IO2GOfferRow.h`,
// `IO2GSession.h`), not measured; no demo login has run this. It fails CLOSED either way: if they do not share a
// frame the difference is hours, never seconds, and the Rust side refuses a quote older than 120 s or
// stamped more than a few seconds in the future (`home_conversion::quote_age`), so a wrong guess costs a
// conversion rate, never a wrong one. A frozen quote that still says "open" ages here, which the ask time of
// the refresh cannot show.
static bool quote_age_ms(IO2GSession* s, IO2GOfferRow* o, long long& out) {
    if (!o->isTimeValid()) return false;
    double now = s->getServerTime();
    double then = o->getTime();
    if (!std::isfinite(now) || !std::isfinite(then) || now <= 0.0 || then <= 0.0) return false;
    double ms = (now - then) * 86400000.0;
    if (!std::isfinite(ms) || ms > 9e15 || ms < -9e15) return false;
    out = (long long)std::llround(ms);
    return true;
}

// The CURRENT Offers table (every instrument the login sees) as a JSON array: a refresh-table request,
// never the login-time snapshot, which froze at login (a quote that never moved). One row per offer, with
// what the Rust side needs to price a conversion and to build a grid: the instrument, its two-sided quote,
// its price precision, its type, contract currency and multiplier, whether it is subscribed and open, and
// the BASE UNIT SIZE on the tradable account (the lot: `Amount` is `baseUnit * lots`; 0 when no account
// is eligible). The rows are not filtered by subscription: the Rust side judges each one it uses.
// `ContractCurrency` is the BASE currency of a forex pair ("EUR in EUR/USD"), per the SDK's Offers
// table page, and the NATIVE (quote) currency of every other class (MEASURED on all 449 non-forex rows of the EUR demo).
// The second half of each row (`offer_id` ... `can_stop_limit`) is the instrument's TERMS from the trading settings and
// the permission checker, additive: `min_qty` / `max_qty` are in BASE UNITS, `mmr` / `emr` / `lmr` are the margins of
// one lot in the account currency (-1 when the session cannot state them).
FC_EXPORT int fc_offers(void* hp, char* out, int out_len) {
    Handle* h = (Handle*)hp; if (!h) return -1;
    O2G2Ptr<IO2GResponse> resp; std::string err;
    int rc = refreshTable(h->session, Offers, 0, resp, err, FACTS_REFRESH_MS);
    if (rc != 0) { cpy(out, out_len, err.c_str()); return rc; }
    O2G2Ptr<IO2GResponseReaderFactory> rf = h->session->getResponseReaderFactory();
    if (!rf) { cpy(out, out_len, "no response reader factory"); return -4; }
    O2G2Ptr<IO2GOffersTableResponseReader> rd = rf->createOffersTableReader(resp);
    if (!rd) { cpy(out, out_len, "the response is not an Offers table"); return -7; }
    O2G2Ptr<IO2GAccountRow> account = firstAccount(h->session);
    O2G2Ptr<IO2GLoginRules> rules = h->session->getLoginRules();
    O2G2Ptr<IO2GTradingSettingsProvider> tsp;
    if (rules) tsp = rules->getTradingSettingsProvider();
    O2G2Ptr<IO2GPermissionChecker> perm;
    if (rules) perm = rules->getPermissionChecker();
    std::string json = "[";
    char buf[2048];
    for (int i = 0; i < rd->size(); ++i) {
        O2G2Ptr<IO2GOfferRow> o = rd->getRow(i);
        if (!o) continue;
        const char* name = o->getInstrument();
        int baseUnit = (account && tsp) ? tsp->getBaseUnitSize(name, account) : 0;
        // The instrument's TERMS (the second half of the row, additive: a reader that wants the first half only
        // ignores them). ⚠ `min_qty` / `max_qty` are in BASE UNITS, the same unit as `Amount`, whatever the SDK header says
        // ("in lots"): MEASURED on the demo, EUR/USD reads 1000 / 50000000 with a base unit of 1000, US30 reads 1 / 4000
        // with a base unit of 1. Money (`mmr`, `emr`, `lmr`) is in the ACCOUNT currency, per lot, as the SDK states; a
        // figure the session cannot give is -1 (2 for a status), never a guess (`getMMR` answers -1 on the EUR demo).
        int minQty = -1, maxQty = -1, progressive = -1, marketStatus = 2, threeLevel = -1;
        int condStop = -1, condLimit = -1, condEntryStop = -1, condEntryLimit = -1;
        double mmr = -1.0, levelMmr = -1.0, emr = -1.0, lmr = -1.0;
        if (account && tsp) {
            minQty = tsp->getMinQuantity(name, account);
            maxQty = tsp->getMaxQuantity(name, account);
            mmr = tsp->getMMR(name, account);
            progressive = tsp->isProgressiveMargin(name, account) ? 1 : 0;
            // The three margin levels of an account on the three-level policy (`getMMR` answers -1 there).
            threeLevel = tsp->getMargins(name, account, levelMmr, emr, lmr) ? 1 : 0;
        }
        if (tsp) {
            marketStatus = (int)tsp->getMarketStatus(name);
            condStop = tsp->getCondDistStopForTrade(name);
            condLimit = tsp->getCondDistLimitForTrade(name);
            condEntryStop = tsp->getCondDistEntryStop(name);
            condEntryLimit = tsp->getCondDistEntryLimit(name);
        }
        // O2GPermissionStatus: 1 enabled, 0 disabled, -2 hidden, 2 unknown (also when there is no checker).
        int canMarketOpen = perm ? (int)perm->canCreateMarketOpenOrder(name) : 2;
        int canMarketClose = perm ? (int)perm->canCreateMarketCloseOrder(name) : 2;
        int canEntry = perm ? (int)perm->canCreateEntryOrder(name) : 2;
        int canStopLimit = perm ? (int)perm->canCreateStopLimitOrder(name) : 2;
        // `null` when the venue gave no usable time: the Rust side then refuses the row as a rate.
        char age[32] = "null";
        long long ageMs = 0;
        if (quote_age_ms(h->session, o, ageMs)) snprintf(age, sizeof(age), "%lld", ageMs);
        snprintf(buf, sizeof(buf),
            "%s{\"instrument\":\"%s\",\"bid\":%.8f,\"ask\":%.8f,\"digits\":%d,"
            "\"instrument_type\":%d,\"contract_currency\":\"%s\",\"contract_multiplier\":%.8f,"
            "\"subscription\":\"%s\",\"trading_status\":\"%s\",\"bid_tradable\":\"%s\","
            "\"ask_tradable\":\"%s\",\"base_unit\":%d,\"quote_age_ms\":%s,"
            "\"offer_id\":\"%s\",\"point_size\":%.8f,\"buy_interest\":%.8f,\"sell_interest\":%.8f,"
            "\"min_qty\":%d,\"max_qty\":%d,\"mmr\":%.8f,\"three_level\":%d,\"level_mmr\":%.8f,\"emr\":%.8f,"
            "\"lmr\":%.8f,\"progressive_margin\":%d,\"market_status\":%d,"
            "\"cond_dist_stop\":%d,\"cond_dist_limit\":%d,\"cond_dist_entry_stop\":%d,"
            "\"cond_dist_entry_limit\":%d,\"can_market_open\":%d,\"can_market_close\":%d,"
            "\"can_entry\":%d,\"can_stop_limit\":%d}",
            (json.size() > 1 ? "," : ""),
            jesc(name).c_str(), finite_or_zero(o->getBid()), finite_or_zero(o->getAsk()),
            o->getDigits(), (int)o->getInstrumentType(),
            jesc(o->getContractCurrency()).c_str(), finite_or_zero(o->getContractMultiplier()),
            jesc(o->getSubscriptionStatus()).c_str(), jesc(o->getTradingStatus()).c_str(),
            jesc(o->getBidTradable()).c_str(), jesc(o->getAskTradable()).c_str(), baseUnit, age,
            jesc(o->getOfferID()).c_str(), finite_or_zero(o->getPointSize()),
            finite_or_zero(o->getBuyInterest()), finite_or_zero(o->getSellInterest()),
            minQty, maxQty, finite_or_zero(mmr), threeLevel, finite_or_zero(levelMmr), finite_or_zero(emr),
            finite_or_zero(lmr), progressive, marketStatus,
            condStop, condLimit, condEntryStop, condEntryLimit,
            canMarketOpen, canMarketClose, canEntry, canStopLimit);
        json += buf;
    }
    json += "]";
    return emit_json(json, out, out_len);
}

static bool sameStr(const char* a, const char* b) { return a && b && strcmp(a, b) == 0; }

// The price grid (10^-digits) of the Offers row with this id, from the login-time snapshot (the id ->
// grid mapping does not move); 0.0 when it cannot be read, which callers treat as "no grid known".
static double tickForOfferId(IO2GSession* s, const char* offerId) {
    if (!offerId || !*offerId) return 0.0;
    O2G2Ptr<IO2GLoginRules> rules = s->getLoginRules();
    if (!rules) return 0.0;
    O2G2Ptr<IO2GResponse> resp = rules->getTableRefreshResponse(Offers);
    if (!resp) return 0.0;
    O2G2Ptr<IO2GResponseReaderFactory> rf = s->getResponseReaderFactory();
    if (!rf) return 0.0;
    O2G2Ptr<IO2GOffersTableResponseReader> rd = rf->createOffersTableReader(resp);
    if (!rd) return 0.0;
    for (int i = 0; i < rd->size(); ++i) {
        O2G2Ptr<IO2GOfferRow> o = rd->getRow(i);
        if (o && sameStr(o->getOfferID(), offerId)) {
            int digits = o->getDigits();
            return (digits >= 0 && digits <= 8) ? std::pow(10.0, -digits) : 0.0;
        }
    }
    return 0.0;
}

// What a placement SENT, to check a row found by RequestID against. `limit` is the shim's own order
// kind (LimitEntry) and `rate` is only meaningful for it; `tick` is the instrument's price grid (0.0
// when unknown).
struct SentTerms { const char* offerId; const char* buySell; int amount; bool limit; double rate; double tick; };

// Collect the ids of every order in a fresh Orders response. False when the response is unreadable.
static bool collectOrderIds(IO2GSession* s, IO2GResponse* resp, std::set<std::string>& ids) {
    O2G2Ptr<IO2GResponseReaderFactory> rf = s->getResponseReaderFactory();
    if (!rf) return false;
    O2G2Ptr<IO2GOrdersTableResponseReader> rd = rf->createOrdersTableReader(resp);
    if (!rd) return false;
    for (int i = 0; i < rd->size(); ++i) {
        O2G2Ptr<IO2GOrderRow> o = rd->getRow(i);
        if (o && o->getOrderID()) ids.insert(o->getOrderID());
    }
    return true;
}

// Find, in a FRESH Orders table, the order a request created. 0 and the order id in `orderId`; -3 not
// there; FC_LOOKUP_AMBIGUOUS when more than one row fits; else the refresh failure code with its
// reason in `err`. The way out of an ambiguous placement (no answer, or an answer with no order id) and
// the body of `fc_find_order`.
//
// ⚠ **The RequestID alone is NOT identity.** The resting limit entries this shim places live across
// sessions, and nothing here proves a RequestID is never reused by a later session, so a row is ours
// only when ALL of these hold: its RequestID is the one sent (a NULL one never matches); its order id
// was not already in the table BEFORE the request was sent (`before`, empty when no snapshot exists);
// and its offer, side, amount, kind and (for a limit) rate are what was sent, the rate within half a
// tick. Two rows that fit are reported as FC_LOOKUP_AMBIGUOUS and NEITHER is claimed: a modify or
// cancel aimed at someone else's order is worse than an unknown outcome the book can wait out.
static int findOrderByRequestId(IO2GSession* s, const char* accountId, const char* requestId,
        const SentTerms& sent, const std::set<std::string>& before,
        std::string& orderId, std::string& err) {
    if (!requestId || !*requestId) { err = "no request id to look up"; return -5; }
    O2G2Ptr<IO2GResponse> resp;
    int rc = refreshTable(s, Orders, accountId, resp, err, LOOKUP_REFRESH_MS);
    if (rc != 0) return rc;
    O2G2Ptr<IO2GResponseReaderFactory> rf = s->getResponseReaderFactory();
    if (!rf) { err = "no response reader factory"; return -4; }
    O2G2Ptr<IO2GOrdersTableResponseReader> rd = rf->createOrdersTableReader(resp);
    if (!rd) { err = "the response is not an Orders table"; return -7; }
    const double halfTick = (sent.tick / 2.0) > 1e-9 ? (sent.tick / 2.0) : 1e-9;
    int hits = 0;
    for (int i = 0; i < rd->size(); ++i) {
        O2G2Ptr<IO2GOrderRow> o = rd->getRow(i);
        if (!o || !sameStr(o->getRequestID(), requestId)) continue;
        const char* oid = o->getOrderID();
        if (!oid || before.count(oid)) continue;
        if (!sameStr(o->getOfferID(), sent.offerId) || !sameStr(o->getBuySell(), sent.buySell)) continue;
        if (o->getAmount() != sent.amount && o->getOriginAmount() != sent.amount) continue;
        const char* type = o->getType();
        if ((type && type[0] == 'L') != sent.limit) continue;  // LE / L = a limit entry
        if (sent.limit && std::fabs(o->getRate() - sent.rate) > halfTick) continue;
        if (++hits == 1) orderId = oid;
    }
    if (hits > 1) { orderId.clear(); err = "more than one order in the Orders table fits the request"; return FC_LOOKUP_AMBIGUOUS; }
    return hits == 1 ? 0 : -3;
}

// Shared CreateOrder path for both placement kinds. Resolves account + offer, sizes the order from
// the venue's base unit, builds the value map and blocks on the response listener for the created
// order id. On success returns 0 and fills order_id/offer_id/account_id (needed to cancel).
//
// `exact_rate` (non-null, limit entries only) is a rate the CALLER chose: it is used as given (after
// `snapRate`'s grid check) and the quote is NOT consulted for it, so the offer comes from the login
// snapshot (only its id and digits are read). Null keeps the old behaviour: a rate derived from the
// FRESH quote and `pips_away`.
//
// `market` selects the order type and therefore the value-map SHAPE, which differs between the two:
// a true-market order carries NO `Rate` key (O2G2::Orders::TrueMarketOpen, "OM" — the shape the
// SDK's own NonTableManagerSamples/OpenPosition sample sends), whereas a limit entry MUST carry the
// rate to rest at. Passing a Rate alongside TrueMarketOpen is what the venue rejects, so the key is
// set only on the limit branch. Negative return codes are shared by both exports. -11/-12 mean the
// request was SENT and its outcome is UNKNOWN (see FC_SENT_NO_ANSWER); everything else negative is a
// definite "not placed" (or, for -6, the venue's own refusal; for -16, `FC_ORDER_NOT_PERMITTED`, the login's own
// permission for this order kind on this instrument reading Disabled: refused by name, nothing sent).
static int fc_place(Handle* h, const char* instrument, const char* buysell,
        int pips_away, const double* exact_rate, int lots, bool market, char* order_id_out, int oid_len,
        char* offer_id_out, int ofid_len, char* acct_out, int acct_len,
        double* rate_out, char* err_out, int err_len) {
    if (!h) return -1;
    const std::string venue_name = venueName(h->session, instrument);
    instrument = venue_name.c_str();
    O2G2Ptr<IO2GAccountRow> account = firstAccount(h->session);
    if (!account) { cpy(err_out,err_len,"no account"); return -2; }
    // A LIMIT entry priced off the quote reads the quote AS IT STANDS (a refresh-table request; the
    // login snapshot froze at login and a resting order priced off it can sit anywhere). A market
    // order, and a limit at a rate the caller named, use the offer's id (and digits) alone and keep
    // the snapshot — see `findOffer`. A refresh that fails refuses the order here, BEFORE anything is
    // sent; it never falls back to the stale quote.
    OfferHold offer; std::string offerErr;
    const bool fresh = !market && !exact_rate;
    int orc = findOffer(h->session, instrument, fresh, offer, offerErr);
    if (orc == -3) { cpy(err_out, err_len, offerErr.empty() ? "offer not found" : offerErr.c_str()); return -3; }
    if (orc != 0) {
        cpy(err_out, err_len, ("cannot read the current quote: " + offerErr).c_str());
        return orc;
    }
    // BEFORE anything is built or sent: an order the account may not create is refused by name. Left to the venue it
    // comes back as `ORA-20102: Access Violation`, which says nothing about why (USD/HKD on the demo login).
    if (orderCreationDisabled(h->session, instrument, market)) {
        std::string why = std::string("the account may not create ") + (market ? "market" : "entry") + " orders on " + instrument +
            ": FXCM's permission for it reads Disabled (the venue answers such an order `ORA-20102: Access Violation`). Nothing was sent";
        cpy(err_out, err_len, why.c_str());
        return FC_ORDER_NOT_PERMITTED;
    }
    cpy(acct_out, acct_len, account->getAccountID());
    cpy(offer_id_out, ofid_len, offer->getOfferID());

    // ⚠ The SAME two lookups `fc_base_unit_size` exports, and that is the whole point of exporting
    // them: the Rust side divides the caller's base-unit `qty` by this figure to get `lots`, and
    // this line multiplies it straight back. Change the source of one and the round trip stops
    // being an identity.
    O2G2Ptr<IO2GLoginRules> rules = h->session->getLoginRules();
    O2G2Ptr<IO2GTradingSettingsProvider> tsp = rules->getTradingSettingsProvider();
    int baseUnit = tsp->getBaseUnitSize(instrument, account);
    int amount = baseUnit * lots;

    // LIMIT entry: buy below ask, sell above bid -> rests, never fills. A market order executes at
    // whatever the venue quotes, so it has no shim-computed rate (reported back as 0.0).
    double rate = 0.0;
    if (!market && exact_rate) {
        std::string gridErr;
        int grc = snapRate(offer.row, *exact_rate, rate, gridErr);
        if (grc != 0) { cpy(err_out, err_len, gridErr.c_str()); return grc; }
    } else if (!market) {
        double pt = offer->getPointSize();
        rate = (strcmp(buysell, O2G2::Buy)==0)
            ? (offer->getAsk() - pips_away * pt)
            : (offer->getBid() + pips_away * pt);
    }
    if (rate_out) *rate_out = rate;

    O2G2Ptr<IO2GRequestFactory> rf = h->session->getRequestFactory();
    if (!rf) { cpy(err_out,err_len,"no request factory"); return -4; }
    O2G2Ptr<IO2GValueMap> vm = rf->createValueMap();
    vm->setString(Command, O2G2::Commands::CreateOrder);
    vm->setString(OrderType, market ? O2G2::Orders::TrueMarketOpen : O2G2::Orders::LimitEntry);
    vm->setString(AccountID, account->getAccountID());
    vm->setString(OfferID, offer->getOfferID());
    vm->setString(BuySell, buysell);
    vm->setInt(Amount, amount);
    if (!market) vm->setDouble(Rate, rate);
    vm->setString(CustomID, market ? "RustShimMarket" : "RustShimEntry");
    O2G2Ptr<IO2GRequest> req = rf->createOrderRequest(vm);
    if (!req) { cpy(err_out, err_len, rf->getLastError()); return -5; }

    // The orders ALREADY in the table, so a lookup after a lost reply can never claim one of them as
    // ours (see `findOrderByRequestId`). Limit entries only: they are what rests across sessions; a
    // market order leaves the table at once. Best effort — a refresh that fails here leaves the set
    // empty and the lookup then rests on its other checks (RequestID, offer, side, amount, kind, rate),
    // it does not refuse an order that has not been sent. Bounded by PRESEND_REFRESH_MS.
    std::set<std::string> before;
    if (!market) {
        O2G2Ptr<IO2GResponse> snap; std::string snapErr;
        if (refreshTable(h->session, Orders, account->getAccountID(), snap, snapErr, PRESEND_REFRESH_MS) != 0
                || !collectOrderIds(h->session, snap, before))
            before.clear();
    }
    SentTerms sent = { offer->getOfferID(), buysell, amount, !market, rate, 0.0 };
    { int digits = offer->getDigits(); if (digits >= 0 && digits <= 8) sent.tick = std::pow(10.0, -digits); }

    RespListener* rl = new RespListener(h->session);
    const std::string reqId = req->getRequestID();
    rl->setRequestID(reqId.c_str());
    h->session->subscribeResponse(rl);
    h->session->sendRequest(req);
    int rc = 0, ambiguous = 0;
    if (rl->wait()) {
        std::string failure = rl->error(), oid = rl->orderId();
        if (!failure.empty()) { cpy(err_out, err_len, failure.c_str()); rc = -6; }
        else if (!oid.empty()) { cpy(order_id_out, oid_len, oid.c_str()); rc = 0; }
        else ambiguous = FC_SENT_NO_ORDER_ID;
    } else ambiguous = FC_SENT_NO_ANSWER;
    h->session->unsubscribeResponse(rl); rl->release();
    if (ambiguous) {
        // ⚠ The request was SENT. It may rest on the venue though no usable answer reached us, so look
        // for it by its RequestID in a fresh Orders table before calling the outcome unknown. Never
        // report it as a refusal: that is how a live order ends up unknown to the book.
        std::string found, lookupErr;
        int lrc = findOrderByRequestId(h->session, account->getAccountID(), reqId.c_str(), sent, before,
            found, lookupErr);
        if (lrc == 0) { cpy(order_id_out, oid_len, found.c_str()); return 0; }
        std::string why = ambiguous == FC_SENT_NO_ANSWER
            ? "the venue gave no answer inside the wait" : "the venue answered without an order id";
        why += lrc == -3 ? "; a fresh Orders table does not hold it either"
                         : "; the Orders lookup failed: " + lookupErr;
        cpy(err_out, err_len, ("outcome UNKNOWN, the request was sent: " + why).c_str());
        // ⚠ On an UNKNOWN outcome `order_id_out` carries the REQUEST id (never an order id: there is
        // none), so the Rust side can ask `fc_find_order` again later, once the venue has had time to
        // book it. `offer_id_out`, `acct_out` and `rate_out` were filled before the send. A definite
        // failure leaves the buffer empty, and an older Rust binary never reads it on a failure.
        cpy(order_id_out, oid_len, reqId.c_str());
        return ambiguous;
    }
    return rc;
}

// Places a resting LIMIT-entry `pips_away` from market (buy below ask / sell above bid) so it can't fill.
FC_EXPORT int fc_place_entry(void* hp, const char* instrument, const char* buysell,
        int pips_away, int lots, char* order_id_out, int oid_len,
        char* offer_id_out, int ofid_len, char* acct_out, int acct_len,
        double* rate_out, char* err_out, int err_len) {
    return fc_place((Handle*)hp, instrument, buysell, pips_away, /*exact_rate=*/0, lots, /*market=*/false,
        order_id_out, oid_len, offer_id_out, ofid_len, acct_out, acct_len, rate_out,
        err_out, err_len);
}

// Places a resting LIMIT entry (O2G2::Orders::LimitEntry, "LE") AT `rate` — the rate the CALLER chose.
//
// ABI 2. `fc_place_entry` above takes a pip DISTANCE, not a price, and that — an `int pips_away` in
// the signature — is the whole reason the Rust adapter could not honor a requested limit price: the
// SDK never limited it (a LE order's `Rate` is just a double in the value map, set below exactly as
// `fc_place_entry` always did with its computed one). A rate off the instrument's price grid is
// REFUSED (-10), never rounded — see `snapRate`. A rate on the wrong side of the market (a buy limit
// at or above the ask) is the VENUE's to refuse; it comes back as -6 with the venue's own words.
// `rate_out` echoes the rate actually sent (the snapped one).
FC_EXPORT int fc_place_entry_at(void* hp, const char* instrument, const char* buysell,
        double rate, int lots, char* order_id_out, int oid_len,
        char* offer_id_out, int ofid_len, char* acct_out, int acct_len,
        double* rate_out, char* err_out, int err_len) {
    return fc_place((Handle*)hp, instrument, buysell, /*pips_away=*/0, &rate, lots, /*market=*/false,
        order_id_out, oid_len, offer_id_out, ofid_len, acct_out, acct_len, rate_out,
        err_out, err_len);
}

// Amends the RATE of a resting entry order in place (O2G2::Commands::EditOrder; the order keeps its
// id). ABI 2. PRICE ONLY — no size: the adapter refuses a size change before it gets here. The
// request carries Command, OrderID, AccountID and Rate; the SDK headers in this tree document the
// command name and the parameter enum but not the per-command parameter set, so whether the venue
// also wants an OfferID is settled by the first demo amend, and a refusal comes back as -6 with the
// venue's own words. `instrument` is only used to read the price precision for `snapRate`.
// Codes as `fc_place`: -1 no handle, -3 offer not found, -4 no factory, -5 request not buildable,
// -6 the venue failed it, -11 SENT but no answer inside ORDER_WAIT_MS (outcome UNKNOWN: the amend may have
// applied, so it is never a refusal); -10 the rate is off the price grid.
FC_EXPORT int fc_edit_order(void* hp, const char* orderId, const char* accountId,
        const char* instrument, double rate, char* err_out, int err_len) {
    Handle* h = (Handle*)hp; if (!h) return -1;
    const std::string venue_name = venueName(h->session, instrument);
    instrument = venue_name.c_str();
    OfferHold offer; std::string offerErr;
    int orc = findOffer(h->session, instrument, /*fresh=*/false, offer, offerErr);
    if (orc != 0) {
        cpy(err_out, err_len, (orc == -3 && offerErr.empty()) ? "offer not found" : offerErr.c_str());
        return orc;
    }
    double snapped = 0.0; std::string gridErr;
    int grc = snapRate(offer.row, rate, snapped, gridErr);
    if (grc != 0) { cpy(err_out, err_len, gridErr.c_str()); return grc; }
    O2G2Ptr<IO2GRequestFactory> rf = h->session->getRequestFactory();
    if (!rf) { cpy(err_out, err_len, "no request factory"); return -4; }
    O2G2Ptr<IO2GValueMap> vm = rf->createValueMap();
    vm->setString(Command, O2G2::Commands::EditOrder);
    vm->setString(OrderID, orderId);
    vm->setString(AccountID, accountId);
    vm->setDouble(Rate, snapped);
    O2G2Ptr<IO2GRequest> req = rf->createOrderRequest(vm);
    if (!req) { cpy(err_out, err_len, rf->getLastError()); return -5; }
    RespListener* rl = new RespListener(h->session);
    rl->setRequestID(req->getRequestID());
    h->session->subscribeResponse(rl);
    h->session->sendRequest(req);
    int rc = 0;
    if (rl->wait()) { std::string failure = rl->error(); if (!failure.empty()) { cpy(err_out, err_len, failure.c_str()); rc = -6; } }
    else {
        // The EditOrder was SENT and may have applied: -11, never a refusal. The caller re-reads the
        // order's rate (`fc_order_rate`) to learn which.
        cpy(err_out, err_len, "outcome UNKNOWN, the amend was sent and the venue gave no answer inside the wait");
        rc = FC_SENT_NO_ANSWER;
    }
    h->session->unsubscribeResponse(rl); rl->release();
    return rc;
}

// The Rate the Orders table holds for ONE order — what the VENUE says the order rests at, read after
// a placement or an amend has completed. Added after ABI 2 as an OPTIONAL export WITHOUT an ABI
// bump: no existing entry point changed meaning, and the loader binds it by name alone, so a shim
// without it degrades to "no echo", never to a refusal.
//
// The request path only proves the venue did not ERROR; `fc_place_entry_at` echoes the rate WE sent.
// This is the venue's own copy (`IO2GOrderRow::getRate()`) from a fresh Orders refresh, so the
// caller can compare it with what it asked for. Read-only. 0 with the rate in `rate_out` (and the
// instrument's tick in `tick_out`, 0 when unknown); else -3 the order is not in the table (reason
// in `err`), or the codes `refreshTable` uses (-4, -5, -6, -8),
// -7 not an Orders table. Never called from an SDK callback thread (it waits for one).
FC_EXPORT int fc_order_rate(void* hp, const char* orderId, const char* accountId,
        double* rate_out, double* tick_out, char* err_out, int err_len) {
    Handle* h = (Handle*)hp; if (!h) return -1;
    if (rate_out) *rate_out = 0.0;
    if (tick_out) *tick_out = 0.0;
    O2G2Ptr<IO2GResponse> resp; std::string err;
    int rc = refreshTable(h->session, Orders, accountId, resp, err, ECHO_REFRESH_MS);
    if (rc != 0) { cpy(err_out, err_len, err.c_str()); return rc; }
    O2G2Ptr<IO2GResponseReaderFactory> rf = h->session->getResponseReaderFactory();
    if (!rf) { cpy(err_out, err_len, "no response reader factory"); return -4; }
    O2G2Ptr<IO2GOrdersTableResponseReader> rd = rf->createOrdersTableReader(resp);
    if (!rd) { cpy(err_out, err_len, "the response is not an Orders table"); return -7; }
    for (int i = 0; i < rd->size(); ++i) {
        O2G2Ptr<IO2GOrderRow> o = rd->getRow(i);
        if (o && strcmp(o->getOrderID(), orderId) == 0) {
            if (rate_out) *rate_out = o->getRate();
            // The price grid (10^-digits), from the login-time Offers snapshot: 0.0 when it cannot be
            // read, which the caller treats as "no grid known", never as an error.
            if (tick_out) {
                OfferHold offer; std::string offerErr;
                std::string instr = instrumentForOfferId(h->session, o->getOfferID());
                if (findOffer(h->session, instr.c_str(), /*fresh=*/false, offer, offerErr) == 0) {
                    int digits = offer->getDigits();
                    if (digits >= 0 && digits <= 8) *tick_out = std::pow(10.0, -digits);
                }
            }
            return 0;
        }
    }
    cpy(err_out, err_len, "the order is not in the Orders table");
    return -3;
}

// Look an order up AGAIN, by the RequestID of the placement whose reply was lost (`fc_place_entry*` /
// `fc_place_market` returned -11/-12 and wrote that RequestID into `order_id_out`). The active
// `confirm` behind the stuck-order watchdog: the Rust exec thread calls it once the venue has had
// time to book the order. Added after ABI 2 as an OPTIONAL export WITHOUT an ABI bump, like
// `fc_order_rate`: no existing entry point changed meaning, the loader binds it by name alone, and a
// shim without it just cannot confirm.
//
// The caller passes back what the placement SENT (`buysell`, `amount` in base units, `limit` non-zero
// for a limit entry with its `rate`), and the match applies every check `findOrderByRequestId` states.
// Unlike the lookup inside a placement it has no pre-send snapshot to exclude: the verification of
// offer, side, amount, kind and rate stands alone. 0 with the order id in `order_id_out`; -3 no such
// order in a fresh Orders table (it never rested, or a market order filled and left); -13 more than one
// fits (none claimed); else the `refreshTable` codes (-4, -5, -6, -7, -8). Read-only. Never from an SDK
// callback thread.
FC_EXPORT int fc_find_order(void* hp, const char* requestId, const char* accountId, const char* offerId,
        const char* buysell, int amount, int limit, double rate,
        char* order_id_out, int oid_len, char* err_out, int err_len) {
    Handle* h = (Handle*)hp; if (!h) return -1;
    SentTerms sent = { offerId, buysell, amount, limit != 0, rate, tickForOfferId(h->session, offerId) };
    std::string found, err; std::set<std::string> none;
    int rc = findOrderByRequestId(h->session, accountId, requestId, sent, none, found, err);
    if (rc == 0) { cpy(order_id_out, oid_len, found.c_str()); return 0; }
    cpy(err_out, err_len, rc == -3 ? "the order is not in the Orders table" : err.c_str());
    return rc;
}

// ---------------- instrument subscription (the owner's "trade ANY ticker of the venue") ----------------
//
// A login sees the venue's whole Offers table (497 rows on the demo login), but each row carries a SUBSCRIPTION
// STATUS: `T` (tradable), `V` (view only; FXCM keeps the conversion legs of the account currency at `V` itself)
// or `D` (disabled). Only `T` rows were ever traded or priced here, and an operator had to subscribe the rest by
// hand in the FXCM UI. The request that does it is the SDK's own `SetSubscriptionStatus` command
// (`NonTableManagerSamples/SubscriptionStatus`): `Command`, `OfferID` and `SubscriptionStatus`, and nothing else
// (no `AccountID`). MEASURED on the demo login (2026-10-26): FXCM answers with a `CommandResponse` 0.19-0.49 s
// later and pushes the Offers update (`subscription` = the new status) at the same moment, to EVERY session of the
// login; an offer id it does not know never leaves the request factory ("OfferID or Symbol is absent.").
//
// `fc_set_subscription` is an OPTIONAL export (no ABI bump). In order:
//   1. returns at once, with NO round trip, when the login-time snapshot already reads `status` for the instrument
//      (MEASURED: the snapshot's STATUS column follows the venue's pushed updates on every session of the login, its
//      prices do not; so an already tradable instrument, the ordinary case, costs the exec thread nothing);
//   2. looks the row up BY NAME in a fresh Offers table (a lookup that FAILS is -8, never a refusal: nothing was sent);
//   3. returns at once when the fresh row reads `status`; refuses a row of a type the SDK does not name (-14);
//   4. sends the request, waits for the answer and then reads the row until it reads `status` (the table read right
//      after the answer was one second behind in 1 of 12 measured requests), all inside `budget_ms`.
// Never from an SDK callback thread (it waits for one).
//   0   the row reads `status` now (`out` = the status)
//   -3  the instrument is not in the Offers table
//   -4/-5 SEND stage only: no request factory / the request could not be built (`out` = the SDK's words), or a bad `status`
//   -6  the venue FAILED the request (`out` = its words, verbatim)
//   -8  the LOOKUP failed (no factory, a refresh that timed out or failed: `out` = why; nothing was sent), or no answer
//       inside the wait (the request may still apply): the outcome is UNKNOWN, never a verdict on the symbol
//   -9  the venue answered and the row still reads another status after the budget (`out` = that status)
//   -14 the instrument is not tradable and its type is none of the SDK's nine: the bridge does not subscribe it
//       (`out` = its type, multiplier and status)
//   -16 the account may create neither a market nor an entry order on the instrument (FC_ORDER_NOT_PERMITTED): nothing
//       is sent (a subscription would change the account for nothing). Checked only when a write would be needed, so an
//       instrument that already reads `T` still answers 0 here and the placement refuses it, by the same code.
static bool snapshotReads(IO2GSession* s, const char* instr, const char* status) {
    O2G2Ptr<IO2GLoginRules> rules = s->getLoginRules();
    if (!rules) return false;
    O2G2Ptr<IO2GResponse> resp = rules->getTableRefreshResponse(Offers);
    if (!resp) return false;
    O2G2Ptr<IO2GResponseReaderFactory> rf = s->getResponseReaderFactory();
    if (!rf) return false;
    O2G2Ptr<IO2GOffersTableResponseReader> rd = rf->createOffersTableReader(resp);
    if (!rd) return false;
    for (int i = 0; i < rd->size(); ++i) {
        O2G2Ptr<IO2GOfferRow> o = rd->getRow(i);
        if (!o || strcmp(instr, o->getInstrument()) != 0) continue;
        const char* st = o->getSubscriptionStatus();
        return st && strcmp(st, status) == 0;
    }
    return false;
}

struct OfferFacts { std::string id, status; int type; double multiplier; };

static int offerRowStatus(IO2GSession* s, const char* instr, unsigned timeoutMs, OfferFacts& facts,
        std::string& err) {
    O2G2Ptr<IO2GResponse> resp;
    int rc = refreshTable(s, Offers, 0, resp, err, timeoutMs);
    if (rc != 0) return rc;
    O2G2Ptr<IO2GResponseReaderFactory> rf = s->getResponseReaderFactory();
    if (!rf) { err = "no response reader factory"; return -4; }
    O2G2Ptr<IO2GOffersTableResponseReader> rd = rf->createOffersTableReader(resp);
    if (!rd) { err = "the response is not an Offers table"; return -7; }
    for (int i = 0; i < rd->size(); ++i) {
        O2G2Ptr<IO2GOfferRow> o = rd->getRow(i);
        if (!o || strcmp(instr, o->getInstrument()) != 0) continue;
        facts.id = o->getOfferID() ? o->getOfferID() : "";
        facts.status = o->getSubscriptionStatus() ? o->getSubscriptionStatus() : "";
        facts.type = (int)o->getInstrumentType();
        facts.multiplier = o->getContractMultiplier();
        return 0;
    }
    return -3;
}

FC_EXPORT int fc_set_subscription(void* hp, const char* instrument, const char* status, int budget_ms,
        char* out, int out_len) {
    Handle* h = (Handle*)hp; if (!h) return -1;
    if (!instrument || !*instrument || !status || !status[0] || status[1] ||
            !strchr("TVD", status[0])) {
        cpy(out, out_len, "the instrument is empty or the status is not one of T, V, D");
        return -5;
    }
    const std::string venue_name = venueName(h->session, instrument);
    instrument = venue_name.c_str();
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::milliseconds(budget_ms > 0 ? budget_ms : 0);
    auto left = [&]() -> unsigned {
        auto d = std::chrono::duration_cast<std::chrono::milliseconds>(deadline - std::chrono::steady_clock::now()).count();
        return d > 0 ? (unsigned)d : 0u;
    };
    auto atMost = [](unsigned a, unsigned b) { return a < b ? a : b; };
    if (snapshotReads(h->session, instrument, status)) { cpy(out, out_len, status); return 0; }
    OfferFacts facts; facts.type = 0; facts.multiplier = 0.0;
    std::string err;
    int rc = offerRowStatus(h->session, instrument, atMost(SUBSCRIBE_LOOKUP_MS, left()), facts, err);
    if (rc == -3) { cpy(out, out_len, "the instrument is not in the Offers table"); return -3; }
    // EVERY other lookup failure is -8, whatever refreshTable called it: nothing has been sent, so it says nothing
    // about the instrument, and -4/-5 below are the SEND stage's codes (the Rust side reads those as a verdict).
    if (rc != 0) { cpy(out, out_len, err.c_str()); return -8; }
    std::string now = facts.status;
    const std::string offerId = facts.id;
    if (now == status) { cpy(out, out_len, now.c_str()); return 0; }
    // Every class the SDK names is subscribed like a forex pair (O2GInstrumentType 1..=9: forex, indices, commodities,
    // treasuries, bullion, shares, FX indices, CFD shares, crypto): the SDK says of `D` that "orders can still be created, and
    // `D` changes to `T`". A type this shim has never heard of is the venue's to name first.
    if (facts.type < (int)InstrumentForex || facts.type > (int)InstrumentCrypto) {
        char b[160];
        snprintf(b, sizeof(b), "type %d, contract multiplier %g, subscription `%s`", facts.type, facts.multiplier, now.c_str());
        cpy(out, out_len, b);
        return FC_UNKNOWN_INSTRUMENT_TYPE;
    }
    // A write that cannot lead to a trade changes the account for nothing: when the account may create neither a market
    // nor an entry order on the instrument (the two kinds this bridge places) it is not subscribed.
    if (orderCreationDisabled(h->session, instrument, true) && orderCreationDisabled(h->session, instrument, false)) {
        cpy(out, out_len, "FXCM's permission to create a market order and an entry order on it both read Disabled");
        return FC_ORDER_NOT_PERMITTED;
    }

    O2G2Ptr<IO2GRequestFactory> rf = h->session->getRequestFactory();
    if (!rf) { cpy(out, out_len, "no request factory"); return -4; }
    O2G2Ptr<IO2GValueMap> vm = rf->createValueMap();
    vm->setString(Command, O2G2::Commands::SetSubscriptionStatus);
    vm->setString(SubscriptionStatus, status);
    vm->setString(OfferID, offerId.c_str());
    O2G2Ptr<IO2GRequest> req = rf->createOrderRequest(vm);
    if (!req) {
        const char* e = rf->getLastError();
        cpy(out, out_len, (e && *e) ? e : "the SetSubscriptionStatus request could not be created");
        return -5;
    }
    RespListener* rl = new RespListener(h->session);
    rl->setRequestID(req->getRequestID());
    h->session->subscribeResponse(rl);
    h->session->sendRequest(req);
    bool answered = rl->wait(atMost(SUBSCRIBE_REPLY_MS, left()));
    std::string failure = rl->error();
    h->session->unsubscribeResponse(rl); rl->release();
    if (!failure.empty()) { cpy(out, out_len, failure.c_str()); return -6; }
    if (!answered) {
        cpy(out, out_len, "no answer to the SetSubscriptionStatus request inside the wait: it may still apply");
        return -8;
    }
    for (;;) {
        OfferFacts again; again.type = 0; again.multiplier = 0.0;
        rc = offerRowStatus(h->session, instrument, atMost(SUBSCRIBE_SETTLE_READ_MS, left()), again, err);
        if (rc == 0) { now = again.status; if (now == status) { cpy(out, out_len, now.c_str()); return 0; } }
        if (left() == 0) break;
        std::this_thread::sleep_for(std::chrono::milliseconds(atMost(SUBSCRIBE_SETTLE_STEP_MS, left())));
    }
    cpy(out, out_len, now.c_str());
    return -9;
}

// ---------------- closing a trade BY ITS TRADE ID (the hedging account) ----------------
//
// On a HEDGING account an opposite market OPEN does not close the trade it faces: it opens a second, opposite trade.
// MEASURED on the demo (2026-10-13/14): a BUY and then a "net-flat" SELL left trades 127974465 B and 127974466 S open
// at once, and both were closed by id with `TrueMarketClose` + `TradeID`. Every close the core can send (flatten,
// market-exit, a reduce-only market, a margin-call or budget liquidation) reached `fc_place_market` as a plain
// TrueMarketOpen and did exactly that. The three exports below are what the bridge closes with instead
// (`crates/bridges/fxcm/src/close.rs`). All three are OPTIONAL (bound by name, no ABI bump: no existing entry point
// changed meaning), so a shim installed before them degrades to a NAMED refusal on the Rust side and never to the
// silent open. All three act on the account order flow targets (`firstAccount`).

// The account's own traits, from the login-time row `firstAccount` already holds: no request, so it cannot time out.
// `maintenance_type` is the discriminator the SDK's own samples use ("0" is a NETTING account, anything else is not,
// `OpenPosition` against `OpenPositionNetting`). `account_kind` and `hedge_margin_pct` are reported for the record.
FC_EXPORT int fc_account_traits(void* hp, char* out, int out_len) {
    Handle* h = (Handle*)hp; if (!h) return -1;
    O2G2Ptr<IO2GAccountRow> a = firstAccount(h->session);
    if (!a) { cpy(out, out_len, "no account"); return -2; }
    char buf[256];
    snprintf(buf, sizeof(buf),
        "{\"maintenance_type\":\"%s\",\"account_kind\":\"%s\",\"hedge_margin_pct\":%.6f}",
        jesc(a->getMaintenanceType()).c_str(), jesc(a->getAccountKind()).c_str(),
        finite_or_zero(a->getHedgeMarginPCT()));
    return emit_json(buf, out, out_len);
}

// The account's OPEN trades as a JSON array, from a FRESH refresh bounded by PRESEND_REFRESH_MS: this runs on the
// exec thread, in front of a close (`fc_trades`, the reconcile read, keeps its own WAIT_MS). `open_time` is the
// trade's `getOpenTime()` as an OLE DATE (days, a double): the planner's FIFO key. Codes as `fc_trades`.
FC_EXPORT int fc_open_trades(void* hp, char* out, int out_len) {
    Handle* h = (Handle*)hp; if (!h) return -1;
    O2G2Ptr<IO2GAccountRow> account = firstAccount(h->session);
    if (!account) { cpy(out, out_len, "no account"); return -2; }
    O2G2Ptr<IO2GResponse> resp; std::string err;
    int rc = refreshTable(h->session, Trades, account->getAccountID(), resp, err, PRESEND_REFRESH_MS);
    if (rc != 0) { cpy(out, out_len, err.c_str()); return rc; }
    O2G2Ptr<IO2GResponseReaderFactory> rf = h->session->getResponseReaderFactory();
    if (!rf) { cpy(out, out_len, "no response reader factory"); return -4; }
    O2G2Ptr<IO2GTradesTableResponseReader> rd = rf->createTradesTableReader(resp);
    if (!rd) { cpy(out, out_len, "the response is not a Trades table"); return -7; }
    std::string json = "[";
    char buf[512];
    for (int i = 0; i < rd->size(); ++i) {
        O2G2Ptr<IO2GTradeRow> t = rd->getRow(i);
        if (!t) continue;
        std::string instr = instrumentForOfferId(h->session, t->getOfferID());
        snprintf(buf, sizeof(buf),
            "%s{\"trade_id\":\"%s\",\"instrument\":\"%s\",\"buysell\":\"%s\",\"amount\":%d,\"open_time\":%.9f}",
            (json.size() > 1 ? "," : ""),
            jesc(t->getTradeID()).c_str(), jesc(instr.c_str()).c_str(), jesc(t->getBuySell()).c_str(),
            t->getAmount(), finite_or_zero(t->getOpenTime()));
        json += buf;
    }
    json += "]";
    return emit_json(json, out, out_len);
}

// Close `amount` base units of ONE trade, by its id: a refresh of the account's Trades (bounded), the row found by id,
// then ONE `CreateOrder` / `TrueMarketClose` carrying the row's own account, offer and trade id and the side AGAINST
// the trade, the request `NonTableManagerSamples/ClosePosition` sends. The ids of the order come back as `fc_place`
// returns them; the close itself arrives on the event lane as a ClosedTrades fill (`EventListener`).
//   0    sent and answered; `order_id_out` is the closing order's id
//   -3   the trade is not in the account's open trades (closed already, or never ours)
//   -10  the amount is not a positive figure within the trade's amount
//   -15  the venue allows no market close for the instrument on this account (FC_CLOSE_NOT_PERMITTED)
//   -11/-12 SENT with no usable answer: the close may have happened. `order_id_out` holds the REQUEST id, never an order
//        id. No Orders lookup can settle it (a market close fills at once and leaves the table); the trade's own row can.
//   else the `refreshTable` codes (-2, -4, -5, -6, -7, -8); a refusal by the venue is -6 with its words.
// Worst case: PRESEND_REFRESH_MS + ORDER_WAIT_MS = 12.5 s, the same bound as a placement's.
FC_EXPORT int fc_close_trade(void* hp, const char* tradeId, int amount,
        char* order_id_out, int oid_len, char* offer_id_out, int ofid_len, char* acct_out, int acct_len,
        char* err_out, int err_len) {
    Handle* h = (Handle*)hp; if (!h) return -1;
    if (!tradeId || !*tradeId || amount <= 0) {
        cpy(err_out, err_len, "a trade id and a positive amount are required");
        return -10;
    }
    O2G2Ptr<IO2GAccountRow> account = firstAccount(h->session);
    if (!account) { cpy(err_out, err_len, "no account"); return -2; }
    O2G2Ptr<IO2GResponse> resp; std::string err;
    int rc = refreshTable(h->session, Trades, account->getAccountID(), resp, err, PRESEND_REFRESH_MS);
    if (rc != 0) { cpy(err_out, err_len, err.c_str()); return rc; }
    O2G2Ptr<IO2GResponseReaderFactory> rf = h->session->getResponseReaderFactory();
    if (!rf) { cpy(err_out, err_len, "no response reader factory"); return -4; }
    O2G2Ptr<IO2GTradesTableResponseReader> rd = rf->createTradesTableReader(resp);
    if (!rd) { cpy(err_out, err_len, "the response is not a Trades table"); return -7; }
    bool found = false, wasBuy = false; int tradeAmount = 0;
    std::string accountId, offerId, instrument;
    for (int i = 0; i < rd->size() && !found; ++i) {
        O2G2Ptr<IO2GTradeRow> t = rd->getRow(i);
        if (!t || !sameStr(t->getTradeID(), tradeId)) continue;
        found = true;
        accountId = t->getAccountID() ? t->getAccountID() : "";
        offerId = t->getOfferID() ? t->getOfferID() : "";
        wasBuy = sameStr(t->getBuySell(), O2G2::Buy);
        tradeAmount = t->getAmount();
        instrument = instrumentForOfferId(h->session, t->getOfferID());
    }
    if (!found) { cpy(err_out, err_len, "the trade is not in the account's open trades"); return -3; }
    if (amount > tradeAmount) {
        char b[160];
        snprintf(b, sizeof(b), "the amount %d is more than the trade's %d", amount, tradeAmount);
        cpy(err_out, err_len, b);
        return -10;
    }
    O2G2Ptr<IO2GLoginRules> rules = h->session->getLoginRules();
    O2G2Ptr<IO2GPermissionChecker> perm;
    if (rules) perm = rules->getPermissionChecker();
    if (perm && perm->canCreateMarketCloseOrder(instrument.c_str()) != PermissionEnabled) {
        cpy(err_out, err_len, ("the venue allows no market close order for " + instrument + " on this account").c_str());
        return FC_CLOSE_NOT_PERMITTED;
    }
    cpy(acct_out, acct_len, accountId.c_str());
    cpy(offer_id_out, ofid_len, offerId.c_str());

    O2G2Ptr<IO2GRequestFactory> rq = h->session->getRequestFactory();
    if (!rq) { cpy(err_out, err_len, "no request factory"); return -4; }
    O2G2Ptr<IO2GValueMap> vm = rq->createValueMap();
    vm->setString(Command, O2G2::Commands::CreateOrder);
    vm->setString(OrderType, O2G2::Orders::TrueMarketClose);
    vm->setString(AccountID, accountId.c_str());
    vm->setString(OfferID, offerId.c_str());
    vm->setString(TradeID, tradeId);
    vm->setString(BuySell, wasBuy ? O2G2::Sell : O2G2::Buy);
    vm->setInt(Amount, amount);
    vm->setString(CustomID, "RustShimClose");
    O2G2Ptr<IO2GRequest> req = rq->createOrderRequest(vm);
    if (!req) { cpy(err_out, err_len, rq->getLastError()); return -5; }

    RespListener* rl = new RespListener(h->session);
    const std::string reqId = req->getRequestID();
    rl->setRequestID(reqId.c_str());
    h->session->subscribeResponse(rl);
    h->session->sendRequest(req);
    int result = 0, ambiguous = 0;
    if (rl->wait()) {
        std::string failure = rl->error(), oid = rl->orderId();
        if (!failure.empty()) { cpy(err_out, err_len, failure.c_str()); result = -6; }
        else if (!oid.empty()) cpy(order_id_out, oid_len, oid.c_str());
        else ambiguous = FC_SENT_NO_ORDER_ID;
    } else ambiguous = FC_SENT_NO_ANSWER;
    h->session->unsubscribeResponse(rl); rl->release();
    if (ambiguous) {
        cpy(err_out, err_len, ambiguous == FC_SENT_NO_ANSWER
            ? "outcome UNKNOWN, the close was sent and the venue gave no answer inside the wait"
            : "outcome UNKNOWN, the close was sent and the venue answered without an order id");
        cpy(order_id_out, oid_len, reqId.c_str());
        return ambiguous;
    }
    return result;
}

// ---------------- TRADE-BOUND exits: a stop-loss (`S`) and a take-profit (`L`) on ONE open trade ----------------
//
// A hedging account's per-trade orders (`NonTableManagerSamples`' attach-stop shape: `OrderType` `S` / `L` with the
// trade's `TradeID`, the side AGAINST the trade, `Amount` and `Rate`). MEASURED on the demo login (2026-10-26; the raw
// logs and the table are `docs/ops/fxcm-forexconnect.md`'s "Trade-bound stop-loss and take-profit"):
//   * the venue holds the order, so it survives a node outage; a trade carries at most ONE `S` and ONE `L`
//     (`ORA-20114`, "Cannot place more than one order of this type within order group") and the two share one
//     contingency group (`ContingencyType` 3, an OCO pair): when one FILLS the venue deletes the other (status `C`) in the
//     same batch, and the fill arrives as the ClosedTrades row whose `CloseOrderID` is the exit's own order id (the
//     listener above already names it);
//   * `Amount` must EQUAL the trade's amount (`ORA-20134`, "Quantity should be the same as in trade"): an exit covers
//     a WHOLE trade, never a part of it;
//   * a trade closed any other way takes its exits with it (the venue deletes them, status `C`), and a PARTIAL close of
//     the trade keeps them: same order ids, `Amount` cut to the remainder, `TradeID` = the remainder's new id;
//   * the price must be on the right side of the market (`ORA-20115`, "Stop/Limit price did not pass validation");
//     `EditOrder` (`fc_edit_order`) moves an exit's rate in place and `DeleteOrder` (`fc_delete_order`) removes one.
// Both exports are OPTIONAL (bound by name, no ABI bump), so a shim installed before them degrades to a NAMED refusal on
// the Rust side and never to a plain entry order, which OPENS a trade when it is hit.

// The account's resting trade-bound exits as a JSON array, from a FRESH Orders refresh bounded by PRESEND_REFRESH_MS: the
// exit binder's input. An exit is an order of type `S` or `L` that names its trade; the entry orders (`SE`, `LE`) have
// other type words, so they never read as cover. A row carries the order's `rate` and its account and offer ids as well
// (all three are what taking over an exit that is already resting needs). Codes as `fc_orders`.
FC_EXPORT int fc_open_exits(void* hp, char* out, int out_len) {
    Handle* h = (Handle*)hp; if (!h) return -1;
    O2G2Ptr<IO2GAccountRow> account = firstAccount(h->session);
    if (!account) { cpy(out, out_len, "no account"); return -2; }
    O2G2Ptr<IO2GResponse> resp; std::string err;
    int rc = refreshTable(h->session, Orders, account->getAccountID(), resp, err, PRESEND_REFRESH_MS);
    if (rc != 0) { cpy(out, out_len, err.c_str()); return rc; }
    O2G2Ptr<IO2GResponseReaderFactory> rf = h->session->getResponseReaderFactory();
    if (!rf) { cpy(out, out_len, "no response reader factory"); return -4; }
    O2G2Ptr<IO2GOrdersTableResponseReader> rd = rf->createOrdersTableReader(resp);
    if (!rd) { cpy(out, out_len, "the response is not an Orders table"); return -7; }
    std::string json = "[";
    char buf[512];
    for (int i = 0; i < rd->size(); ++i) {
        O2G2Ptr<IO2GOrderRow> o = rd->getRow(i);
        if (!o) continue;
        const char* type = o->getType();
        const char* trade = o->getTradeID();
        if (!type || (strcmp(type, "S") != 0 && strcmp(type, "L") != 0) || !trade || !*trade) continue;
        // `rate`, `account_id` and `offer_id` let the Rust side take over an exit that is already resting (a re-sent stop
        // of the same rate after a node restart) and cancel or amend it afterwards: the order's own terms and ids.
        snprintf(buf, sizeof(buf),
            "%s{\"order_id\":\"%s\",\"trade_id\":\"%s\",\"type\":\"%s\",\"buysell\":\"%s\",\"amount\":%d,"
            "\"rate\":%.6f,\"account_id\":\"%s\",\"offer_id\":\"%s\"}",
            (json.size() > 1 ? "," : ""),
            jesc(o->getOrderID()).c_str(), jesc(trade).c_str(), jesc(type).c_str(),
            jesc(o->getBuySell()).c_str(), o->getAmount(),
            finite_or_zero(o->getRate()), jesc(o->getAccountID()).c_str(), jesc(o->getOfferID()).c_str());
        json += buf;
    }
    json += "]";
    return emit_json(json, out, out_len);
}

// Place ONE exit on ONE trade: a refresh of the account's Trades (bounded), the trade found by id, then ONE `CreateOrder`
// of type `S` (kind "S") or `L` (kind "L") carrying the trade's own account, offer and id, the side AGAINST the trade, its
// whole `amount` and `rate` (checked against the instrument's price grid, `snapRate`: REFUSED when off it, never rounded).
// The ids come back as `fc_place` returns them and `rate_out` is the rate sent.
//   0    placed; `order_id_out` is the exit's order id
//   -3   the trade is not in the account's open trades
//   -10  a bad argument, an amount that is not the trade's whole amount, or a rate off the price grid
//   FC_EXIT_NOT_PERMITTED  the venue allows no stop or limit order for the instrument on this account (its value is the #define's)
//   -11/-12/-13 SENT with no usable answer: the Orders table is looked up by the RequestID once (as a placement is); when
//        that finds nothing `order_id_out` holds the REQUEST id, never an order id, for `fc_find_order`
//   else the `refreshTable` codes (-2, -4, -5, -6, -7, -8); a refusal by the venue is -6 with its words.
// Worst case: PRESEND_REFRESH_MS + ORDER_WAIT_MS + LOOKUP_REFRESH_MS = 17.5 s.
FC_EXPORT int fc_place_exit(void* hp, const char* tradeId, const char* kind, double rate, int amount,
        char* order_id_out, int oid_len, char* offer_id_out, int ofid_len, char* acct_out, int acct_len,
        double* rate_out, char* err_out, int err_len) {
    Handle* h = (Handle*)hp; if (!h) return -1;
    if (rate_out) *rate_out = 0.0;
    if (!tradeId || !*tradeId || amount <= 0 || !kind || (strcmp(kind, "S") != 0 && strcmp(kind, "L") != 0)) {
        cpy(err_out, err_len, "a trade id, a kind of S or L and a positive amount are required");
        return -10;
    }
    const bool limit = strcmp(kind, "L") == 0;
    O2G2Ptr<IO2GAccountRow> account = firstAccount(h->session);
    if (!account) { cpy(err_out, err_len, "no account"); return -2; }
    O2G2Ptr<IO2GResponse> resp; std::string err;
    int rc = refreshTable(h->session, Trades, account->getAccountID(), resp, err, PRESEND_REFRESH_MS);
    if (rc != 0) { cpy(err_out, err_len, err.c_str()); return rc; }
    O2G2Ptr<IO2GResponseReaderFactory> rf = h->session->getResponseReaderFactory();
    if (!rf) { cpy(err_out, err_len, "no response reader factory"); return -4; }
    O2G2Ptr<IO2GTradesTableResponseReader> rd = rf->createTradesTableReader(resp);
    if (!rd) { cpy(err_out, err_len, "the response is not a Trades table"); return -7; }
    bool found = false, wasBuy = false; int tradeAmount = 0;
    std::string accountId, offerId, instrument;
    for (int i = 0; i < rd->size() && !found; ++i) {
        O2G2Ptr<IO2GTradeRow> t = rd->getRow(i);
        if (!t || !sameStr(t->getTradeID(), tradeId)) continue;
        found = true;
        accountId = t->getAccountID() ? t->getAccountID() : "";
        offerId = t->getOfferID() ? t->getOfferID() : "";
        wasBuy = sameStr(t->getBuySell(), O2G2::Buy);
        tradeAmount = t->getAmount();
        instrument = instrumentForOfferId(h->session, t->getOfferID());
    }
    if (!found) { cpy(err_out, err_len, "the trade is not in the account's open trades"); return -3; }
    if (amount != tradeAmount) {
        char b[200];
        snprintf(b, sizeof(b), "the venue binds an exit to a WHOLE trade: the amount %d is not the trade's %d (ORA-20134)",
            amount, tradeAmount);
        cpy(err_out, err_len, b);
        return -10;
    }
    O2G2Ptr<IO2GLoginRules> rules = h->session->getLoginRules();
    O2G2Ptr<IO2GPermissionChecker> perm;
    if (rules) perm = rules->getPermissionChecker();
    if (perm && perm->canCreateStopLimitOrder(instrument.c_str()) != PermissionEnabled) {
        cpy(err_out, err_len, ("the venue allows no stop or limit order for " + instrument + " on this account").c_str());
        return FC_EXIT_NOT_PERMITTED;
    }
    OfferHold offer; std::string offerErr;
    int orc = findOffer(h->session, instrument.c_str(), /*fresh=*/false, offer, offerErr);
    if (orc != 0) {
        cpy(err_out, err_len, (orc == -3 && offerErr.empty()) ? "offer not found" : offerErr.c_str());
        return orc;
    }
    double snapped = 0.0; std::string gridErr;
    int grc = snapRate(offer.row, rate, snapped, gridErr);
    if (grc != 0) { cpy(err_out, err_len, gridErr.c_str()); return grc; }
    if (rate_out) *rate_out = snapped;
    cpy(acct_out, acct_len, accountId.c_str());
    cpy(offer_id_out, ofid_len, offerId.c_str());

    const char* buysell = wasBuy ? O2G2::Sell : O2G2::Buy;
    O2G2Ptr<IO2GRequestFactory> rq = h->session->getRequestFactory();
    if (!rq) { cpy(err_out, err_len, "no request factory"); return -4; }
    O2G2Ptr<IO2GValueMap> vm = rq->createValueMap();
    vm->setString(Command, O2G2::Commands::CreateOrder);
    vm->setString(OrderType, limit ? O2G2::Orders::Limit : O2G2::Orders::Stop);
    vm->setString(AccountID, accountId.c_str());
    vm->setString(OfferID, offerId.c_str());
    vm->setString(TradeID, tradeId);
    vm->setString(BuySell, buysell);
    vm->setInt(Amount, amount);
    vm->setDouble(Rate, snapped);
    vm->setString(CustomID, limit ? "RustShimTarget" : "RustShimStop");
    O2G2Ptr<IO2GRequest> req = rq->createOrderRequest(vm);
    if (!req) { cpy(err_out, err_len, rq->getLastError()); return -5; }
    SentTerms sent = { offerId.c_str(), buysell, amount, limit, snapped, 0.0 };
    { int digits = offer->getDigits(); if (digits >= 0 && digits <= 8) sent.tick = std::pow(10.0, -digits); }

    RespListener* rl = new RespListener(h->session);
    const std::string reqId = req->getRequestID();
    rl->setRequestID(reqId.c_str());
    h->session->subscribeResponse(rl);
    h->session->sendRequest(req);
    int result = 0, ambiguous = 0;
    if (rl->wait()) {
        std::string failure = rl->error(), oid = rl->orderId();
        if (!failure.empty()) { cpy(err_out, err_len, failure.c_str()); result = -6; }
        else if (!oid.empty()) cpy(order_id_out, oid_len, oid.c_str());
        else ambiguous = FC_SENT_NO_ORDER_ID;
    } else ambiguous = FC_SENT_NO_ANSWER;
    h->session->unsubscribeResponse(rl); rl->release();
    if (ambiguous) {
        // The request was SENT and an exit RESTS, so it may be in the Orders table though no usable answer reached us:
        // look for it by its RequestID once, as a placement does, before calling the outcome unknown.
        std::set<std::string> none; std::string found2, lookupErr;
        int lrc = findOrderByRequestId(h->session, accountId.c_str(), reqId.c_str(), sent, none, found2, lookupErr);
        if (lrc == 0) { cpy(order_id_out, oid_len, found2.c_str()); return 0; }
        cpy(err_out, err_len, ambiguous == FC_SENT_NO_ANSWER
            ? "outcome UNKNOWN, the exit was sent and the venue gave no answer inside the wait"
            : "outcome UNKNOWN, the exit was sent and the venue answered without an order id");
        cpy(order_id_out, oid_len, reqId.c_str());
        return ambiguous;
    }
    return result;
}

// Places an immediately-executing TRUE MARKET order (O2G2::Orders::TrueMarketOpen). Unlike
// fc_place_entry this is expected to FILL: the created order id still comes back here (so a cancel
// can be attempted and so fills can be routed to a coid), but the resulting Trades(Insert) arrives
// asynchronously on the persistent EventListener above and drains through fc_poll_event.
FC_EXPORT int fc_place_market(void* hp, const char* instrument, const char* buysell,
        int lots, char* order_id_out, int oid_len,
        char* offer_id_out, int ofid_len, char* acct_out, int acct_len,
        char* err_out, int err_len) {
    return fc_place((Handle*)hp, instrument, buysell, /*pips_away=*/0, /*exact_rate=*/0, lots, /*market=*/true,
        order_id_out, oid_len, offer_id_out, ofid_len, acct_out, acct_len, /*rate_out=*/0,
        err_out, err_len);
}

FC_EXPORT int fc_delete_order(void* hp, const char* orderId, const char* accountId,
        const char* offerId, char* err_out, int err_len) {
    Handle* h = (Handle*)hp; if (!h) return -1;
    O2G2Ptr<IO2GRequestFactory> rf = h->session->getRequestFactory();
    if (!rf) return -4;
    O2G2Ptr<IO2GValueMap> vm = rf->createValueMap();
    vm->setString(Command, O2G2::Commands::DeleteOrder);
    vm->setString(OrderID, orderId);
    vm->setString(AccountID, accountId);
    vm->setString(OfferID, offerId);
    O2G2Ptr<IO2GRequest> req = rf->createOrderRequest(vm);
    if (!req) { cpy(err_out, err_len, rf->getLastError()); return -5; }
    RespListener* rl = new RespListener(h->session);
    rl->setRequestID(req->getRequestID());
    h->session->subscribeResponse(rl);
    h->session->sendRequest(req);
    int rc = 0;
    if (rl->wait()) { std::string failure = rl->error(); if (!failure.empty()) { cpy(err_out,err_len,failure.c_str()); rc=-6; } }
    else { cpy(err_out,err_len,"timeout"); rc=-8; }
    h->session->unsubscribeResponse(rl); rl->release();
    return rc;
}

FC_EXPORT void fc_logout(void* hp) {
    Handle* h = (Handle*)hp; if (!h) return;
    if (h->evl) { h->session->unsubscribeResponse(h->evl); h->evl->release(); }
    h->status->reset();
    h->session->logout();
    h->status->wait();
    h->session->unsubscribeSessionStatus(h->status);
    h->status->release();
    h->session->release();
    delete h;
}

} // extern "C"
