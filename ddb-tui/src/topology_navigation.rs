use super::*;

impl App {
    pub fn topology_rows(&self) -> Vec<TopologyRow> {
        let mut hidden_below = None;
        self.full_topology_rows()
            .into_iter()
            .filter(|row| {
                if self.topology_sessions_only && matches!(row.kind, TopologyRowKind::Thread(_)) {
                    return false;
                }
                if hidden_below.is_some_and(|depth| row.depth > depth) {
                    return false;
                }
                hidden_below = self
                    .collapsed_topology
                    .contains(&row.kind)
                    .then_some(row.depth);
                true
            })
            .collect()
    }

    pub fn topology_can_fold(&self, kind: &TopologyRowKind) -> bool {
        match kind {
            TopologyRowKind::Group(id) => self
                .sessions
                .iter()
                .any(|session| session.group_id.as_deref().unwrap_or("") == id),
            TopologyRowKind::Session(id) => {
                !self.topology_sessions_only
                    && self.threads.iter().any(|thread| thread.session_id == *id)
            }
            TopologyRowKind::Thread(_) => false,
        }
    }

    pub fn topology_collapsed(&self, kind: &TopologyRowKind) -> bool {
        self.collapsed_topology.contains(kind)
    }

    fn topology_parent(&self, kind: &TopologyRowKind) -> Option<TopologyRowKind> {
        match kind {
            TopologyRowKind::Thread(id) => self
                .threads
                .iter()
                .find(|thread| thread.id == *id)
                .map(|thread| TopologyRowKind::Session(thread.session_id.clone())),
            TopologyRowKind::Session(id) if self.topology_grouped => self
                .sessions
                .iter()
                .find(|session| session.session_id == *id)
                .map(|session| {
                    TopologyRowKind::Group(session.group_id.clone().unwrap_or_default())
                }),
            _ => None,
        }
    }

    pub fn topology_index(&self, kind: &TopologyRowKind) -> Option<usize> {
        self.topology_rows()
            .iter()
            .position(|row| row.kind == *kind)
    }

    pub(super) fn visible_topology_ancestor(
        &self,
        selected: Option<&TopologyRowKind>,
    ) -> Option<usize> {
        let rows = self.topology_rows();
        let mut candidate = selected.cloned();
        while let Some(kind) = candidate {
            if let Some(index) = rows.iter().position(|row| row.kind == kind) {
                return Some(index);
            }
            candidate = self.topology_parent(&kind);
        }
        None
    }

    /// Rendered mouse rows carry resource identities, not mutable row offsets.
    pub fn select_visible_topology(&mut self, kind: &TopologyRowKind) -> Result<bool, String> {
        let index = self
            .topology_index(kind)
            .ok_or("Topology changed; select a current row")?;
        Ok(self.select_topology_row(index))
    }

    pub fn toggle_topology_fold(&mut self, kind: &TopologyRowKind) -> bool {
        if !self.topology_can_fold(kind) || self.topology_index(kind).is_none() {
            return false;
        }
        let collapsed = !self.collapsed_topology.remove(kind);
        if collapsed {
            self.collapsed_topology.insert(kind.clone());
        }
        self.status = format!(
            "Topology {} · execution scope unchanged ({})",
            if collapsed { "collapsed" } else { "expanded" },
            self.execution_scope_label()
        );
        true
    }

    pub fn toggle_selected_topology_fold(&mut self) -> bool {
        let kind = self
            .topology_rows()
            .get(self.selected_topology_row())
            .map(|row| row.kind.clone());
        kind.is_some_and(|kind| self.toggle_topology_fold(&kind))
    }

    /// Returns true only when navigation selected a thread requiring inspection.
    pub fn navigate_topology_tree(&mut self, expand: bool) -> bool {
        let rows = self.topology_rows();
        let index = self.selected_topology_row();
        let Some(row) = rows.get(index) else {
            return false;
        };
        if self.topology_can_fold(&row.kind) {
            if self.topology_collapsed(&row.kind) == expand {
                self.toggle_topology_fold(&row.kind);
                return false;
            }
            if expand {
                return rows
                    .get(index + 1)
                    .filter(|child| child.depth > row.depth)
                    .map(|child| child.kind.clone())
                    .is_some_and(|kind| self.select_visible_topology(&kind).unwrap_or(false));
            }
        }
        if !expand {
            if let Some(parent) = self.topology_parent(&row.kind) {
                return self.select_visible_topology(&parent).unwrap_or(false);
            }
        }
        false
    }

    pub(super) fn reveal_topology_thread(&mut self, thread_id: &str) {
        let kind = TopologyRowKind::Thread(thread_id.into());
        let mut parent = self.topology_parent(&kind);
        while let Some(kind) = parent {
            self.collapsed_topology.remove(&kind);
            parent = self.topology_parent(&kind);
        }
        self.selected_topology = if self.topology_sessions_only {
            self.topology_parent(&kind)
        } else {
            Some(kind)
        };
    }

    pub fn toggle_topology_sessions(&mut self) {
        self.topology_sessions_only = !self.topology_sessions_only;
        if self.topology_sessions_only {
            if let Some(TopologyRowKind::Thread(id)) =
                self.selected_topology.clone().or_else(|| {
                    self.current_thread()
                        .map(|thread| TopologyRowKind::Thread(thread.id.clone()))
                })
            {
                self.selected_topology = self.topology_parent(&TopologyRowKind::Thread(id));
            }
        }
        self.topology_viewport = Default::default();
        self.status = format!(
            "Topology: {} · toolbar scope unchanged",
            if self.topology_sessions_only {
                "sessions only"
            } else {
                "sessions and threads"
            }
        );
    }

    pub fn selected_topology_session_id(&self) -> Option<String> {
        match &self.topology_rows().get(self.selected_topology_row())?.kind {
            TopologyRowKind::Session(id) => Some(id.clone()),
            TopologyRowKind::Thread(id) => self
                .threads
                .iter()
                .find(|thread| thread.id == *id)
                .map(|thread| thread.session_id.clone()),
            TopologyRowKind::Group(_) => None,
        }
    }

    pub(super) fn prune_topology_folds(&mut self) {
        self.collapsed_topology.retain(|kind| match kind {
            TopologyRowKind::Group(id) => {
                self.groups.iter().any(|group| group.group_id == *id)
                    || self
                        .sessions
                        .iter()
                        .any(|session| session.group_id.as_deref().unwrap_or("") == id)
            }
            TopologyRowKind::Session(id) => self
                .sessions
                .iter()
                .any(|session| session.session_id == *id),
            TopologyRowKind::Thread(_) => false,
        });
    }
}
