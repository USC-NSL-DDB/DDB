use super::*;
use crate::display_text::HorizontalViewport;

/// Read-only capture. Discovery and selection changes must not rewrite a document
/// the user is reading or about to export.
#[derive(Clone, Debug)]
pub struct TopologyDetails {
    pub lines: Vec<String>,
    pub width: usize,
    pub top: usize,
    pub page: usize,
    pub horizontal: HorizontalViewport,
}

impl TopologyDetails {
    pub fn scroll(&mut self, delta: isize) {
        self.top = self
            .top
            .saturating_add_signed(delta)
            .min(self.lines.len().saturating_sub(self.page.max(1)));
    }
}

impl App {
    pub fn open_topology_details(&mut self, row: usize) -> Result<(), String> {
        let kind = self
            .topology_rows()
            .get(row)
            .map(|row| row.kind.clone())
            .ok_or("Topology row no longer exists")?;
        let mut thread_id = None;
        let kind = if let TopologyRowKind::Thread(id) = kind {
            let thread = self
                .threads
                .iter()
                .find(|thread| thread.id == id)
                .ok_or("Thread metadata is unavailable")?;
            thread_id = Some(id);
            TopologyRowKind::Session(thread.session_id.clone())
        } else {
            kind
        };
        let (selection, group_id, sessions) = match kind {
            TopologyRowKind::Group(id) => {
                let sessions = self
                    .sessions
                    .iter()
                    .filter(|session| session.group_id.as_deref().unwrap_or("") == id)
                    .cloned()
                    .collect::<Vec<_>>();
                (
                    if id.is_empty() {
                        "ungrouped_sessions"
                    } else {
                        "group"
                    },
                    id,
                    sessions,
                )
            }
            TopologyRowKind::Session(session_id) => {
                let session = self
                    .sessions
                    .iter()
                    .find(|session| session.session_id == session_id)
                    .ok_or("Session metadata is unavailable")?;
                (
                    "session",
                    session.group_id.clone().unwrap_or_default(),
                    vec![session.clone()],
                )
            }
            TopologyRowKind::Thread(_) => unreachable!("thread resolved to owning session above"),
        };
        let group = self.groups.iter().find(|group| group.group_id == group_id);
        let document = serde_json::json!({
            "schemaVersion": 1,
            "serverInstanceId": self.source_server_instance,
            "stateRevision": self.snapshot_revision.to_string(),
            "selection": selection,
            "selectedThreadId": thread_id,
            "groupId": if group_id.is_empty() { None } else { Some(&group_id) },
            "group": group,
            "sessions": sessions,
        });
        let text = serde_json::to_string_pretty(&document).map_err(|error| error.to_string())?;
        self.topology_details = Some(TopologyDetails {
            width: text
                .lines()
                .map(|line| ratatui::text::Line::from(line).width())
                .max()
                .unwrap_or(0),
            lines: text.lines().map(str::to_owned).collect(),
            top: 0,
            page: 1,
            horizontal: HorizontalViewport::default(),
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_groups_and_thread_owners_preserve_canonical_identity() {
        let mut app = App::new("http://localhost".into());
        app.sessions = vec![
            v2::Session {
                session_id: "same-id".into(),
                ..Default::default()
            },
            v2::Session {
                session_id: "owner".into(),
                group_id: Some("missing-group".into()),
                ..Default::default()
            },
        ];
        app.threads = vec![ThreadItem {
            id: "same-id".into(),
            session_id: "owner".into(),
            name: "thread".into(),
            state: "stopped".into(),
            function: String::new(),
            file: None,
            line: None,
        }];
        for (kind, expected_session, expected_thread) in [
            (
                TopologyRowKind::Session("same-id".into()),
                "same-id",
                Value::Null,
            ),
            (
                TopologyRowKind::Thread("same-id".into()),
                "owner",
                Value::String("same-id".into()),
            ),
            (
                TopologyRowKind::Group("missing-group".into()),
                "owner",
                Value::Null,
            ),
            (
                TopologyRowKind::Group(String::new()),
                "same-id",
                Value::Null,
            ),
        ] {
            let row = app
                .topology_rows()
                .iter()
                .position(|row| row.kind == kind)
                .unwrap();
            app.open_topology_details(row).unwrap();
            let document: Value =
                serde_json::from_str(&app.topology_details.as_ref().unwrap().lines.join("\n"))
                    .unwrap();
            assert_eq!(document["sessions"][0]["sessionId"], expected_session);
            assert_eq!(document["selectedThreadId"], expected_thread);
            assert_eq!(document["group"], Value::Null);
            if kind == TopologyRowKind::Group("missing-group".into()) {
                assert_eq!(document["groupId"], "missing-group");
            }
            if kind == TopologyRowKind::Group(String::new()) {
                assert_eq!(document["selection"], "ungrouped_sessions");
            }
        }
        assert!(app.open_topology_details(usize::MAX).is_err());
    }

    #[test]
    fn details_freeze_canonical_resources_without_changing_execution_scope() {
        let mut app = App::new("http://localhost".into());
        app.apply_snapshot(v2::Snapshot {
            server_instance_id: "instance/original".into(),
            sessions: vec![v2::Session {
                session_id: "session/opaque".into(),
                display_name: "worker".into(),
                group_id: Some("group/opaque".into()),
                tag: "tag".into(),
                ..Default::default()
            }],
            groups: vec![v2::Group {
                group_id: "group/opaque".into(),
                display_name: "workers".into(),
                ..Default::default()
            }],
            ..Default::default()
        });
        let scope = app.execution_target();
        app.open_topology_details(0).unwrap();
        let before = app.topology_details.as_ref().unwrap().lines.join("\n");
        let document: Value = serde_json::from_str(&before).unwrap();
        assert_eq!(document["group"]["groupId"], "group/opaque");
        assert_eq!(document["sessions"][0]["sessionId"], "session/opaque");
        assert_eq!(document["sessions"][0]["tag"], "tag");
        assert!(document["group"].get("hash").is_none());
        assert_eq!(app.execution_target(), scope);
        app.sessions.clear();
        assert_eq!(
            app.topology_details.as_ref().unwrap().lines.join("\n"),
            before
        );
    }
}
