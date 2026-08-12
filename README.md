# agent-cal-crm

CoS operating core: calendar engine + CRM, one libSQL store.

Calendar and CRM are two façades over the same store, so a contact can be
cross-referenced in both directions:

```rust
// CRM → calendar: stamp a booking attendee with the contact
let attendee = crm.attendee_for_contact("derrick", &contact.id).await?;

// calendar → CRM: any booking resolves back to the person
let booking = cal.get_booking("derrick", &booked.booking.id).await?;
let contact = crm.contact_for_booking("derrick", &booking).await?;
// or per-attendee (falls back to email match when unstamped)
let contact = crm.contact_for_attendee("derrick", &booking.attendees[0]).await?;
```

## `cos` — the daemon

`cos` wraps the façades behind a tiny HTTP/JSON server for the agent. The
library itself stays transport-free; the daemon lives in `src/bin/cos.rs`.

```bash
cargo run --bin cos -- seed  --db .agentcal/cos.db          # idempotent starter data
cargo run --bin cos -- serve --addr 127.0.0.1:8788 --db .agentcal/cos.db
```

Protocol — `POST /` with `{"method": ..., "params": {...}}`:

```bash
curl -X POST http://127.0.0.1:8788/ -d '{
  "method": "crm.resolve_by_phone",
  "params": {"owner": "derrick", "phone": "+16142600424"}
}'
# → {"ok":true,"result":{...contact with deals + interactions...}}
```

Methods (all take an `owner`): `ping`, `crm.summary`, `crm.resolve_by_phone`,
`crm.resolve_by_email`, `crm.contact_context`, `crm.search`, `crm.vector_search`,
`crm.create_company/get/list`, `crm.create_contact/get/list`,
`crm.create_deal/get/list/advance`, `crm.log_interaction`,
`crm.interactions_for_contact`, `crm.attendee_for_contact`,
`crm.contact_for_booking`, `cal.create_calendar_simple`, `cal.add_window`,
`cal.block`, `cal.create_link`, `cal.get_slots`, `cal.book`, `cal.get_booking`,
`cal.list_bookings`, `cal.upcoming`, `cal.cancel`, `cal.summary`.

Deployment target: Termux on the phone, with the agent reaching it via
`adb forward tcp:8788 tcp:8788`. See the feature reference for the full API.


