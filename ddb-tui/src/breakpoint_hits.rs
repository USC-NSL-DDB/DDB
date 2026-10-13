use super::*;

/// A current stop, not the breakpoint's cumulative hit counter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BreakpointHit {
    pub breakpoint_id: String,
    pub sub_breakpoint_id: Option<String>,
    pub session_id: String,
    pub thread_id: String,
    pub execution_state_id: String,
    pub revision: u64,
}

impl BreakpointHit {
    pub fn matches_member(&self, breakpoint: &Breakpoint, member: &v2::SubBreakpoint) -> bool {
        self.breakpoint_id == breakpoint.id
            && self.session_id == member.session_id
            && match self.sub_breakpoint_id.as_ref() {
                Some(id) => *id == member.sub_breakpoint_id,
                None => {
                    breakpoint
                        .sub_breakpoints
                        .iter()
                        .filter(|site| site.session_id == member.session_id)
                        .count()
                        == 1
                }
            }
    }
}

#[derive(Clone, Debug)]
pub struct BreakpointHitPicker {
    pub hits: Vec<BreakpointHit>,
    pub cursor: usize,
}

/// Selection is the identity of a stop, never a row offset. A new stop in the
/// same thread must be explicitly selected before it can be revealed.
#[derive(Clone, Debug, Default)]
pub struct HitView {
    pub visible: bool,
    selected: Option<BreakpointHit>,
}

#[derive(Clone, Debug)]
pub struct HitRow {
    pub hit: Option<BreakpointHit>,
    pub label: String,
    pub depth: usize,
}

impl App {
    pub fn toggle_hit_view(&mut self) {
        self.breakpoint_hit_view.visible = !self.breakpoint_hit_view.visible;
        if self.breakpoint_hit_view.visible && self.current_visible_hit().is_none() {
            self.breakpoint_hit_view.selected =
                self.hit_view_rows().into_iter().find_map(|row| row.hit);
        }
        self.breakpoint_viewport = Default::default();
        self.status = if self.breakpoint_hit_view.visible {
            "Current breakpoint hits · Enter opens the selected frame · H returns to all breakpoints"
        } else {
            "All breakpoints · H shows currently paused hits"
        }
        .into();
    }

    pub fn clear_hit_selection(&mut self) {
        self.breakpoint_hit_view.selected = None;
    }

    pub fn current_visible_hit(&self) -> Option<&BreakpointHit> {
        self.breakpoint_hit_view
            .selected
            .as_ref()
            .filter(|hit| self.breakpoint_hits().contains(hit))
    }

    pub fn select_visible_hit(&mut self, hit: BreakpointHit) -> Result<(), String> {
        if !self.breakpoint_hits().contains(&hit) {
            return Err(
                "This breakpoint hit is no longer paused at the same stop. Select a current hit."
                    .into(),
            );
        }
        self.breakpoint_hit_view.selected = Some(hit);
        Ok(())
    }

    pub fn move_visible_hit(&mut self, delta: isize) {
        let hits = self
            .hit_view_rows()
            .into_iter()
            .filter_map(|row| row.hit)
            .collect::<Vec<_>>();
        let selected = self.current_visible_hit();
        let current = hits.iter().position(|hit| Some(hit) == selected);
        let next = current.map_or_else(
            || {
                if delta < 0 {
                    hits.len().saturating_sub(1)
                } else {
                    0
                }
            },
            |current| move_index(current, hits.len(), delta),
        );
        self.breakpoint_hit_view.selected = hits.get(next).cloned();
    }

    pub fn commit_visible_hit(&mut self) -> Result<String, String> {
        let hit = self
            .breakpoint_hit_view
            .selected
            .clone()
            .ok_or("Select a current breakpoint hit with the arrows or mouse")?;
        self.reveal_hit(&hit)
    }

    pub fn hit_view_rows(&self) -> Vec<HitRow> {
        let group_id = |hit: &BreakpointHit| {
            self.sessions
                .iter()
                .find(|session| session.session_id == hit.session_id)
                .and_then(|session| session.group_id.as_deref())
                .unwrap_or("")
        };
        let mut hits = self.breakpoint_hits();
        hits.sort_by(|a, b| {
            (group_id(a), &a.session_id, &a.breakpoint_id, &a.thread_id).cmp(&(
                group_id(b),
                &b.session_id,
                &b.breakpoint_id,
                &b.thread_id,
            ))
        });
        let mut rows = Vec::new();
        let mut previous_group = None;
        let mut previous_session = None;
        for hit in hits {
            let group = group_id(&hit);
            if previous_group.as_deref() != Some(group) {
                let name = self
                    .groups
                    .iter()
                    .find(|entry| entry.group_id == group)
                    .map(|entry| entry.display_name.as_str())
                    .filter(|name| !name.is_empty());
                rows.push(HitRow {
                    hit: None,
                    label: if group.is_empty() {
                        "Ungrouped".into()
                    } else {
                        name.map_or_else(
                            || format!("Group {group}"),
                            |name| {
                                if self.trace_api {
                                    format!("{name} · {group}")
                                } else {
                                    name.into()
                                }
                            },
                        )
                    },
                    depth: 0,
                });
                previous_group = Some(group.to_owned());
                previous_session = None;
            }
            if previous_session.as_deref() != Some(hit.session_id.as_str()) {
                let name = self
                    .sessions
                    .iter()
                    .find(|session| session.session_id == hit.session_id)
                    .map(|session| session.display_name.as_str())
                    .filter(|name| !name.is_empty());
                rows.push(HitRow {
                    hit: None,
                    label: name.map_or_else(
                        || format!("Session {}", hit.session_id),
                        |name| {
                            if self.trace_api {
                                format!("{name} · {}", hit.session_id)
                            } else {
                                name.into()
                            }
                        },
                    ),
                    depth: 1,
                });
                previous_session = Some(hit.session_id.clone());
            }
            let thread = self
                .threads
                .iter()
                .find(|thread| thread.id == hit.thread_id);
            let location = thread
                .and_then(|thread| thread.file.as_ref().zip(thread.line))
                .map(|(path, line)| format!("{}:{line}", short_model_path(path)))
                .or_else(|| thread.map(|thread| thread.function.clone()))
                .filter(|location| !location.is_empty())
                .unwrap_or_else(|| "no source location".into());
            rows.push(HitRow {
                label: format!(
                    "● {location} · {}{}",
                    thread
                        .map(|thread| thread.name.as_str())
                        .filter(|name| !name.is_empty())
                        .unwrap_or(&hit.thread_id),
                    if self.trace_api {
                        format!(
                            " · stop {} · {} · {}",
                            hit.revision, hit.breakpoint_id, hit.thread_id
                        )
                    } else {
                        String::new()
                    }
                ),
                hit: Some(hit),
                depth: 2,
            });
        }
        rows
    }

    pub fn open_breakpoint_hits(&mut self) -> Result<(), String> {
        let breakpoint = self
            .current_breakpoint()
            .ok_or("Select a breakpoint first")?;
        let member = self
            .selected_breakpoint_site
            .and_then(|site| breakpoint.sub_breakpoints.get(site));
        let hits = self
            .breakpoint_hits()
            .into_iter()
            .filter(|hit| {
                hit.breakpoint_id == breakpoint.id
                    && member.is_none_or(|member| hit.matches_member(breakpoint, member))
            })
            .collect::<Vec<_>>();
        if hits.is_empty() {
            return Err("No thread is currently paused at this breakpoint".into());
        }
        self.breakpoint_hit_picker = Some(BreakpointHitPicker { hits, cursor: 0 });
        Ok(())
    }

    pub fn move_breakpoint_hit(&mut self, delta: isize) {
        if let Some(picker) = self.breakpoint_hit_picker.as_mut() {
            picker.cursor = picker
                .cursor
                .saturating_add_signed(delta)
                .min(picker.hits.len().saturating_sub(1));
        }
    }

    pub fn commit_breakpoint_hit(&mut self) -> Result<String, String> {
        let picker = self
            .breakpoint_hit_picker
            .take()
            .ok_or("No paused-hit picker is open")?;
        let hit = picker
            .hits
            .get(picker.cursor)
            .ok_or("Select a paused thread")?;
        self.reveal_hit(hit)
    }

    fn reveal_hit(&mut self, hit: &BreakpointHit) -> Result<String, String> {
        if !self.breakpoint_hits().contains(hit) {
            return Err(
                "This breakpoint hit is no longer paused at the same stop. Reopen the picker."
                    .into(),
            );
        }
        self.reveal_topology_thread(&hit.thread_id);
        self.source_browser.set_following(true);
        Ok(hit.thread_id.clone())
    }

    pub fn breakpoint_hits(&self) -> Vec<BreakpointHit> {
        if !self.api_connected {
            return Vec::new();
        }
        let mut hits =
            self.execution_states
                .iter()
                .filter_map(|execution| {
                    if execution.running {
                        return None;
                    }
                    let target::Selector::Thread(target) =
                        execution.target.as_ref()?.selector.as_ref()?
                    else {
                        return None;
                    };
                    let reason = execution.stop_reason.as_ref()?;
                    if reason.kind != v2::StopReasonKind::Breakpoint as i32
                        || reason
                            .thread_id
                            .as_ref()
                            .is_some_and(|owner| owner != &target.thread_id)
                    {
                        return None;
                    }
                    let id = reason.breakpoint_id.as_ref()?;
                    let thread = self.threads.iter().find(|thread| {
                        thread.id == target.thread_id && thread.state == "stopped"
                    })?;
                    let breakpoint = self.breakpoints.iter().find(|breakpoint| {
                        breakpoint.id == *id
                            || breakpoint.sub_breakpoints.iter().any(|member| {
                                member.sub_breakpoint_id == *id
                                    && member.session_id == thread.session_id
                            })
                    })?;
                    Some(BreakpointHit {
                        breakpoint_id: breakpoint.id.clone(),
                        sub_breakpoint_id: breakpoint
                            .sub_breakpoints
                            .iter()
                            .find(|member| {
                                member.sub_breakpoint_id == *id
                                    && member.session_id == thread.session_id
                            })
                            .map(|member| member.sub_breakpoint_id.clone()),
                        session_id: thread.session_id.clone(),
                        thread_id: thread.id.clone(),
                        execution_state_id: execution.execution_state_id.clone(),
                        revision: execution.revision,
                    })
                })
                .collect::<Vec<_>>();
        hits.sort_by(|a, b| {
            (&a.breakpoint_id, &a.session_id, &a.thread_id).cmp(&(
                &b.breakpoint_id,
                &b.session_id,
                &b.thread_id,
            ))
        });
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> App {
        let mut app = App::new("http://localhost".into());
        app.apply_snapshot(v2::Snapshot {
            sessions: vec![
                v2::Session {
                    session_id: "session/a".into(),
                    ..Default::default()
                },
                v2::Session {
                    session_id: "session/b".into(),
                    ..Default::default()
                },
            ],
            breakpoints: vec![v2::Breakpoint {
                breakpoint_id: "breakpoint/shared".into(),
                spec: Some(v2::BreakpointSpec {
                    location: Some(breakpoint_spec::Location::Function(
                        v2::FunctionBreakpointLocation {
                            function_name: "foo".into(),
                        },
                    )),
                    ..Default::default()
                }),
                hit_count: 100,
                sub_breakpoints: vec![v2::SubBreakpoint {
                    sub_breakpoint_id: "member/b".into(),
                    session_id: "session/b".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        });
        app.apply_threads(
            [
                ("thread/a", "session/a"),
                ("thread/b", "session/b"),
                ("thread/peer", "session/a"),
            ]
            .into_iter()
            .map(|(id, session)| v2::Thread {
                thread_id: id.into(),
                session_id: session.into(),
                state: v2::ThreadState::Stopped as i32,
                ..Default::default()
            })
            .collect(),
        );
        app.execution_states = [
            ("thread/a", "breakpoint/shared", "thread/a"),
            ("thread/b", "member/b", "thread/b"),
            ("thread/peer", "breakpoint/shared", "thread/a"),
        ]
        .into_iter()
        .map(|(thread, bp, owner)| v2::ExecutionState {
            execution_state_id: format!("execution/{thread}"),
            target: Some(thread_target(thread)),
            revision: 10,
            stop_reason: Some(v2::StopReason {
                kind: v2::StopReasonKind::Breakpoint as i32,
                breakpoint_id: Some(bp.into()),
                thread_id: Some(owner.into()),
                ..Default::default()
            }),
            ..Default::default()
        })
        .collect();
        app
    }

    #[test]
    fn persistent_view_projects_only_live_hits_and_preserves_all_selection_and_scope() {
        let mut app = fixture();
        app.selected_breakpoint_site = Some(0);
        let target = app.execution_target();
        app.toggle_hit_view();
        let rows = app.hit_view_rows();
        assert_eq!(rows.iter().filter(|row| row.hit.is_some()).count(), 2);
        assert_eq!(rows.iter().filter(|row| row.depth == 1).count(), 2);
        assert_eq!(app.current_visible_hit().unwrap().thread_id, "thread/a");
        assert!(
            app.current_breakpoint().is_none(),
            "hidden selection must not receive mutations"
        );
        app.move_visible_hit(1);
        assert_eq!(app.commit_visible_hit().unwrap(), "thread/b");
        assert_eq!(app.execution_target(), target);
        app.toggle_hit_view();
        assert_eq!(app.selected_breakpoint_site, Some(0));
        assert_eq!(app.current_breakpoint().unwrap().id, "breakpoint/shared");
    }

    #[test]
    fn persistent_selection_does_not_redirect_on_resume_reorder_or_new_stop() {
        let mut app = fixture();
        app.toggle_hit_view();
        let old_hit = app.current_visible_hit().unwrap().clone();
        app.execution_states.reverse();
        assert_eq!(app.current_visible_hit(), Some(&old_hit));
        app.mark_running(Some("thread/a"));
        assert!(app.current_visible_hit().is_none());
        assert!(app.commit_visible_hit().is_err());
        assert!(app.select_visible_hit(old_hit.clone()).is_err());
        let state = app
            .execution_states
            .iter_mut()
            .find(|state| state.target == Some(thread_target("thread/a")))
            .unwrap();
        state.running = false;
        state.revision += 1;
        app.threads
            .iter_mut()
            .find(|thread| thread.id == "thread/a")
            .unwrap()
            .state = "stopped".into();
        assert!(
            app.current_visible_hit().is_none(),
            "same thread at a new stop is not selected"
        );
        app.move_visible_hit(1);
        assert!(app.current_visible_hit().is_some());
        assert_ne!(app.current_visible_hit(), Some(&old_hit));
    }

    #[test]
    fn persistent_view_survives_empty_state_and_clears_selection_on_disconnect() {
        let mut app = fixture();
        app.toggle_hit_view();
        app.backend_unavailable("disconnected");
        assert!(app.hit_view_rows().is_empty());
        assert!(app.commit_visible_hit().is_err());
        assert!(app.breakpoint_hit_view.visible);
        app.api_connected = true;
        assert!(app.current_visible_hit().is_none());
        app.move_visible_hit(1);
        assert!(app.commit_visible_hit().is_ok());
        app.breakpoints.clear();
        assert!(app.hit_view_rows().is_empty());
        assert!(app.commit_visible_hit().is_err());
        app.move_visible_hit(isize::MAX);
        assert!(app.current_visible_hit().is_none());
    }

    #[test]
    fn ambiguous_member_hits_and_backend_replacement_do_not_navigate() {
        let mut app = fixture();
        app.execution_states[1]
            .stop_reason
            .as_mut()
            .unwrap()
            .breakpoint_id = Some("breakpoint/shared".into());
        app.breakpoints[0].sub_breakpoints.push(v2::SubBreakpoint {
            sub_breakpoint_id: "member/b2".into(),
            session_id: "session/b".into(),
            ..Default::default()
        });
        app.selected_breakpoint_site = Some(0);
        assert!(app.open_breakpoint_hits().is_err());
        app.selected_breakpoint_site = None;
        app.open_breakpoint_hits().unwrap();
        app.source_server_instance = Some("old-server".into());
        app.apply_snapshot(v2::Snapshot {
            server_instance_id: "new-server".into(),
            ..Default::default()
        });
        assert!(app.breakpoint_hit_picker.is_none());
        assert!(app.commit_breakpoint_hit().is_err());
    }

    #[test]
    fn hit_picker_disambiguates_sessions_and_rejects_changed_stops() {
        let mut app = fixture();
        let target = app.execution_target.clone();
        app.open_breakpoint_hits().unwrap();
        assert_eq!(app.breakpoint_hit_picker.as_ref().unwrap().hits.len(), 2);
        app.move_breakpoint_hit(1);
        assert_eq!(app.commit_breakpoint_hit().unwrap(), "thread/b");
        assert_eq!(app.execution_target, target);
        assert!(app.source_browser.following());
        app.open_breakpoint_hits().unwrap();
        app.move_breakpoint_hit(1);
        app.execution_states[1].revision += 1;
        assert!(app
            .commit_breakpoint_hit()
            .unwrap_err()
            .contains("same stop"));
        assert!(app.breakpoint_hit_picker.is_none());
        app.open_breakpoint_hits().unwrap();
        app.mark_running(Some("thread/a"));
        assert!(app.commit_breakpoint_hit().is_err());
        app.selected_breakpoint_site = Some(0);
        app.open_breakpoint_hits().unwrap();
        assert_eq!(app.breakpoint_hit_picker.as_ref().unwrap().hits.len(), 1);
        assert_eq!(app.commit_breakpoint_hit().unwrap(), "thread/b");
        app.backend_unavailable("gone");
        assert!(app.open_breakpoint_hits().is_err());
    }

    #[test]
    fn projects_concurrent_hits_and_excludes_all_stop_peers_and_history() {
        let mut app = fixture();
        let hits = app.breakpoint_hits();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].thread_id, "thread/a");
        assert_eq!(hits[1].thread_id, "thread/b");
        assert!(hits
            .iter()
            .all(|hit| hit.breakpoint_id == "breakpoint/shared" && hit.revision == 10));
        app.mark_running(Some("thread/a"));
        assert_eq!(app.breakpoint_hits().len(), 1);
        // A new thread-stop notification without a matching fresh execution
        // projection must not revive the previous stop's breakpoint marker.
        app.threads
            .iter_mut()
            .find(|thread| thread.id == "thread/a")
            .unwrap()
            .state = "stopped".into();
        assert_eq!(app.breakpoint_hits().len(), 1);
        app.execution_states[1].running = true;
        assert!(app.breakpoint_hits().is_empty());
        assert_eq!(app.breakpoints[0].times, 100);
    }

    #[test]
    fn rejects_wrong_member_session_unknown_breakpoint_and_disconnect() {
        let mut app = fixture();
        app.breakpoints[0].sub_breakpoints[0].session_id = "session/a".into();
        assert_eq!(app.breakpoint_hits().len(), 1);
        app.execution_states[0]
            .stop_reason
            .as_mut()
            .unwrap()
            .breakpoint_id = Some("missing".into());
        assert!(app.breakpoint_hits().is_empty());
        app = fixture();
        app.backend_unavailable("disconnected");
        assert!(app.breakpoint_hits().is_empty());
    }
}
