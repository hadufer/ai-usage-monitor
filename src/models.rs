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

#[derive(Clone, Debug, Default)]
pub struct AppUsageData {
    pub claude_code: Option<UsageData>,
    pub codex: Option<UsageData>,
    pub antigravity: Option<UsageData>,
}
