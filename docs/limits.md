# Deterministic send budgets (`limit_usage`)

Phase 2 limits ledger. Every outgoing message on a **counted channel** decrements
a per-owner, per-day budget. Enforcement is deterministic and durable: the
counter lives in the libSQL `limit_usage` table (or an in-memory map for the
ephemeral store) and **survives process restarts**.

## Budgets

| channel | RPC method    | budget / UTC day |
|---------|---------------|------------------|
| `sms`   | `sms.send`    | 100              |
| `email` | `email.send`  | 200              |

Other channels (`call`, `whatsapp`, `push`) are **not counted** and have no
budget.

## Day boundary

A "day" is a **UTC calendar day**, `YYYY-MM-DD` (zero-padded). Counters reset at
midnight UTC, not at the owner's local timezone. `limit.query` defaults `day` to
today UTC.

## Enforcement

In the RPC dispatch wrapper, a counted send:

1. Validates its params first — a **validation failure consumes no budget**.
2. Checks `usage(owner, channel, day)`; if it is `>=` budget, the send is
   denied with [`AgentError::LimitExceeded`] and error code `limit_exceeded`.
3. On success, records one unit of usage and returns `{ ok, channel, day, used, budget }`.

A send is denied at *exactly* budget (the `(budget + 1)`-th attempt after
`budget` successes).

## Near-exhaustion

`limit.query` reports `near_exhaustion: true` when usage reaches **80%** of
budget. Integer-only math (`used * 5 >= budget * 4`), so at exactly 80% it flips
`true`; one below is `false`.

## `limit.query`

```json
{ "owner": "…", "channel": "sms", "day": "2026-09-08" }   // day optional
```

```json
{ "channel": "sms", "day": "2026-09-08", "used": 80, "budget": 100, "near_exhaustion": true }
```

## `error.catalog` entry

`limit_exceeded` — category `user`, meaning "daily send budget exhausted",
likely cause "too many sends today; retry tomorrow".