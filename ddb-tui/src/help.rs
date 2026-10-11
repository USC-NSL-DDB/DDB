//! Searchable shortcut catalog. Keep descriptions with their owning pane.

#[derive(Clone, Copy, Debug)]
pub struct Entry {
    pub section: &'static str,
    pub keys: &'static str,
    pub description: &'static str,
}

const ENTRIES: &[Entry] = &[
    Entry {
        section: "Panes",
        keys: "Alt+1…5",
        description: "Topology, Breakpoints, Source, Stack, Variables",
    },
    Entry {
        section: "Panes",
        keys: "Alt+6…9/0",
        description: "Registers, Memory, Timeline, Output, Extensions",
    },
    Entry {
        section: "Panes",
        keys: "Tab / Shift+Tab",
        description: "Focus the next / previous visible pane",
    },
    Entry {
        section: "Panes",
        keys: "z / Esc",
        description: "Expand or restore the focused pane / restore layout",
    },
    Entry {
        section: "Panes",
        keys: "t / O",
        description: "Toggle Timeline / Output",
    },
    Entry {
        section: "Panes",
        keys: "Arrows / wheel",
        description: "Browse rows; Left/Right scroll columns; Home resets columns",
    },
    Entry {
        section: "Execution",
        keys: "F5 / F6",
        description: "Continue / pause the selected target",
    },
    Entry {
        section: "Execution",
        keys: "F10 / F11 / Shift+F11",
        description: "Next / step in / step out; available scopes come from DDB",
    },
    Entry {
        section: "Execution",
        keys: "c",
        description: "Cycle execution scope: thread, session, group, all",
    },
    Entry {
        section: "Execution",
        keys: "s / J",
        description: "Choose a signal / jump execution to a source location",
    },
    Entry {
        section: "Source",
        keys: "o / Ctrl+p",
        description: "Open the source explorer / search debugger-known files",
    },
    Entry {
        section: "Source",
        keys: "/ / Ctrl+f / n / N",
        description: "Search whole-file text; toggle exact/fuzzy in prompt; next / previous match",
    },
    Entry {
        section: "Source",
        keys: "g",
        description: "Go to a line or open path:line without moving execution",
    },
    Entry {
        section: "Source",
        keys: "Ctrl+u / Ctrl+d",
        description: "Move half a visible source page up / down",
    },
    Entry {
        section: "Source",
        keys: "PageUp / PageDown",
        description: "Move a visible page; arrows move one line",
    },
    Entry {
        section: "Source",
        keys: "f / F8",
        description: "Follow execution / reveal the selected stack frame",
    },
    Entry {
        section: "Source",
        keys: "Alt+Left / Alt+Right",
        description: "Previous / next source navigation history entry",
    },
    Entry {
        section: "Source",
        keys: "Gutter markers",
        description: "▶ execution, ▸ source cursor, ● breakpoint",
    },
    Entry {
        section: "Breakpoints",
        keys: "b / B",
        description:
            "Toggle source breakpoint / configure condition, temporary, hardware, ignore count",
    },
    Entry {
        section: "Breakpoints",
        keys: "F",
        description: "Create a function breakpoint with explicit session/group scope",
    },
    Entry {
        section: "Breakpoints",
        keys: "Space / x / Delete",
        description: "Toggle enabled / remove the selected breakpoint",
    },
    Entry {
        section: "Breakpoints",
        keys: "P / right-click",
        description: "Choose a currently paused hit, then Enter jumps to its owning frame",
    },
    Entry {
        section: "Breakpoints",
        keys: "Enter / click",
        description: "Open breakpoint source without changing the inspected thread",
    },
    Entry {
        section: "Breakpoints",
        keys: "H / Hits button",
        description: "Switch all breakpoints / live hits grouped by DDB group and session; Enter/click opens the owning frame",
    },
    Entry {
        section: "Breakpoints",
        keys: "v",
        description: "Toggle flat / file-grouped view in all breakpoints",
    },
    Entry {
        section: "Breakpoints",
        keys: "L",
        description: "Set a log message with {expressions}; an empty template removes it",
    },
    Entry {
        section: "Topology",
        keys: "v",
        description: "Toggle grouped / flat sessions without changing execution scope",
    },
    Entry {
        section: "Topology",
        keys: "i / right-click",
        description: "Open a captured details snapshot; arrows, PageUp/PageDown, wheel scroll",
    },
    Entry {
        section: "Topology",
        keys: "K / Kill button",
        description: "Confirm termination of one session; y accepts; Enter/Esc cancel",
    },
    Entry {
        section: "Topology",
        keys: "Details: y / e",
        description: "Copy captured JSON to the terminal clipboard / export a new local file",
    },
    Entry {
        section: "Stack and values",
        keys: "d",
        description:
            "Refresh the DDB distributed stack; --stack-mode local selects local-only frames",
    },
    Entry {
        section: "Stack and values",
        keys: "e / E",
        description: "Evaluate an expression / inspect an expandable one-shot result",
    },
    Entry {
        section: "Stack and values",
        keys: "w / W",
        description: "Add a watch / edit the selected watch expression",
    },
    Entry {
        section: "Stack and values",
        keys: "Enter / Delete",
        description: "Expand selected variable / remove selected watch or Inspect result",
    },
    Entry {
        section: "Stack and values",
        keys: "=",
        description: "Assign the selected variable or register; Esc cancels",
    },
    Entry {
        section: "Stack and values",
        keys: "m / M",
        description: "Read ADDRESS or ADDRESS ; BYTE_COUNT / read selected value's address",
    },
    Entry {
        section: "Extensions",
        keys: "Up/Down / Left/Right",
        description: "Scroll data / select an action; a or Enter invokes it",
    },
    Entry {
        section: "Extensions",
        keys: "Home / End",
        description: "First / last data page",
    },
    Entry {
        section: "Extensions",
        keys: "Shift+Left/Right",
        description: "Scroll action label; Shift+Home resets it",
    },
    Entry {
        section: "Output and commands",
        keys: "Home / End",
        description: "Oldest output / follow new output; PageUp/PageDown scroll",
    },
    Entry {
        section: "Output and commands",
        keys: ": / r",
        description: "Native debugger command in the selected frame / refresh state",
    },
    Entry {
        section: "Output and commands",
        keys: "Prompt editing",
        description: "Arrows edit/history, Home/End, paste; Esc cancels",
    },
    Entry {
        section: "Help",
        keys: "/",
        description: "Search shortcuts, categories, and descriptions; Enter keeps the filter",
    },
    Entry {
        section: "Help",
        keys: "Ctrl+u",
        description: "Clear the search while editing it",
    },
    Entry {
        section: "Help",
        keys: "Esc / ? / q",
        description: "Stop editing the search / close help when not editing",
    },
];

pub fn entries(query: &str) -> Vec<Entry> {
    let tokens = query
        .to_lowercase()
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    ENTRIES
        .iter()
        .copied()
        .filter(|entry| {
            let text =
                format!("{} {} {}", entry.section, entry.keys, entry.description).to_lowercase();
            tokens.iter().all(|token| text.contains(token))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_matches_keys_categories_and_descriptions_without_case_or_order_constraints() {
        assert!(entries("CTRL+D")
            .iter()
            .any(|entry| entry.description.contains("half")));
        assert!(entries("paused breakpoint")
            .iter()
            .any(|entry| entry.keys == "P / right-click"));
        assert!(entries("Topology")
            .iter()
            .all(|entry| entry.section == "Topology" || entry.description.contains("Topology")));
        assert!(entries("not-a-shortcut").is_empty());
        assert_eq!(entries(" ").len(), ENTRIES.len());
    }
}
