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

impl App {
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
        if !self.breakpoint_hits().contains(hit) {
            return Err(
                "This breakpoint hit is no longer paused at the same stop. Reopen the picker."
                    .into(),
            );
        }
        self.selected_topology = Some(TopologyRowKind::Thread(hit.thread_id.clone()));
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
