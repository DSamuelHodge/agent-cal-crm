# AutoTask + CoS — Re-engineered Architecture
### Deprecated as of September 8, 2026

> Internal team doc. Companion to the `architecture.html` overview page.

## TL;DR

The system is a deterministic automation substrate (device signals → rules →
actions) with an LLM policy layer on top that reads state and issues the same
actions a rule would. Everything else in the diagram exists to *support* that
core running reliably on Android, and most of it is solved-problem territory
that should be borrowed, not re-built.

## The core (irreducible)

```
device signals → rules → actions
                  ↑
            LLM policy layer (reads state, issues actions)
```

Time-critical reflexes (SMS triage, notification firing, WhatsApp send
dispatch) never block on a model. The rule engine guarantees sane behavior
with no model available; the LLM makes the system intelligent, not merely
obedient.

---

## System Flowchart

```mermaid
flowchart TB
    subgraph PACKAGE ["AutoTask + CoS (single APK, shared UID)"]
        subgraph ENGINE ["PROCESS :engine — Automation OS"]
            EVENTBUS["Typed Event Bus\nsensors · telephony · notifications · app lifecycle · calendar"]
            MATCHER["Matcher Engine\n50+ triggers → conditions → actions\ncooldown · priority · sub-profile composition"]
            KTOR["Ktor Server\nUNIX domain socket (@cosd) · token-auth"]
            ROUTES["/v1/status · /v1/schema · /v1/capabilities\n/v1/profiles · /v1/events · /v1/logs\n/v1/contacts · /v1/http · /v1/web/*"]
            WEBDRIVER["WEB DRIVER\nWebView + injected JS\nsessionStorage handoff · in-tab deep links"]
            HEALTH["HealthMonitor (passive)\ndetect grant drift → deep-link repair prompt\nverify post-repair · report via /v1/status\nnever silently re-grants"]
            SUPERVISOR["Brain Supervisor\nhealthcheck every 2s · exp. backoff restart 2s→60s cap\nkill-switch after N crashes/window\nPID-file guard · WAL-safe resume"]
            OTA["OTA Self-Update\nsigned APK install · no adb\nalso re-extracts bundled libcosd.so"]
            WORK["WorkManager Scheduler\nTimeTriggerWorker · alarms"]
        end

        subgraph BRAIN ["PROCESS :brain — Rust CoS Core"]
            DAEMON["cos daemon — native PIE executable\nextracted from jniLibs/{abi}/libcosd.so\n(extractNativeLibs=true · ProcessBuilder-spawned)\nlibSQL CRM + calendar"]
            HTTPC["HTTP server\nUNIX domain socket (@cosd)\ndebug: adb forward tcp:8790 ↔ localabstract:cosd"]
            RPC["aware.sms · aware.call · aware.whatsapp\naware.whatsapp.send · aware.capture\naware.meeting · aware.travel"]
            SYNC["Sync to self-hosted\nLogseq graph · contacts mirror"]
        end

        LLM["LLM POLICY LAYER\nplanner · judge · communicator\ninterprets inbound · drafts replies · writes journal\nreads state via aware.* surface · never in hot path"]
        AGENT["Agent (on-device embedded model\nor remote — e.g. opencode on the Mac)"]

        AGENT --> LLM
        LLM -. issues aware.* calls .-> RPC
        LLM -. reads state via .-> ROUTES
    end

    DEVICEWORLD["Device signals\nSMS · calls · notifications · GPS\nWi-Fi · battery · sensors · calendar"]

    EXTERNAL["External\nWhatsApp Web · OSRM · geocoding\nMeta · Tesla · HTTP APIs"]

    USER["User\nphone notifications · speak · UI"]

    subgraph STORAGE ["Storage"]
        ROOM[("Room\nengine state · profiles · logs")]
        LIBSQL[("libSQL (app-private)\ncontacts · deals · interactions")]
        LOGSEQ[("Logseq\nself-hosted graph")]
    end

    DEVICEWORLD --> EVENTBUS
    EVENTBUS --> MATCHER
    MATCHER --> KTOR
    KTOR --> ROUTES
    MATCHER --> WEBDRIVER
    WEBDRIVER <--> EXTERNAL
    KTOR <-->|UNIX domain socket| DAEMON
    HTTPC --> RPC
    BRAIN -.|/v1/http proxy| EXTERNAL
    ROUTES -->|aware.* RPC| HTTPC
    ROUTES -->|/v1/web/send| WEBDRIVER
    HEALTH -. watches .-> KTOR
    SUPERVISOR -. healthcheck + restart .-> DAEMON
    SUPERVISOR -.->|liveness via /v1/status| ROUTES
    OTA --> PACKAGE
    MATCHER --> ROOM
    DAEMON <--> LIBSQL
    DAEMON <--> SYNC
    SYNC --> LOGSEQ
    MATCHER -. post informed notification .-> USER
    ROOM --> WORK
```

> Note: the LLM layer and the `/v1/http` proxy (brain → external) were added
> in v2.1 — the brain's outbound HTTPS routes through the engine's `/v1/http`
> proxy, it does not talk to external services directly.

---

## Where the AI sits

The system is deliberately split: a **deterministic substrate** (AutoTask's
event→rule engine, the Rust daemon's `aware.*` handlers, libSQL CRM) that is
fast, always-on, and never hallucinates — and an **LLM layer on top** that
plays planner, judge, and communicator. Time-critical device reflexes (SMS
triage, notification firing, WhatsApp send dispatch) never block on a model;
they're handled by rules and the daemon. The LLM enters at the seams: it
interprets ambiguous inbound ("call Shaun Ford re: Curtis Jewell"), decides
whether and how to act (draft the reply, open the travel route, surface a
contact), generates the informed notification text a human reads, and—via the
agent on the Mac, or an embedded model later—reflects over the day's events
to write the journal and plan follow-ups. In the diagram, the LLM is not a
box in the pipeline but a **policy layer above it**: it reads state through
the same HTTP/`aware.*` surface any tool uses, and it acts only by issuing
those same calls, so every decision it makes is auditable, reversible, and
separable from the always-on core.

---

## Key Design Decisions

1. **Unified Package.** Two processes, one UID — the Rust brain ships as a
   supervised child/`.so`, owned by the OS. No Termux, no Shizuku, no
   `run-as`, no adb-deploy.
2. **Passive Drift Detection.** Android gives no API to silently re-grant
   listener/DND/`WRITE_SETTINGS` access — HealthMonitor is a tripwire, not a
   fighter: it detects drift and prompts a precise one-tap deep-link repair.
3. **First-Class WebDriver.** The WhatsApp bridge pattern is generalized to
   any SPA via one config schema, making web automation universally robust.
4. **Domain Socket + Auth.** Engine↔brain IPC moved from loopback TCP to a
   UNIX domain socket — other apps on-device can no longer even see the port.
   Token-auth layers on top; no more empty-bearer.
5. **App-Private Storage.** libSQL moves out of Termux home into app-private
   storage with sync mechanisms. The Logseq graph becomes the source mirror.
6. **OTA Self-Update.** The dev loop (rebuild → reinstall → re-pair) becomes
   fast in-app updates, featuring a debug hot-swap override capability.
7. **W^X-Safe Native Execution.** Android 10+ blocks `exec()` from
   app-writable storage. The daemon ships as `jniLibs/{abi}/libcosd.so`
   (`extractNativeLibs=true`), extracted to the OS-owned `nativeLibraryDir`,
   and is spawned — never `dlopen`'d — via `ProcessBuilder`. Updating it can
   only happen through a real APK install, so it rides the same OTA path by
   construction.
8. **Explicit Brain Supervision.** The OS won't restart a spawned child, so
   `:engine` owns that job: 2s healthchecks, exponential backoff (2s→60s cap)
   on crash, a kill-switch after N crashes/window, a PID-file guard against
   double-spawn, and WAL-mode libSQL for crash-safe resume. Liveness surfaces
   through `/v1/status`.
9. **LLM Policy Layer (planner, not pipeline).** The substrate is
   deterministic, always-on, and never hallucinates. Time-critical reflexes
   never block on a model. The LLM sits *above* the system as a policy layer:
   it reads state through the same HTTP/`aware.*` surface any tool uses, and
   acts only by issuing those same calls — so every decision is auditable,
   reversible, and separable. The rule engine keeps the device sane with no
   model available; the LLM makes it intelligent rather than merely obedient.

---

## Simplification Opportunities

A lot of what's in this diagram is solved-problem territory that got
custom-built instead of borrowed. The core — device signals → rules → actions
+ an LLM that reads state and issues the same actions a rule would — is
genuinely simple. Almost everything else exists to *support* that core
running reliably on Android, and that's where the borrowing opportunity is.
This section separates what's actually irreducible from what's accumulated
complexity, and names the specific existing thing to lean on for each.

### A. The process-separation fork (the biggest lever)

The entire IPC layer — Ktor server, domain socket, token auth, the Brain
Supervisor's healthcheck/backoff/kill-switch, HealthMonitor watching Ktor —
exists because `:brain` is a separate spawned OS process. If the Rust core
were instead loaded in-process as a JNI library, all of that infrastructure
disappears: no socket, no auth, no supervisor loop, no liveness reporting,
because a JNI call can't "be down" the way a separate process can.

**This is the fork that generates a third of this diagram's complexity. Make
it a conscious trade, not an accumulated one.**

There are three options:

| Option | Crash isolation | IPC / supervisor / auth / W^X | Notes |
|---|---|---|---|
| **In-process JNI** | ✗ (a Rust panic takes the UI down) | None needed | Simplest. No socket, no auth, no supervisor. |
| **`:brain` binder process** (recommended) | ✓ | Nearly none — binder is kernel-enforced, `linkToDeath` is the liveness signal | Declared via `android:process=":brain"`, talk AIDL/binder. W^X `libcosd.so` exec dance disappears (loaded, not exec'd). Token auth on IPC disappears. |
| **Spawned exec (current plan)** | ✓ | Full set — socket, auth, supervisor, W^X | The diagram as drawn. Works, but it's the expensive version. |

**Recommendation:** declare `:brain` as a real Android process
(`android:process=":brain"`) and talk to it over **AIDL/binder**. You keep
crash isolation, Android owns process lifecycle, `linkToDeath` replaces the
bespoke healthcheck loop, and binder's kernel-enforced permissions replace the
domain socket + token auth. The separate-process choice then costs roughly
zero of the diagram's complexity.

### B. Storage: three stores where you need two

Room and libSQL are both SQLite under the hood — libSQL is wire-compatible
with SQLite's C API. Right now the plan syncs between them as if they were
different systems when they're the same engine with two drivers pointed at
it. Room can be pointed at a `SupportSQLiteOpenHelper` targeting the libSQL
file directly.

**Collapse Room + libSQL into one on-disk database** (engine tables, CRM
tables, same file), and the entire engine↔brain data-sync problem disappears
— there's nothing to keep in sync because one database is read by both
processes. Logseq stays as the one deliberate *outward* mirror, which is a
much smaller, one-directional problem.

Caveat: the CRM uses libSQL-only extensions — **FTS5 + vector search**. Room
writes the same file, but those features stay Rust-side. The merge works
*provided* libSQL is compiled in SQLite-compatible mode and both sides agree
on the schema. It kills the sync problem; it doesn't eliminate the Rust
dependency.

### C. The LLM policy layer is already an MCP server — name it that

"Reads state through the same HTTP/`aware.*` surface any tool uses, acts only
by issuing those same calls" is a description of **MCP tool-calling**, not a
bespoke pattern. Rather than hand-building an agent harness for "opencode on
the Mac", expose the `/v1/*` routes and `aware.*` RPCs as **MCP tools** and
drive them with any existing MCP client — Claude Code, Claude Desktop,
opencode — getting tool schemas, multi-turn state, and planning for free.

Precedent: we already wired **Logseq's MCP server** into opencode this
session. The same pattern applies to the CoS surface.

Nuance: it's a **layer, not a replacement**. The deterministic rule engine
(AutoTask profiles) still needs direct HTTP; MCP is the LLM's front door onto
the same surface. Both share one implementation. This is the cheapest part of
the system to *not* build from scratch.

### D. Smaller borrows, lower stakes

- **Matcher vocabulary.** The Matcher Engine (50+ triggers, cooldown,
  priority) is a textbook event-condition-action rule engine. You can't embed
  Home Assistant on Android, but its automation schema (`trigger` /
  `condition` / `action`, `mode: single|restart|queued|parallel`, `for:`
  duration conditions) is a mature, battle-tested vocabulary. Check the 50
  rules against it — it surfaces edge cases (e.g. what happens when a trigger
  fires while its own action is still running) that are easy to miss when
  designing from scratch.
- **Restart backoff math.** The supervisor's 2s→60s exponential backoff +
  kill-switch is what `systemd`/`s6`/`daemontools` already implement as a
  small, well-tested state machine. `WorkManager.BackoffPolicy.EXPONENTIAL`
  exists too — **but** it governs scheduled *jobs*, not a live daemon
  process. Borrow the backoff *math*, don't swap the supervisor for
  WorkManager. (The `:brain` binder option makes most of this moot anyway.)
- **WebDriver action vocabulary.** The wait-for-selector / click / fill /
  extract action schema is exactly what Tampermonkey/userscripts and
  browser-automation tools already settled on. Borrow the *words*, not the
  code — low risk of reinventing that badly if you match a known shape.

### If you only did three things

1. **Merge Room + libSQL into one store** (highest value / lowest risk).
2. **Expose the `aware.*` / HTTP surface as actual MCP tools** instead of a
   bespoke agent harness.
3. **Re-confirm the process model** for `:brain`, knowing the spawned-exec
   version is the reason the supervisor/socket/auth subsystem exists — and
   prefer the `:brain` binder middle path.

---

## Work Item: Storage Merge (Room + libSQL → one file)

**Goal:** one on-disk SQLite database, read by both `:engine` (via Room) and
`:brain` (via libSQL). Eliminate engine↔brain data sync.

### What changes

- Single DB file at `<filesDir>/cos.db` (app-private).
- Engine tables: profiles, execution logs, engine state (Room-managed).
- CRM tables: contacts, companies, deals, interactions (libSQL-managed).
- Both sides open the *same file*; WAL mode + standard SQLite locking handles
  cross-process access (SQLite is designed for it).

### The pieces

1. **libSQL in SQLite-compatible mode.** Build libSQL so the on-disk format
   is standard SQLite (`SQLITE` storage mode, not the native libSQL format).
   Verify FTS5 + vector-search extensions still work in that mode.
2. **Room `SupportSQLiteOpenHelper` targeting the file.** Provide a custom
   `SupportSQLiteOpenHelper.Factory` that opens `<filesDir>/cos.db` directly,
   instead of Room creating its own file. Room then owns engine tables in the
   same physical DB.
3. **Schema agreement.** One shared schema document. Engine tables and CRM
   tables coexist; neither side touches the other's tables. Migration must be
   coordinated (Room's `Migration` on engine tables + a Rust-side version
   check on CRM tables).
4. **Keep FTS5/vector on the Rust side.** Vector search and FTS stay
   libSQL-only; Room never queries them.
5. **Logseq remains the single outward mirror.** `SYNC` continues to push
   contacts/interactions/journal to the self-hosted Logseq graph. This is the
   only deliberate cross-store copy left.

### Risks / decisions to confirm

- **File-format lock-in:** libSQL must be pinned to SQLite-compatible output
  or the merge is void. Decision: which libSQL build flags / version.
- **Locking contention:** SQLite handles multi-process WAL, but heavy
  concurrent writes from both processes should be avoided; the CRM and engine
  tables are written by different processes by construction, which is clean.
- **Migration coordination:** two writers, one file → schema changes must be
  coordinated across the JVM and Rust sides. Decide the migration authority
  (likely Room for engine tables, libSQL for CRM, never overlapping).

### Definition of done

- `:engine` and `:brain` both open `<filesDir>/cos.db` and read/write their
  own tables with no sync layer between them.
- CRM features (FTS5, vector search, `aware.*` RPCs) unchanged.
- Engine state (profiles, logs) unchanged.
- Logseq mirror still working.
- Crash-safe: WAL-mode resume verified after killing `:brain` mid-write.
