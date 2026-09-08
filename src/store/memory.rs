//! In-process store. Value-copy semantics via clone.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;

use super::CalendarStore;
use crate::crm::store::{CrmStore, SearchHit};
use crate::crm::types::{Company, Contact, CrmSummary, Deal, Interaction};
use crate::error::Result;
use crate::inbox::InboxRecord;
use crate::types::{BookingLink, Calendar};

/// Ephemeral in-memory store (default, zero persistence).
#[derive(Debug, Default, Clone)]
pub struct MemoryStore {
    calendars: Arc<Mutex<HashMap<String, Calendar>>>,
    links: Arc<Mutex<HashMap<String, BookingLink>>>,
    companies: Arc<Mutex<HashMap<String, Company>>>,
    contacts: Arc<Mutex<HashMap<String, Contact>>>,
    deals: Arc<Mutex<HashMap<String, Deal>>>,
    interactions: Arc<Mutex<Vec<Interaction>>>,
    /// Inbox ledger keyed by `owner_id\0channel\0external_id` (the dedup key).
    inbox: Arc<Mutex<HashMap<String, InboxRecord>>>,
}

fn inbox_key(owner_id: &str, channel: &str, external_id: &str) -> String {
    format!("{owner_id}\0{channel}\0{external_id}")
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl CalendarStore for MemoryStore {
    async fn save(&self, calendar: &Calendar) -> Result<()> {
        self.calendars
            .lock()
            .await
            .insert(calendar.owner_id.clone(), calendar.clone());
        Ok(())
    }

    async fn load(&self, owner_id: &str) -> Result<Option<Calendar>> {
        Ok(self.calendars.lock().await.get(owner_id).cloned())
    }

    async fn delete(&self, owner_id: &str) -> Result<bool> {
        Ok(self.calendars.lock().await.remove(owner_id).is_some())
    }

    async fn list_ids(&self) -> Result<Vec<String>> {
        Ok(self.calendars.lock().await.keys().cloned().collect())
    }

    async fn save_link(&self, link: &BookingLink) -> Result<()> {
        self.links
            .lock()
            .await
            .insert(link.id.clone(), link.clone());
        Ok(())
    }

    async fn load_link(&self, link_id: &str) -> Result<Option<BookingLink>> {
        Ok(self.links.lock().await.get(link_id).cloned())
    }

    async fn load_links(&self, owner_id: &str) -> Result<Vec<BookingLink>> {
        Ok(self
            .links
            .lock()
            .await
            .values()
            .filter(|l| l.owner_id == owner_id)
            .cloned()
            .collect())
    }

    async fn delete_link(&self, link_id: &str) -> Result<bool> {
        Ok(self.links.lock().await.remove(link_id).is_some())
    }
}

#[async_trait]
impl CrmStore for MemoryStore {
    async fn save_company(&self, company: &Company) -> Result<()> {
        self.companies
            .lock()
            .await
            .insert(company.id.clone(), company.clone());
        Ok(())
    }

    async fn load_company(&self, owner_id: &str, company_id: &str) -> Result<Option<Company>> {
        let map = self.companies.lock().await;
        Ok(map
            .get(company_id)
            .filter(|c| c.owner_id == owner_id)
            .cloned())
    }

    async fn delete_company(&self, owner_id: &str, company_id: &str) -> Result<bool> {
        let mut map = self.companies.lock().await;
        let existed = map
            .get(company_id)
            .filter(|c| c.owner_id == owner_id)
            .is_some();
        if existed {
            map.remove(company_id);
        }
        Ok(existed)
    }

    async fn list_companies(&self, owner_id: &str) -> Result<Vec<Company>> {
        let map = self.companies.lock().await;
        let mut out: Vec<Company> = map
            .values()
            .filter(|c| c.owner_id == owner_id)
            .cloned()
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    async fn save_contact(&self, contact: &Contact) -> Result<()> {
        self.contacts
            .lock()
            .await
            .insert(contact.id.clone(), contact.clone());
        Ok(())
    }

    async fn load_contact(&self, owner_id: &str, contact_id: &str) -> Result<Option<Contact>> {
        let map = self.contacts.lock().await;
        Ok(map
            .get(contact_id)
            .filter(|c| c.owner_id == owner_id)
            .cloned())
    }

    async fn delete_contact(&self, owner_id: &str, contact_id: &str) -> Result<bool> {
        let mut map = self.contacts.lock().await;
        let existed = map
            .get(contact_id)
            .filter(|c| c.owner_id == owner_id)
            .is_some();
        if existed {
            map.remove(contact_id);
        }
        Ok(existed)
    }

    async fn list_contacts(&self, owner_id: &str) -> Result<Vec<Contact>> {
        let map = self.contacts.lock().await;
        let mut out: Vec<Contact> = map
            .values()
            .filter(|c| c.owner_id == owner_id)
            .cloned()
            .collect();
        out.sort_by(|a, b| {
            a.last_name
                .cmp(&b.last_name)
                .then(a.first_name.cmp(&b.first_name))
        });
        Ok(out)
    }

    async fn save_deal(&self, deal: &Deal) -> Result<()> {
        self.deals
            .lock()
            .await
            .insert(deal.id.clone(), deal.clone());
        Ok(())
    }

    async fn load_deal(&self, owner_id: &str, deal_id: &str) -> Result<Option<Deal>> {
        let map = self.deals.lock().await;
        Ok(map.get(deal_id).filter(|d| d.owner_id == owner_id).cloned())
    }

    async fn delete_deal(&self, owner_id: &str, deal_id: &str) -> Result<bool> {
        let mut map = self.deals.lock().await;
        let existed = map
            .get(deal_id)
            .filter(|d| d.owner_id == owner_id)
            .is_some();
        if existed {
            map.remove(deal_id);
        }
        Ok(existed)
    }

    async fn list_deals(&self, owner_id: &str) -> Result<Vec<Deal>> {
        let map = self.deals.lock().await;
        let mut out: Vec<Deal> = map
            .values()
            .filter(|d| d.owner_id == owner_id)
            .cloned()
            .collect();
        out.sort_by_key(|b| std::cmp::Reverse(b.updated_at));
        Ok(out)
    }

    async fn list_deals_for_company(&self, owner_id: &str, company_id: &str) -> Result<Vec<Deal>> {
        let map = self.deals.lock().await;
        let mut out: Vec<Deal> = map
            .values()
            .filter(|d| d.owner_id == owner_id && d.company_id == company_id)
            .cloned()
            .collect();
        out.sort_by_key(|b| std::cmp::Reverse(b.updated_at));
        Ok(out)
    }

    async fn save_interaction(&self, interaction: &Interaction) -> Result<()> {
        self.interactions.lock().await.push(interaction.clone());
        Ok(())
    }

    async fn list_interactions_for_contact(
        &self,
        owner_id: &str,
        contact_id: &str,
    ) -> Result<Vec<Interaction>> {
        let list = self.interactions.lock().await;
        let mut out: Vec<Interaction> = list
            .iter()
            .filter(|i| i.owner_id == owner_id && i.contact_id == contact_id)
            .cloned()
            .collect();
        out.sort_by_key(|b| std::cmp::Reverse(b.at));
        Ok(out)
    }

    async fn resolve_by_phone(&self, owner_id: &str, phone: &str) -> Result<Option<Contact>> {
        let normalized: String = phone.chars().filter(|c| c.is_ascii_digit()).collect();
        let map = self.contacts.lock().await;
        for c in map.values().filter(|c| c.owner_id == owner_id) {
            for candidate in c.all_phones() {
                let candidate_digits: String =
                    candidate.chars().filter(|ch| ch.is_ascii_digit()).collect();
                if candidate == phone
                    || (!candidate_digits.is_empty() && candidate_digits.ends_with(&normalized))
                {
                    return Ok(Some(c.clone()));
                }
            }
        }
        Ok(None)
    }

    async fn resolve_by_email(&self, owner_id: &str, email: &str) -> Result<Option<Contact>> {
        let map = self.contacts.lock().await;
        Ok(map
            .values()
            .find(|c| c.owner_id == owner_id && c.email == email)
            .cloned())
    }

    async fn contacts_for_company(&self, owner_id: &str, company_id: &str) -> Result<Vec<Contact>> {
        let map = self.contacts.lock().await;
        let mut out: Vec<Contact> = map
            .values()
            .filter(|c| c.owner_id == owner_id && c.company_id.as_deref() == Some(company_id))
            .cloned()
            .collect();
        out.sort_by(|a, b| a.last_name.cmp(&b.last_name));
        Ok(out)
    }

    async fn insert_inbox_event(&self, record: &InboxRecord) -> Result<bool> {
        let mut map = self.inbox.lock().await;
        let key = inbox_key(&record.owner_id, &record.channel, &record.external_id);
        if map.contains_key(&key) {
            return Ok(false);
        }
        map.insert(key, record.clone());
        Ok(true)
    }

    async fn load_inbox_event(
        &self,
        owner_id: &str,
        channel: &str,
        external_id: &str,
    ) -> Result<Option<InboxRecord>> {
        let map = self.inbox.lock().await;
        Ok(map.get(&inbox_key(owner_id, channel, external_id)).cloned())
    }

    async fn list_inbox_events(
        &self,
        owner_id: &str,
        channel: Option<&str>,
        limit: usize,
    ) -> Result<Vec<InboxRecord>> {
        let map = self.inbox.lock().await;
        let mut out: Vec<InboxRecord> = map
            .values()
            .filter(|r| r.owner_id == owner_id && channel.map(|c| r.channel == c).unwrap_or(true))
            .cloned()
            .collect();
        out.sort_by_key(|b| std::cmp::Reverse(b.at));
        Ok(out.into_iter().take(limit).collect())
    }

    async fn search_crm(
        &self,
        owner_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        let q = query.to_lowercase();
        let mut out: Vec<SearchHit> = Vec::new();

        {
            let map = self.companies.lock().await;
            for c in map.values().filter(|c| c.owner_id == owner_id) {
                if c.name.to_lowercase().contains(&q)
                    || c.industry.to_lowercase().contains(&q)
                    || c.notes.to_lowercase().contains(&q)
                {
                    out.push(SearchHit::company(c));
                }
            }
        }
        {
            let map = self.contacts.lock().await;
            for c in map.values().filter(|c| c.owner_id == owner_id) {
                let hay = format!(
                    "{} {} {} {} {}",
                    c.first_name, c.last_name, c.email, c.phone, c.notes
                )
                .to_lowercase();
                if hay.contains(&q) {
                    out.push(SearchHit::contact(c));
                }
            }
        }
        {
            let map = self.deals.lock().await;
            for d in map.values().filter(|d| d.owner_id == owner_id) {
                let hay = format!("{} {} {}", d.name, d.next_action, d.notes).to_lowercase();
                if hay.contains(&q) {
                    out.push(SearchHit::deal(d));
                }
            }
        }

        Ok(out.into_iter().take(limit).collect())
    }

    async fn vector_search(
        &self,
        owner_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        let q = crate::crm::types::embed_text(query);
        let map = self.contacts.lock().await;
        let mut scored: Vec<(f32, SearchHit)> = map
            .values()
            .filter(|c| c.owner_id == owner_id)
            .map(|c| {
                let v = crate::crm::types::embed_text(&format!(
                    "{} {} {} {} {}",
                    c.first_name,
                    c.last_name,
                    c.title,
                    c.notes,
                    c.tags.join(" ")
                ));
                let dot: f32 = q.iter().zip(&v).map(|(a, b)| a * b).sum();
                (dot, SearchHit::contact(c))
            })
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        Ok(scored.into_iter().take(limit).map(|(_, h)| h).collect())
    }

    async fn crm_summary(&self, owner_id: &str) -> Result<CrmSummary> {
        let mut s = CrmSummary::empty(owner_id);
        s.companies = self
            .companies
            .lock()
            .await
            .values()
            .filter(|c| c.owner_id == owner_id)
            .count();
        let contacts = self
            .contacts
            .lock()
            .await
            .values()
            .filter(|c| c.owner_id == owner_id)
            .cloned()
            .collect::<Vec<_>>();
        s.contacts = contacts.len();
        s.vip_contacts = contacts.iter().filter(|c| c.is_vip).count();
        let deals = self
            .deals
            .lock()
            .await
            .values()
            .filter(|d| d.owner_id == owner_id)
            .cloned()
            .collect::<Vec<_>>();
        s.deals = deals.len();
        s.open_deals = deals.iter().filter(|d| d.stage.is_open()).count();
        s.closed_won = deals
            .iter()
            .filter(|d| d.stage == crate::crm::types::DealStage::ClosedWon)
            .count();
        s.interactions = self
            .interactions
            .lock()
            .await
            .iter()
            .filter(|i| i.owner_id == owner_id)
            .count();
        Ok(s)
    }
}
