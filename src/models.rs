use crate::providers::ProviderId;
use std::time::SystemTime;

#[derive(Clone, Debug, Default)]
pub struct UsageSection {
    pub percentage: f64,
    pub resets_at: Option<SystemTime>,
}

/// A weekly limit that applies to one model rather than the whole plan. The
/// label is whatever the API calls the model, so a rename follows through.
#[derive(Clone, Debug)]
pub struct ScopedUsage {
    pub label: String,
    pub section: UsageSection,
}

#[derive(Clone, Debug, Default)]
pub struct UsageData {
    pub session: UsageSection,
    pub weekly: UsageSection,
    pub scoped: Option<ScopedUsage>,
}

/// One provider's usage, as handed around by the poll loop and the renderer.
#[derive(Clone, Debug)]
pub struct ProviderUsage {
    pub id: ProviderId,
    pub data: UsageData,
}

/// What a single poll assembled, keyed by provider rather than by three named
/// fields: the poll loop, the UI state and the renderer all walk
/// [`crate::providers::PROVIDERS`] and ask this for each one in turn.
#[derive(Clone, Debug, Default)]
pub struct AppUsageData {
    entries: Vec<ProviderUsage>,
}

impl AppUsageData {
    pub fn get(&self, id: ProviderId) -> Option<&UsageData> {
        self.entries
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| &entry.data)
    }

    pub fn set(&mut self, id: ProviderId, data: UsageData) {
        match self.entries.iter_mut().find(|entry| entry.id == id) {
            Some(entry) => entry.data = data,
            None => self.entries.push(ProviderUsage { id, data }),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &ProviderUsage> {
        self.entries.iter()
    }
}
