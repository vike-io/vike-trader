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
#include <mutex>
#include <atomic>
#include <condition_variable>
#include <chrono>
#include "forexconnect/ForexConnect.h"

// C-ABI export decoration — the only platform split left. On Windows the exports must carry
// `__declspec(dllexport)` (as the original shim did); on ELF (Linux) default visibility is the
// portable equivalent. Threading below is fully portable std, so no other `#ifdef` remains.
#ifdef _WIN32
    #define FC_EXPORT __declspec(dllexport)
#else
    #define FC_EXPORT __attribute__((visibility("default")))
#endif

#define WAIT_MS 30000

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
class StatusListener : public IO2GSessionStatus {
    std::atomic<unsigned int> mRef; AutoResetEvent mEvt; bool mConnected; IO2GSession* mSession;
public:
    StatusListener(IO2GSession* s): mRef(1), mConnected(false), mSession(s) {}
    ~StatusListener() {}
    long addRef() { return (long)++mRef; }
    long release() { long r = (long)--mRef; if (r==0) delete this; return r; }
    void reset() { mConnected = false; mEvt.reset(); }
    bool wait() { return mEvt.wait(WAIT_MS); }
    bool isConnected() { return mConnected; }
    void onSessionStatusChanged(O2GSessionStatus status) {
        if (status == IO2GSessionStatus::Connected) { mConnected = true; mEvt.set(); }
        else if (status == IO2GSessionStatus::Disconnected) { mConnected = false; mEvt.set(); }
    }
    void onLoginFailed(const char* error) { mConnected = false; mEvt.set(); }
};

// ---------------- response listener (captures created order id) ----------------
class RespListener : public IO2GResponseListener {
    std::atomic<unsigned int> mRef; AutoResetEvent mEvt; IO2GSession* mSession;
    std::string mReqId, mOrderId, mError;
public:
    RespListener(IO2GSession* s): mRef(1), mSession(s) { s->addRef(); }
    ~RespListener() { mSession->release(); }
    long addRef() { return (long)++mRef; }
    long release() { long r = (long)--mRef; if (r==0) delete this; return r; }
    void setRequestID(const char* id) { mReqId = id; mOrderId=""; mError=""; mEvt.reset(); }
    bool wait() { return mEvt.wait(WAIT_MS); }
    const char* orderId() { return mOrderId.c_str(); }
    const char* error() { return mError.c_str(); }
    void onRequestCompleted(const char* requestId, IO2GResponse* response) {
        if (response && mReqId == requestId) {
            if (response->getType() != CreateOrderResponse) mEvt.set();
        }
    }
    void onRequestFailed(const char* requestId, const char* error) {
        if (mReqId == requestId) { mError = error ? error : "request failed"; mEvt.set(); }
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
                if (o && mReqId == o->getRequestID()) { mOrderId = o->getOrderID(); mEvt.set(); }
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

// Persistent response listener subscribed for the whole session. Turns Trades(Insert) into a
// "fill" envelope and Orders(Delete) into a "canceled" envelope, enqueued for `fc_poll_event`.
// Audit A3: after a reconnect ForexConnect re-delivers the current Trades/Orders tables, so the
// gap fills re-surface here; the Rust core dedups by trade_id.
// The IO2GTradeRow accessors below are the base-row surface (getTradeID/getOpenOrderID/getOfferID/
// getBuySell/getAmount/getOpenRate/getCommission); the instrument is NOT a base-row field, so it is
// resolved from the trade's offer id via instrumentForOfferId() above.
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
                std::string instrument = instrumentForOfferId(mSession, t->getOfferID());
                snprintf(buf, sizeof(buf),
                    "{\"kind\":\"fill\",\"order_id\":\"%s\",\"trade_id\":\"%s\",\"instrument\":\"%s\","
                    "\"side\":\"%s\",\"amount\":%d,\"rate\":%.6f,\"commission\":%.6f,\"ts\":0}",
                    jesc(t->getOpenOrderID()).c_str(), jesc(t->getTradeID()).c_str(),
                    jesc(instrument.c_str()).c_str(), jesc(t->getBuySell()).c_str(),
                    t->getAmount(), t->getOpenRate(), t->getCommission());
                mQ->push(buf);
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
static IO2GOfferRow* findOffer(IO2GSession* s, const char* instr) {
    O2G2Ptr<IO2GLoginRules> rules = s->getLoginRules();
    if (!rules) return 0;
    O2G2Ptr<IO2GResponse> resp = rules->getTableRefreshResponse(Offers);
    if (!resp) return 0;
    O2G2Ptr<IO2GResponseReaderFactory> rf = s->getResponseReaderFactory();
    if (!rf) return 0;
    O2G2Ptr<IO2GOffersTableResponseReader> rd = rf->createOffersTableReader(resp);
    for (int i = 0; i < rd->size(); ++i) {
        O2G2Ptr<IO2GOfferRow> o = rd->getRow(i);
        if (o && strcmp(instr, o->getInstrument())==0 && strcmp(o->getSubscriptionStatus(),"T")==0)
            return o.Detach();
    }
    return 0;
}
static void cpy(char* dst, int len, const char* src) {
    if (!dst || len <= 0) return; strncpy(dst, src ? src : "", len-1); dst[len-1]=0;
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

extern "C" {

FC_EXPORT void* fc_login(const char* user, const char* pw, const char* url, const char* conn) {
    IO2GSession* s = CO2GTransport::createSession();
    StatusListener* sl = new StatusListener(s);
    s->subscribeSessionStatus(sl);
    sl->reset();
    s->login(user, pw, url, conn);
    if (!(sl->wait() && sl->isConnected())) {
        s->unsubscribeSessionStatus(sl); sl->release(); s->release(); return 0;
    }
    Handle* h = new Handle(); h->session = s; h->status = sl;
    // Persistent async event listener: captures fills/cancels for fc_poll_event (A3 fill lane).
    h->evl = new EventListener(s, &h->events);
    s->subscribeResponse(h->evl);
    return h;
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
// everywhere else placed that many LOTS: `qty: 10000` on EUR/USD was ten thousand lots, a hundred
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

FC_EXPORT int fc_account(void* hp, char* acct_out, int acct_len, double* bal_out) {
    Handle* h = (Handle*)hp; if (!h) return -1;
    O2G2Ptr<IO2GAccountRow> a = firstAccount(h->session);
    if (!a) return -2;
    cpy(acct_out, acct_len, a->getAccountID());
    if (bal_out) *bal_out = a->getBalance();
    return 0;
}

// ---------------- reconcile table snapshots (read-only) ----------------
// The point-in-time Orders/Trades table snapshots the `ReconClient` (recon_client.rs) diffs against
// local state. Both read the login-rules table-refresh response for the table and enumerate it with
// the response-reader factory — the SAME pattern firstAccount()/findOffer() already use for the
// Accounts/Offers tables, just with the Orders/Trades reader types. The row's human instrument is
// resolved from its offer id via instrumentForOfferId() (the base IO2GOrderRow/IO2GTradeRow carry
// getOfferID but NOT getInstrument — the same limitation the async EventListener already works
// around). Each returns a JSON array string via emit_json's grow-and-retry length protocol, or a
// negative code on a bad handle / missing factory. An absent table response yields "[]" (no rows) —
// the reconcile parsers treat an empty trades snapshot as a flat position, never as "unknown".

// Snapshot the current Orders table (resting entry/limit/stop working orders) as a JSON array.
FC_EXPORT int fc_orders(void* hp, char* out, int out_len) {
    Handle* h = (Handle*)hp; if (!h) return -1;
    O2G2Ptr<IO2GLoginRules> rules = h->session->getLoginRules();
    if (!rules) return -2;
    O2G2Ptr<IO2GResponseReaderFactory> rf = h->session->getResponseReaderFactory();
    if (!rf) return -4;
    std::string json = "[";
    O2G2Ptr<IO2GResponse> resp = rules->getTableRefreshResponse(Orders);
    if (resp) {
        O2G2Ptr<IO2GOrdersTableResponseReader> rd = rf->createOrdersTableReader(resp);
        if (rd) {
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
        }
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
    O2G2Ptr<IO2GLoginRules> rules = h->session->getLoginRules();
    if (!rules) return -2;
    O2G2Ptr<IO2GResponseReaderFactory> rf = h->session->getResponseReaderFactory();
    if (!rf) return -4;
    std::string json = "[";
    O2G2Ptr<IO2GResponse> resp = rules->getTableRefreshResponse(Trades);
    if (resp) {
        O2G2Ptr<IO2GTradesTableResponseReader> rd = rf->createTradesTableReader(resp);
        if (rd) {
            char buf[512];
            for (int i = 0; i < rd->size(); ++i) {
                O2G2Ptr<IO2GTradeRow> t = rd->getRow(i);
                if (!t) continue;
                std::string instr = instrumentForOfferId(h->session, t->getOfferID());
                snprintf(buf, sizeof(buf),
                    "%s{\"trade_id\":\"%s\",\"order_id\":\"%s\",\"instrument\":\"%s\","
                    "\"buysell\":\"%s\",\"amount\":%d,\"open_rate\":%.6f,\"commission\":%.6f}",
                    (json.size() > 1 ? "," : ""),
                    jesc(t->getTradeID()).c_str(), jesc(t->getOpenOrderID()).c_str(),
                    jesc(instr.c_str()).c_str(), jesc(t->getBuySell()).c_str(),
                    t->getAmount(), t->getOpenRate(), t->getCommission());
                json += buf;
            }
        }
    }
    json += "]";
    return emit_json(json, out, out_len);
}

// Shared CreateOrder path for both placement kinds. Resolves account + offer, sizes the order from
// the venue's base unit, builds the value map and blocks on the response listener for the created
// order id. On success returns 0 and fills order_id/offer_id/account_id (needed to cancel).
//
// `market` selects the order type and therefore the value-map SHAPE, which differs between the two:
// a true-market order carries NO `Rate` key (O2G2::Orders::TrueMarketOpen, "OM" — the shape the
// SDK's own NonTableManagerSamples/OpenPosition sample sends), whereas a limit entry MUST carry the
// rate to rest at. Passing a Rate alongside TrueMarketOpen is what the venue rejects, so the key is
// set only on the limit branch. Negative return codes are shared by both exports.
static int fc_place(Handle* h, const char* instrument, const char* buysell,
        int pips_away, int lots, bool market, char* order_id_out, int oid_len,
        char* offer_id_out, int ofid_len, char* acct_out, int acct_len,
        double* rate_out, char* err_out, int err_len) {
    if (!h) return -1;
    O2G2Ptr<IO2GAccountRow> account = firstAccount(h->session);
    if (!account) { cpy(err_out,err_len,"no account"); return -2; }
    O2G2Ptr<IO2GOfferRow> offer = findOffer(h->session, instrument);
    if (!offer) { cpy(err_out,err_len,"offer not found"); return -3; }
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
    if (!market) {
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

    RespListener* rl = new RespListener(h->session);
    h->session->subscribeResponse(rl);
    rl->setRequestID(req->getRequestID());
    h->session->sendRequest(req);
    int rc = 0;
    if (rl->wait()) {
        if (strlen(rl->error()) > 0) { cpy(err_out,err_len,rl->error()); rc = -6; }
        else if (strlen(rl->orderId()) > 0) { cpy(order_id_out, oid_len, rl->orderId()); rc = 0; }
        else { cpy(err_out,err_len,"no order id returned"); rc = -7; }
    } else { cpy(err_out,err_len,"timeout"); rc = -8; }
    h->session->unsubscribeResponse(rl); rl->release();
    return rc;
}

// Places a resting LIMIT-entry `pips_away` from market (buy below ask / sell above bid) so it can't fill.
FC_EXPORT int fc_place_entry(void* hp, const char* instrument, const char* buysell,
        int pips_away, int lots, char* order_id_out, int oid_len,
        char* offer_id_out, int ofid_len, char* acct_out, int acct_len,
        double* rate_out, char* err_out, int err_len) {
    return fc_place((Handle*)hp, instrument, buysell, pips_away, lots, /*market=*/false,
        order_id_out, oid_len, offer_id_out, ofid_len, acct_out, acct_len, rate_out,
        err_out, err_len);
}

// Places an immediately-executing TRUE MARKET order (O2G2::Orders::TrueMarketOpen). Unlike
// fc_place_entry this is expected to FILL: the created order id still comes back here (so a cancel
// can be attempted and so fills can be routed to a coid), but the resulting Trades(Insert) arrives
// asynchronously on the persistent EventListener above and drains through fc_poll_event.
FC_EXPORT int fc_place_market(void* hp, const char* instrument, const char* buysell,
        int lots, char* order_id_out, int oid_len,
        char* offer_id_out, int ofid_len, char* acct_out, int acct_len,
        char* err_out, int err_len) {
    return fc_place((Handle*)hp, instrument, buysell, /*pips_away=*/0, lots, /*market=*/true,
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
    h->session->subscribeResponse(rl);
    rl->setRequestID(req->getRequestID());
    h->session->sendRequest(req);
    int rc = 0;
    if (rl->wait()) { if (strlen(rl->error())>0) { cpy(err_out,err_len,rl->error()); rc=-6; } }
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
