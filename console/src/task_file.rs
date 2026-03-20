// Task file operations: parsing and updating .dispatch/tasks.md (dispatch-1lc.3).
//
// Task format:
//   - [ ] t1: Description                    (open, no deps)
//   - [ ] t2: Description -> t1              (open, blocked by t1)
//   - [~] t3: Description | agent: Alpha     (in progress)
//   - [x] t4: Description                    (done)
//
// A task is "ready" when status is [ ] and all -> deps are [x].

use dispatch_core::tasks::{ParsedTask, parse_task_line};

use crate::types::{QueuedTask, TaskEntry, SlotState, MAX_SLOTS};

pub fn tasks_md_path(repo_root: &str) -> String {
    format!("{}/.dispatch/tasks.md", repo_root)
}

/// Read and parse .dispatch/tasks.md. Returns (all lines, parsed tasks).
pub fn parse_tasks_md(repo_root: &str) -> (Vec<String>, Vec<ParsedTask>) {
    let path = tasks_md_path(repo_root);
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return (vec![], vec![]),
    };
    let lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
    let mut tasks = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        if let Some(task) = parse_task_line(line, idx) {
            tasks.push(task);
        }
    }
    (lines, tasks)
}

/// Reconstruct a task line from parsed components.
pub fn format_task_line(task: &ParsedTask) -> String {
    let mut line = format!("{}- [{}] {}: {}", task.prefix, task.status, task.id, task.title);
    if !task.deps.is_empty() {
        line.push_str(&format!(" -> {}", task.deps.join(", ")));
    }
    if let Some(agent) = &task.agent {
        line.push_str(&format!(" | agent: {}", agent));
    }
    line
}

/// Fetch tasks ready for dispatch: status [ ] with all -> deps marked [x].
pub fn fetch_ready_tasks(repo_root: &str) -> Vec<QueuedTask> {
    let (_, tasks) = parse_tasks_md(repo_root);
    let done: std::collections::HashSet<&str> = tasks
        .iter()
        .filter(|t| t.status == 'x')
        .map(|t| t.id.as_str())
        .collect();
    tasks
        .iter()
        .filter(|t| t.status == ' ' && t.deps.iter().all(|d| done.contains(d.as_str())))
        .map(|t| QueuedTask { id: t.id.clone(), title: t.title.clone() })
        .collect()
}

/// Update a task's status and agent annotation in .dispatch/tasks.md.
pub fn update_task_in_file(repo_root: &str, id: &str, new_status: char, agent: Option<&str>) -> bool {
    let (mut lines, tasks) = parse_tasks_md(repo_root);
    let task = match tasks.iter().find(|t| t.id == id) {
        Some(t) => t,
        None => return false,
    };
    let updated = ParsedTask {
        id: task.id.clone(),
        title: task.title.clone(),
        status: new_status,
        deps: task.deps.clone(),
        agent: agent.map(|s| s.to_string()),
        line_idx: task.line_idx,
        prefix: task.prefix.clone(),
    };
    lines[task.line_idx] = format_task_line(&updated);
    let path = tasks_md_path(repo_root);
    std::fs::write(&path, lines.join("\n") + "\n").is_ok()
}

/// Create a new task entry in .dispatch/tasks.md. Returns the generated ID.
pub fn create_task_in_file(repo_root: &str, prompt: &str) -> Option<String> {
    let dispatch_dir = format!("{}/.dispatch", repo_root);
    let _ = std::fs::create_dir_all(&dispatch_dir);

    let (lines, tasks) = parse_tasks_md(repo_root);

    // Next sequential ID: find highest top-level t{N} and increment.
    let max_num = tasks
        .iter()
        .filter_map(|t| {
            let num = t.id.strip_prefix('t')?;
            if num.contains('.') { return None; }
            num.parse::<u32>().ok()
        })
        .max()
        .unwrap_or(0);
    let new_id = format!("t{}", max_num + 1);
    let new_line = format!("- [ ] {}: {}", new_id, prompt);

    let path = tasks_md_path(repo_root);
    let content = if lines.is_empty() {
        format!("# Tasks\n\n{}\n", new_line)
    } else {
        let mut c = lines.join("\n");
        if !c.ends_with('\n') {
            c.push('\n');
        }
        c.push_str(&new_line);
        c.push('\n');
        c
    };
    std::fs::write(&path, &content).ok()?;
    Some(new_id)
}

/// Fetch all tasks for the task list overlay (dispatch-1lc.3, dispatch-1lc.4).
/// Cross-references with active agent slots to annotate in-progress tasks.
pub fn fetch_task_list_from_file(
    repo_root: &str,
    slots: &[Option<SlotState>; MAX_SLOTS],
) -> Vec<TaskEntry> {
    let mut slot_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for slot in slots.iter().flatten() {
        if let Some(id) = &slot.task_id {
            slot_map.insert(id.clone(), slot.display_name().to_string());
        }
    }

    let (_, tasks) = parse_tasks_md(repo_root);
    tasks
        .iter()
        .map(|t| {
            let status = match t.status {
                '~' => "in_progress",
                'x' => "closed",
                _ => "open",
            };
            let agent = slot_map.get(&t.id).cloned().or_else(|| t.agent.clone());
            TaskEntry {
                id: t.id.clone(),
                title: t.title.clone(),
                status: status.to_string(),
                agent,
                deps: t.deps.clone(),
            }
        })
        .collect()
}
