use super::*;

impl App {
    pub fn start_watch_edit(&mut self) -> Result<(), String> {
        let root = self
            .variables
            .get(..=self.selected_variable)
            .and_then(|variables| variables.iter().rev().find(|variable| variable.depth == 0))
            .and_then(|variable| variable.watch_id)
            .ok_or("Select a retained watch or one of its children to edit")?;
        let watch = self
            .watches
            .iter()
            .find(|watch| watch.id == root)
            .ok_or("Selected watch no longer exists")?
            .clone();
        self.start_input(InputMode::EditWatch);
        self.editing_watch = Some(watch.id);
        self.insert_input(&watch.expression);
        Ok(())
    }

    pub fn commit_watch_edit(
        &mut self,
        expression: String,
    ) -> Result<Option<WatchEvaluation>, String> {
        let id = self.editing_watch.take().ok_or("No watch edit is active")?;
        if expression.trim().is_empty() {
            return Err("Enter a watch expression; use Delete to remove a watch".into());
        }
        let watch = self
            .watches
            .iter_mut()
            .find(|watch| watch.id == id)
            .ok_or("Watch was removed while editing")?;
        if watch.expression == expression {
            return Ok(None);
        }
        watch.expression = expression;
        // Invalidate pending evaluations even when no stopped frame is available.
        watch.revision += 1;
        let row = watch_variable(watch);
        let index = if let Some(start) = self
            .variables
            .iter()
            .position(|variable| variable.watch_id == Some(id))
        {
            let end = self.variables[start + 1..]
                .iter()
                .position(|variable| variable.depth == 0)
                .map_or(self.variables.len(), |offset| start + 1 + offset);
            self.variables.drain(start..end);
            start
        } else {
            0
        };
        self.variables.insert(index, row);
        self.selected_variable = index;
        self.focus = Focus::Variables;
        Ok(self.prepare_watch(id))
    }

    pub fn add_watch(&mut self, expression: String) -> Result<(), String> {
        if expression.trim().is_empty() {
            return Err("Enter a watch expression".into());
        }
        if self.watches.len() >= 64 {
            return Err("At most 64 watch expressions are supported".into());
        }
        self.watches.push(WatchExpression {
            id: self.next_watch_id,
            expression,
            revision: 0,
        });
        self.next_watch_id += 1;
        self.reset_watch_rows();
        self.selected_variable = self.watches.len() - 1;
        self.focus = Focus::Variables;
        Ok(())
    }

    pub fn remove_selected_watch(&mut self) -> bool {
        let Some(id) = self
            .variables
            .get(self.selected_variable)
            .and_then(|v| v.watch_id)
        else {
            return false;
        };
        let start = self.selected_variable;
        let end = self.variables[start + 1..]
            .iter()
            .position(|v| v.depth == 0)
            .map_or(self.variables.len(), |offset| start + 1 + offset);
        self.variables.drain(start..end);
        self.watches.retain(|watch| watch.id != id);
        self.selected_variable = self
            .selected_variable
            .min(self.variables.len().saturating_sub(1));
        true
    }

    pub(super) fn reset_watch_rows(&mut self) {
        let mut in_watch = false;
        self.variables.retain(|variable| {
            if variable.depth == 0 {
                in_watch = variable.watch_id.is_some();
            }
            !in_watch
        });
        let roots = self.watches.iter().map(watch_variable).collect::<Vec<_>>();
        self.variables.splice(0..0, roots);
    }

    pub fn prepare_watches(&mut self) -> Vec<WatchEvaluation> {
        self.reset_watch_rows();
        let ids = self
            .watches
            .iter()
            .map(|watch| watch.id)
            .collect::<Vec<_>>();
        ids.into_iter()
            .filter_map(|id| self.prepare_watch(id))
            .collect()
    }

    fn prepare_watch(&mut self, id: u64) -> Option<WatchEvaluation> {
        let frame = self
            .current_frame()
            .filter(|frame| !frame.boundary)
            .cloned()?;
        let origin = self
            .current_thread()
            .filter(|thread| thread.state == "stopped")
            .map(|thread| thread.id.clone())?;
        if let Some(variable) = self
            .variables
            .iter_mut()
            .find(|variable| variable.watch_id == Some(id))
        {
            variable.loading = true;
            variable.value = "Evaluating…".into();
        }
        let watch = self.watches.iter_mut().find(|watch| watch.id == id)?;
        watch.revision += 1;
        Some(WatchEvaluation {
            watch: watch.clone(),
            generation: self.inspection_generation,
            origin_thread: origin,
            owner_thread: frame.thread_id,
            frame_id: frame.id,
        })
    }

    pub fn finish_watch(
        &mut self,
        request: WatchEvaluation,
        result: Result<v2::EvaluationResult, String>,
    ) {
        if !self.inspection_is_current(request.generation, &request.origin_thread)
            || !self
                .current_frame()
                .is_some_and(|frame| frame.id == request.frame_id)
            || !self.watches.iter().any(|watch| {
                watch.id == request.watch.id && watch.revision == request.watch.revision
            })
        {
            return;
        }
        let Some(variable) = self
            .variables
            .iter_mut()
            .find(|v| v.watch_id == Some(request.watch.id))
        else {
            return;
        };
        variable.loading = false;
        match result {
            Ok(result) => {
                variable.id = result.variable_id.unwrap_or_default();
                variable.address = result.address;
                variable.value = result.value;
                variable.type_name = result.type_name.unwrap_or_default();
                variable.has_children = self.values_formatting.expandable()
                    && result.has_children
                    && !variable.id.is_empty();
                variable.children = result.child_count.unwrap_or_default() as usize;
            }
            Err(error) => variable.value = format!("Unavailable: {error}"),
        }
    }
}

fn watch_variable(watch: &WatchExpression) -> Variable {
    Variable {
        id: String::new(),
        inspection: false,
        address: None,
        watch_id: Some(watch.id),
        scope: false,
        loading: false,
        name: format!("Watch: {}", watch.expression),
        type_name: String::new(),
        value: "Waiting for a stopped frame".into(),
        has_children: false,
        children: 0,
        depth: 0,
        expanded: false,
    }
}
