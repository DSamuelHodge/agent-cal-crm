//! CoS situational-awareness bridge (binary-only; the lib stays HTTP-free).
//!
//! Each `aware.*` RPC method resolves the incoming signal against the cos
//! brain (calendar + CRM), builds an *informed* human-readable summary, and
//! fires it back to AutoTask as a high-priority notification by posting to
//! AutoTask's `POST /v1/events` with the internal `cos-informed-notify`
//! profile. The AutoTask template allowlist is fixed, so cos re-uses the
//! `sender` + `smsBody` payload keys to carry its informed answer.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};

use agentcal::crm::{InteractionDirection, InteractionInput, InteractionKind};
use agentcal::error::Result;
use agentcal::{AgentCal, AgentCrm};

/// AutoTask loopback server (same phone). Overridable for local testing.
pub fn autotask_url() -> String {
    std::env::var("AUTOTASK_URL").unwrap_or_else(|_| "http://127.0.0.1:8788".to_string())
}

/// POST a JSON body to the AutoTask server and return the response body.
fn autotask_post(path: &str, body: &serde_json::Value) -> std::io::Result<String> {
    let base = autotask_url();
    let url = format!("{base}{path}");
    // parse http://host:port/path
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| std::io::Error::other("only http:// supported"))?;
    let (host_port, req_path) = match rest.split_once('/') {
        Some((hp, p)) => (hp, format!("/{p}")),
        None => (rest, "/".to_string()),
    };
    let (host, port) = match host_port.split_once(':') {
        Some((h, p)) => (h.to_string(), p.parse::<u16>().unwrap_or(80)),
        None => (host_port.to_string(), 80),
    };
    let body_str = body.to_string();
    let request = format!(
        "POST {req_path} HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body_str.len(),
        body_str
    );
    let mut stream = TcpStream::connect((host.as_str(), port))?;
    stream.write_all(request.as_bytes())?;
    stream.shutdown(Shutdown::Write)?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf)?;
    String::from_utf8_lossy(&buf).to_string().into_io_result()
}

trait IoResultExt<T> {
    fn into_io_result(self) -> std::io::Result<T>;
}
impl<T> IoResultExt<T> for T {
    fn into_io_result(self) -> std::io::Result<T> {
        Ok(self)
    }
}

/// Minimal HTTPS GET (via the device's `curl`) returning the response body.
fn http_get(url: &str) -> std::io::Result<String> {
    const TOOLS: [&str; 2] = [
        "/data/data/com.termux/files/usr/bin/curl", // phone (Termux)
        "/usr/bin/curl",                            // Mac / other
    ];
    let mut last_err = std::io::Error::other("curl not found");
    for tool in TOOLS {
        match std::process::Command::new(tool)
            .arg("-s")
            .arg("-m")
            .arg("10")
            .arg(url)
            .output()
        {
            Ok(out) if out.status.success() => {
                return Ok(String::from_utf8_lossy(&out.stdout).to_string());
            }
            Ok(out) => {
                last_err = std::io::Error::other(String::from_utf8_lossy(&out.stderr).to_string())
            }
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

/// Fire an informed notification through AutoTask's `cos-informed-notify`.
///
/// Runs on a background thread: AutoTask's HTTP action is blocking on the
/// response to the very call that triggered us, so if we POST back to
/// AutoTask synchronously here we deadlock until its 10s timeout. Firing on
/// a detached thread returns immediately and lets AutoTask process the
/// notification once the HTTP action completes.
pub fn notify(title: &str, text: &str) {
    let t = title.to_string();
    let x = text.to_string();
    std::thread::spawn(move || {
        let evt = serde_json::json!({
            "triggerType": "MANUAL",
            "profileId": "cos-informed-notify",
            "payload": { "sender": t, "smsBody": x },
        });
        let _ = autotask_post("/v1/events", &evt);
    });
}

// ── Scenario handlers ─────────────────────────────────────────────────────────

/// A contact from the device's address book (`termux-contact-list`).
struct DeviceContact {
    name: String,
    number: String,
}

/// Read the phone's contacts via `termux-contact-list` (Termux has
/// READ_CONTACTS granted). Uses the absolute path because the daemon's PATH
/// (started via `setsid` from a bare shell) is the Android default, not
/// Termux's. Returns `None` if the tool is unavailable.
fn device_contacts() -> Option<Vec<DeviceContact>> {
    const TOOL: &str = "/data/data/com.termux/files/usr/bin/termux-contact-list";
    let out = std::process::Command::new(TOOL)
        .env("HOME", "/data/data/com.termux/files/home")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some(
        v.as_array()?
            .iter()
            .filter_map(|c| {
                let name = c.get("name")?.as_str()?.to_string();
                let number = c
                    .get("number")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                if name.is_empty() {
                    None
                } else {
                    Some(DeviceContact { name, number })
                }
            })
            .collect(),
    )
}

/// Find device contacts whose name appears in the SMS body (case-insensitive).
fn mentioned_device_contacts(body: &str) -> Vec<DeviceContact> {
    let lower = body.to_lowercase();
    device_contacts()
        .unwrap_or_default()
        .into_iter()
        .filter(|c| {
            let n = c.name.to_lowercase();
            !n.is_empty() && lower.contains(&n)
        })
        .collect()
}

/// Split a display name like "Shaun Ford" into (first, last).
fn split_name(name: &str) -> (String, String) {
    let mut parts = name.split_whitespace();
    let first = parts.next().unwrap_or("").to_string();
    let last = parts.collect::<Vec<_>>().join(" ");
    (first, last)
}

/// Enrich context with device contacts mentioned in the message, cross-referenced
/// against the CRM. Anyone named but not yet in the CRM is **auto-captured**
/// as a contact (the CoS builds its own relationship graph from conversation).
///
/// Returns `(display_string, newly_created_contact_ids)`. Display looks like
/// ` · mentions Shaun Ford (+16144460190) ✓new, Curtis Jewell (614-519-1846)`.
async fn mention_context(crm: &AgentCrm, owner: &str, body: &str) -> (String, Vec<String>) {
    let mentioned = mentioned_device_contacts(body);
    if mentioned.is_empty() {
        return (String::new(), Vec::new());
    }
    let mut parts: Vec<String> = Vec::new();
    let mut captured: Vec<String> = Vec::new();
    for c in mentioned {
        let in_crm = crm
            .resolve_by_phone(owner, &c.number)
            .await
            .ok()
            .flatten()
            .is_some();
        if !in_crm && !c.number.is_empty() {
            let (first, last) = split_name(&c.name);
            if let Ok(contact) = crm
                .create_contact(owner, &first, &last)
                .await
                .map(|contact| contact.with_phone(&c.number))
            {
                let _ = crm
                    .log_interaction(
                        owner,
                        InteractionInput::new(&contact.id, InteractionKind::Note)
                            .with_summary(format!("Auto-captured from SMS mention: {body}")),
                    )
                    .await;
                crm.update_contact(&contact).await.ok();
                captured.push(contact.id);
            }
        }
        parts.push(format!(
            "{}{}",
            c.name,
            if c.number.is_empty() {
                String::new()
            } else if in_crm {
                format!(" ({}) ✓", c.number)
            } else {
                format!(" ({}) ✓new", c.number)
            }
        ));
    }
    (format!(" · mentions {}", parts.join(", ")), captured)
}

/// Scenario 1 — SMS-aware triage: resolve sender, log the interaction, and
/// surface context (or flag an unknown number for capture).
pub async fn aware_sms(crm: &AgentCrm, p: &serde_json::Value) -> Result<serde_json::Value> {
    let owner = strp(p, "owner")?;
    let sender = strp(p, "sender")?;
    let body = strp(p, "smsBody").unwrap_or("");
    let (mentions, captured) = mention_context(crm, owner, body).await;

    match crm.resolve_by_phone(owner, sender).await? {
        Some(c) => {
            // Log the inbound interaction against the known contact.
            let _ = crm
                .log_interaction(
                    owner,
                    InteractionInput::new(&c.id, InteractionKind::Sms)
                        .with_direction(InteractionDirection::Inbound)
                        .with_summary(format!("SMS: {body}")),
                )
                .await;
            let ctx = crm.contact_context(owner, &c.id).await.unwrap_or_default();
            let deal_str = ctx
                .get("deals")
                .and_then(|d| d.as_array())
                .map(|d| format!(" · {} open deal(s)", d.len()))
                .unwrap_or_default();
            let vip = if c.is_vip { " · VIP" } else { "" };
            let title = c.display_name();
            let text = format!(
                "{}: \"{}\"\n{}{} · {}{}",
                title, body, vip, deal_str, c.title, mentions
            );
            notify(&title, &text);
            Ok(serde_json::json!({
                "known": true,
                "contact_id": c.id,
                "title": title,
                "text": text,
                "captured_contacts": captured,
            }))
        }
        None => {
            // Unknown number — flag for capture (no contact created here).
            let title = format!("Unknown sender ({sender})");
            let text = format!("Not in CRM.\n\"{body}\"{mentions}");
            notify(&title, &text);
            Ok(serde_json::json!({
                "known": false,
                "sender": sender,
                "text": format!("{sender} not in CRM"),
                "captured_contacts": captured,
            }))
        }
    }
}

/// Resolve a contact by display name (case-insensitive). WhatsApp notifications
/// carry the sender's display name (not their number), so the triage hook works
/// on the name instead of the phone number.
async fn resolve_by_name(
    crm: &AgentCrm,
    owner: &str,
    name: &str,
) -> Result<Option<agentcal::crm::Contact>> {
    let needle = name.trim().to_lowercase();
    if needle.is_empty() {
        return Ok(None);
    }
    let contacts = crm.list_contacts(owner).await?;
    // Exact display-name match first.
    if let Some(c) = contacts
        .iter()
        .find(|c| c.display_name().to_lowercase() == needle)
    {
        return Ok(Some(c.clone()));
    }
    // WhatsApp titles are often a bare first name (or nickname); match the
    // first token against the start of a CRM display name.
    let first_token = needle.split_whitespace().next().unwrap_or("");
    if let Some(c) = contacts.iter().find(|c| {
        !first_token.is_empty()
            && c.display_name().to_lowercase().starts_with(first_token)
    }) {
        return Ok(Some(c.clone()));
    }
    // Last resort: substring containment (e.g. "Mariam" vs "Mariam El Mezabi Wife").
    Ok(contacts
        .iter()
        .find(|c| c.display_name().to_lowercase().contains(&needle))
        .cloned())
}

/// Scenario 2 — WhatsApp message triage. WhatsApp notifications surface the
/// sender's display name in the notification title; the body is the message.
/// Mirrors `aware_sms` but resolves by name and logs the interaction as a
/// message (not SMS).
pub async fn aware_whatsapp(crm: &AgentCrm, p: &serde_json::Value) -> Result<serde_json::Value> {
    let owner = strp(p, "owner")?;
    let sender = strp(p, "sender").unwrap_or("Unknown");
    let body = strp(p, "text").unwrap_or("");
    let (mentions, captured) = mention_context(crm, owner, body).await;

    match resolve_by_name(crm, owner, sender).await? {
        Some(c) => {
            let _ = crm
                .log_interaction(
                    owner,
                    InteractionInput::new(&c.id, InteractionKind::Message)
                        .with_direction(InteractionDirection::Inbound)
                        .with_summary(format!("WhatsApp: {body}")),
                )
                .await;
            let ctx = crm.contact_context(owner, &c.id).await.unwrap_or_default();
            let deal_str = ctx
                .get("deals")
                .and_then(|d| d.as_array())
                .map(|d| format!(" · {} open deal(s)", d.len()))
                .unwrap_or_default();
            let vip = if c.is_vip { " · VIP" } else { "" };
            let title = c.display_name();
            let text = format!(
                "WA: \"{}\"\n{}{} · {}{}",
                body, vip, deal_str, c.title, mentions
            );
            notify(&title, &text);
            Ok(serde_json::json!({
                "known": true,
                "contact_id": c.id,
                "title": title,
                "text": text,
                "captured_contacts": captured,
            }))
        }
        None => {
            let title = format!("Unknown sender ({sender})");
            let text = format!("Not in CRM.\n\"{body}\"{mentions}");
            notify(&title, &text);
            Ok(serde_json::json!({
                "known": false,
                "sender": sender,
                "text": format!("{sender} not in CRM"),
                "captured_contacts": captured,
            }))
        }
    }
}

/// Scenario 3 — incoming-call context flash.
pub async fn aware_call(crm: &AgentCrm, p: &serde_json::Value) -> Result<serde_json::Value> {
    let owner = strp(p, "owner")?;
    let number = strp(p, "number")?;
    match crm.resolve_by_phone(owner, number).await? {
        Some(c) => {
            let vip = if c.is_vip { " · VIP" } else { "" };
            let title = c.display_name();
            let text = format!("Incoming call from {}{} — {}", title, vip, c.title);
            notify(&title, &text);
            Ok(serde_json::json!({ "known": true, "contact_id": c.id, "text": text }))
        }
        None => {
            notify("Incoming call", &format!("Unknown number {number}"));
            Ok(serde_json::json!({ "known": false }))
        }
    }
}

/// Scenario 3 — capture a new lead into the CRM.
pub async fn aware_capture(crm: &AgentCrm, p: &serde_json::Value) -> Result<serde_json::Value> {
    let owner = strp(p, "owner")?;
    let first = strp(p, "first_name")?;
    let last = strp(p, "last_name")?;
    let company_name = strp(p, "company").unwrap_or("");
    let phone = strp(p, "phone").unwrap_or("");

    // Resolve company by name, else create it.
    let company_id = if company_name.is_empty() {
        None
    } else {
        let existing = crm
            .list_companies(owner)
            .await?
            .into_iter()
            .find(|c| c.name == company_name);
        let cid = match existing {
            Some(c) => c.id,
            None => crm.create_company(owner, company_name, "").await?.id,
        };
        Some(cid)
    };

    let mut contact = crm.create_contact(owner, first, last).await?;
    if let Some(cid) = &company_id {
        contact = contact.with_company(cid);
    }
    if !phone.is_empty() {
        contact = contact.with_phone(phone);
    }
    if let Ok(email) = strp(p, "email") {
        if !email.is_empty() {
            contact = contact.with_email(email);
        }
    }
    crm.update_contact(&contact).await?;

    // Create a starter deal if an amount was given.
    let deal_id = match (company_id.as_ref(), f64p(p, "amount")) {
        (Some(cid), Some(amount)) => {
            let name = format!("{first} {last} — new lead");
            Some(crm.create_deal(owner, cid, &name, amount).await?.id)
        }
        _ => None,
    };

    let title = format!("New lead: {first} {last}");
    let mut text = "Captured into CRM".to_string();
    if !company_name.is_empty() {
        text.push_str(&format!(" at {company_name}"));
    }
    if deal_id.is_some() {
        text.push_str(" with starter deal");
    }
    text.push('.');
    notify(&title, &text);
    Ok(serde_json::json!({
        "contact_id": contact.id,
        "company_id": company_id,
        "deal_id": deal_id,
        "text": text,
    }))
}

/// Sync the phone's address book into the CRM. Every device contact with a
/// number that isn't already in the CRM (matched by any number, primary or
/// alt) is created. Returns created + skipped counts.
pub async fn sync_contacts(crm: &AgentCrm, p: &serde_json::Value) -> Result<serde_json::Value> {
    let owner = strp(p, "owner")?;
    let contacts = device_contacts().unwrap_or_default();
    let mut created: Vec<String> = Vec::new();
    let mut skipped = 0usize;

    for c in contacts {
        if c.number.is_empty() {
            skipped += 1;
            continue;
        }
        // Already known by any number?
        let known = crm
            .resolve_by_phone(owner, &c.number)
            .await
            .ok()
            .flatten()
            .is_some();
        if known {
            skipped += 1;
            continue;
        }
        let (first, last) = split_name(&c.name);
        if let Ok(contact) = crm
            .create_contact(owner, &first, &last)
            .await
            .map(|contact| contact.with_phone(&c.number))
        {
            let _ = crm
                .log_interaction(
                    owner,
                    InteractionInput::new(&contact.id, InteractionKind::Note)
                        .with_summary("Synced from device address book"),
                )
                .await;
            crm.update_contact(&contact).await.ok();
            created.push(contact.id);
        }
    }

    let title = "Contacts sync";
    let text = format!("{} created, {} already known", created.len(), skipped);
    notify(title, &text);
    Ok(serde_json::json!({
        "created": created.len(),
        "skipped": skipped,
        "created_ids": created,
    }))
}

/// Get the device's current GPS position via `termux-location`.
fn current_position() -> Option<(f64, f64)> {
    const TOOL: &str = "/data/data/com.termux/files/usr/bin/termux-location";
    let out = std::process::Command::new(TOOL)
        .env("HOME", "/data/data/com.termux/files/home")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let lat = v.get("latitude")?.as_f64()?;
    let lon = v.get("longitude")?.as_f64()?;
    Some((lat, lon))
}

/// Geocode an address string to (lat, lon) via Nominatim.
fn geocode(address: &str) -> Option<(f64, f64)> {
    let q = urlencode(address);
    let url = format!("https://nominatim.openstreetmap.org/search?format=json&limit=1&q={q}");
    let text = http_get(&url).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let first = v.as_array()?.first()?;
    let lat = first.get("lat")?.as_str()?.parse::<f64>().ok()?;
    let lon = first.get("lon")?.as_str()?.parse::<f64>().ok()?;
    Some((lat, lon))
}

/// Driving time (seconds) + distance (m) from one (lat,lon) to another via OSRM.
fn osrm_travel(o_lat: f64, o_lon: f64, d_lat: f64, d_lon: f64) -> Option<(f64, f64)> {
    let url = format!(
        "https://router.project-osrm.org/route/v1/driving/{o_lon:.6},{o_lat:.6};{d_lon:.6},{d_lat:.6}?overview=false"
    );
    let text = http_get(&url).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let route = v.get("routes")?.as_array()?.first()?;
    let duration = route.get("duration")?.as_f64()?;
    let distance = route.get("distance")?.as_f64()?;
    Some((duration, distance))
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Scenario 7 — travel time + maps. Given a destination address, compute the
/// drive time from the device's GPS and fire an informed notification.
pub async fn aware_travel(_crm: &AgentCrm, p: &serde_json::Value) -> Result<serde_json::Value> {
    let dest = strp(p, "destination")?;

    let origin = current_position();
    let dest_coords = geocode(dest);
    match (origin, dest_coords) {
        (Some((olat, olon)), Some((dlat, dlon))) => match osrm_travel(olat, olon, dlat, dlon) {
            Some((seconds, meters)) => {
                let mins = (seconds / 60.0).round();
                let km = meters / 1000.0;
                let title = "Travel time";
                let text = format!("{:.0} min · {km:.1} km → {dest}", mins);
                notify(title, &text);
                Ok(serde_json::json!({
                    "origin": [olat, olon],
                    "destination": [dlat, dlon],
                    "duration_minutes": mins,
                    "distance_km": km,
                    "maps_intent": format!("google.navigation:q={dlat:.6},{dlon:.6}"),
                    "text": text,
                }))
            }
            None => {
                notify("Travel time", &format!("Could not route to {dest}"));
                Ok(serde_json::json!({ "error": "no route" }))
            }
        },
        _ => {
            notify(
                "Travel time",
                "Could not get location or geocode the destination.",
            );
            Ok(serde_json::json!({ "error": "no location/geocode" }))
        }
    }
}

/// Scenario 4 — meeting-prep nudge: the next booking on the calendar, with
/// the attendee's CRM context (who, which company, what's in flight).
pub async fn aware_meeting(
    cal: &AgentCal,
    crm: &AgentCrm,
    p: &serde_json::Value,
) -> Result<serde_json::Value> {
    let owner = strp(p, "owner")?;
    let upcoming = cal.upcoming(owner, 1).await?;
    match upcoming.first() {
        Some(b) => {
            let slot_str = format!("{}", b.slot.start.format("%H:%M"));
            let mut names: Vec<String> = Vec::new();
            let mut context = String::new();
            for a in &b.attendees {
                if let Ok(c) = crm.contact_for_attendee(owner, a).await {
                    names.push(c.display_name());
                    if let Ok(ctx) = crm.contact_context(owner, &c.id).await {
                        let deals = ctx
                            .get("deals")
                            .and_then(|d| d.as_array())
                            .map(|d| d.len())
                            .unwrap_or(0);
                        let company = ctx
                            .get("company")
                            .and_then(|c| c.get("name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or("");
                        context = format!("{company} · {deals} open deal(s)");
                    }
                } else {
                    names.push(a.name.clone());
                }
            }
            let who = if names.is_empty() {
                "your guest".to_string()
            } else {
                names.join(", ")
            };
            let title = format!("Next: {} at {}", b.title, slot_str);
            let text = if context.is_empty() {
                format!("with {}", who)
            } else {
                format!("with {} — {}", who, context)
            };
            notify(&title, &text);
            Ok(serde_json::json!({ "booking_id": b.id, "title": title, "text": text }))
        }
        None => {
            notify("No meetings", "Nothing on the calendar next.");
            Ok(serde_json::json!({ "text": "no upcoming" }))
        }
    }
}

/// Scenario 5 — morning briefing: today's calendar + CRM headline stats.
pub async fn aware_briefing(
    cal: &AgentCal,
    crm: &AgentCrm,
    p: &serde_json::Value,
) -> Result<serde_json::Value> {
    let owner = strp(p, "owner")?;
    let upcoming = cal.upcoming(owner, 10).await?;
    let summary = crm.summary(owner).await?;

    let meetings = upcoming.len();
    let lines: Vec<String> = upcoming
        .iter()
        .take(5)
        .map(|b| format!("• {} at {}", b.title, b.slot.start.format("%H:%M")))
        .collect();

    let title = format!(
        "Good morning — {} meeting(s), {} contact(s), {} open deal(s)",
        meetings, summary.contacts, summary.open_deals
    );
    let text = if lines.is_empty() {
        "Clear day. No bookings.".to_string()
    } else {
        lines.join("\n")
    };
    notify(&title, &text);
    Ok(serde_json::json!({ "title": title, "text": text }))
}

/// Scenario 6 — deal nudge: surface open deals with their next action.
pub async fn aware_deals(crm: &AgentCrm, p: &serde_json::Value) -> Result<serde_json::Value> {
    let owner = strp(p, "owner")?;
    let deals = crm.list_deals(owner).await?;
    let open: Vec<_> = deals.into_iter().filter(|d| d.stage.is_open()).collect();

    if open.is_empty() {
        notify("No open deals", "Pipeline is clear.");
        return Ok(serde_json::json!({ "count": 0 }));
    }

    let title = format!("{} open deal(s) in flight", open.len());
    let text = open
        .iter()
        .take(5)
        .map(|d| {
            let next = if d.next_action.is_empty() {
                "no next step set"
            } else {
                &d.next_action
            };
            format!(
                "• {} (${:.0}k) — {:?}: {}",
                d.name,
                d.amount / 1000.0,
                d.stage,
                next
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    notify(&title, &text);
    Ok(serde_json::json!({ "count": open.len(), "text": text }))
}

// ── small param helpers (binary-local) ────────────────────────────────────────

fn strp<'a>(p: &'a serde_json::Value, key: &str) -> Result<&'a str> {
    p.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| agentcal::AgentError::Validation(format!("missing param: {key}")))
}

fn f64p(p: &serde_json::Value, key: &str) -> Option<f64> {
    p.get(key).and_then(|v| v.as_f64())
}
