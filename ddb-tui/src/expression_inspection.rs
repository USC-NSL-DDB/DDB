use super::*;

impl App {
    pub fn start_expression_inspection(&mut self) -> Result<(), String> {
        if !self.capabilities.supports_inspection("evaluate") {
            return Err("DDB does not support expression evaluation".into());
        }
        let frame = self
            .current_frame()
            .filter(|frame| !frame.boundary)
            .ok_or("Select an inspectable frame before inspecting an expression")?;
        let origin = self
            .current_thread()
            .filter(|thread| thread.state == "stopped")
            .ok_or("Select a stopped thread before inspecting an expression")?;
        let request = ExpressionInspection {
            id: self.next_expression_id,
            expression: String::new(),
            generation: self.inspection_generation,
            origin_thread: origin.id.clone(),
            owner_thread: frame.thread_id.clone(),
            frame_id: frame.id.clone(),
        };
        self.next_expression_id = self.next_expression_id.wrapping_add(1).max(1);
        self.clear_expression_inspection();
        self.expression_inspection = Some(request);
        self.start_input(InputMode::InspectExpression);
        Ok(())
    }

    pub fn prepare_expression_inspection(
        &mut self,
        expression: String,
    ) -> Result<ExpressionInspection, String> {
        if expression.trim().is_empty() {
            self.clear_expression_inspection();
            return Err("Enter an expression to inspect".into());
        }
        let mut request = self
            .expression_inspection
            .clone()
            .ok_or("Inspection context changed; open Inspect again")?;
        if !request.expression.is_empty() {
            return Err("Inspection already submitted; open Inspect again".into());
        }
        if !self.inspection_is_current(request.generation, &request.origin_thread)
            || !self
                .current_frame()
                .is_some_and(|frame| frame.id == request.frame_id && !frame.boundary)
            || !self
                .current_thread()
                .is_some_and(|thread| thread.state == "stopped")
        {
            self.clear_expression_inspection();
            return Err("Inspection context changed; open Inspect again".into());
        }
        request.expression = expression;
        self.variables.insert(
            0,
            Variable {
                id: String::new(),
                inspection: true,
                address: None,
                watch_id: None,
                scope: false,
                loading: true,
                name: format!("Inspect: {}", request.expression),
                type_name: String::new(),
                value: "Evaluating…".into(),
                has_children: false,
                children: 0,
                depth: 0,
                expanded: false,
            },
        );
        self.expression_inspection = Some(request.clone());
        self.selected_variable = 0;
        self.focus = Focus::Variables;
        Ok(request)
    }

    pub fn clear_expression_inspection(&mut self) {
        self.expression_inspection = None;
        let mut in_inspector = false;
        self.variables.retain(|variable| {
            if variable.depth == 0 {
                in_inspector = variable.inspection;
            }
            !in_inspector
        });
        self.selected_variable = self
            .selected_variable
            .min(self.variables.len().saturating_sub(1));
    }

    pub fn finish_expression_inspection(
        &mut self,
        request: ExpressionInspection,
        result: Result<v2::EvaluationResult, String>,
    ) {
        if !self
            .expression_inspection
            .as_ref()
            .is_some_and(|current| current.id == request.id)
            || !self.inspection_is_current(request.generation, &request.origin_thread)
            || !self
                .current_frame()
                .is_some_and(|frame| frame.id == request.frame_id)
        {
            return;
        }
        let Some(row) = self.variables.iter_mut().find(|row| row.inspection) else {
            return;
        };
        row.loading = false;
        match result {
            Ok(result) => {
                row.id = result.variable_id.unwrap_or_default();
                row.address = result.address;
                row.value = result.value;
                row.type_name = result.type_name.unwrap_or_default();
                row.has_children = self.values_formatting.expandable()
                    && result.has_children
                    && !row.id.is_empty();
                row.children = result.child_count.unwrap_or_default() as usize;
            }
            Err(error) => row.value = format!("Unavailable: {error}"),
        }
    }
}
