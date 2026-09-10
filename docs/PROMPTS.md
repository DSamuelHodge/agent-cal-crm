# `agent-cal-crm` — Agent Execution Prompts

Generated from the Comprehensive Architecture Review (2026-09-10). Each prompt is
scoped to be handed to a coding agent independently, in the order listed —
later prompts assume earlier ones landed. Every prompt names the exact files
it touches, states acceptance criteria, and calls out what's explicitly out of
scope so an agent doesn't wander into an adjacent refactor.

Ordering principle (inherited from `ROADMAP.md`'s own logic, extended with
this review's findings): **fix what's silently broken → harden the transport
→ decouple from AutoTask → close the correctness/security gaps → add
observability → automate the pipeline.**

---

## P1 — Fix the kill-switch wiring gap (CRITICAL)

**Context:** `ChannelGate` (`src/kill_switch.rs`) is fully implemented and
unit-tested, and `ROADMAP.md` marks it "✅ Done." But `src/bin/cos.rs::route()`
calls `agentcal::rpc::dispatch()`, which internally builds a fresh
`ChannelGate::new()` (all channels enabled) on every single call — there is no
code path in the running daemon that ever constructs a *disabled* gate. The
kill-switch currently has zero effect in production.

**Objective:** Wire a persistent, mutable `ChannelGate` into the live daemon
and expose it as an admin-controllable resource.

**Do:**
1. In `src/bin/cos.rs`, hold the gate as `Arc<tokio::sync::RwLock<ChannelGate>>`
   (or `arc_swap::ArcSwap<ChannelGate>` if you prefer lock-free reads),
   constructed once in `serve()` alongside `cal`/`crm`, and cloned into each
   connection handler.
2. Replace the `dispatch(cal, crm, method_name, &params)` call in `route()`
   with `dispatch_with_gate(cal, crm, &gate.read().await, method_name, &params)`.
3. Add two new RPC methods to `src/rpc.rs`'s `dispatch_inner`:
   `kill_switch.disable` and `kill_switch.enable`, each taking `{owner, channel}`
   — `owner` is accepted for consistency with the rest of the API but the gate
   itself is process-wide, not per-owner (document this explicitly in the
   RPC docstring). Both mutate the shared gate and return the resulting
   `{channel, enabled}`.
4. Add a `kill_switch.status` read method returning all `KNOWN_CHANNELS` with
   their current enabled/disabled state.
5. Persist gate state to the `limit_usage`-style table pattern already in
   `libsql_store.rs` (new table `channel_gate(channel TEXT PRIMARY KEY,
   disabled INTEGER NOT NULL DEFAULT 0)`) so a daemon restart doesn't silently
   re-enable everything — load it once at `serve()` startup, write-through on
   every `disable`/`enable` call.

**Acceptance criteria:**
- A test in `tests/kill_switch.rs` (or a new `tests/kill_switch_daemon.rs`)
  proves that calling `kill_switch.disable` followed by a channel-carrying
  method (e.g. `inbox.ingest` with that channel) returns `ChannelDisabled`,
  and that the effect survives a simulated daemon restart (reopen the same DB
  file, gate state reloads as disabled).
- No existing test in `tests/kill_switch.rs` regresses.

**Out of scope:** any UI/CLI for toggling channels — RPC only for now.

---

## P2 — Harden the HTTP parser (request size limits + malformed-input safety)

**Context:** `src/bin/cos.rs::handle()` reads into an unbounded `Vec<u8>`
buffer until `Content-Length` bytes arrive, with no maximum. A large or
spoofed `Content-Length` header can exhaust memory. The header scan
(`find_header_end`) and the manual `Authorization` line parse are also not
resilient to malformed/adversarial input (partial headers, no terminating
`\r\n\r\n`, oversized header block).

**Objective:** Cap request size and make the manual parser fail closed on
malformed input, without yet replacing the transport (that's P3).

**Do:**
1. Add a `MAX_REQUEST_BYTES: usize` constant (start at `1_048_576` — 1 MiB;
   make it overridable via a `--max-request-bytes` CLI flag once P6 lands,
   but hardcode it for this prompt).
2. In `handle()`'s read loop, after each `stream.read()`, if `buf.len()`
   exceeds `MAX_REQUEST_BYTES` before headers are even complete, or if the
   parsed `Content-Length` exceeds it, immediately respond `413 Payload Too
   Large` (`http_json(413, &json_err("request body too large"))`) and close
   the connection without reading further.
3. Add a `MAX_HEADER_BYTES` cap (e.g. 8 KiB) on the header block itself,
   returned as `400` if exceeded — prevents an attacker sending an endless
   header without a terminating `\r\n\r\n` from growing `buf` unbounded before
   `content_length` is even known.
4. Add a total read-loop iteration timeout distinct from the per-`read()`
   socket timeout already present, so a slow-loris-style drip-feed can't hold
   a thread open indefinitely (10s total, not just 10s per read).

**Acceptance criteria:** new tests exercising `route()`/`handle()` directly
(oversized body, oversized headers, no terminator ever sent, slow drip-feed)
each get a clean `4xx` response and the connection closes — no panics, no
unbounded memory growth (verify with a bounded `buf.capacity()` assertion in
the test or by sending a body larger than `MAX_REQUEST_BYTES` and confirming
the server never reads past the cap).

**Out of scope:** replacing the transport — that's P3. Don't add `axum` here.

---

## P3 — Replace the hand-rolled HTTP server with `axum` on the shared runtime

**Context:** `serve()` spawns a **new OS thread and a new `tokio::Runtime`
per accepted connection** (`std::thread::spawn` + `tokio::runtime::Runtime::new()`
at `src/bin/cos.rs:135-136, 163-164`), and every response sets
`Connection: close` (no keep-alive). This is both wasteful (thread + runtime
setup cost per RPC call) and a resource-exhaustion risk under concurrent load
or abuse.

**Objective:** Move to `axum` (or `hyper` directly if you want to stay
minimal-dependency) running on the single multi-thread runtime already
created in `main()`, with `tokio::spawn` per connection instead of
thread+runtime-per-connection.

**Do:**
1. Add `axum = "0.7"` (or current stable) and `tower = "0.4"` to
   `[dependencies]` in `Cargo.toml`. Keep `tokio` as-is.
2. Rewrite `serve()` in `src/bin/cos.rs` to build an `axum::Router` with:
   - `POST /` → the existing `route()` logic, adapted to axum's
     `State<AppState>` extractor (`AppState { cal: Arc<AgentCal>, crm:
     Arc<AgentCrm>, gate: Arc<RwLock<ChannelGate>>, token: Option<String> }`).
   - `GET /ping` and `GET /healthz` → same behavior as today.
   - A `tower::limit::RequestBodyLimitLayer` replacing the manual cap from P2
     (keep P2's explicit tests passing against the new stack).
3. Preserve **both** the TCP (`--addr`) and Unix-socket (`--sock`) bind modes
   — axum supports both via `axum::serve` with a custom listener
     (`tokio::net::UnixListener` works with `axum::serve` the same way as
     `TcpListener`).
4. Delete the manual `Stream` trait, `find_header_end`, `headers_and_body_len`,
   and the thread-per-connection loop entirely once the axum path is proven
   equivalent.
5. Keep the exact same wire protocol (`{"method","params"}` → `{"ok","result"}`
   / `{"ok","error"}`) — this is an internal transport swap, not an API change.

**Acceptance criteria:**
- Every existing test that exercises `route()` (directly or via
  `tests/rpc.rs`, `tests/integration.rs`) passes unmodified against the new
  axum-based server (adapt call sites to spin up the router instead of the
  raw TCP loop, but keep assertions identical).
- A basic load test (e.g. 200 concurrent `ping` calls via a test harness)
  completes without spawning 200 OS threads — verify via `/proc` thread count
  or a `tracing` span count if P12 has landed, otherwise via a comment
  documenting manual verification.
- `cargo clippy` is clean on the new code.

**Out of scope:** TLS termination (P8/security prompt below may address this
later if the deployment target changes); this prompt keeps the daemon
loopback/adb-forward-only as today.

---

## P4 — Foreign keys + a versioned migration runner

**Context:** `PRAGMA foreign_keys = ON;` is set in `src/store/libsql_store.rs`
but **no table declares a `FOREIGN KEY`/`REFERENCES` clause** — referential
integrity between `contacts.company_id`, `deals.company_id`,
`interactions.contact_id`, `attendees.booking_id`, etc. is unenforced at the
DB layer. Additionally, the schema is one embedded `CREATE TABLE IF NOT
EXISTS` string re-run at every startup, with no version tracking.

**Objective:** Add real foreign keys where the domain model implies them, and
introduce a minimal versioned migration mechanism so future schema changes
don't require another giant string edit.

**Do:**
1. Audit `src/store/libsql_store.rs`'s `SCHEMA` constant and add
   `FOREIGN KEY (...) REFERENCES ...` clauses for every column that's
   logically a foreign key today (at minimum: `contacts.company_id →
   companies.id`, `deals.company_id → companies.id`, `interactions.contact_id
   → contacts.id`, `attendees.booking_id → bookings.id`, and the various
   `owner_id` columns if/when an `owners` table exists — if it doesn't yet,
   skip `owner_id` FKs and note that as a follow-up, don't invent an owners
   table in this prompt).
2. Decide `ON DELETE` semantics per relationship (likely `ON DELETE CASCADE`
   for `attendees→bookings`, `ON DELETE SET NULL` for `contacts.company_id`
   so deleting a company doesn't cascade-delete contacts) and document the
   choice in a comment above each constraint.
3. Introduce a `schema_migrations(version INTEGER PRIMARY KEY, applied_at
   TEXT NOT NULL)` table. Replace the single `SCHEMA` string with an ordered
   `Vec<(u32, &str)>` of migration steps; on `LibSqlStore::open`, apply any
   migration whose version isn't yet in `schema_migrations`, in order, inside
   a transaction per migration.
4. Migration 1 = the current schema as-is (baseline). Migration 2 = the new
   foreign keys (since adding FKs to existing tables in SQLite/libSQL
   requires a table rebuild — `CREATE TABLE ... new`, copy data, drop old,
   rename — implement that rebuild pattern explicitly for each altered
   table).

**Acceptance criteria:**
- A fresh `LibSqlStore::open` on an empty file ends with `schema_migrations`
  containing both versions and the new FK constraints active
  (`PRAGMA foreign_key_check` returns no rows).
- Opening a store that was seeded under the *old* code (pre-migration) and
  then upgraded also ends in the same state, with existing data intact
  (write a test that seeds via the old-style raw `SCHEMA` string, then opens
  it through the new migrating `open()`, and asserts row counts match).
- Attempting to insert a `deals` row with a non-existent `company_id` now
  fails with a `StoreError`/`AgentError` instead of silently succeeding.

**Out of scope:** don't add an `owners` table or owner-level FKs in this
prompt — that's a bigger multi-tenancy change (see P9).

---

## P5 — Introduce a pluggable `Transport` trait; decouple `aware.rs` from AutoTask

**Context:** `src/bin/aware.rs` hard-codes every outbound side effect (SMS,
WhatsApp, generic HTTP, notifications) as a raw TCP `POST` to
`http://127.0.0.1:8788` ("AutoTask"), with 13 call sites doing
`let _ = autotask_post(...)` — errors are silently discarded. This is the
single biggest blocker to running `cos` as a standalone service: it cannot
act on the world without a co-located sibling process.

**Objective:** Define a `Transport` trait in the library (`src/transport.rs`,
new module) that abstracts "send an SMS," "send a WhatsApp message," "make an
HTTP call," "post a notification," and "sync contacts." Provide the existing
AutoTask behavior as one implementation (`AutoTaskTransport`), so behavior is
unchanged by default, but the daemon can be configured with a different
implementation.

**Do:**
1. In `src/transport.rs`, define:
   ```rust
   #[async_trait::async_trait]
   pub trait Transport: Send + Sync {
       async fn send_sms(&self, to: &str, body: &str) -> Result<()>;
       async fn send_whatsapp(&self, to: &str, body: &str) -> Result<()>;
       async fn notify(&self, event: &serde_json::Value) -> Result<()>;
       async fn http(&self, method: &str, url: &str, data: Option<&serde_json::Value>) -> Result<String>;
       async fn sync_contacts(&self) -> Result<serde_json::Value>;
   }
   ```
   Return `crate::error::Result<T>` — add a new `AgentError::Transport(String)`
   variant (extend `error_info`/`error_catalog` exhaustively, per the existing
   pattern in `src/error.rs`).
2. Move the current AutoTask HTTP-proxy logic (`autotask_post`, `proxy_http`,
   `proxy_http_ua`, the `/v1/http`, `/v1/events`, `/v1/wa/send`,
   `/v1/contacts`, `/v1/location` calls) out of `src/bin/aware.rs` and into a
   new `AutoTaskTransport` struct implementing `Transport`, in
   `src/bin/autotask_transport.rs` (bin-local, since it's still an
   AutoTask-specific adapter — the trait lives in the lib, the adapter stays
   in the bin per the existing "no HTTP in the lib" rule).
3. Rewrite every `aware_*` handler in `aware.rs` to take `&dyn Transport`
   instead of calling `autotask_post` directly, and to **propagate transport
   errors instead of swallowing them** — replace every `let _ =
   autotask_post(...)` with `transport.notify(&evt).await?` (or `.ok()` only
   where a genuine best-effort semantic is intended, and if so, log the
   discarded error at `warn` level once P12's `tracing` lands — for this
   prompt, at minimum `eprintln!` the error so it's not silent).
4. Wire `route()` in `cos.rs` to hold `Arc<dyn Transport>`, defaulting to
   `AutoTaskTransport::new(autotask_url())` to preserve today's behavior
   exactly.
5. Add a second, minimal implementation — `NullTransport` (logs and no-ops,
   for tests) and a `DirectHttpTransport` stub that makes real outbound HTTP
   calls itself (using a minimal HTTP client — reuse the existing manual TCP
   POST helper pattern, or add `ureq` as a lightweight blocking-client
   dependency) as the first concrete step toward *not* needing AutoTask at
   all. `DirectHttpTransport`'s SMS/WhatsApp methods can return
   `AgentError::Transport("not yet configured — set a provider".into())` for
   now; the point of this prompt is the seam, not a full Twilio integration.

**Acceptance criteria:**
- `aware.rs` no longer references `autotask_post`/`proxy_http` directly —
  only through `&dyn Transport`.
- A test suite using `NullTransport` exercises every `aware_*` handler and
  asserts transport calls happen with the expected arguments (requires
  `NullTransport` to record calls, e.g. via an internal `Mutex<Vec<Call>>`).
- Default runtime behavior (AutoTask on `127.0.0.1:8788`) is unchanged —
  no regression in any existing manual/integration test that assumes
  AutoTask is present.

**Out of scope:** actually implementing a working Twilio/SMTP
`DirectHttpTransport` — that's a follow-up once a provider is chosen. This
prompt is about the seam.

---

## P6 — Centralize configuration; replace `.expect()` CLI parsing

**Context:** CLI flags in `src/bin/cos.rs::main()` are hand-parsed with
`.expect("addr value")`-style calls that panic on malformed input instead of
printing a usage error. Env vars (`COS_FULLY_GATED`, `COS_APPROVAL_TTL_MS`,
`AUTOTASK_URL`) are read ad hoc, inline, in whatever module needs them,
rather than being centralized.

**Objective:** Introduce a single `Config` struct, built once at startup from
CLI args (via `clap`, derive API) with env-var fallbacks, validated eagerly
with human-readable errors.

**Do:**
1. Add `clap = { version = "4", features = ["derive"] }` to `Cargo.toml`.
2. Define `Config` in a new `src/bin/config.rs`:
   ```rust
   #[derive(clap::Parser)]
   struct Cli { #[command(subcommand)] cmd: Command }

   #[derive(clap::Subcommand)]
   enum Command {
       Serve {
           #[arg(long, default_value = "127.0.0.1:8790")] addr: Option<String>,
           #[arg(long)] sock: Option<String>,
           #[arg(long, default_value = ".agentcal/cos.db")] db: String,
           #[arg(long, env = "COS_TOKEN")] token: Option<String>,
           #[arg(long, env = "COS_FULLY_GATED", default_value_t = false)] fully_gated: bool,
           #[arg(long, env = "COS_APPROVAL_TTL_MS")] approval_ttl_ms: Option<i64>,
           #[arg(long, env = "AUTOTASK_URL", default_value = "http://127.0.0.1:8788")] autotask_url: String,
           #[arg(long, default_value_t = 1_048_576)] max_request_bytes: usize,
       },
       Seed { #[arg(long, default_value = ".agentcal/cos.db")] db: String },
   }
   ```
   (adjust field set to match whatever P1/P2/P3/P5 actually added).
3. Replace `ApprovalConfig::from_env()` call sites (`src/rpc.rs`, wherever
   else) with values threaded from `Config` instead of re-reading env vars at
   call time — this makes config observable/testable in one place instead of
   scattered `std::env::var` calls.
4. `main()` becomes `let cli = Cli::parse();` — clap handles `--help`,
   invalid flags, and missing required values with proper exit codes and
   messages for free; delete every `.expect("... value")` in the old manual
   parser.

**Acceptance criteria:**
- `cos serve --addr` (missing value) now prints a clap-generated usage error
  and exits non-zero, no panic/backtrace.
- `cos --help` and `cos serve --help` produce readable usage text.
- All existing behavior (token auth, fully-gated mode, AutoTask URL override)
  is unchanged in effect, just centrally sourced.
- No remaining bare `std::env::var(...)` calls outside `config.rs` for the
  values now covered by `Config` (grep `src/` to confirm).

**Out of scope:** a config *file* (TOML/YAML) — flags + env only for this
prompt; file support can be a later addition to `Config`.

---

## P7 — Propagate structured error codes over RPC; fix the error-catalog gap

**Context:** `AgentError` → `ErrorInfo` (`code`, `category`, `meaning`,
`likely_cause`) is a well-built taxonomy in `src/error.rs`, but
`src/bin/cos.rs::route()` still serializes every error via `e.to_string()`
into `{"ok":false,"error":"<string>"}` — callers can't branch on error
category without string-matching. Separately, `error_catalog()`'s exemplar
list is missing `AgentError::LimitExceeded` and `AgentError::ChannelDisabled`
even though `error_info()` already handles them — the `error.catalog` RPC
method under-reports two real error codes.

**Objective:** Finish `ROADMAP.md` Phase 2 item 4 for real, and fix the
catalog gap in the same change (they're the same code path).

**Do:**
1. In `src/error.rs::error_catalog()`, add `AgentError::LimitExceeded(String::new())`
   and `AgentError::ChannelDisabled(String::new())` to the `exemplars` vec.
   The exhaustiveness `match` guard already lists both — this was presumably
   left out only because the two features landed in sibling PRs after this
   function was last touched; no other changes needed here.
2. Change `json_err` in `src/bin/cos.rs` to take an `&AgentError` instead of
   `&str`, and build the response as:
   ```json
   {"ok": false, "code": "booking_not_found", "category": "user",
    "message": "booking \"abc\" not found"}
   ```
   using `error_info(&e)` for `code`/`category` and `e.to_string()` for
   `message` (keep `message` for human debugging; `code` is the
   machine-readable contract).
3. Update `route()`'s `Err(e) => http_json(200, &json_err(&e.to_string()))`
   to `Err(e) => http_json(200, &json_err(&e))` (passing the `AgentError`,
   not its string).
4. Update `README.md`'s protocol example to show the new error shape.

**Acceptance criteria:**
- `tests/errors.rs` gains a test asserting a known failure (e.g. booking a
  nonexistent link) returns `{"ok":false,"code":"link_not_found","category":"user",...}`
  over the actual RPC path (not just via `error_info()` directly).
- `error.catalog` RPC response now includes both `limit_exceeded` and
  `channel_disabled` entries — add an assertion for this to whichever test
  currently covers `error.catalog`.
- No caller of the old `json_err(&str)` signature remains (compiler will
  catch this).

**Out of scope:** don't renumber or rename any existing `code` string —
they're documented as stability-pinned (`error_code` in `src/actions.rs`,
the `error_codes_stable` test) and must not change.

---

## P8 — Idempotency keys on mutating RPC methods

**Context:** `ROADMAP.md` Phase 2 item 5 flags this as missing; confirmed —
`cal.book`, `crm.create_contact`, `crm.create_deal`, `crm.log_interaction`
have no idempotency mechanism, so a client retry after a network blip (the
expected failure mode on a phone/adb-forward setup) can double-book or
double-create records.

**Objective:** Accept an optional `idempotency_key` param on the four
mutating methods above (extend to others if you find more while
implementing), store `(owner, method, idempotency_key) → result_json` with a
TTL, and replay the stored result on a repeat call instead of re-executing.

**Do:**
1. New table in `src/store/libsql_store.rs`'s migrations (see P4 — add this
   as the next migration version):
   ```sql
   CREATE TABLE IF NOT EXISTS idempotency_keys (
       owner_id   TEXT NOT NULL,
       method     TEXT NOT NULL,
       key        TEXT NOT NULL,
       result_json TEXT NOT NULL,
       created_at TEXT NOT NULL,
       PRIMARY KEY (owner_id, method, key)
   );
   CREATE INDEX IF NOT EXISTS idx_idempotency_created ON idempotency_keys(created_at);
   ```
2. In `src/rpc.rs::dispatch_inner`, for the four target methods: if
   `params.idempotency_key` is present, check the table first — if a row
   exists, deserialize and return `result_json` directly without touching
   `AgentCal`/`AgentCrm`. If not, execute normally, then write the
   `(owner, method, key) → result` row before returning.
3. Add a TTL sweep: on `LibSqlStore::open` (or a periodic task if P3's axum
   server makes that easy via `tokio::spawn` + `tokio::time::interval`),
   delete rows older than 24h (matching `DEFAULT_PENDING_TTL_MS` from
   `approvals.rs` for consistency — reuse that constant rather than
   inventing a new one).
4. Document the contract in `src/rpc.rs`'s module docstring: same
   `idempotency_key` + same `method` + same `owner` → same result, replayed,
   *regardless of whether the params otherwise differ* (matching standard
   idempotency-key semantics — callers are responsible for not reusing a key
   across logically different requests).

**Acceptance criteria:**
- A test calls `cal.book` twice with an identical `idempotency_key` and
  different-but-compatible params; asserts only one booking exists in the
  store and both calls return the identical `booking.id`.
- A test calls `crm.create_contact` twice with the same key; asserts only
  one contact row exists.
- Omitting `idempotency_key` entirely preserves today's behavior exactly
  (no regression to existing tests).

**Out of scope:** don't add idempotency to read methods or to `cal.cancel`
(which already has approval-gating semantics that partially cover this) —
scope is exactly the four methods named above unless you find clear evidence
another mutating method needs it too.

---

## P9 — Replace the single shared bearer token with per-owner API keys

**Context:** The daemon's single `--token` authenticates the *connection*,
not the *tenant*. Any caller holding that one token can pass an arbitrary
`owner` string in RPC params and read/write that owner's data — there is no
binding between "who's allowed in" and "whose data they're allowed to touch."

**Objective:** Introduce per-owner API keys so a caller's credential
determines (and restricts) which `owner` value they may use, closing the
impersonation gap, while preserving a simple single-owner mode for the
existing phone deployment (don't force multi-tenancy complexity on the
common case).

**Do:**
1. New table (as a migration, per P4): `api_keys(key_hash TEXT PRIMARY KEY,
   owner_id TEXT NOT NULL, created_at TEXT NOT NULL, label TEXT NOT NULL
   DEFAULT '')`. Store a hash (SHA-256 is fine — this isn't a password, it's
   a bearer credential, but don't store it in plaintext) rather than the raw
   key.
2. Add `cos keys create --owner <id> --label <text>` and `cos keys revoke
   --key-hash <hash>` subcommands (extend the `Config`/`clap` setup from P6)
   that generate a random 32-byte key, print it once (never again), and
   store its hash.
3. In `route()` (or the axum middleware if P3 landed first), replace the
   single-token check with: look up the presented bearer token's hash in
   `api_keys`; if found, the resolved `owner_id` becomes an **implicit,
   enforced** value — reject any RPC call whose `params.owner` doesn't match
   the resolved owner (`403`, not `401`) unless the daemon is running in the
   existing single-shared-token legacy mode (keep `--token` working exactly
   as today when set, for backward compatibility with the current phone
   deployment — make per-owner keys opt-in via a new `--multi-tenant` flag
   or by simply having any rows in `api_keys`).
4. Update `DEPLOY.md` with the new key-management commands, keeping the
   existing single-token instructions intact as the default/simple path.

**Acceptance criteria:**
- A test creates two owner-scoped keys (`alice`, `bob`), and asserts a
  request authenticated as `alice`'s key with `params.owner = "bob"` is
  rejected with `403`, while `params.owner = "alice"` succeeds.
- Existing single-shared-token tests are unaffected (legacy mode still
  works exactly as today).
- Key hashes, not raw keys, are the only thing ever persisted or logged.

**Out of scope:** key rotation-without-restart automation, and any kind of
admin UI — RPC/CLI only.

---

## P10 — `rpc.describe`: generate the method table from a single source of truth

**Context:** ~35+ RPC methods are hand-matched in `src/rpc.rs::dispatch_inner`
(a 500+ line function), and the method list is duplicated by hand in
`README.md` and `DEPLOY.md`. `ROADMAP.md` Phase 3 item 7 flags this.

**Objective:** Build a method registry that `dispatch` is generated from (or
at minimum kept in lockstep with via a compile-time check), and expose it via
a new `rpc.describe` method.

**Do:**
1. Define a `MethodSpec { name: &'static str, params: &'static [ParamSpec],
   risk_tier: &'static str, description: &'static str }` and
   `ParamSpec { name: &'static str, required: bool, kind: &'static str }` in
   a new `src/rpc_registry.rs`.
2. Build a `const METHODS: &[MethodSpec]` table listing every method
   currently in `dispatch_inner`'s `match`, cross-referencing
   `approvals.rs`'s existing risk-tier lists (`read`/`low`/`high`) for the
   `risk_tier` field — this also surfaces and lets you fix the documentation
   drift found in the review (methods like `crm.delete_contact` listed as
   `high` tier that don't actually exist as dispatch arms — either implement
   them or remove them from the tier table in the same change).
3. Add a test that walks `METHODS` and asserts every `name` is a string
   literal that also appears as a `match` arm string in `dispatch_inner`
   (simple `grep`-style compile-time-adjacent check via a build script, or a
   `#[test]` that reads `src/rpc.rs`'s source via `include_str!` and greps
   for each method name — pragmatic, not elegant, but it catches drift).
4. Add `"rpc.describe" => Ok(serde_json::to_value(rpc_registry::METHODS)?)`
   to `dispatch_inner`.

**Acceptance criteria:**
- `rpc.describe` returns all methods with their param shapes and risk tier.
- The drift-check test fails if a method is added to `dispatch_inner` without
  a corresponding `MethodSpec`, or vice versa.
- `README.md`'s method list is replaced with "call `rpc.describe`, or see
  `src/rpc_registry.rs`" rather than a hand-maintained list.

**Out of scope:** don't try to derive `METHODS` via a proc-macro in this
prompt — a hand-maintained-but-tested table is the pragmatic first step.

---

## P11 — Batched RPC calls

**Context:** `ROADMAP.md` Phase 3 item 8: one method per `POST /` today.
Common agent flows ("resolve contact → log interaction → check upcoming")
cost 3 round trips.

**Objective:** Accept an array of `{method, params}` in one `POST /` and
execute them in order against the same store handle, returning ordered
results.

**Do:**
1. In `route()` (`src/bin/cos.rs`, or the axum handler if P3 landed), detect
   whether the parsed body is a JSON array vs. a single object. If an array,
   iterate and call `dispatch_with_gate` for each entry in order, collecting
   `{"ok":.., "result"/"error":..}` per entry into a JSON array response.
2. Cap batch size (`MAX_BATCH_SIZE = 50`, reject larger batches with a `400`)
   to prevent a single request from becoming an unbounded amount of work —
   consistent with the request-size hardening in P2.
3. Each item in the batch still goes through the action log
   (`dispatch_with_gate` already does this per-call) — no special-casing
   needed there.
4. Document in `src/rpc.rs`'s module docstring: batch items execute
   sequentially, not transactionally — a failure partway through does **not**
   roll back earlier items in the batch (make this explicit so callers don't
   assume atomicity they don't get).

**Acceptance criteria:**
- A test sends `[{"method":"ping",...}, {"method":"crm.summary",...}]` and
  asserts an ordered two-element array response, each with the same shape a
  single call would produce.
- A test confirms a batch of `MAX_BATCH_SIZE + 1` is rejected with `400`
  before any item executes.
- Existing single-object `POST /` behavior is completely unchanged (single
  object in → single object out, not wrapped in an array).

**Out of scope:** transactional/atomic batches — explicitly a non-goal per
the docstring above; don't build a rollback mechanism.

---

## P12 — Structured logging + minimal metrics

**Context:** The daemon has zero structured logging — no `tracing`/`log`
dependency at all, just 18 scattered `println!`/`eprintln!` calls. No log
levels, no correlation IDs, no metrics/telemetry of any kind.

**Objective:** Add `tracing` with a `tracing-subscriber` output, structured
per-request logging (method, owner, duration, result code), and a minimal
counter/histogram set exposed via a `/metrics` endpoint (Prometheus text
format is the pragmatic default).

**Do:**
1. Add `tracing = "0.1"`, `tracing-subscriber = { version = "0.3", features
   = ["env-filter"] }` to `Cargo.toml`. Add `metrics = "0.23"` and
   `metrics-exporter-prometheus = "0.15"` (or current stable equivalents) if
   you want metrics in this same prompt — otherwise split metrics into a
   follow-up and just do logging here (prefer splitting if time-boxed).
2. Initialize the subscriber once in `main()`, respecting `RUST_LOG`
   (default to `info` if unset).
3. In `dispatch_with_gate` (`src/rpc.rs`), wrap the call in a `tracing::info_span!`
   carrying `method`, `owner`, and log the outcome (`ok` / error `code`) and
   duration at `info` (success) or `warn` (error) on completion — this
   naturally covers every RPC call site in one place rather than
   instrumenting each handler individually.
4. Replace every `eprintln!`/`println!` in `src/bin/cos.rs` and
   `src/bin/aware.rs` with `tracing::info!`/`tracing::warn!`/`tracing::error!`
   as appropriate (in particular: the `let _ = autotask_post(...)` sites
   from P5 should now log the discarded error at `warn` instead of staying
   silent, if P5 hasn't already made them propagate).
5. If doing metrics in this prompt: add counters for
   `rpc_calls_total{method,result}` and a histogram
   `rpc_duration_seconds{method}`, and a `GET /metrics` route.

**Acceptance criteria:**
- Running `cos serve` with `RUST_LOG=info` produces one structured log line
  per RPC call with method/owner/result/duration.
- No remaining bare `println!`/`eprintln!` in `src/bin/*.rs` (grep to
  confirm), except the genuinely one-off startup banner
  (`"cos listening on ..."`) which is fine to leave as-is or convert, your
  call.
- If metrics included: `curl /metrics` returns valid Prometheus text format
  and reflects at least one call after a `ping`.

**Out of scope:** shipping logs/metrics anywhere (no OTLP exporter, no log
aggregation integration) — stdout/local `/metrics` only for this prompt.

---

## P13 — Integration tests for the daemon's network surface

**Context:** All 110 existing tests call `AgentCal`/`AgentCrm`/`dispatch()`
directly, in-process. The HTTP parser, the auth check, and the entire
AutoTask bridge — the most externally exposed and most hand-rolled code in
the repo — have **zero** test coverage.

**Objective:** Add integration tests that actually start the daemon (or the
axum router from P3) on an ephemeral port/socket and drive it over real
HTTP/Unix-socket connections, plus tests for the auth boundary and the
`Transport` trait seam from P5.

**Do:**
1. New `tests/daemon.rs`: spin up `serve()` (or the axum `Router`) bound to
   `127.0.0.1:0` (OS-assigned port) in a background `tokio::spawn`, using a
   `tempfile`-backed DB (already a dev-dependency). Drive it with a simple
   HTTP client — either hand-roll a minimal client matching the existing
   manual-TCP style already in the codebase (`aware.rs`'s `autotask_post` is
   a template) or add `reqwest` as a dev-dependency for test ergonomics
   (preferred — it's a dev-dep, doesn't touch the shipped binary's
   dependency footprint).
2. Cover: a successful `ping`, a `401` on missing/wrong token when `--token`
   is set, a `413`/`400` on oversized requests (from P2), a full RPC
   round-trip (`crm.create_contact` → `crm.get_contact`), and — if P11
   landed — a batch call.
3. New `tests/transport.rs` (depends on P5): exercise `aware_sms_send` /
   `aware_whatsapp_send` / etc. against `NullTransport`, asserting the
   recorded calls match expectations, and a negative test asserting a
   `Transport` error propagates as an `AgentError::Transport` out of the
   `aware_*` handler rather than being swallowed.
4. If a Unix-socket bind mode exists (it does — `--sock`), add at least one
   test exercising that path too, not just TCP.

**Acceptance criteria:**
- `cargo test` covers the previously-untested `src/bin/cos.rs` and
  `src/bin/aware.rs` network paths — confirm via `cargo tarpaulin` (or
  similar coverage tool) showing non-zero coverage on both files if
  reasonable to add to the toolchain; otherwise, coverage is demonstrated by
  the fact these new tests fail if you deliberately break `route()`'s auth
  check (do this as a sanity check while writing the tests, then revert).

**Out of scope:** load/stress testing — that's implicitly covered by P3's
acceptance criteria; this prompt is about correctness, not performance.

---

## P14 — CI pipeline

**Context:** No `.github/workflows` (or any CI) exists. Nothing currently
blocks a broken build or a failing test from landing.

**Objective:** Add a GitHub Actions workflow running fmt, clippy, and the
full test suite on every push/PR.

**Do:**
1. `.github/workflows/ci.yml`:
   ```yaml
   name: CI
   on: [push, pull_request]
   jobs:
     test:
       runs-on: ubuntu-latest
       steps:
         - uses: actions/checkout@v4
         - uses: dtolnay/rust-toolchain@stable
           with: { components: clippy, rustfmt }
         - uses: Swatinem/rust-cache@v2
         - run: cargo fmt --all -- --check
         - run: cargo clippy --all-targets --all-features -- -D warnings
         - run: cargo test --all-features
   ```
2. Fix whatever `cargo fmt --check` and `cargo clippy -D warnings` surface on
   the current tree (run locally first, land formatting/lint fixes as a
   separate preceding commit so the CI-introduction commit itself is green).
3. Add a `cargo build --release --bin cos` step to catch release-profile-only
   issues (unlikely given the crate's simplicity, but cheap insurance).
4. If the project wants to keep supporting the Android/NDK cross-compile
   target documented in `DEPLOY.md`, add a second, non-blocking job that
   attempts `cargo ndk -t arm64-v8a build --release --bin cos` (allow this
   job to fail without blocking merges initially, given the NDK toolchain
   setup complexity in CI — mark `continue-on-error: true`).

**Acceptance criteria:**
- A PR with a deliberately broken test fails CI.
- A PR with a `cargo fmt` violation fails CI.
- `main` is green after this lands.

**Out of scope:** deployment automation (that's a separate, larger effort
given the current `adb`-driven manual process) — this prompt is build/test
verification only.

---

## P15 — Real embedding model pluggability for `crm.vector_search`

**Context:** `embed_text()` (`src/crm/types.rs`) is a deterministic 64-dim
hashed bag-of-words — useful as a zero-dependency placeholder, but not actual
semantic search. `ROADMAP.md`'s "structured + semantic recall" framing
overstates current capability.

**Objective:** Make the embedding function pluggable so a real model can be
substituted without touching the storage layer (which is already correctly
shaped — `F32_BLOB(64)`/`vector_distance_cos` — just fed a weak vector today).

**Do:**
1. Define an `Embedder` trait in `src/crm/embed.rs`:
   ```rust
   #[async_trait::async_trait]
   pub trait Embedder: Send + Sync {
       async fn embed(&self, text: &str) -> Result<Vec<f32>>;
       fn dims(&self) -> usize;
   }
   ```
2. Wrap the existing `embed_text()` as `HashEmbedder` (default,
   dependency-free, `dims() == 64`) implementing `Embedder`.
3. Thread `Arc<dyn Embedder>` through `AgentCrm` (constructor param, default
   to `HashEmbedder` if not specified, preserving today's behavior exactly).
4. Note in the trait docs that swapping to a real model requires also
   changing the `F32_BLOB(64)` column width to match the new model's
   dimensionality (e.g. 384/768/1536) — that's a schema migration (use the
   P4 mechanism), explicitly out of scope for *this* prompt, which is only
   about the seam.
5. Do **not** implement a real external-API-backed `Embedder` in this prompt
   (no OpenAI/local-model dependency) — the goal is making it pluggable, not
   picking a provider.

**Acceptance criteria:**
- `AgentCrm::new_with_embedder(store, embedder)` exists alongside the
  existing `AgentCrm::new(store)` (which defaults to `HashEmbedder`).
- All existing `crm.vector_search` tests pass unmodified (behavior
  unchanged by default).
- A test constructs `AgentCrm` with a mock `Embedder` (fixed output vector)
  and asserts `crm.vector_search` uses it instead of `embed_text()` directly.

**Out of scope:** picking/integrating a real embedding provider, and the
schema-width migration for a different dimensionality — both explicitly
deferred.

---

## P16 — Document the single-writer trade-off; formalize backup/restore

**Context:** `ROADMAP.md` explicitly declares "no multi-node replication" a
non-goal, and the store is a single SQLite/libSQL file behind one mutex-
guarded connection (see P... wait, addressed structurally in P3/architecture
notes). Backup today is a manual `adb pull` sequence in `DEPLOY.md`. This
prompt doesn't change the architecture — it makes the trade-off explicit and
gives the operator a real backup story, which "standalone service" requires
regardless of scale.

**Objective:** Add a `cos backup --db <path> --out <path>` subcommand doing a
consistent snapshot (SQLite's online backup API, exposed by libSQL), and
document the single-writer trade-off explicitly in `ROADMAP.md`/`README.md`
rather than leaving it implicit.

**Do:**
1. Add a `Backup { db: String, out: String }` variant to the `Command` enum
   from P6, implemented via libSQL's backup/checkpoint API (check what's
   exposed by the `libsql` crate version pinned in `Cargo.lock` — if only
   file-copy-while-locked is available, take the connection mutex for the
   duration of the copy to guarantee consistency, and document that this
   briefly blocks the live daemon if run against a hot DB — prefer running
   backups against a WAL checkpoint if the crate supports it).
2. Add a short "Scaling model" section to `ROADMAP.md` (or a new
   `docs/OPERATIONS.md`) stating plainly: one writer, one file, per owner-set;
   horizontal scaling is out of scope by design; backup is `cos backup`, run
   on whatever cadence the deployment needs (cron on the phone via Termux, or
   an external trigger).
3. Update `DEPLOY.md`'s "Persistence" section to reference the new command
   instead of the manual `adb pull` instructions (keep the `adb pull`
   instructions as a fallback for pulling the resulting backup file off the
   phone — that part of the process is unavoidable given the deployment
   target).

**Acceptance criteria:**
- `cos backup --db foo.db --out foo.bak` produces a valid, independently
  openable libSQL file with all current data, verified by a test that seeds
  a DB, backs it up, reopens the backup via `LibSqlStore::open`, and asserts
  the summary matches.
- Running `cos backup` against a DB currently being written to by a
  concurrent `serve()` process doesn't corrupt either file (test with a
  concurrent writer task if feasible; at minimum document the locking
  behavior clearly if a live concurrency test proves impractical in the test
  harness).

**Out of scope:** scheduled/automatic backups, remote backup targets (S3
etc.) — local file-to-file only for this prompt.

---

### Suggested execution order

`P1 → P2 → P3 → P4 → P5 → P6 → P7 → P8 → P9 → P10 → P11 → P12 → P13 → P14 → P15 → P16`

P1–P3 fix things that are silently broken or actively dangerous under load.
P4–P9 close correctness/security gaps. P10–P11 are the ergonomics work
`ROADMAP.md` already scoped. P12–P14 make the result operable and safe to
merge against going forward. P15–P16 round out the remaining gaps this review
surfaced beyond `ROADMAP.md`'s own list.
