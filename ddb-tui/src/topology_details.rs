use super::*;
use crate::display_text::HorizontalViewport;
use anyhow::Context;
use std::{io::Write, path::PathBuf};

/// Explicit local export; no debugger client or terminal clipboard is involved.
#[derive(Debug)]
pub struct DetailsExport {
    path: PathBuf,
    lines: Vec<String>,
}

impl DetailsExport {
    pub fn write(self) -> anyhow::Result<PathBuf> {
        let name = self
            .path
            .file_name()
            .context("Export requires a file name")?;
        let parent = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new("."));
        let parent = parent
            .canonicalize()
            .context("Export directory must already exist")?;
        let path = parent.join(name);
        let mut temporary = tempfile::NamedTempFile::new_in(&parent)
            .context("Could not create temporary details export")?;
        {
            let mut writer = std::io::BufWriter::new(temporary.as_file_mut());
            for line in self.lines {
                writeln!(writer, "{line}")?;
            }
            writer.flush()?;
        }
        temporary.as_file().sync_all()?;
        temporary
            .persist_noclobber(&path)
            .context("Could not export details; existing files are never overwritten")?;
        #[cfg(unix)]
        std::fs::File::open(&parent)?
            .sync_all()
            .context("Details file was created, but syncing its directory failed")?;
        Ok(path)
    }
}

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
    pub fn start_details_export(&mut self) {
        if let Some(details) = self.topology_details.take() {
            self.start_input(InputMode::ExportDetails);
            self.pending_details_export = Some(details.lines);
            self.status = "Export captured details to a new file; Enter saves, Esc cancels".into();
        }
    }

    pub fn commit_details_export(&mut self, path: String) -> Result<Option<DetailsExport>, String> {
        let lines = self
            .pending_details_export
            .take()
            .ok_or("No details snapshot to export")?;
        if path.is_empty() {
            self.status = "Details export cancelled".into();
            return Ok(None);
        }
        Ok(Some(DetailsExport {
            path: PathBuf::from(path),
            lines,
        }))
    }

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

    fn captured(app: &mut App) -> Vec<String> {
        app.apply_snapshot(v2::Snapshot {
            server_instance_id: "original-instance".into(),
            sessions: vec![v2::Session {
                session_id: "original".into(),
                ..Default::default()
            }],
            ..Default::default()
        });
        app.open_topology_details(0).unwrap();
        app.topology_details.as_ref().unwrap().lines.clone()
    }

    #[tokio::test]
    async fn local_export_worker_reports_success_and_failure_without_a_backend() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("snapshot.json");
        let mut app = App::new("http://127.0.0.1:1".into());
        let client =
            crate::api::ApiClient::new(crate::api::ClientConfig::new("http://127.0.0.1:1"))
                .unwrap();
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let (messages, mut updates) = tokio::sync::mpsc::channel(4);
        let task = tokio::spawn(crate::worker::run(
            client,
            app.source_workspace.clone(),
            rx,
            messages,
        ));
        for exists in [false, true] {
            captured(&mut app);
            app.start_details_export();
            let export = app
                .commit_details_export(path.to_str().unwrap().into())
                .unwrap()
                .unwrap();
            tx.send(BackendRequest::ExportDetails(export))
                .await
                .unwrap();
            let update = tokio::time::timeout(std::time::Duration::from_secs(5), updates.recv())
                .await
                .unwrap()
                .unwrap();
            if exists {
                assert!(
                    matches!(update, UiMessage::Error(message) if message.contains("never overwritten"))
                );
            } else {
                assert!(
                    matches!(update, UiMessage::Notice(message) if message.contains("Details exported"))
                );
            }
        }
        drop(tx);
        task.await.unwrap();
    }

    #[test]
    fn export_requires_confirmation_and_keeps_the_captured_document() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("snapshot.json");
        let mut app = App::new("http://localhost".into());
        let lines = captured(&mut app);
        app.start_details_export();
        assert_eq!(app.input_mode, InputMode::ExportDetails);
        assert!(app.topology_details.is_none());
        assert!(!path.exists());
        app.apply_snapshot(v2::Snapshot {
            server_instance_id: "replacement".into(),
            ..Default::default()
        });
        assert_eq!(app.input_mode, InputMode::ExportDetails);
        let export = app
            .commit_details_export(path.to_str().unwrap().into())
            .unwrap()
            .unwrap();
        assert!(!path.exists());
        assert_eq!(export.write().unwrap(), path);
        let bytes = std::fs::read_to_string(&path).unwrap();
        assert_eq!(bytes, format!("{}\n", lines.join("\n")));
        assert_eq!(
            serde_json::from_str::<Value>(&bytes).unwrap()["sessions"][0]["sessionId"],
            "original"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        captured(&mut app);
        app.start_details_export();
        app.cancel_input();
        assert!(app.commit_details_export("unused".into()).is_err());
        captured(&mut app);
        app.start_details_export();
        assert!(app.commit_details_export(String::new()).unwrap().is_none());
        assert!(app.pending_details_export.is_none());
    }

    #[test]
    fn export_never_overwrites_existing_paths_and_cleans_temporary_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("existing");
        std::fs::write(&path, b"keep this").unwrap();
        let export = |path| {
            DetailsExport {
                path,
                lines: vec!["new content".into()],
            }
            .write()
        };
        assert!(export(path.clone()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"keep this");
        let subdir = directory.path().join("subdir");
        std::fs::create_dir(&subdir).unwrap();
        assert!(export(subdir).is_err());
        assert!(export(directory.path().join("missing-parent/output.json")).is_err());
        #[cfg(unix)]
        {
            let symlink = directory.path().join("symlink");
            std::os::unix::fs::symlink(&path, &symlink).unwrap();
            assert!(export(symlink).is_err());
            let dangling = directory.path().join("dangling");
            std::os::unix::fs::symlink(directory.path().join("absent"), &dangling).unwrap();
            assert!(export(dangling).is_err());
            assert!(!directory.path().join("absent").exists());
            assert_eq!(std::fs::read(&path).unwrap(), b"keep this");
        }
        assert!(std::fs::read_dir(directory.path())
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".tmp")));
    }

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
