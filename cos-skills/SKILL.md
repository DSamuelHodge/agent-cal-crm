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
| `aware.sms` | owner, sender, smsBody | resolve → log interaction → notify name + context (or "Unknown sender") |
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
