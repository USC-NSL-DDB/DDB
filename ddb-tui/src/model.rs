use std::collections::VecDeque;

use ddb_api_client::v2::{self, breakpoint_spec, extension_payload, operation_result, target};
use ratatui::layout::Rect;
use serde_json::Value;

use crate::api::{group_target, session_target, thread_target, BreakpointTarget, CapabilitiesExt};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Focus {
    Threads,
    Breakpoints,
    Source,
    Stack,
    Variables,
    Extensions,
    Timeline,
}

impl Focus {
    const CORE: [Self; 6] = [
        Self::Threads,
        Self::Breakpoints,
        Self::Source,
        Self::Stack,
        Self::Variables,
        Self::Timeline,
    ];
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputMode {
    Normal,
    Command,
    Evaluate,
    Memory,
    Jump,
    Signal,
    GotoLine,
    Breakpoint,
    ExtensionAction,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Control {
    Continue,
    Interrupt,
    Next,
    StepIn,
    StepOut,
    CycleScope,
    Refresh,
    RefreshStack,
}

impl Control {
    pub fn action_name(self) -> Option<&'static str> {
        match self {
            Self::Continue => Some("continue"),
            Self::Interrupt => Some("interrupt"),
            Self::Next => Some("next"),
            Self::StepIn => Some("step_in"),
            Self::StepOut => Some("step_out"),
            Self::CycleScope | Self::Refresh | Self::RefreshStack => None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct UiAreas {
    pub panels: Vec<PanelHitArea>,
    pub controls: Vec<(Control, Rect)>,
    pub breakpoint_rows: Vec<(usize, Rect)>,
    pub extension_action_rows: Vec<(usize, Rect)>,
    pub breakpoint_target_rows: Vec<(usize, Rect)>,
    pub breakpoint_target_apply: Option<Rect>,
    pub breakpoint_target_cancel: Option<Rect>,
    pub signal_rows: Vec<(usize, Rect)>,
    pub signal_cancel: Option<Rect>,
}

#[derive(Clone, Debug)]
pub struct PanelHitArea {
    pub focus: Focus,
    pub area: Rect,
    pub first_item: usize,
    pub item_height: usize,
}

impl UiAreas {
    pub fn panel_at(&self, column: u16, row: u16) -> Option<Focus> {
        self.panels
            .iter()
            .find_map(|panel| contains(panel.area, column, row).then_some(panel.focus))
    }

    pub fn control_at(&self, column: u16, row: u16) -> Option<Control> {
        self.controls
            .iter()
            .find_map(|(control, area)| contains(*area, column, row).then_some(*control))
    }

    pub fn item_at(&self, column: u16, row: u16) -> Option<(Focus, usize)> {
        let panel = self
            .panels
            .iter()
            .find(|panel| panel.item_height > 0 && contains_inner(panel.area, column, row))?;
        let relative = row.saturating_sub(panel.area.y.saturating_add(1)) as usize;
        Some((
            panel.focus,
            panel.first_item + relative / panel.item_height.max(1),
        ))
    }

    pub fn add_panel(&mut self, focus: Focus, area: Rect, first_item: usize, item_height: usize) {
        debug_assert!(item_height > 0);
        self.panels.push(PanelHitArea {
            focus,
            area,
            first_item,
            item_height,
        });
    }

    pub fn add_focus_panel(&mut self, focus: Focus, area: Rect) {
        self.panels.push(PanelHitArea {
            focus,
            area,
            first_item: 0,
            item_height: 0,
        });
    }
    pub fn breakpoint_at(&self, column: u16, row: u16) -> Option<usize> {
        self.breakpoint_rows
            .iter()
            .find_map(|(index, area)| contains(*area, column, row).then_some(*index))
    }

    pub fn extension_action_at(&self, column: u16, row: u16) -> Option<usize> {
        self.extension_action_rows
            .iter()
            .find_map(|(index, area)| contains(*area, column, row).then_some(*index))
    }

    pub fn signal_at(&self, column: u16, row: u16) -> Option<usize> {
        self.signal_rows
            .iter()
            .find_map(|(index, area)| contains(*area, column, row).then_some(*index))
    }

    pub fn breakpoint_target_at(&self, column: u16, row: u16) -> Option<usize> {
        self.breakpoint_target_rows
            .iter()
            .find_map(|(index, area)| contains(*area, column, row).then_some(*index))
    }

    pub fn breakpoint_target_apply_at(&self, column: u16, row: u16) -> bool {
        self.breakpoint_target_apply
            .is_some_and(|area| contains(area, column, row))
    }

    pub fn breakpoint_target_cancel_at(&self, column: u16, row: u16) -> bool {
        self.breakpoint_target_cancel
            .is_some_and(|area| contains(area, column, row))
    }
    pub fn signal_cancel_at(&self, column: u16, row: u16) -> bool {
        self.signal_cancel
            .is_some_and(|area| contains(area, column, row))
    }
}

fn contains(area: Rect, x: u16, y: u16) -> bool {
    x >= area.x
        && x < area.x.saturating_add(area.width)
        && y >= area.y
        && y < area.y.saturating_add(area.height)
}

fn contains_inner(area: Rect, x: u16, y: u16) -> bool {
    area.width > 2
        && area.height > 2
        && x > area.x
        && x < area.x.saturating_add(area.width).saturating_sub(1)
        && y > area.y
        && y < area.y.saturating_add(area.height).saturating_sub(1)
}

#[derive(Clone, Debug)]
pub struct ThreadItem {
    pub id: String,
    pub session_id: String,
    pub name: String,
    pub state: String,
    pub function: String,
    pub file: Option<String>,
    pub line: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TopologyRowKind {
    Group(String),
    Session(String),
    Thread(String),
}

#[derive(Clone, Debug)]
pub struct TopologyRow {
    pub kind: TopologyRowKind,
    pub depth: usize,
    pub label: String,
    pub detail: String,
    pub state: Option<String>,
}

#[derive(Clone, Debug)]
pub struct StackFrame {
    pub id: String,
    pub distributed_index: usize,
    pub session_id: String,
    pub thread_id: String,
    pub level: usize,
    pub address: String,
    pub function: String,
    pub file: Option<String>,
    pub line: Option<u64>,
    pub boundary: bool,
}

#[derive(Clone, Debug)]
pub struct Variable {
    pub id: String,
    pub name: String,
    pub type_name: String,
    pub value: String,
    pub has_children: bool,
    pub children: usize,
    pub depth: usize,
    pub expanded: bool,
}

#[derive(Clone, Debug)]
pub struct Register {
    pub name: String,
    pub value: String,
    pub unavailable: bool,
}

#[derive(Clone, Debug)]
pub struct MemoryBlock {
    pub begin: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct SourceView {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub total_lines: Option<usize>,
    pub lines: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceLocation {
    pub source: String,
    pub line: usize,
}

#[derive(Clone, Debug)]
pub struct ExtensionPanelView {
    pub extension_title: String,
    pub description: String,
    pub panel_title: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

#[derive(Clone, Debug)]
pub struct ExtensionActionView {
    pub extension_id: String,
    pub extension_version: String,
    pub extension_title: String,
    pub action_id: String,
    pub title: String,
    pub description: String,
    pub request_schema_uri: String,
}

/// Breakpoint fields arranged for rendering. This is deliberately a UI view,
/// not a wire DTO; it is derived only from the generated public v2 contract.
#[derive(Clone, Debug)]
pub struct Breakpoint {
    pub id: String,
    pub target: Option<v2::Target>,
    pub location: BreakpointLocation,
    pub enabled: bool,
    pub condition: Option<String>,
    pub ignore_count: Option<u64>,
    pub temporary: bool,
    pub hardware: bool,
    pub times: u64,
    pub verified: bool,
    pub pending: bool,
    pub message: Option<String>,
    pub sub_breakpoints: Vec<v2::SubBreakpoint>,
}

#[derive(Clone, Debug, Default)]
pub struct BreakpointLocation {
    pub src: String,
    pub line: u64,
}

#[derive(Clone, Debug)]
pub struct BreakpointDraft {
    pub source: String,
    pub line: u64,
    pub options: BreakpointOptions,
}

#[derive(Clone, Debug)]
pub struct BreakpointTargetChoice {
    pub target: BreakpointTarget,
    pub label: String,
    pub detail: String,
    pub selected: bool,
}

#[derive(Clone, Debug)]
pub struct BreakpointTargetPicker {
    pub draft: BreakpointDraft,
    pub choices: Vec<BreakpointTargetChoice>,
    pub cursor: usize,
}

#[derive(Clone, Debug)]
pub struct SignalPicker {
    pub target: v2::Target,
    pub signals: Vec<v2::DebuggerSignal>,
    pub cursor: usize,
}

pub struct App {
    pub api_endpoint: String,
    pub api_protocol: String,
    pub api_connected: bool,
    pub event_stream_connected: bool,
    pub capabilities: v2::Capabilities,
    pub focus: Focus,
    pub input_mode: InputMode,
    pub input: String,
    pub signal_picker: Option<SignalPicker>,
    pub registers: Vec<Register>,
    pub input_cursor: usize,
    input_history: VecDeque<String>,
    history_index: Option<usize>,
    history_draft: String,
    pub show_help: bool,
    pub should_quit: bool,
    pub sessions: Vec<v2::Session>,
    pub groups: Vec<v2::Group>,
    pub threads: Vec<ThreadItem>,
    pub frames: Vec<StackFrame>,
    pub distributed_stack_truncation: Option<String>,
    pub variables: Vec<Variable>,
    pub memory: Vec<MemoryBlock>,
    pub extension_actions: Vec<ExtensionActionView>,
    pub selected_extension_action: usize,
    pub pending_extension_action: Option<ExtensionActionView>,
    pub extension_panels: Vec<ExtensionPanelView>,
    pub breakpoints: Vec<Breakpoint>,
    pub breakpoint_target_picker: Option<BreakpointTargetPicker>,
    pub execution_target: Option<v2::Target>,
    pub selected_thread: usize,
    pub selected_frame: usize,
    selected_topology: Option<TopologyRowKind>,
    pub selected_variable: usize,
    pub selected_breakpoint: usize,
    pub inspection_generation: u64,
    inspected_thread_id: Option<String>,
    pub source: Option<SourceView>,
    pub execution_location: Option<SourceLocation>,
    pub source_cursor: Option<SourceLocation>,
    source_cursor_pinned: bool,
    pub source_scroll: usize,
    pub timeline_scroll: usize,
    pub extension_scroll: usize,
    pub timeline: VecDeque<String>,
    pub status: String,
    pub pending_commands: usize,
    pub areas: UiAreas,
}

impl App {
    pub fn new(api_endpoint: String) -> Self {
        Self {
            api_endpoint,
            api_protocol: "negotiating".to_string(),
            api_connected: false,
            event_stream_connected: false,
            capabilities: v2::Capabilities::default(),
            focus: Focus::Source,
            signal_picker: None,
            input_mode: InputMode::Normal,
            registers: Vec::new(),
            input: String::new(),
            input_cursor: 0,
            input_history: VecDeque::with_capacity(100),
            history_index: None,
            history_draft: String::new(),
            show_help: false,
            should_quit: false,
            sessions: Vec::new(),
            groups: Vec::new(),
            threads: Vec::new(),
            extension_actions: Vec::new(),
            selected_extension_action: 0,
            pending_extension_action: None,
            frames: Vec::new(),
            distributed_stack_truncation: None,
            variables: Vec::new(),
            memory: Vec::new(),
            extension_panels: Vec::new(),
            breakpoint_target_picker: None,
            execution_target: None,
            breakpoints: Vec::new(),
            selected_thread: 0,
            selected_topology: None,
            selected_frame: 0,
            selected_variable: 0,
            selected_breakpoint: 0,
            inspection_generation: 0,
            inspected_thread_id: None,
            source: None,
            execution_location: None,
            source_cursor: None,
            source_cursor_pinned: false,
            source_scroll: 0,
            timeline_scroll: 0,
            extension_scroll: 0,
            timeline: VecDeque::with_capacity(500),
            status: "connecting to DDB…".to_string(),
            pending_commands: 0,
            areas: UiAreas::default(),
        }
    }

    pub fn apply_capabilities(&mut self, capabilities: v2::Capabilities) {
        self.api_protocol = protocol_label(&capabilities);
        self.extension_actions = extension_action_views(&capabilities.extensions);
        self.selected_extension_action =
            bounded(self.selected_extension_action, self.extension_actions.len());
        self.capabilities = capabilities;
    }

    pub fn start_extension_action_input(&mut self) -> Result<(), String> {
        let action = self
            .extension_actions
            .get(self.selected_extension_action)
            .cloned()
            .ok_or_else(|| "select a declared extension action".to_string())?;
        self.pending_extension_action = Some(action);
        self.start_input(InputMode::ExtensionAction);
        self.input = "{}".to_string();
        self.input_cursor = 1;
        Ok(())
    }

    pub fn apply_snapshot(&mut self, snapshot: v2::Snapshot) {
        let selected_thread_id = self.current_thread().map(|thread| thread.id.clone());
        if let Some(capabilities) = snapshot.capabilities.clone() {
            self.apply_capabilities(capabilities);
        }
        self.sessions = snapshot.sessions;
        self.groups = snapshot.groups;
        self.breakpoints = snapshot
            .breakpoints
            .into_iter()
            .filter_map(breakpoint_view)
            .collect();
        self.pending_commands = snapshot.pending_commands.len();
        self.extension_panels =
            extension_panel_views(&self.capabilities.extensions, &snapshot.extension_states);
        if self.extension_panels.is_empty()
            && self.extension_actions.is_empty()
            && self.focus == Focus::Extensions
        {
            self.focus = Focus::Timeline;
        }
        self.api_connected = true;
        self.status = format!(
            "{} sessions · {} groups · {} breakpoints",
            self.sessions.len(),
            self.groups.len(),
            self.breakpoints.len()
        );
        self.selected_breakpoint = bounded(self.selected_breakpoint, self.breakpoints.len());
        self.threads.retain(|thread| {
            self.sessions
                .iter()
                .any(|session| session.session_id == thread.session_id)
        });
        self.selected_thread = selected_thread_id
            .as_ref()
            .and_then(|id| self.threads.iter().position(|thread| thread.id == *id))
            .unwrap_or_else(|| bounded(self.selected_thread, self.threads.len()));
        if self.current_thread().map(|thread| thread.id.as_str()) != selected_thread_id.as_deref() {
            self.invalidate_inspection();
        }
        if self.sessions.is_empty() {
            self.threads.clear();
            self.frames.clear();
            self.variables.clear();
            self.registers.clear();
            self.memory.clear();
            self.source = None;
            self.distributed_stack_truncation = None;
            self.execution_location = None;
            self.source_cursor = None;
            self.source_cursor_pinned = false;
            self.selected_thread = 0;
            self.selected_topology = None;
            self.selected_frame = 0;
            self.selected_variable = 0;
            self.inspection_generation = self.inspection_generation.wrapping_add(1).max(1);
        } else {
            self.reconcile_topology_selection();
        }
    }

    pub fn apply_threads(&mut self, items: Vec<v2::Thread>) {
        let selected_id = self.current_thread().map(|thread| thread.id.clone());
        let reported_selected_id = items
            .iter()
            .find(|thread| thread.selected)
            .map(|thread| thread.thread_id.clone());
        let mut threads = items
            .into_iter()
            .map(|thread| {
                let location = thread.location.as_ref();
                let id = thread.thread_id;
                ThreadItem {
                    name: thread
                        .name
                        .or(thread.backend_thread_id)
                        .unwrap_or_else(|| format!("thread {id}")),
                    id,
                    session_id: thread.session_id,
                    state: thread_state_name(thread.state).to_string(),
                    function: location
                        .and_then(|location| location.function_name.clone())
                        .unwrap_or_default(),
                    file: location.and_then(|location| location.path.clone()),
                    line: location.map(|location| u64::from(location.line)),
                }
            })
            .collect::<Vec<_>>();
        threads.sort_by(|left, right| left.id.cmp(&right.id));
        self.threads = threads;
        // `Thread.selected` is backend-session local: more than one session can
        // legitimately report a selected thread. Preserve the TUI's global
        // navigation choice while that thread still exists, and only consult
        // the backend flags when establishing or recovering a selection.
        self.selected_thread = selected_id
            .clone()
            .or(reported_selected_id)
            .and_then(|id| self.threads.iter().position(|thread| thread.id == id))
            .unwrap_or_else(|| bounded(self.selected_thread, self.threads.len()));
        let target_is_valid =
            self.execution_target
                .as_ref()
                .is_some_and(|target| match target.selector.as_ref() {
                    Some(target::Selector::Thread(value)) => self
                        .threads
                        .iter()
                        .any(|thread| thread.id == value.thread_id),
                    Some(target::Selector::Session(value)) => self
                        .sessions
                        .iter()
                        .any(|session| session.session_id == value.session_id),
                    Some(target::Selector::Group(value)) => self
                        .groups
                        .iter()
                        .any(|group| group.group_id == value.group_id),
                    Some(target::Selector::SessionSet(value)) => {
                        value.session_ids.iter().all(|id| {
                            self.sessions
                                .iter()
                                .any(|session| session.session_id == *id)
                        })
                    }
                    Some(target::Selector::Broadcast(_) | target::Selector::Multiple(_)) => true,
                    _ => false,
                });
        if !target_is_valid {
            self.execution_target = self
                .current_thread()
                .map(|thread| thread_target(thread.id.clone()));
        }
        let current_id = self.current_thread().map(|thread| thread.id.clone());
        let current_is_stopped = self
            .current_thread()
            .is_some_and(|thread| thread.state.to_ascii_lowercase().contains("stop"));
        self.reconcile_topology_selection();
        if current_id != selected_id || !current_is_stopped {
            self.invalidate_inspection();
        }
    }

    pub fn apply_frames(&mut self, items: Vec<v2::Frame>) {
        self.distributed_stack_truncation = None;
        let session_id = self
            .current_thread()
            .map(|thread| thread.session_id.clone())
            .unwrap_or_default();
        let frames = items
            .into_iter()
            .enumerate()
            .map(|(distributed_index, frame)| {
                let location = frame.location.as_ref();
                StackFrame {
                    id: frame.frame_id,
                    distributed_index,
                    session_id: session_id.clone(),
                    thread_id: frame.thread_id,
                    level: frame.level as usize,
                    address: location
                        .and_then(|location| location.address.clone())
                        .unwrap_or_default(),
                    function: frame
                        .function_name
                        .or_else(|| location.and_then(|location| location.function_name.clone()))
                        .unwrap_or_else(|| "??".to_string()),
                    file: location.and_then(|location| location.path.clone()),
                    line: location.map(|location| u64::from(location.line)),
                    boundary: false,
                }
            })
            .collect::<Vec<_>>();
        self.apply_stack_frames(frames);
    }

    pub fn apply_distributed_frames(&mut self, items: Vec<v2::DistributedFrame>) {
        self.distributed_stack_truncation = None;
        let frames = items
            .into_iter()
            .map(|distributed| {
                let boundary = distributed.boundary.is_some() || distributed.frame.is_none();
                let frame = distributed.frame.unwrap_or_default();
                let location = frame.location.as_ref();
                StackFrame {
                    id: frame.frame_id,
                    distributed_index: distributed.index as usize,
                    session_id: distributed.session_id,
                    thread_id: distributed.thread_id,
                    level: frame.level as usize,
                    address: location
                        .and_then(|location| location.address.clone())
                        .unwrap_or_default(),
                    function: if boundary {
                        distributed
                            .boundary_label
                            .clone()
                            .unwrap_or_else(|| "distributed call boundary".to_string())
                    } else {
                        frame
                            .function_name
                            .or_else(|| {
                                location.and_then(|location| location.function_name.clone())
                            })
                            .unwrap_or_else(|| "??".to_string())
                    },
                    file: location.and_then(|location| location.path.clone()),
                    line: location.map(|location| u64::from(location.line)),
                    boundary,
                }
            })
            .collect::<Vec<_>>();
        self.apply_stack_frames(frames);
    }

    fn apply_stack_frames(&mut self, frames: Vec<StackFrame>) {
        let selected_thread_id = self.current_thread().map(|thread| thread.id.clone());
        self.frames = frames;
        self.selected_frame = bounded(self.selected_frame, self.frames.len());
        self.execution_location = self
            .frames
            .iter()
            .filter(|frame| {
                !frame.boundary
                    && selected_thread_id
                        .as_ref()
                        .is_none_or(|thread_id| frame.thread_id == *thread_id)
            })
            .min_by_key(|frame| frame.distributed_index)
            .and_then(|frame| {
                Some(SourceLocation {
                    source: frame.file.clone()?,
                    line: frame.line? as usize,
                })
            });
    }

    pub fn apply_variables(&mut self, items: Vec<v2::Variable>) {
        self.variables = items
            .into_iter()
            .map(|variable| variable_view(variable, 0))
            .collect();
        self.selected_variable = bounded(self.selected_variable, self.variables.len());
    }

    pub fn apply_variable_children(&mut self, parent_id: &str, items: Vec<v2::Variable>) {
        let Some(parent_index) = self
            .variables
            .iter()
            .position(|variable| variable.id == parent_id)
        else {
            return;
        };
        let depth = self.variables[parent_index].depth;
        let end = self.variables[parent_index + 1..]
            .iter()
            .position(|variable| variable.depth <= depth)
            .map(|offset| parent_index + 1 + offset)
            .unwrap_or(self.variables.len());
        self.variables.drain(parent_index + 1..end);
        let children = items
            .into_iter()
            .map(|variable| variable_view(variable, depth + 1))
            .collect::<Vec<_>>();
        self.variables
            .splice(parent_index + 1..parent_index + 1, children);
        self.variables[parent_index].expanded = true;
        self.selected_variable = bounded(self.selected_variable, self.variables.len());
    }

    pub fn toggle_selected_variable(&mut self) -> Option<String> {
        let index = self.selected_variable;
        let variable = self.variables.get(index)?;
        if !variable.has_children {
            return None;
        }
        let id = variable.id.clone();
        let depth = variable.depth;
        if variable.expanded {
            let end = self.variables[index + 1..]
                .iter()
                .position(|variable| variable.depth <= depth)
                .map(|offset| index + 1 + offset)
                .unwrap_or(self.variables.len());
            self.variables.drain(index + 1..end);
            self.variables[index].expanded = false;
            None
        } else {
            Some(id)
        }
    }

    pub fn apply_registers(&mut self, items: Vec<v2::Register>) {
        self.registers = items
            .into_iter()
            .map(|register| Register {
                name: register.name,
                value: register.formatted_value.unwrap_or(register.value),
                unavailable: register.unavailable,
            })
            .collect();
    }

    pub fn apply_memory(&mut self, block: v2::MemoryBlock) {
        self.memory = vec![MemoryBlock {
            begin: block.address,
            bytes: block.data,
        }];
    }

    pub fn apply_source(&mut self, content: v2::SourceContent, active_line: usize) {
        let first_line = (content.start_line as usize).max(1);
        let lines = decode_source_lines(&content.content, content.line_count);
        let last_line = first_line.saturating_add(lines.len().saturating_sub(1));
        let active_line = active_line.clamp(first_line, last_line);
        let path = content
            .source
            .as_ref()
            .and_then(|source| source.path.clone())
            .or_else(|| content.source.as_ref().map(|source| source.name.clone()))
            .unwrap_or_else(|| "<source>".to_string());
        let preserved_cursor_line = self
            .source_cursor
            .as_ref()
            .filter(|_| self.source_cursor_pinned)
            .filter(|cursor| same_source(&cursor.source, &path))
            .map(|cursor| cursor.line)
            .filter(|line| (first_line..=last_line).contains(line));
        let cursor_line = preserved_cursor_line.unwrap_or(active_line);
        self.source_cursor_pinned = preserved_cursor_line.is_some();
        self.source_cursor = Some(SourceLocation {
            source: path.clone(),
            line: cursor_line,
        });
        self.source_scroll = active_line.saturating_sub(first_line).saturating_sub(8);
        self.source = Some(SourceView {
            path,
            start_line: first_line,
            end_line: last_line,
            total_lines: (!content.has_more).then_some(last_line),
            lines,
        });
    }

    pub fn current_thread(&self) -> Option<&ThreadItem> {
        self.threads.get(self.selected_thread)
    }

    fn reconcile_topology_selection(&mut self) {
        let selected_is_present = self
            .selected_topology
            .as_ref()
            .is_some_and(|selected| self.topology_rows().iter().any(|row| &row.kind == selected));
        if !selected_is_present {
            self.selected_topology = self
                .current_thread()
                .map(|thread| TopologyRowKind::Thread(thread.id.clone()));
        }
    }

    pub fn execution_target(&self) -> Option<v2::Target> {
        self.execution_target.clone().or_else(|| {
            self.current_thread()
                .map(|thread| thread_target(thread.id.clone()))
        })
    }

    pub fn execution_scope_label(&self) -> String {
        let target = self.execution_target();
        self.target_label(target.as_ref())
    }

    fn target_label(&self, target: Option<&v2::Target>) -> String {
        match target.and_then(|target| target.selector.as_ref()) {
            Some(target::Selector::Thread(value)) => self
                .threads
                .iter()
                .find(|thread| thread.id == value.thread_id)
                .map(|thread| format!("thread {}", thread.name))
                .unwrap_or_else(|| format!("thread {}", value.thread_id)),
            Some(target::Selector::Session(value)) => self
                .sessions
                .iter()
                .find(|session| session.session_id == value.session_id)
                .map(|session| {
                    let name = if session.display_name.is_empty() {
                        session.session_id.as_str()
                    } else {
                        session.display_name.as_str()
                    };
                    format!("session {name}")
                })
                .unwrap_or_else(|| format!("session {}", value.session_id)),
            Some(target::Selector::Group(value)) => self
                .groups
                .iter()
                .find(|group| group.group_id == value.group_id)
                .map(|group| {
                    let name = if group.display_name.is_empty() {
                        group.group_id.as_str()
                    } else {
                        group.display_name.as_str()
                    };
                    format!("group {name}")
                })
                .unwrap_or_else(|| format!("group {}", value.group_id)),
            Some(target::Selector::SessionSet(value)) => {
                format!("{} sessions", value.session_ids.len())
            }
            Some(target::Selector::Broadcast(_)) => "all sessions".to_string(),
            Some(target::Selector::Multiple(value)) => format!("{} targets", value.targets.len()),
            Some(target::Selector::CurrentThread(_)) => "current thread".to_string(),
            Some(target::Selector::CurrentSession(_)) => "current session".to_string(),
            Some(target::Selector::First(_)) => "first session".to_string(),
            Some(target::Selector::Operation(_)) => "operation".to_string(),
            None => "no target".to_string(),
        }
    }

    pub fn cycle_execution_scope(&mut self) {
        let next = match self.execution_target().and_then(|target| target.selector) {
            Some(target::Selector::Thread(_)) => self
                .current_thread()
                .map(|thread| session_target(thread.session_id.clone())),
            Some(target::Selector::Session(value)) => self
                .sessions
                .iter()
                .find(|session| session.session_id == value.session_id)
                .and_then(|session| session.group_id.clone())
                .map(group_target)
                .or(Some(v2::Target {
                    selector: Some(target::Selector::Broadcast(v2::BroadcastTarget {})),
                })),
            Some(target::Selector::Group(_))
            | Some(target::Selector::SessionSet(_))
            | Some(target::Selector::Multiple(_)) => Some(v2::Target {
                selector: Some(target::Selector::Broadcast(v2::BroadcastTarget {})),
            }),
            Some(target::Selector::Broadcast(_))
            | Some(target::Selector::CurrentThread(_))
            | Some(target::Selector::CurrentSession(_))
            | Some(target::Selector::First(_))
            | Some(target::Selector::Operation(_))
            | None => self
                .current_thread()
                .map(|thread| thread_target(thread.id.clone())),
        };
        self.execution_target = next;
        self.status = format!("execution scope: {}", self.execution_scope_label());
    }

    pub fn topology_rows(&self) -> Vec<TopologyRow> {
        let mut groups = self.groups.iter().collect::<Vec<_>>();
        groups.sort_by(|left, right| {
            left.display_name
                .cmp(&right.display_name)
                .then_with(|| left.group_id.cmp(&right.group_id))
        });
        let mut rows = Vec::new();
        for group in groups {
            let mut sessions = self
                .sessions
                .iter()
                .filter(|session| session.group_id.as_deref() == Some(group.group_id.as_str()))
                .collect::<Vec<_>>();
            sessions.sort_by(|left, right| left.display_name.cmp(&right.display_name));
            rows.push(TopologyRow {
                kind: TopologyRowKind::Group(group.group_id.clone()),
                depth: 0,
                label: if group.display_name.is_empty() {
                    group.group_id.clone()
                } else {
                    group.display_name.clone()
                },
                detail: format!("{} sessions", sessions.len()),
                state: None,
            });
            for session in sessions {
                self.append_session_topology(&mut rows, session, 1);
            }
        }

        let mut unresolved_group_ids = self
            .sessions
            .iter()
            .filter_map(|session| session.group_id.as_deref())
            .filter(|group_id| !self.groups.iter().any(|group| group.group_id == *group_id))
            .collect::<Vec<_>>();
        unresolved_group_ids.sort_unstable();
        unresolved_group_ids.dedup();
        for group_id in unresolved_group_ids {
            let mut sessions = self
                .sessions
                .iter()
                .filter(|session| session.group_id.as_deref() == Some(group_id))
                .collect::<Vec<_>>();
            sessions.sort_by(|left, right| left.display_name.cmp(&right.display_name));
            rows.push(TopologyRow {
                kind: TopologyRowKind::Group(group_id.to_string()),
                depth: 0,
                label: format!("Unresolved group · {group_id}"),
                detail: format!("{} sessions · awaiting group metadata", sessions.len()),
                state: None,
            });
            for session in sessions {
                self.append_session_topology(&mut rows, session, 1);
            }
        }

        let mut ungrouped = self
            .sessions
            .iter()
            .filter(|session| session.group_id.is_none())
            .collect::<Vec<_>>();
        ungrouped.sort_by(|left, right| left.display_name.cmp(&right.display_name));
        if !ungrouped.is_empty() {
            rows.push(TopologyRow {
                kind: TopologyRowKind::Group(String::new()),
                depth: 0,
                label: "Ungrouped".to_string(),
                detail: format!("{} sessions", ungrouped.len()),
                state: None,
            });
            for session in ungrouped {
                self.append_session_topology(&mut rows, session, 1);
            }
        }
        rows
    }

    fn append_session_topology(
        &self,
        rows: &mut Vec<TopologyRow>,
        session: &v2::Session,
        depth: usize,
    ) {
        let session_name = if session.display_name.is_empty() {
            session.session_id.clone()
        } else {
            session.display_name.clone()
        };
        let session_state = session_status_name(session.status).to_string();
        let thread_indices = self
            .threads
            .iter()
            .enumerate()
            .filter_map(|(index, thread)| {
                (thread.session_id == session.session_id).then_some(index)
            })
            .collect::<Vec<_>>();
        rows.push(TopologyRow {
            kind: TopologyRowKind::Session(session.session_id.clone()),
            depth,
            label: session_name,
            detail: match session
                .status_detail
                .as_deref()
                .filter(|detail| !detail.is_empty())
            {
                Some(detail) => format!("{} threads · {detail}", thread_indices.len()),
                None => format!("{} threads", thread_indices.len()),
            },
            state: Some(session_state),
        });
        for index in thread_indices {
            let thread = &self.threads[index];
            let location = match (thread.file.as_deref(), thread.line) {
                (Some(file), Some(line)) => format!("{}:{line}", short_model_path(file)),
                _ => thread.function.clone(),
            };
            rows.push(TopologyRow {
                kind: TopologyRowKind::Thread(thread.id.clone()),
                depth: depth + 1,
                label: thread.name.clone(),
                detail: location,
                state: Some(thread.state.clone()),
            });
        }
    }

    pub fn selected_topology_row(&self) -> usize {
        let selected = self.selected_topology.clone().or_else(|| {
            self.current_thread()
                .map(|thread| TopologyRowKind::Thread(thread.id.clone()))
        });
        self.topology_rows()
            .iter()
            .position(|row| selected.as_ref() == Some(&row.kind))
            .unwrap_or(0)
    }

    pub fn selected_topology_thread_id(&self) -> Option<&str> {
        match self.selected_topology.as_ref() {
            Some(TopologyRowKind::Thread(thread_id)) => Some(thread_id),
            None => self.current_thread().map(|thread| thread.id.as_str()),
            Some(TopologyRowKind::Group(_) | TopologyRowKind::Session(_)) => None,
        }
    }

    pub fn select_topology_row(&mut self, row: usize) -> bool {
        let rows = self.topology_rows();
        let Some(kind) = rows.get(row).map(|row| row.kind.clone()) else {
            return false;
        };
        self.selected_topology = Some(kind.clone());
        self.execution_target = match &kind {
            TopologyRowKind::Thread(thread_id) => Some(thread_target(thread_id.clone())),
            TopologyRowKind::Session(session_id) => Some(session_target(session_id.clone())),
            TopologyRowKind::Group(group_id) if group_id.is_empty() => {
                let session_ids = self
                    .sessions
                    .iter()
                    .filter(|session| session.group_id.is_none())
                    .map(|session| session.session_id.clone())
                    .collect::<Vec<_>>();
                (!session_ids.is_empty()).then_some(v2::Target {
                    selector: Some(target::Selector::SessionSet(v2::SessionSetTarget {
                        session_ids,
                    })),
                })
            }
            TopologyRowKind::Group(group_id) => Some(group_target(group_id.clone())),
        };
        if let TopologyRowKind::Thread(thread_id) = kind {
            if let Some(index) = self
                .threads
                .iter()
                .position(|thread| thread.id == thread_id)
            {
                self.selected_thread = index;
                return true;
            }
        }
        false
    }

    fn move_topology_selection(&mut self, delta: isize) {
        let row_count = self.topology_rows().len();
        if row_count == 0 {
            return;
        }
        let current = self.selected_topology_row().min(row_count - 1);
        let selected = move_index(current, row_count, delta);
        let _ = self.select_topology_row(selected);
    }
    pub fn current_frame(&self) -> Option<&StackFrame> {
        self.frames.get(self.selected_frame)
    }

    pub fn current_breakpoint(&self) -> Option<&Breakpoint> {
        self.breakpoints.get(self.selected_breakpoint)
    }

    pub fn open_signal_picker(
        &mut self,
        target: v2::Target,
        signals: Vec<v2::DebuggerSignal>,
    ) -> Result<(), String> {
        if signals.is_empty() {
            return Err("the selected DDB session reported no available signals".to_string());
        }
        self.signal_picker = Some(SignalPicker {
            target,
            signals,
            cursor: 0,
        });
        self.status = "choose a signal to send; press f for a custom signal".to_string();
        Ok(())
    }

    pub fn move_signal_picker(&mut self, delta: isize) {
        let Some(picker) = self.signal_picker.as_mut() else {
            return;
        };
        picker.cursor = move_index(picker.cursor, picker.signals.len(), delta);
    }

    pub fn select_signal(&mut self, index: usize) {
        let Some(picker) = self.signal_picker.as_mut() else {
            return;
        };
        if index < picker.signals.len() {
            picker.cursor = index;
        }
    }

    pub fn cancel_signal_picker(&mut self) {
        self.signal_picker = None;
        self.status = "signal selection cancelled".to_string();
    }

    pub fn commit_signal_picker(&mut self) -> Result<(String, v2::Target), String> {
        let (signal, target) = {
            let picker = self
                .signal_picker
                .as_ref()
                .ok_or_else(|| "no signal selection is active".to_string())?;
            let signal = picker
                .signals
                .get(picker.cursor)
                .ok_or_else(|| "select a signal to send".to_string())?;
            (signal.name.clone(), picker.target.clone())
        };
        self.signal_picker = None;
        Ok((signal, target))
    }

    pub fn start_breakpoint_target_picker(
        &mut self,
        options: BreakpointOptions,
    ) -> Result<(), String> {
        if !self.capabilities.supports_breakpoint_action("create") {
            return Err("source breakpoint creation is not supported by this DDB API".to_string());
        }
        if options.condition.is_some()
            && !self.capabilities.supports_breakpoint_action("conditional")
        {
            return Err("conditional breakpoints are not supported by this DDB API".to_string());
        }
        if options.temporary && !self.capabilities.supports_breakpoint_action("temporary") {
            return Err("temporary breakpoints are not supported by this DDB API".to_string());
        }
        if options.hardware && !self.capabilities.supports_breakpoint_action("hardware") {
            return Err("hardware breakpoints are not supported by this DDB API".to_string());
        }
        let (source, line) = self
            .source_cursor_location()
            .map(|(source, line)| (source.to_string(), line as u64))
            .ok_or_else(|| "load a source file before setting a breakpoint".to_string())?;
        if self.sessions.is_empty() {
            return Err("DDB has no sessions eligible for a breakpoint".to_string());
        }

        let selected_session_id = self
            .current_thread()
            .map(|thread| thread.session_id.clone());
        let mut choices = Vec::new();
        if self.capabilities.supports_breakpoint_action("distributed") {
            choices.push(BreakpointTargetChoice {
                target: BreakpointTarget::Broadcast,
                label: "All eligible DDB sessions".to_string(),
                detail: "server-resolved broadcast".to_string(),
                selected: false,
            });
        }

        if self
            .capabilities
            .supports_breakpoint_action("group_inheritance")
        {
            let mut groups = self.groups.iter().collect::<Vec<_>>();
            groups.sort_by(|left, right| {
                left.display_name
                    .cmp(&right.display_name)
                    .then_with(|| left.group_id.cmp(&right.group_id))
            });
            for group in groups {
                let session_count = self
                    .sessions
                    .iter()
                    .filter(|session| session.group_id.as_deref() == Some(group.group_id.as_str()))
                    .count();
                let name = if group.display_name.is_empty() {
                    group.group_id.as_str()
                } else {
                    group.display_name.as_str()
                };
                choices.push(BreakpointTargetChoice {
                    target: BreakpointTarget::Group(group.group_id.clone()),
                    label: format!("Group · {name}"),
                    detail: format!("{session_count} sessions · {}", group.group_id),
                    selected: false,
                });
            }
        }

        let mut sessions = self.sessions.iter().collect::<Vec<_>>();
        sessions.sort_by(|left, right| {
            left.display_name
                .cmp(&right.display_name)
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
        for session in sessions {
            let name = if session.display_name.is_empty() {
                session.session_id.as_str()
            } else {
                session.display_name.as_str()
            };
            let group = session
                .group_id
                .as_ref()
                .and_then(|group_id| {
                    self.groups
                        .iter()
                        .find(|group| group.group_id == *group_id)
                        .map(|group| {
                            if group.display_name.is_empty() {
                                group.group_id.as_str()
                            } else {
                                group.display_name.as_str()
                            }
                        })
                })
                .unwrap_or("ungrouped");
            choices.push(BreakpointTargetChoice {
                target: BreakpointTarget::Session(session.session_id.clone()),
                label: format!("Session · {name}"),
                detail: format!("{group} · {}", session.session_id),
                selected: selected_session_id.as_deref() == Some(session.session_id.as_str()),
            });
        }
        let cursor = choices
            .iter()
            .position(|choice| choice.selected)
            .unwrap_or(0);
        self.breakpoint_target_picker = Some(BreakpointTargetPicker {
            draft: BreakpointDraft {
                source,
                line,
                options,
            },
            choices,
            cursor,
        });
        self.status = "choose one or more DDB breakpoint targets".to_string();
        Ok(())
    }

    pub fn move_breakpoint_target_picker(&mut self, delta: isize) {
        let Some(picker) = self.breakpoint_target_picker.as_mut() else {
            return;
        };
        picker.cursor = move_index(picker.cursor, picker.choices.len(), delta);
    }

    pub fn toggle_breakpoint_target_choice(&mut self) {
        let Some(picker) = self.breakpoint_target_picker.as_mut() else {
            return;
        };
        let cursor = picker.cursor.min(picker.choices.len().saturating_sub(1));
        let broadcast = matches!(
            picker.choices.get(cursor).map(|choice| &choice.target),
            Some(BreakpointTarget::Broadcast)
        );
        let selected = picker
            .choices
            .get(cursor)
            .is_some_and(|choice| choice.selected);
        if broadcast {
            for choice in &mut picker.choices {
                choice.selected = false;
            }
        } else if !selected {
            if let Some(choice) = picker
                .choices
                .iter_mut()
                .find(|choice| matches!(choice.target, BreakpointTarget::Broadcast))
            {
                choice.selected = false;
            }
        }
        if let Some(choice) = picker.choices.get_mut(cursor) {
            choice.selected = !selected;
        }
    }

    pub fn select_breakpoint_target_choice(&mut self, index: usize) {
        let Some(picker) = self.breakpoint_target_picker.as_mut() else {
            return;
        };
        if index >= picker.choices.len() {
            return;
        }
        picker.cursor = index;
        self.toggle_breakpoint_target_choice();
    }

    pub fn cancel_breakpoint_target_picker(&mut self) {
        self.breakpoint_target_picker = None;
        self.status = "breakpoint target selection cancelled".to_string();
    }

    pub fn commit_breakpoint_target_picker(
        &mut self,
    ) -> Result<(BreakpointDraft, BreakpointTarget), String> {
        let Some(picker) = self.breakpoint_target_picker.as_ref() else {
            return Err("no breakpoint target selection is active".to_string());
        };
        let mut targets = picker
            .choices
            .iter()
            .filter(|choice| choice.selected)
            .map(|choice| choice.target.clone())
            .collect::<Vec<_>>();
        if targets.is_empty() {
            return Err("select at least one available DDB breakpoint target".to_string());
        }
        if targets.len() > 1 && !self.capabilities.supports_breakpoint_action("distributed") {
            return Err("multi-target breakpoints are not supported by this DDB API".to_string());
        }
        if targets
            .iter()
            .any(|target| matches!(target, BreakpointTarget::Broadcast))
        {
            targets = vec![BreakpointTarget::Broadcast];
        } else {
            let selected_groups = targets
                .iter()
                .filter_map(|target| match target {
                    BreakpointTarget::Group(group_id) => Some(group_id.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            targets.retain(|target| match target {
                BreakpointTarget::Session(session_id) => !self.sessions.iter().any(|session| {
                    session.session_id == *session_id
                        && session
                            .group_id
                            .as_ref()
                            .is_some_and(|group_id| selected_groups.contains(group_id))
                }),
                _ => true,
            });
        }
        let target = if targets.len() == 1 {
            targets.remove(0)
        } else {
            BreakpointTarget::Multiple(targets)
        };
        let draft = self
            .breakpoint_target_picker
            .take()
            .expect("picker checked above")
            .draft;
        Ok((draft, target))
    }
    pub fn begin_inspection(&mut self) -> u64 {
        self.inspection_generation = self.inspection_generation.wrapping_add(1).max(1);
        self.inspected_thread_id = self.current_thread().map(|thread| thread.id.clone());
        self.frames.clear();
        self.distributed_stack_truncation = None;
        self.execution_location = None;
        self.clear_inspection_details();
        self.inspection_generation
    }

    pub fn begin_frame_inspection(&mut self) -> u64 {
        self.inspection_generation = self.inspection_generation.wrapping_add(1).max(1);
        self.source_cursor_pinned = false;
        self.clear_inspection_details();
        self.inspection_generation
    }

    pub fn mark_running(&mut self, thread_id: Option<&str>) {
        let affects_current = match (thread_id, self.current_thread()) {
            (None, Some(_)) => true,
            (Some(running), Some(current)) => running == current.id,
            _ => false,
        };
        for thread in &mut self.threads {
            if thread_id.is_none_or(|running| running == thread.id) {
                thread.state = "running".to_string();
            }
        }
        if affects_current {
            self.invalidate_inspection();
        }
    }

    pub fn start_input(&mut self, mode: InputMode) {
        self.input_mode = mode;
        self.input.clear();
        self.input_cursor = 0;
        self.history_index = None;
        self.history_draft.clear();
    }

    pub fn cancel_input(&mut self) {
        if self.input_mode == InputMode::ExtensionAction {
            self.pending_extension_action = None;
        }
        self.input_mode = InputMode::Normal;
        self.input.clear();
        self.input_cursor = 0;
        self.history_index = None;
        self.history_draft.clear();
    }

    pub fn insert_input(&mut self, text: &str) {
        let text = text
            .replace(['\r', '\n'], " ")
            .chars()
            .filter(|character| !character.is_control())
            .collect::<String>();
        let byte_index = char_byte_index(&self.input, self.input_cursor);
        self.input.insert_str(byte_index, &text);
        self.input_cursor += text.chars().count();
        self.history_index = None;
    }

    pub fn backspace_input(&mut self) {
        if self.input_cursor == 0 {
            return;
        }
        let end = char_byte_index(&self.input, self.input_cursor);
        let start = char_byte_index(&self.input, self.input_cursor - 1);
        self.input.replace_range(start..end, "");
        self.input_cursor -= 1;
        self.history_index = None;
    }

    pub fn delete_input(&mut self) {
        if self.input_cursor >= self.input.chars().count() {
            return;
        }
        let start = char_byte_index(&self.input, self.input_cursor);
        let end = char_byte_index(&self.input, self.input_cursor + 1);
        self.input.replace_range(start..end, "");
        self.history_index = None;
    }

    pub fn move_input_cursor(&mut self, delta: isize) {
        self.input_cursor = (self.input_cursor as isize + delta)
            .clamp(0, self.input.chars().count() as isize) as usize;
    }

    pub fn set_input_cursor(&mut self, position: usize) {
        self.input_cursor = position.min(self.input.chars().count());
    }

    pub fn previous_input(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        let index = match self.history_index {
            Some(index) => index.saturating_sub(1),
            None => {
                self.history_draft = self.input.clone();
                self.input_history.len() - 1
            }
        };
        self.history_index = Some(index);
        self.input = self.input_history[index].clone();
        self.input_cursor = self.input.chars().count();
    }

    pub fn next_input(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 < self.input_history.len() {
            self.history_index = Some(index + 1);
            self.input = self.input_history[index + 1].clone();
        } else {
            self.history_index = None;
            self.input = std::mem::take(&mut self.history_draft);
        }
        self.input_cursor = self.input.chars().count();
    }

    pub fn commit_input(&mut self) -> String {
        let input = self.input.trim().to_string();
        if !input.is_empty()
            && self
                .input_history
                .back()
                .is_none_or(|previous| previous != &input)
        {
            if self.input_history.len() == 100 {
                self.input_history.pop_front();
            }
            self.input_history.push_back(input.clone());
        }
        self.input_mode = InputMode::Normal;
        self.input.clear();
        self.input_cursor = 0;
        self.history_index = None;
        self.history_draft.clear();
        input
    }

    pub fn prepare_source_navigation(
        &mut self,
        requested_source: Option<String>,
        line: usize,
    ) -> Option<(String, usize, u64)> {
        let (path, total_lines) = match requested_source {
            Some(path) => {
                let total_lines = self
                    .source
                    .as_ref()
                    .filter(|source| same_source(&source.path, &path))
                    .and_then(|source| source.total_lines);
                (path, total_lines)
            }
            None => {
                let source = self.source.as_ref()?;
                (source.path.clone(), source.total_lines)
            }
        };
        let line = total_lines.map_or_else(|| line.max(1), |total| line.clamp(1, total.max(1)));
        self.source_cursor = Some(SourceLocation {
            source: path.clone(),
            line,
        });
        self.source_cursor_pinned = true;
        self.inspection_generation = self.inspection_generation.wrapping_add(1).max(1);
        self.inspected_thread_id = self.current_thread().map(|thread| thread.id.clone());
        Some((path, line, self.inspection_generation))
    }

    fn invalidate_inspection(&mut self) {
        self.frames.clear();
        self.distributed_stack_truncation = None;
        self.execution_location = None;
        self.source_cursor = None;
        self.source_cursor_pinned = false;
        self.selected_frame = 0;
        self.selected_variable = 0;
        self.inspected_thread_id = None;
        self.clear_inspection_details();
        self.inspection_generation = self.inspection_generation.wrapping_add(1).max(1);
    }

    fn clear_inspection_details(&mut self) {
        self.variables.clear();
        self.registers.clear();
        self.memory.clear();
        self.source = None;
    }

    pub fn inspection_is_current(&self, generation: u64, thread_id: &str) -> bool {
        generation == self.inspection_generation
            && self.current_thread().map(|thread| thread.id.as_str()) == Some(thread_id)
    }

    pub fn thread_inspection_requested(&self, thread_id: &str) -> bool {
        self.inspected_thread_id.as_deref() == Some(thread_id)
    }

    pub fn source_cursor_location(&self) -> Option<(&str, usize)> {
        let source = self.source.as_ref()?;
        let cursor = self.source_cursor.as_ref()?;
        same_source(&cursor.source, &source.path).then_some((source.path.as_str(), cursor.line))
    }

    pub fn set_source_cursor_line(&mut self, line: usize) {
        let Some(source) = self.source.as_ref() else {
            return;
        };
        let first_line = source.start_line.max(1);
        let last_line = source.end_line.max(first_line);
        let line = line.clamp(first_line, last_line);
        self.source_cursor = Some(SourceLocation {
            source: source.path.clone(),
            line,
        });
        self.source_cursor_pinned = true;
        self.source_scroll = line.saturating_sub(source.start_line).saturating_sub(8);
    }

    pub fn breakpoint_at_source_cursor(&self, target: BreakpointTarget) -> Option<&Breakpoint> {
        let (source, line) = self.source_cursor_location()?;
        self.breakpoints.iter().find(|breakpoint| {
            breakpoint.location.line == line as u64
                && same_source(&breakpoint.location.src, source)
                && (breakpoint
                    .target
                    .as_ref()
                    .is_some_and(|candidate| target.matches(candidate))
                    || breakpoint
                        .sub_breakpoints
                        .iter()
                        .any(|member| match &target {
                            BreakpointTarget::Session(session_id) => {
                                member.session_id == *session_id
                            }
                            BreakpointTarget::Group(group_id) => {
                                member.inherited_from_group_id.as_ref() == Some(group_id)
                            }
                            BreakpointTarget::Broadcast => true,
                            BreakpointTarget::Multiple(targets) => targets
                                .iter()
                                .any(|target| target_matches_sub_breakpoint(target, member)),
                        }))
        })
    }

    pub fn move_selection(&mut self, delta: isize) {
        match self.focus {
            Focus::Threads => self.move_topology_selection(delta),
            Focus::Breakpoints => {
                self.selected_breakpoint =
                    move_index(self.selected_breakpoint, self.breakpoints.len(), delta)
            }
            Focus::Stack => {
                self.selected_frame = move_index(self.selected_frame, self.frames.len(), delta)
            }
            Focus::Source => {
                let line = self
                    .source_cursor_location()
                    .map(|(_, line)| line)
                    .or_else(|| self.source.as_ref().map(|source| source.start_line))
                    .unwrap_or(1);
                self.set_source_cursor_line(move_line(line, delta));
            }
            Focus::Timeline => {
                self.timeline_scroll = move_line(self.timeline_scroll, delta);
            }
            Focus::Extensions => {
                if self.extension_actions.is_empty() {
                    self.extension_scroll = move_line(self.extension_scroll, delta);
                } else {
                    self.selected_extension_action = move_index(
                        self.selected_extension_action,
                        self.extension_actions.len(),
                        delta,
                    );
                    if let Some(action) = self.extension_actions.get(self.selected_extension_action)
                    {
                        if !action.description.is_empty() {
                            self.status = action.description.clone();
                        }
                    }
                }
            }
            Focus::Variables => {
                self.selected_variable =
                    move_index(self.selected_variable, self.variables.len(), delta)
            }
        }
    }

    pub fn cycle_focus(&mut self, reverse: bool) {
        let mut visible = Focus::CORE.to_vec();
        if !self.extension_panels.is_empty() || !self.extension_actions.is_empty() {
            visible.insert(5, Focus::Extensions);
        }
        let index = visible
            .iter()
            .position(|item| *item == self.focus)
            .unwrap_or(0);
        self.focus = if reverse {
            visible[(index + visible.len() - 1) % visible.len()]
        } else {
            visible[(index + 1) % visible.len()]
        };
    }

    pub fn push_timeline(&mut self, message: impl Into<String>) {
        if self.timeline.len() == 500 {
            self.timeline.pop_front();
        }
        self.timeline.push_back(message.into());
        self.timeline_scroll = self.timeline.len().saturating_sub(1);
    }

    pub fn push_receipt(&mut self, label: &str, receipt: &v2::Operation) {
        let state = operation_state_name(receipt.state);
        self.status = format!("{label}: {state}");
        let icon = if receipt.error.is_some() {
            "✗"
        } else {
            "✓"
        };
        self.push_timeline(format!("{icon} {label}: {state}"));
        if let Some(error) = receipt.error.as_ref() {
            self.push_timeline(format!("  {}", error.message));
        }
        if let Some(summary) = receipt.result.as_ref().and_then(operation_result_summary) {
            self.push_timeline(format!("  {summary}"));
        }
        if !receipt.target_outcomes.is_empty() {
            let succeeded = receipt
                .target_outcomes
                .iter()
                .filter(|outcome| outcome.succeeded)
                .count();
            let failed = receipt.target_outcomes.len() - succeeded;
            self.push_timeline(format!("  fanout: {succeeded} succeeded · {failed} failed"));
            for outcome in receipt
                .target_outcomes
                .iter()
                .filter(|outcome| !outcome.succeeded)
            {
                let target = self.target_label(outcome.target.as_ref());
                let error = outcome
                    .error
                    .as_ref()
                    .map(|error| error.message.as_str())
                    .unwrap_or("unknown target failure");
                self.push_timeline(format!("  ✗ {target}: {error}"));
            }
        }
    }

    pub fn error(&mut self, error: impl ToString) {
        self.status = error.to_string();
        self.push_timeline(format!("✗ {}", self.status));
    }

    pub fn backend_unavailable(&mut self, error: impl ToString) {
        self.api_connected = false;
        self.error(error);
    }
}

fn decode_source_lines(content: &str, line_count: u32) -> Vec<String> {
    if line_count == 0 {
        return Vec::new();
    }

    let lines = content.split('\n').map(str::to_string).collect::<Vec<_>>();
    debug_assert_eq!(
        lines.len(),
        line_count as usize,
        "SourceContent line_count must match its newline-delimited content"
    );
    lines
}

fn protocol_label(capabilities: &v2::Capabilities) -> String {
    if capabilities.api_version == "v1" {
        "v1/http+websocket fallback".to_string()
    } else {
        format!("v2/http+json {}", capabilities.schema_version)
    }
}

fn compact_json(value: &Value) -> String {
    let mut rendered = value.to_string();
    if rendered.chars().count() > 240 {
        rendered = rendered.chars().take(237).collect();
        rendered.push('…');
    }
    rendered
}

fn char_byte_index(text: &str, character_index: usize) -> usize {
    text.char_indices()
        .nth(character_index)
        .map(|(index, _)| index)
        .unwrap_or(text.len())
}

fn bounded(index: usize, len: usize) -> usize {
    if len == 0 {
        0
    } else {
        index.min(len - 1)
    }
}

fn move_index(index: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    (index as isize + delta).clamp(0, len as isize - 1) as usize
}

fn move_line(line: usize, delta: isize) -> usize {
    (line as isize + delta).max(0) as usize
}

pub(crate) fn same_source(left: &str, right: &str) -> bool {
    let left_parts = source_components(left);
    let right_parts = source_components(right);
    if left_parts == right_parts {
        return true;
    }
    let left_absolute = source_is_absolute(left);
    let right_absolute = source_is_absolute(right);
    match (left_absolute, right_absolute) {
        (true, false) => right_parts.len() >= 2 && left_parts.ends_with(&right_parts),
        (false, true) => left_parts.len() >= 2 && right_parts.ends_with(&left_parts),
        (true, true) | (false, false) => false,
    }
}

fn source_components(path: &str) -> Vec<&str> {
    let absolute = source_is_absolute(path);
    let mut components = Vec::new();
    let drive_prefix_len = usize::from(
        path.as_bytes()
            .get(1)
            .is_some_and(|separator| *separator == b':'),
    );
    for component in path.split(['/', '\\']) {
        match component {
            "" | "." => {}
            ".." if components.len() > drive_prefix_len => {
                components.pop();
            }
            ".." if !absolute => components.push(component),
            ".." => {}
            _ => components.push(component),
        }
    }
    components
}

fn source_is_absolute(path: &str) -> bool {
    path.starts_with(['/', '\\'])
        || path
            .as_bytes()
            .get(1)
            .is_some_and(|separator| *separator == b':')
}

fn short_model_path(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn session_status_name(status: i32) -> &'static str {
    match v2::SessionStatus::try_from(status).unwrap_or(v2::SessionStatus::Unspecified) {
        v2::SessionStatus::Starting => "starting",
        v2::SessionStatus::Ready => "ready",
        v2::SessionStatus::Running => "running",
        v2::SessionStatus::Stopped => "stopped",
        v2::SessionStatus::Exited => "exited",
        v2::SessionStatus::Failed => "failed",
        v2::SessionStatus::Unspecified => "unknown",
    }
}

fn target_matches_sub_breakpoint(target: &BreakpointTarget, member: &v2::SubBreakpoint) -> bool {
    match target {
        BreakpointTarget::Session(session_id) => member.session_id == *session_id,
        BreakpointTarget::Group(group_id) => {
            member.inherited_from_group_id.as_ref() == Some(group_id)
        }
        BreakpointTarget::Broadcast => true,
        BreakpointTarget::Multiple(targets) => targets
            .iter()
            .any(|target| target_matches_sub_breakpoint(target, member)),
    }
}

fn variable_view(variable: v2::Variable, depth: usize) -> Variable {
    Variable {
        id: variable.variable_id,
        name: variable.name,
        type_name: variable.type_name.unwrap_or_default(),
        value: variable.value,
        has_children: variable.has_children,
        children: variable.child_count.unwrap_or_default() as usize,
        depth,
        expanded: false,
    }
}

fn thread_state_name(state: i32) -> &'static str {
    match v2::ThreadState::try_from(state).unwrap_or(v2::ThreadState::Unspecified) {
        v2::ThreadState::Running => "running",
        v2::ThreadState::Stopped => "stopped",
        v2::ThreadState::Exited => "exited",
        v2::ThreadState::Unavailable => "unavailable",
        v2::ThreadState::Unspecified => "unknown",
    }
}

fn operation_state_name(state: i32) -> &'static str {
    match v2::OperationState::try_from(state).unwrap_or(v2::OperationState::Unspecified) {
        v2::OperationState::Accepted => "accepted",
        v2::OperationState::Running => "running",
        v2::OperationState::Completed => "completed",
        v2::OperationState::Failed => "failed",
        v2::OperationState::Cancelled => "cancelled",
        v2::OperationState::Unspecified => "unknown",
    }
}

fn operation_result_summary(result: &v2::OperationResult) -> Option<String> {
    Some(match result.value.as_ref()? {
        operation_result::Value::Execution(state) => {
            if state.running {
                "target running".to_string()
            } else if let Some(location) = state.location.as_ref() {
                format!(
                    "stopped at {}:{}",
                    location.path.as_deref().unwrap_or("<source>"),
                    location.line
                )
            } else {
                "target stopped".to_string()
            }
        }
        operation_result::Value::Selection(selection) => format!(
            "selected thread {}",
            selection.thread_id.as_deref().unwrap_or("<unknown>")
        ),
        operation_result::Value::Evaluation(evaluation) => {
            format!("{} = {}", evaluation.expression, evaluation.value)
        }
        operation_result::Value::Breakpoint(breakpoint) => {
            format!("breakpoint {}", breakpoint.breakpoint_id)
        }
        operation_result::Value::RawCommand(result) => result
            .text
            .clone()
            .unwrap_or_else(|| "raw command completed".to_string()),
        operation_result::Value::DistributedBacktrace(result) => {
            let mut summary = format!("{} distributed frames", result.frames.len());
            if result.truncated {
                if let Some(reason) = result.truncation_reason.as_deref() {
                    summary.push_str(&format!(" · truncated: {reason}"));
                } else {
                    summary.push_str(" · truncated");
                }
            }
            summary
        }
        operation_result::Value::ExtensionAction(_) => "extension action completed".to_string(),
        operation_result::Value::NoContent(_) => "completed".to_string(),
    })
}

fn breakpoint_view(breakpoint: v2::Breakpoint) -> Option<Breakpoint> {
    let spec = breakpoint.spec?;
    let location = match spec.location.as_ref() {
        Some(breakpoint_spec::Location::Source(location)) => BreakpointLocation {
            src: location.source.clone(),
            line: u64::from(location.line),
        },
        Some(breakpoint_spec::Location::Function(location)) => BreakpointLocation {
            src: format!("function {}", location.function_name),
            line: 0,
        },
        Some(breakpoint_spec::Location::Address(location)) => BreakpointLocation {
            src: location.address.clone(),
            line: 0,
        },
        None => breakpoint
            .sub_breakpoints
            .iter()
            .find_map(|member| member.location.as_ref())
            .map(|location| BreakpointLocation {
                src: location
                    .path
                    .clone()
                    .or_else(|| location.source_reference.clone())
                    .unwrap_or_else(|| "<pending>".to_string()),
                line: u64::from(location.line),
            })
            .unwrap_or_default(),
    };
    Some(Breakpoint {
        id: breakpoint.breakpoint_id,
        target: breakpoint.target,
        location,
        // New v2 servers always populate resource presence. Treat absence as
        // false for compatibility with draft servers that omitted proto3's
        // false default from their resource representation.
        enabled: spec.enabled.unwrap_or(false),
        condition: spec.condition,
        ignore_count: spec.ignore_count,
        temporary: spec.temporary,
        hardware: spec.hardware,
        times: breakpoint.hit_count,
        verified: breakpoint.verified,
        pending: breakpoint.pending,
        message: breakpoint.message,
        sub_breakpoints: breakpoint.sub_breakpoints,
    })
}

fn extension_action_views(descriptors: &[v2::ExtensionDescriptor]) -> Vec<ExtensionActionView> {
    descriptors
        .iter()
        .flat_map(|descriptor| {
            descriptor
                .actions
                .iter()
                .map(move |action| ExtensionActionView {
                    extension_id: descriptor.extension_id.clone(),
                    extension_version: descriptor.version.clone(),
                    extension_title: descriptor.title.clone(),
                    action_id: action.id.clone(),
                    title: action.title.clone(),
                    description: action.description.clone().unwrap_or_default(),
                    request_schema_uri: action.request_schema_uri.clone(),
                })
        })
        .collect()
}

fn extension_panel_views(
    descriptors: &[v2::ExtensionDescriptor],
    states: &[v2::ExtensionState],
) -> Vec<ExtensionPanelView> {
    descriptors
        .iter()
        .flat_map(|descriptor| {
            let state = states
                .iter()
                .find(|state| state.extension_id == descriptor.extension_id);
            descriptor
                .presentations
                .iter()
                .filter_map(move |presentation| {
                    let kind = v2::ExtensionPresentationKind::try_from(presentation.kind).ok()?;
                    if kind == v2::ExtensionPresentationKind::Unspecified {
                        return None;
                    }
                    let value = state.and_then(extension_state_json);
                    let (columns, rows) = presentation_content(value.as_ref(), presentation, kind);
                    Some(ExtensionPanelView {
                        extension_title: descriptor.title.clone(),
                        description: presentation
                            .description
                            .clone()
                            .unwrap_or_else(|| descriptor.description.clone()),
                        panel_title: presentation.title.clone(),
                        columns,
                        rows,
                    })
                })
        })
        .collect()
}

fn extension_state_json(state: &v2::ExtensionState) -> Option<Value> {
    state
        .payloads
        .iter()
        .find_map(|payload| match payload.payload.as_ref()? {
            extension_payload::Payload::PayloadJson(value) => serde_json::from_str(value).ok(),
            extension_payload::Payload::PayloadBytes(_) => None,
        })
}

fn presentation_content(
    value: Option<&Value>,
    presentation: &v2::ExtensionPresentationDescriptor,
    kind: v2::ExtensionPresentationKind,
) -> (Vec<String>, Vec<Vec<String>>) {
    let data = value.and_then(|value| presentation_data(value, &presentation.id));
    match kind {
        v2::ExtensionPresentationKind::Table => (
            presentation
                .columns
                .iter()
                .map(|column| column.title.clone())
                .collect(),
            data.and_then(table_rows).unwrap_or_default(),
        ),
        v2::ExtensionPresentationKind::KeyValue => (
            vec!["Key".to_string(), "Value".to_string()],
            data.and_then(key_value_rows).unwrap_or_default(),
        ),
        v2::ExtensionPresentationKind::Tree => (
            vec!["Node".to_string()],
            data.map(tree_rows).unwrap_or_default(),
        ),
        v2::ExtensionPresentationKind::Text => (
            vec!["Text".to_string()],
            data.and_then(|data| data.get("text"))
                .and_then(Value::as_str)
                .map(|text| vec![vec![text.to_string()]])
                .unwrap_or_default(),
        ),
        v2::ExtensionPresentationKind::Action => {
            let action = presentation.action_id.as_deref().unwrap_or("unknown");
            let enabled = data
                .and_then(|data| data.get("enabled"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            (
                vec!["Action".to_string()],
                vec![vec![format!(
                    "{} ({action}) · {}",
                    presentation.title,
                    if enabled { "available" } else { "unavailable" }
                )]],
            )
        }
        v2::ExtensionPresentationKind::Unspecified => (Vec::new(), Vec::new()),
    }
}

fn presentation_data<'a>(value: &'a Value, presentation_id: &str) -> Option<&'a Value> {
    if let Some(data) = value
        .get("presentations")
        .and_then(|presentations| presentations.get(presentation_id))
    {
        return Some(data);
    }
    // v1 built-ins used a table-only `panels` array. Keep this one-way
    // translation isolated while canonical providers use `presentations`.
    value
        .get("panels")?
        .as_array()?
        .iter()
        .find(|panel| panel.get("id").and_then(Value::as_str) == Some(presentation_id))
}

fn table_rows(data: &Value) -> Option<Vec<Vec<String>>> {
    Some(
        data.get("rows")?
            .as_array()?
            .iter()
            .filter_map(Value::as_array)
            .map(|row| row.iter().map(extension_cell).collect())
            .take(1_000)
            .collect(),
    )
}

fn key_value_rows(data: &Value) -> Option<Vec<Vec<String>>> {
    Some(
        data.get("entries")?
            .as_array()?
            .iter()
            .filter_map(|entry| {
                if let Some(values) = entry.as_array() {
                    return (values.len() == 2)
                        .then(|| vec![extension_cell(&values[0]), extension_cell(&values[1])]);
                }
                Some(vec![
                    extension_cell(entry.get("key")?),
                    extension_cell(entry.get("value")?),
                ])
            })
            .take(1_000)
            .collect(),
    )
}

fn tree_rows(data: &Value) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut pending = data
        .get("nodes")
        .and_then(Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .rev()
                .map(|node| (node, 0usize))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    while let Some((node, depth)) = pending.pop() {
        if rows.len() >= 1_000 || depth > 32 {
            break;
        }
        let Some(label) = node.get("label").and_then(Value::as_str) else {
            continue;
        };
        let suffix = node
            .get("value")
            .filter(|value| !value.is_null())
            .map(|value| format!(": {}", extension_cell(value)))
            .unwrap_or_default();
        rows.push(vec![format!("{}{label}{suffix}", "  ".repeat(depth))]);
        if let Some(children) = node.get("children").and_then(Value::as_array) {
            pending.extend(children.iter().rev().map(|child| (child, depth + 1)));
        }
    }
    rows
}

fn extension_cell(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Null => String::new(),
        value => compact_json(value),
    }
}

#[derive(Debug)]
pub enum BackendRequest {
    Bootstrap,
    Refresh,
    InspectThread {
        thread_id: String,
        generation: u64,
    },
    InspectFrame {
        thread_id: String,
        owner_thread_id: String,
        generation: u64,
        frame_id: String,
        source: Option<String>,
        line: Option<u64>,
    },
    LoadSource {
        thread_id: String,
        generation: u64,
        source: String,
        line: usize,
    },
    Control(Control, v2::Target),
    Jump {
        location: String,
        target: v2::Target,
    },
    SendSignal {
        signal: String,
        target: v2::Target,
    },
    ListSignals {
        target: v2::Target,
    },
    CreateBreakpoint {
        source: String,
        line: u64,
        target: BreakpointTarget,
        options: BreakpointOptions,
    },
    DeleteBreakpoint {
        id: String,
        target: v2::Target,
    },
    SetBreakpointEnabled {
        id: String,
        target: v2::Target,
        enabled: bool,
    },
    Evaluate {
        expression: String,
        thread_id: String,
        frame_id: Option<String>,
    },
    ExpandVariable {
        variable_id: String,
        thread_id: String,
        generation: u64,
    },
    ReadMemory {
        address: String,
        count: u64,
        thread_id: String,
        generation: u64,
    },
    InvokeExtensionAction {
        extension_id: String,
        extension_version: String,
        action_id: String,
        request_schema_uri: String,
        payload_json: String,
        target: v2::Target,
    },
    RawCommand {
        command: String,
        target: v2::Target,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BreakpointOptions {
    pub condition: Option<String>,
    pub temporary: bool,
    pub hardware: bool,
}

#[derive(Debug)]
pub enum UiMessage {
    Capabilities(v2::Capabilities),
    Snapshot(v2::Snapshot),
    Threads(Vec<v2::Thread>),
    Signals {
        target: v2::Target,
        signals: Vec<v2::DebuggerSignal>,
    },
    Frames {
        generation: u64,
        thread_id: String,
        frames: Vec<v2::Frame>,
    },
    DistributedFrames {
        generation: u64,
        thread_id: String,
        result: v2::DistributedBacktraceResult,
    },
    VariableChildren {
        generation: u64,
        thread_id: String,
        parent_id: String,
        variables: Vec<v2::Variable>,
    },
    Registers {
        generation: u64,
        thread_id: String,
        registers: Vec<v2::Register>,
    },
    Variables {
        generation: u64,
        thread_id: String,
        variables: Vec<v2::Variable>,
    },
    Memory {
        generation: u64,
        thread_id: String,
        memory: v2::MemoryBlock,
    },
    Source {
        generation: u64,
        thread_id: String,
        source: v2::SourceContent,
        line: usize,
    },
    InspectionError {
        generation: u64,
        thread_id: String,
        error: String,
    },
    Receipt(String, v2::Operation),
    Notice(String),
    Output(String),
    DebuggerEvent(DebuggerEvent),
    EventStream(EventStreamStatus),
    Error(String),
    BackendUnavailable(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DebuggerEvent {
    pub summary: String,
    pub refresh: bool,
    pub activity: DebuggerActivity,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DebuggerActivity {
    None,
    Running(Option<String>),
    Stopped(Option<String>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EventStreamStatus {
    Connected,
    Reconnecting(String),
}

#[cfg(test)]
mod v2_tests {
    use ddb_api_client::v2::{self, operation_result};
    use ratatui::layout::Rect;

    use super::*;
    use crate::api::{group_target, session_target};

    fn thread(id: &str, state: v2::ThreadState, path: &str, line: u32) -> v2::Thread {
        v2::Thread {
            thread_id: id.to_string(),
            session_id: "session/alpha".to_string(),
            name: Some("main".to_string()),
            state: state as i32,
            selected: true,
            location: Some(v2::SourceLocation {
                path: Some(path.to_string()),
                line,
                function_name: Some("main".to_string()),
                ..Default::default()
            }),
            revision: 1,
            ..Default::default()
        }
    }

    fn frame(id: &str, level: u32, path: &str, line: u32) -> v2::Frame {
        v2::Frame {
            frame_id: id.to_string(),
            thread_id: "thread/alpha".to_string(),
            level,
            function_name: Some(if level == 0 { "main" } else { "caller" }.to_string()),
            location: Some(v2::SourceLocation {
                path: Some(path.to_string()),
                line,
                address: Some(format!("0x{level:x}")),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn source(path: &str, start_line: u32, line_count: usize) -> v2::SourceContent {
        v2::SourceContent {
            source: Some(v2::SourceFile {
                source_reference: format!("source/{path}"),
                path: Some(path.to_string()),
                name: path.to_string(),
                media_type: "text/plain".to_string(),
                content_hash: None,
            }),
            start_line,
            content: (0..line_count)
                .map(|index| format!("line {}", start_line as usize + index))
                .collect::<Vec<_>>()
                .join("\n"),
            line_count: line_count as u32,
            has_more: false,
        }
    }

    #[test]
    fn source_decoder_preserves_contractual_blank_lines() {
        assert_eq!(decode_source_lines("", 0), Vec::<String>::new());
        assert_eq!(decode_source_lines("", 1), vec![String::new()]);
        assert_eq!(
            decode_source_lines("first\n", 2),
            vec!["first".to_string(), String::new()]
        );
    }

    fn snapshot_with_session() -> v2::Snapshot {
        v2::Snapshot {
            sessions: vec![v2::Session {
                session_id: "session/alpha".to_string(),
                display_name: "mock".to_string(),
                group_id: Some("group/alpha".to_string()),
                revision: 1,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn populated_app(state: v2::ThreadState) -> App {
        let mut app = App::new("http://127.0.0.1:5000".to_string());
        app.apply_snapshot(snapshot_with_session());
        app.apply_threads(vec![thread("thread/alpha", state, "/src/main.rs", 12)]);
        app.apply_frames(vec![frame("frame/0", 0, "/src/main.rs", 12)]);
        app.apply_variables(vec![v2::Variable {
            variable_id: "variable/argc".to_string(),
            name: "argc".to_string(),
            value: "1".to_string(),
            type_name: Some("int".to_string()),
            ..Default::default()
        }]);
        app.apply_source(source("/src/main.rs", 1, 40), 12);
        app
    }

    #[test]
    fn maps_typed_thread_frame_variable_and_memory_resources() {
        let mut app = populated_app(v2::ThreadState::Stopped);
        app.apply_memory(v2::MemoryBlock {
            address: "0x1000".to_string(),
            data: vec![0xde, 0xad],
            unreadable_bytes: 0,
        });
        assert_eq!(app.threads[0].id, "thread/alpha");
        assert_eq!(app.frames[0].id, "frame/0");
        assert_eq!(app.variables[0].value, "1");
        assert_eq!(app.memory[0].bytes, vec![0xde, 0xad]);
        assert_eq!(app.execution_location.as_ref().unwrap().line, 12);
    }

    #[test]
    fn distributed_stack_preserves_owners_boundaries_and_leaf_execution_location() {
        let mut app = populated_app(v2::ThreadState::Stopped);
        app.sessions.push(v2::Session {
            session_id: "session/parent".to_string(),
            display_name: "parent".to_string(),
            revision: 1,
            ..Default::default()
        });
        app.apply_distributed_frames(vec![
            v2::DistributedFrame {
                index: 0,
                session_id: "session/alpha".to_string(),
                thread_id: "thread/alpha".to_string(),
                frame: Some(frame("frame/leaf", 0, "/src/main.rs", 12)),
                boundary: None,
                boundary_label: None,
            },
            v2::DistributedFrame {
                index: 1,
                session_id: "session/parent".to_string(),
                thread_id: "thread/parent".to_string(),
                frame: None,
                boundary: Some(v2::DistributedBoundaryKind::Call as i32),
                boundary_label: Some("RPC call".to_string()),
            },
            v2::DistributedFrame {
                index: 2,
                session_id: "session/parent".to_string(),
                thread_id: "thread/parent".to_string(),
                frame: Some(v2::Frame {
                    thread_id: "thread/parent".to_string(),
                    ..frame("frame/parent", 0, "/src/service.rs", 40)
                }),
                boundary: None,
                boundary_label: None,
            },
        ]);

        assert_eq!(app.frames.len(), 3);
        assert_eq!(app.frames[0].id, "frame/leaf");
        assert!(app.frames[1].boundary);
        assert_eq!(app.frames[1].function, "RPC call");
        assert_eq!(app.frames[2].session_id, "session/parent");
        assert_eq!(app.frames[2].thread_id, "thread/parent");
        assert_eq!(app.execution_location.as_ref().unwrap().line, 12);
    }

    #[test]
    fn source_cursor_remains_independent_when_execution_moves() {
        let mut app = populated_app(v2::ThreadState::Stopped);
        app.set_source_cursor_line(13);
        app.apply_frames(vec![frame("frame/next", 0, "/src/main.rs", 20)]);
        app.apply_source(source("/src/main.rs", 1, 40), 20);
        assert_eq!(app.execution_location.as_ref().unwrap().line, 20);
        assert_eq!(app.source_cursor.as_ref().unwrap().line, 13);
    }

    #[test]
    fn untouched_source_cursor_follows_a_new_execution_stop() {
        let mut app = populated_app(v2::ThreadState::Stopped);
        app.apply_frames(vec![frame("frame/next", 0, "/src/main.rs", 20)]);
        app.apply_source(source("/src/main.rs", 1, 40), 20);
        assert_eq!(app.execution_location.as_ref().unwrap().line, 20);
        assert_eq!(app.source_cursor.as_ref().unwrap().line, 20);
    }

    #[test]
    fn changing_or_running_the_selected_thread_invalidates_stop_details() {
        let mut app = populated_app(v2::ThreadState::Stopped);
        app.apply_threads(vec![thread(
            "thread/beta",
            v2::ThreadState::Stopped,
            "/src/worker.rs",
            7,
        )]);
        assert!(app.frames.is_empty());
        assert!(app.execution_location.is_none());
        assert!(app.source.is_none());

        let mut app = populated_app(v2::ThreadState::Stopped);
        app.apply_threads(vec![thread(
            "thread/alpha",
            v2::ThreadState::Running,
            "/src/main.rs",
            12,
        )]);
        assert!(app.frames.is_empty());
        assert!(app.variables.is_empty());
        assert!(app.execution_location.is_none());
    }

    #[test]
    fn session_local_selected_flags_do_not_replace_tui_thread_selection() {
        let mut app = populated_app(v2::ThreadState::Stopped);
        let child = thread("thread/alpha", v2::ThreadState::Stopped, "/src/main.rs", 12);
        let mut parent = thread(
            "thread/parent",
            v2::ThreadState::Stopped,
            "/src/parent.rs",
            30,
        );
        parent.session_id = "session/parent".to_string();
        assert!(child.selected && parent.selected);

        app.apply_threads(vec![parent, child]);

        assert_eq!(app.current_thread().unwrap().id, "thread/alpha");
    }

    #[test]
    fn selecting_a_caller_frame_moves_only_the_navigation_cursor() {
        let mut app = populated_app(v2::ThreadState::Stopped);
        app.apply_frames(vec![
            frame("frame/0", 0, "/src/main.rs", 12),
            frame("frame/1", 1, "/src/lib.rs", 5),
        ]);
        app.selected_frame = 1;
        app.begin_frame_inspection();
        app.apply_source(source("/src/lib.rs", 1, 20), 5);
        assert_eq!(app.execution_location.as_ref().unwrap().line, 12);
        assert_eq!(app.source_cursor.as_ref().unwrap().line, 5);
    }

    #[test]
    fn source_matching_handles_remote_absolute_and_local_relative_paths() {
        assert!(same_source("/build/project/src/main.rs", "src/main.rs"));
        assert!(same_source("src\\main.rs", "src/main.rs"));
        assert!(!same_source("/a/main.rs", "/b/main.rs"));
    }

    #[test]
    fn panel_hit_testing_excludes_borders_and_applies_scroll_offset() {
        let mut areas = UiAreas::default();
        areas.add_panel(Focus::Threads, Rect::new(10, 5, 20, 8), 4, 1);
        assert_eq!(areas.item_at(11, 6), Some((Focus::Threads, 4)));
        assert_eq!(areas.item_at(11, 9), Some((Focus::Threads, 7)));
        assert_eq!(areas.item_at(10, 6), None);
    }

    #[test]
    fn breakpoint_lookup_is_scoped_to_the_distributed_target() {
        let mut app = populated_app(v2::ThreadState::Stopped);
        app.breakpoints = vec![
            Breakpoint {
                id: "breakpoint/group".to_string(),
                target: Some(group_target("group/alpha")),
                location: BreakpointLocation {
                    src: "/src/main.rs".to_string(),
                    line: 12,
                },
                enabled: true,
                condition: None,
                ignore_count: None,
                temporary: false,
                hardware: false,
                times: 0,
                verified: true,
                pending: false,
                message: None,
                sub_breakpoints: Vec::new(),
            },
            Breakpoint {
                id: "breakpoint/session".to_string(),
                target: Some(session_target("session/beta")),
                location: BreakpointLocation {
                    src: "/src/main.rs".to_string(),
                    line: 12,
                },
                enabled: true,
                condition: None,
                ignore_count: None,
                temporary: false,
                hardware: false,
                times: 0,
                verified: true,
                pending: false,
                message: None,
                sub_breakpoints: Vec::new(),
            },
        ];
        assert_eq!(
            app.breakpoint_at_source_cursor(BreakpointTarget::Group("group/alpha".to_string()))
                .map(|breakpoint| breakpoint.id.as_str()),
            Some("breakpoint/group")
        );
    }

    #[test]
    fn prompt_editing_is_unicode_safe_and_supports_history() {
        let mut app = App::new("http://localhost".to_string());
        app.start_input(InputMode::Evaluate);
        app.insert_input("π界");
        app.move_input_cursor(-1);
        app.backspace_input();
        assert_eq!(app.input, "界");
        app.insert_input("λ");
        assert_eq!(app.commit_input(), "λ界");
        app.start_input(InputMode::Evaluate);
        app.previous_input();
        assert_eq!(app.input, "λ界");
    }

    #[test]
    fn typed_unicode_operation_results_are_safe_for_the_timeline() {
        let mut app = App::new("http://localhost".to_string());
        app.push_receipt(
            "evaluate",
            &v2::Operation {
                state: v2::OperationState::Completed as i32,
                result: Some(v2::OperationResult {
                    value: Some(operation_result::Value::Evaluation(v2::EvaluationResult {
                        expression: "变量".to_string(),
                        value: "界".repeat(80),
                        ..Default::default()
                    })),
                }),
                ..Default::default()
            },
        );
        assert!(app.timeline.back().unwrap().contains("变量"));
    }

    #[test]
    fn partial_fanout_receipts_identify_failed_ddb_targets() {
        let mut app = App::new("http://localhost".to_string());
        app.push_receipt(
            "continue",
            &v2::Operation {
                state: v2::OperationState::Failed as i32,
                target_outcomes: vec![
                    v2::TargetOutcome {
                        target: Some(session_target("session/alpha")),
                        succeeded: true,
                        ..Default::default()
                    },
                    v2::TargetOutcome {
                        target: Some(session_target("session/beta")),
                        error: Some(v2::DdbError {
                            message: "backend timed out".to_string(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        );
        let timeline = app.timeline.iter().cloned().collect::<Vec<_>>().join("\n");
        assert!(timeline.contains("fanout: 1 succeeded · 1 failed"));
        assert!(timeline.contains("session session/beta: backend timed out"));
    }

    #[test]
    fn distributed_backtrace_receipt_reports_truncation_reason() {
        let mut app = App::new("http://localhost".to_string());
        app.push_receipt(
            "distributed backtrace",
            &v2::Operation {
                state: v2::OperationState::Completed as i32,
                result: Some(v2::OperationResult {
                    value: Some(operation_result::Value::DistributedBacktrace(
                        v2::DistributedBacktraceResult {
                            frames: Vec::new(),
                            truncated: true,
                            truncation_reason: Some("max_frames limit reached".to_string()),
                        },
                    )),
                }),
                ..Default::default()
            },
        );

        assert!(app
            .timeline
            .back()
            .unwrap()
            .contains("0 distributed frames · truncated: max_frames limit reached"));
    }
    #[test]
    fn topology_and_breakpoint_picker_preserve_explicit_ddb_scope() {
        let mut app = App::new("http://localhost".to_string());
        app.apply_capabilities(v2::Capabilities {
            breakpoint_features: vec![
                v2::BreakpointFeature::Source as i32,
                v2::BreakpointFeature::Distributed as i32,
                v2::BreakpointFeature::GroupInheritance as i32,
            ],
            supported_operations: vec![v2::OperationKind::CreateBreakpoint as i32],
            ..Default::default()
        });
        app.apply_snapshot(v2::Snapshot {
            groups: vec![
                v2::Group {
                    group_id: "group/a".to_string(),
                    display_name: "A".to_string(),
                    revision: 1,
                    ..Default::default()
                },
                v2::Group {
                    group_id: "group/b".to_string(),
                    display_name: "B".to_string(),
                    revision: 1,
                    ..Default::default()
                },
            ],
            sessions: vec![
                v2::Session {
                    session_id: "session/alpha".to_string(),
                    display_name: "alpha".to_string(),
                    group_id: Some("group/a".to_string()),
                    revision: 1,
                    ..Default::default()
                },
                v2::Session {
                    session_id: "session/beta".to_string(),
                    display_name: "beta".to_string(),
                    group_id: Some("group/a".to_string()),
                    revision: 1,
                    ..Default::default()
                },
                v2::Session {
                    session_id: "session/gamma".to_string(),
                    display_name: "gamma".to_string(),
                    group_id: Some("group/b".to_string()),
                    status: v2::SessionStatus::Failed as i32,
                    status_detail: Some("connection refused".to_string()),
                    revision: 1,
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        let alpha = thread("thread/alpha", v2::ThreadState::Stopped, "/src/main.rs", 12);
        let mut beta = thread("thread/beta", v2::ThreadState::Stopped, "/src/worker.rs", 8);
        beta.session_id = "session/beta".to_string();
        beta.name = Some("worker".to_string());
        app.apply_threads(vec![alpha, beta]);
        app.apply_source(source("/src/main.rs", 1, 30), 12);

        let rows = app.topology_rows();
        assert_eq!(
            rows.iter().map(|row| row.kind.clone()).collect::<Vec<_>>(),
            vec![
                TopologyRowKind::Group("group/a".to_string()),
                TopologyRowKind::Session("session/alpha".to_string()),
                TopologyRowKind::Thread("thread/alpha".to_string()),
                TopologyRowKind::Session("session/beta".to_string()),
                TopologyRowKind::Thread("thread/beta".to_string()),
                TopologyRowKind::Group("group/b".to_string()),
                TopologyRowKind::Session("session/gamma".to_string()),
            ]
        );
        assert_eq!(rows[6].state.as_deref(), Some("failed"));
        assert!(rows[6].detail.contains("connection refused"));
        assert_eq!(app.selected_topology_row(), 2);
        assert!(!app.select_topology_row(5));
        assert_eq!(app.selected_topology_row(), 5);
        assert_eq!(app.current_thread().unwrap().id, "thread/alpha");
        assert!(matches!(
            app.execution_target().and_then(|target| target.selector),
            Some(target::Selector::Group(value)) if value.group_id == "group/b"
        ));
        app.cycle_execution_scope();
        assert!(matches!(
            app.execution_target().and_then(|target| target.selector),
            Some(target::Selector::Broadcast(_))
        ));
        app.cycle_execution_scope();
        assert!(matches!(
            app.execution_target().and_then(|target| target.selector),
            Some(target::Selector::Thread(value)) if value.thread_id == "thread/alpha"
        ));

        app.focus = Focus::Threads;
        app.move_selection(-1);
        assert_eq!(app.selected_topology_row(), 4);
        assert_eq!(app.current_thread().unwrap().id, "thread/beta");
        assert!(matches!(
            app.execution_target().and_then(|target| target.selector),
            Some(target::Selector::Thread(value)) if value.thread_id == "thread/beta"
        ));
        app.move_selection(-1);
        assert_eq!(app.selected_topology_row(), 3);
        assert_eq!(app.current_thread().unwrap().id, "thread/beta");
        assert!(matches!(
            app.execution_target().and_then(|target| target.selector),
            Some(target::Selector::Session(value)) if value.session_id == "session/beta"
        ));
        app.move_selection(-1);
        assert_eq!(app.selected_topology_row(), 2);
        assert_eq!(app.current_thread().unwrap().id, "thread/alpha");

        app.start_breakpoint_target_picker(BreakpointOptions::default())
            .unwrap();
        let group_index = app
            .breakpoint_target_picker
            .as_ref()
            .unwrap()
            .choices
            .iter()
            .position(|choice| choice.target == BreakpointTarget::Group("group/a".to_string()))
            .unwrap();
        app.select_breakpoint_target_choice(group_index);
        let beta_index = app
            .breakpoint_target_picker
            .as_ref()
            .unwrap()
            .choices
            .iter()
            .position(|choice| {
                choice.target == BreakpointTarget::Session("session/beta".to_string())
            })
            .unwrap();
        app.select_breakpoint_target_choice(beta_index);

        let (draft, target) = app.commit_breakpoint_target_picker().unwrap();
        assert_eq!(draft.source, "/src/main.rs");
        assert_eq!(draft.line, 12);
        assert_eq!(target, BreakpointTarget::Group("group/a".to_string()));
    }

    #[test]
    fn reduced_breakpoint_capabilities_hide_native_scopes_and_reject_unsupported_options() {
        let mut app = populated_app(v2::ThreadState::Stopped);
        app.sessions.push(v2::Session {
            session_id: "session/beta".to_string(),
            display_name: "zeta".to_string(),
            revision: 1,
            ..Default::default()
        });
        app.apply_capabilities(v2::Capabilities {
            breakpoint_features: vec![v2::BreakpointFeature::Source as i32],
            supported_operations: vec![v2::OperationKind::CreateBreakpoint as i32],
            ..Default::default()
        });

        let error = app
            .start_breakpoint_target_picker(BreakpointOptions {
                hardware: true,
                ..Default::default()
            })
            .unwrap_err();
        assert!(error.contains("hardware breakpoints are not supported"));

        app.start_breakpoint_target_picker(BreakpointOptions::default())
            .unwrap();
        let picker = app.breakpoint_target_picker.as_ref().unwrap();
        assert_eq!(picker.choices.len(), 2);
        assert!(picker
            .choices
            .iter()
            .all(|choice| matches!(choice.target, BreakpointTarget::Session(_))));

        app.move_breakpoint_target_picker(1);
        app.toggle_breakpoint_target_choice();
        let error = app.commit_breakpoint_target_picker().unwrap_err();
        assert!(
            error.contains("multi-target breakpoints are not supported"),
            "{error}"
        );
    }
    #[test]
    fn empty_snapshot_clears_all_debuggee_views() {
        let mut app = populated_app(v2::ThreadState::Stopped);
        app.apply_snapshot(v2::Snapshot::default());
        assert!(app.threads.is_empty());
        assert!(app.frames.is_empty());
        assert!(app.variables.is_empty());
        assert!(app.execution_location.is_none());
        assert!(app.source_cursor.is_none());
    }

    #[test]
    fn signal_picker_preserves_target_and_selected_typed_signal() {
        let mut app = App::new("http://127.0.0.1:5000".to_string());
        let target = session_target("session/alpha");
        app.open_signal_picker(
            target.clone(),
            vec![
                v2::DebuggerSignal {
                    name: "SIGINT".to_string(),
                    stop: true,
                    print: true,
                    pass: false,
                    description: Some("Interrupt".to_string()),
                },
                v2::DebuggerSignal {
                    name: "SIGUSR1".to_string(),
                    stop: false,
                    print: true,
                    pass: true,
                    description: Some("User signal".to_string()),
                },
            ],
        )
        .unwrap();
        app.move_signal_picker(1);
        let (signal, committed_target) = app.commit_signal_picker().unwrap();
        assert_eq!(signal, "SIGUSR1");
        assert_eq!(committed_target, target);
        assert!(app.signal_picker.is_none());
    }

    #[test]
    fn lazy_variable_tree_collapses_descendants_and_registers_use_requested_format() {
        let mut app = App::new("http://127.0.0.1:5000".to_string());
        app.apply_variables(vec![v2::Variable {
            variable_id: "variable/root".to_string(),
            name: "root".to_string(),
            value: "Root".to_string(),
            has_children: true,
            child_count: Some(1),
            ..Default::default()
        }]);
        assert_eq!(
            app.toggle_selected_variable().as_deref(),
            Some("variable/root")
        );
        app.apply_variable_children(
            "variable/root",
            vec![v2::Variable {
                variable_id: "variable/child".to_string(),
                name: "child".to_string(),
                value: "7".to_string(),
                ..Default::default()
            }],
        );
        assert_eq!(app.variables.len(), 2);
        assert_eq!(app.variables[1].depth, 1);
        assert!(app.variables[0].expanded);
        assert!(app.toggle_selected_variable().is_none());
        assert_eq!(app.variables.len(), 1);
        assert!(!app.variables[0].expanded);

        app.apply_registers(vec![v2::Register {
            name: "rip".to_string(),
            value: "4096".to_string(),
            formatted_value: Some("0x1000".to_string()),
            unavailable: false,
        }]);
        assert_eq!(app.registers[0].name, "rip");
        assert_eq!(app.registers[0].value, "0x1000");
    }

    #[test]
    fn sessions_remain_visible_while_group_metadata_is_missing() {
        let mut app = App::new("http://127.0.0.1:5000".to_string());
        app.apply_snapshot(snapshot_with_session());
        let rows = app.topology_rows();
        assert!(matches!(
            rows[0].kind,
            TopologyRowKind::Group(ref group_id) if group_id == "group/alpha"
        ));
        assert!(rows[0].label.contains("Unresolved group"));
        assert!(matches!(
            rows[1].kind,
            TopologyRowKind::Session(ref session_id) if session_id == "session/alpha"
        ));
    }

    #[test]
    fn snapshot_prunes_threads_from_removed_sessions() {
        let mut app = populated_app(v2::ThreadState::Stopped);
        app.apply_snapshot(v2::Snapshot {
            sessions: vec![v2::Session {
                session_id: "session/beta".to_string(),
                revision: 1,
                ..Default::default()
            }],
            ..Default::default()
        });
        assert!(app.threads.is_empty());
        assert!(app.source.is_none());
    }
}
