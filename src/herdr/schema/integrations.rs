// Adapted from herdrdev/herdr @ 2ecf19a92fc3a7fff75b6ce3655ef04f4625ec01 (Apache-2.0).
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationState {
    NotInstalled,
    Current,
    Outdated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationInfo {
    pub target: IntegrationTarget,
    pub label: String,
    pub command: String,
    pub available: bool,
    pub state: IntegrationState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationInstallParams {
    pub target: IntegrationTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationUninstallParams {
    pub target: IntegrationTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationTarget {
    Pi,
    Omp,
    Claude,
    Codex,
    Copilot,
    Devin,
    Droid,
    Kimi,
    Opencode,
    Kilo,
    Hermes,
    Qodercli,
    Qwen,
    Cursor,
    Mastracode,
    AntigravityCli,
    Grok,
}

impl IntegrationTarget {
    pub(crate) const ALL: [Self; 17] = [
        Self::Pi,
        Self::Omp,
        Self::Claude,
        Self::Codex,
        Self::Copilot,
        Self::Devin,
        Self::Droid,
        Self::Kimi,
        Self::Opencode,
        Self::Kilo,
        Self::Hermes,
        Self::Qodercli,
        Self::Qwen,
        Self::Cursor,
        Self::Mastracode,
        Self::AntigravityCli,
        Self::Grok,
    ];
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationInstallResult {
    pub messages: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationUninstallResult {
    pub messages: Vec<String>,
}
