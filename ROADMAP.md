# agent-cal-crm Roadmap

> Supersedes the planning content of `docs/ARCHITECTURE.md` (deprecated 2026-09-08,
> kept for historical context only). This is the forward plan, distilled from a
> design review of the current tree and ordered by leverage.

## The design we're committed to

**Two façades, one store.** `AgentCal` (calendar) and `AgentCrm` (contacts,
companies, deals, interactions) share one libSQL store, with bidirectional
resolution — booking→contact via `crm.contact_for_booking`, contact→attendee
via `crm.attendee_for_contact` (`src/rpc.rs`). Calendar and CRM data cannot
drift out of sync because there is only one copy of the truth.

Principles we keep as we grow:

- **Transport-free library.** All logic lives in the lib (`src/`); the
  HTTP/JSON layer exists only in `src/bin/cos.rs::route`. Swapping the daemon
  for a Unix socket (already supported in `serve`), an MCP server, or anything
  else must never touch the core.
- **`owner` on everything.** Every RPC method takes an owner id
  (`owner(p)` in `src/rpc.rs`). Single-user today, multi-tenant-capable by
  construction — don't regress this.
- **Structured + semantic recall.** `crm.search` for exact lookups,
  `crm.vector_search` (`src/crm/agent.rs`) for semantic recall. Agent-facing
  APIs need both.
- **Phone-first daemon.** Termux + `adb forward` deployment (`DEPLOY.md`,
  `cos` on port 8790) stays the reference target. No cloud dependency for the
  core loop.

## Where we are (verified against the tree)

| Claim from review | Status |
|---|---|
| Bearer-token auth + Unix socket on the daemon | ✅ Done — `serve` (`src/bin/cos.rs:105`), token-guarded `route` (`:280`), commit `d7036bb` |
| Rich error enum in the lib | ✅ Done in-lib — `AgentError` (`src/error.rs:61`): `*NotFound`, `Conflict`, `Validation`, `BookingFull`, `AttendeeExists` |
| Machine-readable errors over RPC | ❌ Missing — flattened to `{"ok":false,"reason":"…"}` by `to_dict` (`src/error.rs:9`) and `json_err` (`src/bin/cos.rs:378`) |
| Idempotency keys on writes | ❌ Missing — no key param on `cal.book`, `crm.create_contact`, etc. |
| `describe`/schema introspection | ❌ Missing — ~35 methods hand-matched in `dispatch` (`src/rpc.rs:38`), no self-description |
| Batched calls | ❌ Missing — one method per `POST /` |
| Calendar sync (Google/CalDAV) | ❌ Missing — `cos` is an island |
| Webhooks / push channel | ❌ Missing — agents must poll (`cal.upcoming`) |
| Pipeline automation | ❌ Missing — `crm.advance_deal` is manual, no stage-change triggers |
| Ingestion (email/SMS → interactions) | ❌ Missing — `crm.log_interaction` is manual |
| Comms/inbox façade | ❌ Missing — outbound `aware.*` actions exist, but there is no unified inbound inbox (SMS/WhatsApp/notifications resolved to contacts) |
| Action log | ❌ Missing — the `audit_log` table (`src/store/libsql_store.rs:256`) is data-provenance for the dedup review queue (`table_name, record_id, source_type, confidence`), not a record of issued actions |
| Pending-approval state | ❌ Missing — no gate between "agent decided" and "message sent" anywhere in the tree |

## Phase 1 — Safety substrate (inbox, action log, approvals)

Goal: nothing the daemon can *send* on your behalf happens without a record
and, for high-risk actions, explicit approval. This gates everything below —
no two-way calendar sync, no push-triggered sends, no auto-ingestion pipelines
until this exists.

1. **Comms/inbox façade.** A unified inbound surface mirroring the outbound
   `aware.*` actions: inbound SMS/WhatsApp/notification events ingested once,
   resolved to contacts (`crm.resolve_by_phone` / `resolve_by_email`), and
   filed as `interactions` with direction=inbound. One ingestion path, not one
   per channel — the same "two façades, one store" shape as calendar/CRM.
2. **Action log.** Append-only, owner-scoped record of every issued action:
   who asked (rule / LLM / RPC caller), method + params, result, timestamp.
   This is distinct from the existing `audit_log` provenance table — it
   answers "what did the daemon *do*?" for debugging, accountability, and
   agent self-review.
3. **`pending_approval` state.** Risk tiers on outbound actions: reads and
   low-risk writes auto-approve; sends, deletes, and external side-effects go
   to `pending_approval` with `approve` / `reject` RPC methods and expiry.
   The daemon must be able to run fully-gated (every send waits) or
   tiered-gated from config.

## Phase 2 — Production-grade daemon (robustness)

Goal: the daemon is safe to expose beyond loopback and safe to retry against.

4. **Structured error codes over RPC.** Propagate the `AgentError` variant
   across the boundary: `{"ok":false,"code":"booking_not_found","message":"…"}`.
   The enum already exists — this is a serialization change in `to_dict` /
   `json_err` plus a documented code table. Agents must be able to distinguish
   not-found vs validation vs conflict without string-matching.
5. **Idempotency keys on writes.** Accept an optional `idempotency_key` on
   `cal.book`, `crm.create_contact`, `crm.create_deal`, `crm.log_interaction`;
   store keys with a TTL and return the original result on replay. Kills the
   double-create-on-retry class of bugs.
6. **Token hardening.** Per-method scopes (read vs write), token rotation
   without restart, and a documented bind policy (loopback by default, explicit
   opt-in otherwise). The bearer + socket foundation is there — finish it.

## Phase 3 — Agent ergonomics

Goal: cut round-trips and kill hand-maintained method lists.

7. **`rpc.describe`.** One method returning all methods + param shapes,
   generated from the same match arms as `dispatch` (or a registry `dispatch`
   is built from). Docs stop drifting from code by construction.
8. **Batched calls.** Accept an array of `{method, params}` in one `POST /`,
   execute in order against the same store handle, return ordered results.
   Turns "resolve contact → log interaction → check upcoming" from 3
   round-trips into 1. Pairs naturally with idempotency keys.

## Phase 4 — Real-world integration (highest leverage)

Goal: `cos` replaces a real scheduling tool instead of running beside one.

9. **Calendar sync (one-way first).** Import from Google Calendar / CalDAV into
   the store; two-way only after idempotency (Phase 2) lands, or every retry
   doubles bookings. This is the single highest-leverage feature.
10. **Push, not poll.** Webhook registration (`notify.on_booking`,
    `notify.on_deal_stage`) or a lightweight SSE/long-poll channel so agents
    learn about bookings without polling `cal.upcoming`.

## Phase 5 — CRM depth

Goal: the CRM fills itself in.

11. **Pipeline triggers.** Stage-change hooks: `crm.advance_deal` fires
    configured actions (auto-`log_interaction`, notifications). Small rule
    table, no workflow engine.
12. **Ingestion.** Email/SMS → `interactions` auto-population (explicit opt-in
    per source). Manual `log_interaction` becomes the fallback, not the flow.
    Builds on the Phase 1 inbox façade rather than duplicating it.

## Non-goals

- No workflow engine / DAG scheduler — triggers are a rule table, not a platform.
- No multi-node replication — one store per owner; sync is import/export, not consensus.
- No cloud-hosted control plane — the phone stays the reference deployment.

## How to read progress

Each phase lands as small PRs against this file's checkboxes. When in doubt,
order by: safety (Phase 1) → correctness (Phase 2) → round-trips (Phase 3) →
island-breaking (Phase 4) → automation (Phase 5).
