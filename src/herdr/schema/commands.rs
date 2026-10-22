// Adapted from herdrdev/herdr @ 2ecf19a92fc3a7fff75b6ce3655ef04f4625ec01 (MIT).
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandInvokeParams {
    /// Opaque endpoint-issued command identifier from the client-shell projection.
    pub command_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    /// Client-owned selection coordinates, validated against the pane's content revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<super::PaneSelectionReadParams>,
}
