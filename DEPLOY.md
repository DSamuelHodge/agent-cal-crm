# Deploying `cos` to the G63 via Termux

The CoS core (calendar + CRM) runs on the phone as a Termux binary; the
Mac-side agent reaches it over `adb forward`.

## Prerequisites on the phone

- Termux + `com.termux.boot` installed.
- Rust installed in Termux: `pkg install rust` (large; one-time).
- USB debugging connected, or wireless adb.

## Build natively on the phone (bionic)

```bash
adb push agent-cal-crm.tar.gz /data/local/tmp/
adb shell "run-as com.termux tar xzf /data/local/tmp/agent-cal-crm.tar.gz \
    -C /data/data/com.termux/files/home/projects"
adb shell "run-as com.termux sh -c 'export HOME=/data/data/com.termux/files/home; \
    cd \$HOME/projects && cargo build --bin cos --release'"
```

> Result (verified 2026-08-12): libsql 0.9.30 + vector extensions compile
> cleanly against bionic; `target/release/cos` is an 11MB arm64 ELF linked by
> `/system/bin/linker64` (NDK r29). Build takes ~8m on first run (cold cache).

## Run

```bash
# seed the DB once (idempotent)
adb shell "run-as com.termux sh -c 'cd /data/data/com.termux/files/home/projects && \
    ./target/release/cos seed --db \$HOME/cos.db'"

# start the daemon (loopback only)
adb shell "run-as com.termux sh -c 'nohup \
    /data/data/com.termux/files/home/projects/target/release/cos \
    serve --addr 127.0.0.1:8790 --db \$HOME/cos.db > \$HOME/serve.log 2>&1 &'"
```

> Port 8788 is used by the AutoTask server — use **8790** for `cos`.

## Tunnel + point the agent at it

```bash
adb forward tcp:8790 tcp:8790
curl -s http://127.0.0.1:8790/ping          # {"ok":true,...}
```

The agent (LLM on the Mac, or any tool) calls `POST http://127.0.0.1:8790/`
with `{"method": "...", "params": {"owner": "derrick", ...}}`.

## Persistence

- DB lives at `/data/data/com.termux/files/home/cos.db` — survives daemon
  restarts (proven). 
- Back up by pulling it: `adb pull /data/local/tmp` first
  (`run-as` cannot read Termux home from a normal shell, so either run the
  copy from inside Termux or use `run-as com.termux`).

## Auto-start (Termux:Boot) — DONE

The boot script lives in the repo at `termux/20-cos-daemon`. It is installed
on the phone at `~/.termux/boot/20-cos-daemon` and runs on every boot:

```sh
# already installed; reinstall after edits:
adb push termux/20-cos-daemon /data/local/tmp/
adb shell "run-as com.termux cp /data/local/tmp/20-cos-daemon \
    /data/data/com.termux/files/home/.termux/boot/20-cos-daemon"
adb shell "run-as com.termux chmod 755 \
    /data/data/com.termux/files/home/.termux/boot/20-cos-daemon"
```

Behavior (verified 2026-08-12):
- Waits 20s after boot for the system to settle.
- Idempotently seeds the DB, then starts `cos serve` on `127.0.0.1:8790`,
  detached via `setsid nohup` so it survives the boot script's exit
  (daemon has PPID 1).
- Health-checks by `curl http://127.0.0.1:8790/ping`, retrying for ~15s;
  exits 0 only once the daemon actually responds.
- Idempotent: re-running with the daemon already up logs `OK already running`.
- Logs to `~/.termux/boot-logs/cos-daemon.log`.

On the Mac, after each USB reconnect:
```bash
adb forward tcp:8790 tcp:8790   # port forwarding is per-device-connection
```

## Full CoS RPC methods

`ping`, `crm.summary`, `crm.resolve_by_phone`, `crm.resolve_by_email`,
`crm.contact_context`, `crm.search`, `crm.vector_search`,
`crm.create_company/get/list`, `crm.create_contact/get/list`,
`crm.create_deal/get/list/advance`, `crm.log_interaction`,
`crm.interactions_for_contact`, `crm.attendee_for_contact`,
`crm.contact_for_booking`, `cal.create_calendar_simple`, `cal.add_window`,
`cal.block`, `cal.create_link`, `cal.get_slots`, `cal.book`,
`cal.get_booking`, `cal.list_bookings`, `cal.upcoming`, `cal.cancel`,
`cal.summary`.
