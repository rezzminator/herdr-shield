use super::schema::Method;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    Allow,
    Stream,
    Refuse,
}

macro_rules! policies {
    ($(($variant:ident, $name:literal, $policy:ident)),* $(,)?) => {
        pub const METHODS: &[(&str, Policy)] = &[$(($name, Policy::$policy)),*];
        pub fn policy(method: &Method) -> Policy {
            match method { $(Method::$variant(_) => Policy::$policy),* }
        }
    };
}

policies! {
    (Ping, "ping", Allow),
    (ServerStop, "server.stop", Refuse),
    (ServerLiveHandoff, "server.live_handoff", Refuse),
    (ServerReloadConfig, "server.reload_config", Refuse),
    (ServerSshAgentRegister, "server.ssh_agent.register", Refuse),
    (ServerAgentManifests, "server.agent_manifests", Allow),
    (ServerReloadAgentManifests, "server.reload_agent_manifests", Refuse),
    (NotificationShow, "notification.show", Allow),
    (ProductAnnouncementDismiss, "product_announcement.dismiss", Allow),
    (ReleaseNotesDismiss, "release_notes.dismiss", Allow),
    (CommandInvoke, "command.invoke", Allow),
    (ClientWindowTitleSet, "client.window_title.set", Allow),
    (ClientWindowTitleClear, "client.window_title.clear", Allow),
    (ClientShellSurfaceSet, "client_shell.surface.set", Refuse),
    (SessionSnapshot, "session.snapshot", Allow),
    (WorkspaceCreate, "workspace.create", Allow),
    (WorkspaceList, "workspace.list", Allow),
    (WorkspaceGet, "workspace.get", Allow),
    (WorkspaceFocus, "workspace.focus", Allow),
    (WorkspaceRename, "workspace.rename", Allow),
    (WorkspaceMove, "workspace.move", Allow),
    (WorkspaceMoveBlock, "workspace.move_block", Allow),
    (WorkspaceReportMetadata, "workspace.report_metadata", Allow),
    (WorkspaceClose, "workspace.close", Allow),
    (WorktreeList, "worktree.list", Allow),
    (WorktreeCreate, "worktree.create", Allow),
    (WorktreeOpen, "worktree.open", Allow),
    (WorktreeRemove, "worktree.remove", Allow),
    (TabCreate, "tab.create", Allow),
    (TabList, "tab.list", Allow),
    (TabGet, "tab.get", Allow),
    (TabFocus, "tab.focus", Allow),
    (TabRename, "tab.rename", Allow),
    (TabMove, "tab.move", Allow),
    (TabClose, "tab.close", Allow),
    (AgentList, "agent.list", Allow),
    (AgentGet, "agent.get", Allow),
    (AgentRead, "agent.read", Allow),
    (AgentExplain, "agent.explain", Refuse),
    (AgentSendKeys, "agent.send_keys", Allow),
    (AgentRename, "agent.rename", Allow),
    (AgentViewSet, "agent.view.set", Allow),
    (AgentViewClear, "agent.view.clear", Allow),
    (AgentFocus, "agent.focus", Allow),
    (AgentStart, "agent.start", Allow),
    (AgentPrompt, "agent.prompt", Stream),
    (AgentWait, "agent.wait", Stream),
    (PaneSplit, "pane.split", Allow),
    (PaneSwap, "pane.swap", Allow),
    (PaneMove, "pane.move", Allow),
    (PaneZoom, "pane.zoom", Allow),
    (PaneLayout, "pane.layout", Allow),
    (PaneProcessInfo, "pane.process_info", Allow),
    (LayoutExport, "layout.export", Allow),
    (LayoutApply, "layout.apply", Allow),
    (LayoutSetSplitRatio, "layout.set_split_ratio", Allow),
    (PaneNeighbor, "pane.neighbor", Allow),
    (PaneEdges, "pane.edges", Allow),
    (PaneFocusDirection, "pane.focus_direction", Allow),
    (PaneResize, "pane.resize", Allow),
    (PaneScroll, "pane.scroll", Allow),
    (PaneClear, "pane.clear", Allow),
    (PaneEditScrollback, "pane.edit_scrollback", Allow),
    (PaneSelectionRead, "pane.selection.read", Allow),
    (PaneCopyMotion, "pane.copy_motion", Allow),
    (PaneCopySearch, "pane.copy_search", Allow),
    (PaneList, "pane.list", Allow),
    (PaneCurrent, "pane.current", Allow),
    (PaneGet, "pane.get", Allow),
    (PaneFocus, "pane.focus", Allow),
    (PaneInputSet, "pane.input.set", Allow),
    (PaneLinkActivate, "pane.link.activate", Allow),
    (PaneLinkResolve, "pane.link.resolve", Allow),
    (PaneRename, "pane.rename", Allow),
    (PaneSendText, "pane.send_text", Allow),
    (PaneSendKeys, "pane.send_keys", Allow),
    (PaneSendInput, "pane.send_input", Allow),
    (PaneRead, "pane.read", Allow),
    (PaneReportAgent, "pane.report_agent", Allow),
    (PaneReportAgentSession, "pane.report_agent_session", Allow),
    (PaneReportMetadata, "pane.report_metadata", Allow),
    (PaneClearAgentAuthority, "pane.clear_agent_authority", Allow),
    (PaneReleaseAgent, "pane.release_agent", Allow),
    (PaneClose, "pane.close", Allow),
    (PopupClose, "popup.close", Allow),
    (EventsSubscribe, "events.subscribe", Stream),
    (EventsWait, "events.wait", Stream),
    (PaneWaitForOutput, "pane.wait_for_output", Stream),
    (IntegrationList, "integration.list", Allow),
    (IntegrationInstall, "integration.install", Refuse),
    (IntegrationUninstall, "integration.uninstall", Refuse),
    (PluginLink, "plugin.link", Refuse),
    (PluginList, "plugin.list", Allow),
    (PluginUnlink, "plugin.unlink", Refuse),
    (PluginEnable, "plugin.enable", Refuse),
    (PluginDisable, "plugin.disable", Refuse),
    (PluginActionList, "plugin.action.list", Allow),
    (PluginActionInvoke, "plugin.action.invoke", Allow),
    (PluginLogList, "plugin.log.list", Allow),
    (PluginPaneOpen, "plugin.pane.open", Allow),
    (PluginPaneFocus, "plugin.pane.focus", Allow),
    (PluginPaneClose, "plugin.pane.close", Allow),
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_102_pinned_methods_have_exactly_one_policy() {
        let source = include_str!("schema/mod.rs");
        let names: std::collections::BTreeSet<_> = METHODS.iter().map(|(name, _)| *name).collect();
        assert_eq!(METHODS.len(), 102);
        assert_eq!(names.len(), 102);
        for line in source
            .lines()
            .filter(|line| line.contains("#[serde(rename = "))
        {
            let name = line.split('"').nth(1).unwrap();
            assert!(names.contains(name), "missing {name}");
        }
        assert_eq!(
            METHODS.iter().filter(|(_, p)| *p == Policy::Stream).count(),
            5
        );
        assert_eq!(
            METHODS.iter().filter(|(_, p)| *p == Policy::Refuse).count(),
            13
        );
    }
}
