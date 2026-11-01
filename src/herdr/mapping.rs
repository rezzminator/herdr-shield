use std::collections::BTreeMap;

use serde_json::Value;

use crate::holder::registry::Metadata;
use crate::{Result, fail};

#[derive(Clone, Default)]
pub struct Aliases(BTreeMap<String, Option<String>>);

impl Aliases {
    pub fn for_holder(meta: &Metadata) -> Self {
        let mut aliases = Self::default();
        for binding in std::iter::once(&meta.birth).chain(meta.history.iter()) {
            for (old, new) in [
                (&binding.pane, &meta.binding.pane),
                (&binding.tab, &meta.binding.tab),
                (&binding.workspace, &meta.binding.workspace),
            ] {
                aliases.insert(old, new);
            }
        }
        aliases
    }

    pub fn verify_live(&mut self, snapshot: Option<&Value>) {
        let snapshot = snapshot.filter(|value| {
            ["panes", "tabs", "workspaces"].iter().all(|key| {
                value[*key].as_array().is_some_and(|items| {
                    items.iter().all(|item| {
                        item[match *key {
                            "panes" => "pane_id",
                            "tabs" => "tab_id",
                            _ => "workspace_id",
                        }]
                        .is_string()
                    })
                })
            })
        });
        let mut live = std::collections::BTreeSet::new();
        if let Some(snapshot) = snapshot {
            for (collection, key) in [
                ("panes", "pane_id"),
                ("tabs", "tab_id"),
                ("workspaces", "workspace_id"),
            ] {
                if let Some(values) = snapshot[collection].as_array() {
                    for value in values {
                        if let Some(id) = value[key].as_str() {
                            live.insert(id.to_owned());
                        }
                    }
                }
            }
        }
        for (old, current) in &mut self.0 {
            if current.as_deref() != Some(old) && (snapshot.is_none() || live.contains(old)) {
                *current = None;
            }
        }
    }

    pub fn insert(&mut self, old: &str, new: &str) {
        if let Some(existing) = self.0.get_mut(old) {
            if existing.as_deref() != Some(new) {
                *existing = None;
            }
        } else {
            self.0.insert(old.into(), Some(new.into()));
        }
    }

    fn map(&self, value: &mut Value) -> Result<()> {
        if let Some(text) = value.as_str() {
            if let Some(mapped) = self.0.get(text) {
                *value = Value::String(mapped.clone().ok_or("invalid_binding: ambiguous alias")?);
            }
        }
        Ok(())
    }

    fn direct(&self, value: &mut Value) -> Result<()> {
        for key in [
            "pane_id",
            "target_pane_id",
            "source_pane_id",
            "caller_pane_id",
            "focused_pane_id",
            "tab_id",
            "workspace_id",
            "source_workspace_id",
            "before_workspace_id",
        ] {
            if let Some(field) = value.get_mut(key) {
                self.map(field)?;
            }
        }
        if let Some(ids) = value.get_mut("workspace_ids").and_then(Value::as_array_mut) {
            for id in ids {
                self.map(id)?;
            }
        }
        Ok(())
    }

    pub fn request(&self, method: &str, params: &mut Value) -> Result<()> {
        self.direct(params)?;
        match method {
            "pane.move" => {
                if let Some(value) = params.get_mut("destination") {
                    self.direct(value)?;
                }
            }
            "layout.apply" => {
                if let Some(root) = params.get_mut("root") {
                    self.layout(root, 0)?;
                }
            }
            "command.invoke" => {
                if let Some(value) = params.get_mut("selection") {
                    self.direct(value)?;
                }
            }
            "plugin.action.invoke" => {
                if let Some(value) = params.get_mut("context") {
                    self.direct(value)?;
                }
            }
            "events.subscribe" => {
                if let Some(values) = params
                    .get_mut("subscriptions")
                    .and_then(Value::as_array_mut)
                {
                    for value in values {
                        self.direct(value)?;
                    }
                }
            }
            "events.wait" => {
                if let Some(value) = params.get_mut("match_event") {
                    self.direct(value)?;
                }
            }
            "agent.view.set" => {
                if let Some(value) = params.get_mut("filter") {
                    self.filter(value, 0)?;
                }
            }
            method if method.starts_with("agent.") => {
                if let Some(value) = params.get_mut("target") {
                    // Only the native public-pane selector grammar is an identity; agent names are opaque.
                    if value
                        .as_str()
                        .is_some_and(|s| s.starts_with('w') && s.contains(":p"))
                    {
                        self.map(value)?;
                    }
                }
            }
            _ => (),
        }
        Ok(())
    }

    fn layout(&self, node: &mut Value, depth: usize) -> Result<()> {
        if depth > 64 {
            return fail("invalid_frame: layout nesting limit");
        }
        self.direct(node)?;
        for key in ["first", "second"] {
            if let Some(child) = node.get_mut(key) {
                self.layout(child, depth + 1)?;
            }
        }
        Ok(())
    }

    fn filter(&self, node: &mut Value, depth: usize) -> Result<()> {
        if depth > 64 {
            return fail("invalid_frame: filter nesting limit");
        }
        if matches!(
            node["field"].as_str(),
            Some("pane_id" | "tab_id" | "workspace_id")
        ) {
            if let Some(value) = node.get_mut("value") {
                self.map(value)?;
            }
            if let Some(values) = node.get_mut("values").and_then(Value::as_array_mut) {
                for value in values {
                    self.map(value)?;
                }
            }
        }
        if let Some(child) = node.get_mut("filter") {
            self.filter(child, depth + 1)?;
        }
        if let Some(children) = node.get_mut("filters").and_then(Value::as_array_mut) {
            for child in children {
                self.filter(child, depth + 1)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn closed_identity_field_and_nested_request_matrix_preserves_opaque_values() {
        let mut aliases = Aliases::default();
        for (old, new) in [("w1:p1", "w4:p4"), ("w1:t1", "w4:t4"), ("w1", "w4")] {
            aliases.insert(old, new);
        }
        let fields = [
            "pane_id",
            "target_pane_id",
            "source_pane_id",
            "caller_pane_id",
            "focused_pane_id",
            "tab_id",
            "workspace_id",
            "source_workspace_id",
            "before_workspace_id",
        ];
        for field in fields {
            let (old, new) = if field.ends_with("workspace_id") {
                ("w1", "w4")
            } else if field == "tab_id" {
                ("w1:t1", "w4:t4")
            } else {
                ("w1:p1", "w4:p4")
            };
            let mut params = json!({field:old,"source":old,"agent_session_id":old,"text":old,"argv":[old],"env":{field:old},"tokens":{field:old}});
            aliases.request("pane.get", &mut params).unwrap();
            assert_eq!(params[field], new, "direct identity {field}");
            for opaque in ["source", "agent_session_id", "text"] {
                assert_eq!(params[opaque], old);
            }
            assert_eq!(params["argv"][0], old);
            assert_eq!(params["env"][field], old);
            assert_eq!(params["tokens"][field], old);
        }
        let cases = [
            (
                "workspace.create",
                json!({"source_workspace_id":"w1"}),
                json!({"source_workspace_id":"w4"}),
            ),
            (
                "workspace.move_block",
                json!({"workspace_ids":["w1","w9"],"before_workspace_id":"w1"}),
                json!({"workspace_ids":["w4","w9"],"before_workspace_id":"w4"}),
            ),
            (
                "pane.move",
                json!({"pane_id":"w1:p1","destination":{"type":"tab","tab_id":"w1:t1","target_pane_id":"w1:p1","split":"right"}}),
                json!({"pane_id":"w4:p4","destination":{"type":"tab","tab_id":"w4:t4","target_pane_id":"w4:p4","split":"right"}}),
            ),
            (
                "pane.move",
                json!({"pane_id":"w1:p1","destination":{"type":"new_tab","workspace_id":"w1"}}),
                json!({"pane_id":"w4:p4","destination":{"type":"new_tab","workspace_id":"w4"}}),
            ),
            (
                "command.invoke",
                json!({"command_id":"opaque","pane_id":"w1:p1","selection":{"pane_id":"w1:p1","start":{"line":0,"col":0},"end":{"line":0,"col":1},"content_revision":1}}),
                json!({"command_id":"opaque","pane_id":"w4:p4","selection":{"pane_id":"w4:p4","start":{"line":0,"col":0},"end":{"line":0,"col":1},"content_revision":1}}),
            ),
            (
                "plugin.action.invoke",
                json!({"plugin_id":"opaque","action_id":"opaque","context":{"workspace_id":"w1","tab_id":"w1:t1","focused_pane_id":"w1:p1","selection_text":"w1:p1"}}),
                json!({"plugin_id":"opaque","action_id":"opaque","context":{"workspace_id":"w4","tab_id":"w4:t4","focused_pane_id":"w4:p4","selection_text":"w1:p1"}}),
            ),
        ];
        for (method, mut params, expected) in cases {
            aliases.request(method, &mut params).unwrap();
            assert_eq!(params, expected, "nested method {method}");
        }
        for selector in [
            "pane.output_matched",
            "pane.agent_status_changed",
            "pane.scroll_changed",
        ] {
            let mut params = json!({"subscriptions":[{"type":selector,"pane_id":"w1:p1"}]});
            aliases.request("events.subscribe", &mut params).unwrap();
            assert_eq!(params["subscriptions"][0]["pane_id"], "w4:p4");
        }
        for event in [
            "workspace_created",
            "workspace_updated",
            "workspace_closed",
            "workspace_renamed",
            "workspace_moved",
            "workspace_focused",
            "tab_created",
            "tab_closed",
            "tab_renamed",
            "tab_moved",
            "tab_focused",
            "pane_created",
            "pane_closed",
            "pane_focused",
            "pane_moved",
            "pane_output_changed",
            "pane_exited",
            "pane_agent_detected",
            "pane_agent_status_changed",
        ] {
            let mut params = json!({"match_event":{"event":event,"pane_id":"w1:p1","tab_id":"w1:t1","workspace_id":"w1","label":"w1:p1"}});
            aliases.request("events.wait", &mut params).unwrap();
            assert_eq!(params["match_event"]["pane_id"], "w4:p4");
            assert_eq!(params["match_event"]["tab_id"], "w4:t4");
            assert_eq!(params["match_event"]["workspace_id"], "w4");
            assert_eq!(params["match_event"]["label"], "w1:p1");
        }
        let mut filter = json!({"filter":{"op":"all","filters":[{"op":"any","filters":[{"op":"eq","field":"workspace_id","value":"w1"},{"op":"in","field":"tab_id","values":["w1:t1","w9:t9"]}]},{"op":"not","filter":{"op":"eq","field":"pane_id","value":"w1:p1"}},{"op":"exists","field":"pane_id"},{"op":"eq","field":{"token":"pane_id"},"value":"w1:p1"},{"op":"eq","field":"pane_id","value":{"context":"current_workspace_id"}}]}});
        aliases.request("agent.view.set", &mut filter).unwrap();
        assert_eq!(filter["filter"]["filters"][0]["filters"][0]["value"], "w4");
        assert_eq!(
            filter["filter"]["filters"][0]["filters"][1]["values"],
            json!(["w4:t4", "w9:t9"])
        );
        assert_eq!(filter["filter"]["filters"][1]["filter"]["value"], "w4:p4");
        assert_eq!(filter["filter"]["filters"][3]["value"], "w1:p1");
        assert_eq!(
            filter["filter"]["filters"][4]["value"],
            json!({"context":"current_workspace_id"})
        );
        for method in [
            "tab.create",
            "tab.list",
            "pane.swap",
            "pane.zoom",
            "pane.layout",
            "pane.process_info",
            "pane.neighbor",
            "pane.edges",
            "pane.focus_direction",
            "pane.resize",
            "pane.current",
            "layout.export",
            "worktree.list",
        ] {
            let mut omitted = json!({});
            aliases.request(method, &mut omitted).unwrap();
            assert_eq!(
                omitted,
                json!({}),
                "omitted optionals must preserve native defaults: {method}"
            );
        }
        for method in [
            "agent.get",
            "agent.explain",
            "agent.focus",
            "agent.read",
            "agent.send_keys",
            "agent.rename",
            "agent.prompt",
            "agent.wait",
        ] {
            let mut params = json!({"target":"w1:p1"});
            aliases.request(method, &mut params).unwrap();
            assert_eq!(params["target"], "w4:p4");
            let mut name = json!({"target":"agent name w1:p1"});
            aliases.request(method, &mut name).unwrap();
            assert_eq!(name["target"], "agent name w1:p1");
        }
    }

    #[test]
    fn every_intermediate_alias_maps_unless_reused_or_census_unknown() {
        use crate::holder::registry::Binding;
        let make = |n| Binding {
            socket: "/socket".into(),
            pane: format!("w{n}:p1"),
            tab: format!("w{n}:t1"),
            workspace: format!("w{n}"),
            server: crate::lifetime::identity::Identity::read(std::process::id()).unwrap(),
        };
        let mut meta = Metadata::new(make(1), "/".into()).unwrap();
        meta.history = vec![make(1), make(2), make(3)];
        meta.binding = make(4);
        let mut aliases = Aliases::for_holder(&meta);
        aliases.verify_live(Some(&json!({"panes":[{"pane_id":"w4:p1"}],"tabs":[{"tab_id":"w4:t1"}],"workspaces":[{"workspace_id":"w4"}]})));
        for n in 1..4 {
            let mut params = json!({"pane_id":format!("w{n}:p1"),"tab_id":format!("w{n}:t1"),"workspace_id":format!("w{n}")});
            aliases.request("pane.get", &mut params).unwrap();
            assert_eq!(
                params,
                json!({"pane_id":"w4:p1","tab_id":"w4:t1","workspace_id":"w4"})
            );
        }
        aliases.verify_live(Some(
            &json!({"panes":[{"pane_id":"w2:p1"}],"tabs":[],"workspaces":[]}),
        ));
        assert!(
            aliases
                .request("pane.get", &mut json!({"pane_id":"w2:p1"}))
                .is_err(),
            "reused intermediate alias must never route to retained workload"
        );
        let mut unknown = Aliases::for_holder(&meta);
        unknown.verify_live(None);
        assert!(
            unknown
                .request("pane.get", &mut json!({"pane_id":"w3:p1"}))
                .is_err()
        );
    }

    #[test]
    fn nested_identities_map_but_user_strings_env_argv_and_tokens_do_not() {
        let mut aliases = Aliases::default();
        aliases.insert("w1:p1", "w2:p3");
        let mut params = json!({"root":{"type":"split","first":{"type":"pane","pane_id":"w1:p1","command":["w1:p1"]},"second":{"type":"pane"}},"env":{"pane_id":"w1:p1"},"label":"w1:p1"});
        aliases.request("layout.apply", &mut params).unwrap();
        assert_eq!(params["root"]["first"]["pane_id"], "w2:p3");
        assert_eq!(params["root"]["first"]["command"][0], "w1:p1");
        assert_eq!(params["env"]["pane_id"], "w1:p1");
        let mut filter = json!({"filter":{"op":"all","filters":[{"op":"eq","field":"pane_id","value":"w1:p1"},{"op":"eq","field":{"token":"pane_id"},"value":"w1:p1"}]}});
        aliases.request("agent.view.set", &mut filter).unwrap();
        assert_eq!(filter["filter"]["filters"][0]["value"], "w2:p3");
        assert_eq!(filter["filter"]["filters"][1]["value"], "w1:p1");
        aliases.insert("w1:p1", "w3:p9");
        assert!(
            aliases
                .request("pane.get", &mut json!({"pane_id":"w1:p1"}))
                .is_err()
        );
    }
}
