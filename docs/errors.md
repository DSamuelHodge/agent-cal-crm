# Error catalog

Central error taxonomy (Phase 2.1). Single source of truth is
`error_info()` in `src/error.rs`; `error_code()` in `src/actions.rs` is a
thin accessor over `error_info(e).code`, and the `error.catalog` RPC serves
`error_catalog()`.

Categories (exactly three):

- `user` — bad input or missing resource. Caller can fix and retry.
- `approval` — the approval gate refused the call. Drive the
  `approval.request` → `approval.approve` → retry flow first.
- `internal` — the store failed. Retry, then inspect the store.

## Codes

| code | category | meaning | likely cause |
|---|---|---|---|
| `calendar_not_found` | user | The requested calendar does not exist. | Wrong calendar id, or the calendar was never created for this owner. |
| `calendar_already_exists` | user | A calendar with that identifier already exists. | Retried create with the same id, or a name/id collision. |
| `link_not_found` | user | The requested booking link does not exist. | Wrong link id, or the link was deleted. |
| `booking_not_found` | user | The requested booking does not exist. | Wrong booking id, the booking was cancelled/purged, or the wrong owner. |
| `conflict` | user | The request conflicts with existing state. | Overlapping booking/window, or a concurrent write under a reject policy. |
| `validation` | user | A request parameter failed validation. | Missing or malformed param (bad datetime, empty string, wrong type). |
| `attendee_exists` | user | That attendee is already on this booking. | Retried add with the same email, or a duplicated attendee list. |
| `booking_full` | user | The booking reached its attendee limit. | Event at capacity; raise the limit or pick another slot. |
| `contact_not_found` | user | The requested contact does not exist. | Wrong contact id, or the contact belongs to another owner. |
| `company_not_found` | user | The requested company does not exist. | Wrong company id, or the company belongs to another owner. |
| `deal_not_found` | user | The requested deal does not exist. | Wrong deal id, or the deal belongs to another owner. |
| `crm_validation` | user | A CRM field failed validation. | Missing required CRM field, unknown stage, or invalid enum value. |
| `approval_required` | approval | This action needs approval before it can run. | High-risk method called without a valid approval_id; request approval first. |
| `approval_not_found` | approval | The referenced approval does not exist. | Wrong approval id, pruned record, or the wrong owner. |
| `approval_not_approved` | approval | The approval is not in the approved state. | Still pending, rejected, or expired; approve it with matching params, then retry. |
| `limit_exceeded` | user | The daily send budget for this channel is exhausted. | Too many sends today; low-risk agent traffic is capped per UTC day — retry tomorrow. |
| `channel_disabled` | internal | The channel is kill-switched off. | Re-enable the channel or use another channel for this send. |
| `store_error` | internal | The persistence layer failed. | Disk/IO failure, corrupt row, or migration issue; retry, then inspect the store. |

## RPC

`error.catalog` takes no params (`owner` accepted but ignored) and returns
a JSON array of `{code, category, meaning, likely_cause}` — one entry per
variant, in enum-declaration order.

## Coordination contract

Sibling streams add one variant each (`LimitExceeded` → `limit_exceeded`,
`ChannelDisabled` → `channel_disabled`). Both `error_info()` and the
`error_catalog()` guard `match` are exhaustive over `&AgentError`, so those
PRs fail to compile until they extend the taxonomy here. Do not add those
variants in this stream.
