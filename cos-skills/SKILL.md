# CoS Situational-Awareness Skill

Trigger when: wiring phone events (SMS, calls, time, battery) to the CoS
brain (calendar + CRM) so the device reacts *informed* instead of dumb.

## Architecture

```
AutoTask trigger (SMS / INCOMING_CALL / TIME / ...)
  └─ profile action: HTTP POST http://127.0.0.1:8790/   (cos daemon)
       {"method":"aware.sms","params":{...}}
  └─ cos resolves against CRM/calendar, logs, decides
  └─ cos fires POST /v1/events → AutoTask cos-informed-notify (background)
       payload.sender = title, payload.smsBody = text
  └─ AutoTask renders high-priority notification
```

## Requirements (all proven on-device 2026-08-12)

1. `cos` daemon on the phone at `127.0.0.1:8790` (see DEPLOY.md), with the
   `aware.*` methods (rebuilt binary in `src/bin/aware.rs`).
2. AutoTask APK rebuilt with `android:usesCleartextTraffic="true"`
   (Android 9+ blocks plain HTTP to loopback otherwise). Fix committed in
   `AutoTask-360/app/src/main/AndroidManifest.xml`.
3. `cos-informed-notify` profile on-device (MANUAL; renders `{{sender}}` +
   `{{smsBody}}`).
4. Runtime permission granted: `POST_NOTIFICATIONS` (and SMS/phone perms).

## The deadlock pitfall (critical)

AutoTask's HTTP action blocks on the response. If `aware.sms` POSTs back to
AutoTask's `/v1/events` synchronously, both block until AutoTask's 10s
timeout. Fix: cos fires the notification from a **background thread** and
returns immediately. Already implemented in `notify()`.

## Available `aware.*` methods

| Method | Params | Behavior |
|--------|--------|----------|
| `aware.sms` | owner, sender, smsBody | resolve (incl. alt phones) → log interaction → notify name + context; cross-references names in the body against the device contacts app (`termux-contact-list`) and appends `· mentions <name> (<number>)` |
| `aware.sync_contacts` | owner | one-way sync: device address book → CRM (idempotent; creates any device contact with a number not already in the CRM, logs a "Synced from device address book" NOTE) |

## Address-book sync (verified 2026-08-12)

`aware.sync_contacts` mirrors the phone's contacts into the CRM so everyone
is already known before they text/call. Ran on the G63: 3 created
(Ada Lovelace, John Lombela, Mariam El Mezabi Wife), 3 skipped (already
known); re-run is a no-op. The boot script (`termux/20-cos-daemon`) runs it
automatically after startup.

## Alt-phone + device-contact enrichment (verified 2026-08-12)

- `Contact` supports multiple numbers via `with_alt_phone` (stored in
  `metadata.alt_phones`); `resolve_by_phone` matches every number.
  Example: Derrick Hodge's second line `+16144074920` resolves to him.
- `aware.sms` runs `termux-contact-list` (absolute path
  `/data/data/com.termux/files/usr/bin/termux-contact-list` — the daemon's
  PATH is the Android default, not Termux's) and matches contact names
  mentioned in the SMS body. Result verified on-device:
  > Derrick Hodge: "Please call Shaun Ford and follow-up on when we will meet
  > with Curtis Jewell this week" · VIP · 1 open deal(s) · President and CEO
  > · mentions Curtis Jewell (614-519-1846), Shaun Ford (+16144460190)
- Prereq: Termux needs `android.permission.READ_CONTACTS` granted (already
  granted on the G63).

## Auto-capture: the CoS builds its own relationship graph

`aware.sms` doesn't just mention device contacts — anyone named in the SMS who
is **not yet in the CRM** is auto-added as a contact (first/last split from the
display name, phone attached), and a `NOTE` interaction is logged
("Auto-captured from SMS mention: <body>"). Verified on-device: an SMS naming
Shaun Ford + Curtis Jewell added both to the CRM with their device numbers,
and the notification marked them `✓new`.
| `aware.call` | owner, number | resolve → notify "Name calling — VIP/title" |
| `aware.capture` | owner, first_name, last_name, company?, phone?, email?, amount? | create company/contact/deal → confirm |
| `aware.meeting` | owner | next booking + attendee CRM context → prep nudge |
| `aware.briefing` | owner | today's meetings + CRM stats → morning briefing |
| `aware.deals` | owner | open deals + next actions → deal nudge |

## Verification snippet (Mac side)

```bash
adb forward tcp:8788 tcp:8788; adb forward tcp:8790 tcp:8790
curl -X POST http://127.0.0.1:8788/v1/events -d '{
  "triggerType":"SMS",
  "payload":{"sender":"+16142600424","smsBody":"hi from the test"}
}'   # expect: cos-aware-sms SUCCESS
adb shell dumpsys notification --noredact | grep -A1 'android.title='  # Derrick Hodge
curl -X POST http://127.0.0.1:8790/ -d '{"method":"crm.interactions_for_contact",
  "params":{"owner":"derrick","contact_id":"<id>"}}'   # SMS INBOUND logged
```

## Profile inventory (on-device, 28)

Backed up to `termux/profiles/profiles-live.json`. Key ones:
`cos-aware-sms` (SMS→aware.sms), `cos-aware-call` (INCOMING_CALL→aware.call),
`cos-informed-notify` (internal renderer), plus the original device-reaction
set (battery, focus, night mode, wifi welcome, etc.).
