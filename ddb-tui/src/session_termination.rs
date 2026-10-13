use super::*;

/// Frozen identity and resource version, shared by confirmation and worker.
#[derive(Clone, Debug)]
pub struct SessionTermination {
    pub instance: String,
    pub session: v2::Session,
    pub scroll: u16,
}

impl SessionTermination {
    pub fn validate(&self, instance: &str, sessions: &[v2::Session]) -> Result<(), String> {
        if instance != self.instance {
            return Err("DDB instance changed; select the session and confirm again".into());
        }
        let current = sessions
            .iter()
            .find(|session| session.session_id == self.session.session_id)
            .ok_or("Session no longer exists; termination cancelled")?;
        if current.revision != self.session.revision
            || current.process_id != self.session.process_id
            || current.created_at != self.session.created_at
            || !terminable(current)
        {
            return Err("Session changed or exited; select it and confirm again".into());
        }
        Ok(())
    }
}

fn terminable(session: &v2::Session) -> bool {
    matches!(
        v2::SessionStatus::try_from(session.status),
        Ok(v2::SessionStatus::Ready | v2::SessionStatus::Running | v2::SessionStatus::Stopped)
    )
}

impl App {
    pub fn open_session_termination(&mut self) -> Result<(), String> {
        let id = match self
            .topology_rows()
            .get(self.selected_topology_row())
            .map(|row| &row.kind)
        {
            Some(TopologyRowKind::Session(id)) => id.clone(),
            Some(TopologyRowKind::Thread(id)) => self
                .threads
                .iter()
                .find(|thread| &thread.id == id)
                .map(|thread| thread.session_id.clone())
                .ok_or("Selected thread no longer exists")?,
            _ => return Err("Select one session or thread in Topology, not a group".into()),
        };
        if !self
            .capabilities
            .supports_execution_target(v2::ExecutionAction::Signal, &session_target(id.clone()))
        {
            return Err("DDB does not support sending signals to a session".into());
        }
        let session = self
            .sessions
            .iter()
            .find(|session| session.session_id == id)
            .filter(|session| terminable(session))
            .ok_or("Select a ready, running, or stopped session")?
            .clone();
        let instance = self
            .source_server_instance
            .clone()
            .filter(|id| !id.is_empty())
            .ok_or("Wait for a DDB snapshot before terminating a session")?;
        self.session_termination = Some(SessionTermination {
            instance,
            session,
            scroll: 0,
        });
        Ok(())
    }

    pub fn cancel_session_termination(&mut self) {
        self.session_termination = None;
        self.status = "Session termination cancelled".into();
    }

    pub fn commit_session_termination(&mut self) -> Result<SessionTermination, String> {
        let request = self
            .session_termination
            .take()
            .ok_or("No session termination is pending")?;
        request.validate(
            self.source_server_instance.as_deref().unwrap_or(""),
            &self.sessions,
        )?;
        Ok(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn termination_rejects_removed_changed_exited_and_replaced_sessions() {
        let session = v2::Session {
            session_id: "session/original".into(),
            revision: 3,
            process_id: Some("123".into()),
            status: v2::SessionStatus::Stopped as i32,
            ..Default::default()
        };
        let request = SessionTermination {
            instance: "backend".into(),
            session: session.clone(),
            scroll: 0,
        };
        assert!(request
            .validate("backend", std::slice::from_ref(&session))
            .is_ok());
        assert!(request
            .validate("replacement", std::slice::from_ref(&session))
            .is_err());
        assert!(request.validate("backend", &[]).is_err());
        for changed in [
            v2::Session {
                revision: 4,
                ..session.clone()
            },
            v2::Session {
                process_id: Some("456".into()),
                ..session.clone()
            },
            v2::Session {
                status: v2::SessionStatus::Exited as i32,
                ..session.clone()
            },
            v2::Session {
                status: v2::SessionStatus::Failed as i32,
                ..session.clone()
            },
        ] {
            assert!(request.validate("backend", &[changed]).is_err());
        }
    }
}
