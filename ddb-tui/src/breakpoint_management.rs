use super::*;

#[derive(Clone, Debug)]
pub struct DeletionChoice {
    pub target: v2::Target,
    pub label: String,
    pub warning: String,
}

#[derive(Clone, Debug)]
pub struct BreakpointDeletion {
    instance: Option<String>,
    pub id: String,
    original_target: v2::Target,
    pub choices: Vec<DeletionChoice>,
    pub cursor: usize,
}

fn rule_targets(target: &v2::Target, rules: &mut Vec<v2::Target>) {
    match target.selector.as_ref() {
        Some(target::Selector::Multiple(value)) => {
            for target in &value.targets {
                rule_targets(target, rules);
            }
        }
        Some(target::Selector::SessionSet(value)) => {
            for id in &value.session_ids {
                rules.push(session_target(id));
            }
        }
        Some(
            target::Selector::Session(_)
            | target::Selector::Group(_)
            | target::Selector::Broadcast(_),
        ) if !rules.contains(target) => rules.push(target.clone()),
        _ => {}
    }
}

impl Breakpoint {
    pub fn rule_kind(&self) -> &'static str {
        let mut rules = Vec::new();
        if let Some(target) = &self.target {
            rule_targets(target, &mut rules);
        }
        let groups = rules
            .iter()
            .filter(|target| matches!(target.selector, Some(target::Selector::Group(_))))
            .count();
        if rules.is_empty() {
            "Unknown"
        } else if groups == rules.len() {
            "Group"
        } else if groups == 0 {
            "Session"
        } else {
            "Mixed"
        }
    }
}

impl App {
    pub fn open_breakpoint_deletion(&mut self) -> Result<(), String> {
        if !self.capabilities.supports_breakpoint_action("delete") {
            return Err("Breakpoint deletion is not supported by DDB".into());
        }
        let bp = self
            .current_breakpoint()
            .ok_or("Select a breakpoint first")?;
        let target = bp
            .target
            .as_ref()
            .ok_or("Breakpoint has no explicit target; refresh first")?;
        let mut rules = Vec::new();
        rule_targets(target, &mut rules);
        if rules.is_empty() {
            return Err("Breakpoint has no manageable session or group rules".into());
        }
        let selected = self
            .selected_breakpoint_site
            .and_then(|index| bp.sub_breakpoints.get(index))
            .map(|member| {
                member
                    .inherited_from_group_id
                    .as_ref()
                    .map_or_else(|| session_target(&member.session_id), group_target)
            });
        let cursor = selected
            .as_ref()
            .and_then(|target| rules.iter().position(|rule| rule == target))
            .unwrap_or(0);
        let mut choices = rules.into_iter().map(|target| {
            let group = matches!(target.selector, Some(target::Selector::Group(_)));
            DeletionChoice {
                label: format!("{} rule · {}", if group { "Group" } else { "Session" }, self.target_label(Some(&target))),
                warning: if group {
                    "Removes this group rule from all current and future members. Other rules are retained."
                } else { "Removes only this independent session rule. Other rules are retained." }.into(),
                target,
            }
        }).collect::<Vec<_>>();
        if choices.len() > 1 {
            choices.push(DeletionChoice {
                target: target.clone(),
                label: "ALL rules of this logical breakpoint".into(),
                warning: "Removes every session and group rule for this logical breakpoint.".into(),
            });
        }
        self.breakpoint_deletion = Some(BreakpointDeletion {
            instance: self.source_server_instance.clone(),
            id: bp.id.clone(),
            original_target: target.clone(),
            choices,
            cursor,
        });
        Ok(())
    }

    pub fn move_breakpoint_deletion(&mut self, delta: isize) {
        if let Some(picker) = self.breakpoint_deletion.as_mut() {
            picker.cursor = move_index(picker.cursor, picker.choices.len(), delta);
        }
    }

    pub fn commit_breakpoint_deletion(&mut self) -> Result<BackendRequest, String> {
        let picker = self
            .breakpoint_deletion
            .take()
            .ok_or("No breakpoint deletion is pending")?;
        if picker.instance != self.source_server_instance
            || !self
                .breakpoints
                .iter()
                .any(|bp| bp.id == picker.id && bp.target.as_ref() == Some(&picker.original_target))
        {
            return Err(
                "Breakpoint rules or DDB instance changed; select the rule and confirm again"
                    .into(),
            );
        }
        let choice = picker
            .choices
            .get(picker.cursor)
            .ok_or("Select a rule to delete")?;
        Ok(BackendRequest::DeleteBreakpoint {
            id: picker.id,
            target: choice.target.clone(),
        })
    }
}
