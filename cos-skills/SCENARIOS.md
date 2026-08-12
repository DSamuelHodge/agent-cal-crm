# CoS Situational-Awareness Scenario Suite

The CoS = **AutoTask (phone sensors/triggers) + cos daemon (calendar/CRM brain)**.

## The bridge (proven)

1. AutoTask trigger (SMS, incoming call, time, battery, ...) fires a profile.
2. Profile's `HTTP` action POSTs the raw event (`{{sender}}`, `{{number}}`,
   `{{smsBody}}`, ...) to `cos` on `127.0.0.1:8790` (loopback — same phone).
3. `cos` resolves/decides against calendar + CRM.
4. `cos` fires `POST /v1/events` back to AutoTask (`127.0.0.1:8788`) with the
   **informed** content in `payload.sender` + `payload.smsBody`.
5. The internal `cos-informed-notify` profile renders it as a high-priority
   notification (and/or SPEAK, SEND_SMS, DND — whatever the scenario needs).

Template allowlist on the AutoTask side is fixed
(`{{sender}} {{number}} {{smsBody}} {{ssid}} {{levelPercent}} ...`), so cos
*re-uses* those keys to carry its informed answers back to the phone UI.
No engine changes required — verified end-to-end on-device.

## Scenarios

| # | Scenario | Trigger | cos work | Phone effect |
|---|----------|---------|----------|--------------|
| 1 | SMS-aware triage | SMS | `resolve_by_phone` → context; if unknown, create contact + log | High-pri notification: "Derrick Hodge — VIP, CEO, 1 open deal" + the message |
| 2 | Incoming-call context flash | INCOMING_CALL | `resolve_by_phone` → context | Flash/notify "Derrick Hodge calling — VIP" |
| 3 | Meeting prep nudge | TIME (manual for test) | `upcoming` + `contact_for_booking` | "Next: Intro call with Derrick Hodge 14:00 — Hodge Luke, deal $250k" |
| 4 | New-lead capture | MANUAL | `create_company`+`create_contact`+`create_deal` | Confirmation notification + CRM row |
| 5 | Morning briefing | TIME (or MANUAL) | `upcoming` + `summary` + open deals | Full briefing notification |
| 6 | Deal nudge | MANUAL / TIME | `list_deals` open + stale | "3 open deals — Enterprise AI scaling ($250k) untouched 14d" |

## Profile inventory (on-device, 2026-08-12)

- `cos-informed-notify` (MANUAL, internal) — renders cos payload as notification.
- `cos-aware-sms` (SMS) — POSTs the raw SMS to `aware.sms`.
- `cos-aware-call` (INCOMING_CALL) — POSTs the caller to `aware.call`.
- Existing 25 profiles cover raw device reactions; the new ones make them
  *informed* by calling the cos brain.

## Verified outcomes (2026-08-12, all on-device)

| Scenario | Trigger path | Result |
|----------|--------------|--------|
| 1 SMS-aware triage | SMS event → `cos-aware-sms` → `aware.sms` | **SUCCESS**: sender resolved, interaction logged, notification "Derrick Hodge: ..." on phone |
| 2 Incoming-call flash | `aware.call` direct | "Incoming call from Derrick Hodge · VIP — President and CEO" |
| 3 New-lead capture | `aware.capture` | Ava Chen → CRM (company, contact, $50k deal) + "New lead: Ava Chen" |
| 4 Meeting prep | `aware.meeting` | "Next: 30-min at 09:00 — with Derrick Hodge — Hodge Luke · 1 open deal(s)" |
| 5 Morning briefing | `aware.briefing` | "Good morning — 1 meeting(s), 2 contact(s), 1 open deal(s)" |
| 6 Deal nudge | `aware.deals` | "1 open deal(s) in flight — Enterprise AI scaling engagement ($250k) — Lead: no next step set" |

## Two infrastructure fixes required

1. **Cleartext**: AutoTask blocked plain HTTP to 127.0.0.1 (Android 9+
   policy). Fixed by rebuilding the APK with
   `android:usesCleartextTraffic="true"` in the manifest.
   → The installed server app signature differs from the old one; a fresh
     install was required (profiles re-imported from backup — see
     `termux/profiles/profiles-live.json`).
2. **Deadlock**: AutoTask's HTTP action blocks on the response, so cos
   POSTing back to `/v1/events` synchronously deadlocked (10s timeout).
   Fixed: `notify()` fires the informed event from a background thread.

Reinstall reminder: after replacing the AutoTask APK, re-grant
`POST_NOTIFICATIONS` (and SMS/phone) runtime permissions, and re-import
profiles via `POST /v1/profiles` from the backup.
