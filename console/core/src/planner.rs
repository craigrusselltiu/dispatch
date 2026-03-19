// Task plan parsing for .dispatch/tasks.md files.
//
// Extracted from main.rs — contains the ParsedTask struct and parse_task_line
// function used by the console to track task dependencies and dispatch order.

/// A parsed task line from .dispatch/tasks.md.
pub struct ParsedTask {
    pub id: String,
    pub title: String,
    pub status: char,           // ' ', '~', 'x'
    pub deps: Vec<String>,      // task IDs from -> arrows
    pub agent: Option<String>,  // from | agent: annotation
    pub line_idx: usize,        // 0-based line index in the file
    pub prefix: String,         // leading whitespace before "- ["
}

/// Parse a single line as a task entry. Returns None if not a task line.
pub fn parse_task_line(line: &str, line_idx: usize) -> Option<ParsedTask> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with("- [") {
        return None;
    }
    let prefix = line[..line.len() - trimmed.len()].to_string();

    // Status char: ' ', '~', or 'x'
    let status = trimmed.as_bytes().get(3).copied()? as char;
    if status != ' ' && status != '~' && status != 'x' {
        return None;
    }
    if !trimmed[4..].starts_with("] ") {
        return None;
    }

    let rest = &trimmed[6..]; // after "- [s] "

    // Task ID: everything up to ": "
    let colon_pos = rest.find(": ")?;
    let id = rest[..colon_pos].to_string();
    let after_id = &rest[colon_pos + 2..];

    // Split off " | agent: Name" from the end
    let (body, agent) = match after_id.rfind(" | agent: ") {
        Some(pos) => (
            &after_id[..pos],
            Some(after_id[pos + 10..].trim().to_string()),
        ),
        None => (after_id, None),
    };

    // Split off " -> dep1, dep2" from the end
    let (title, deps) = match body.rfind(" -> ") {
        Some(pos) => {
            let dep_str = &body[pos + 4..];
            let deps: Vec<String> = dep_str
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            (body[..pos].to_string(), deps)
        }
        None => (body.to_string(), vec![]),
    };

    Some(ParsedTask { id, title, status, deps, agent, line_idx, prefix })
}

// --- Unit tests ----------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_tasks() {
        let lines = vec![
            "# Plan title",
            "",
            "- [ ] t1: First task",
            "- [ ] t2: Second task -> t1",
            "- [x] t3: Done task",
            "- [~] t4: In progress task | agent: Alpha",
        ];
        let tasks: Vec<ParsedTask> = lines.iter().enumerate()
            .filter_map(|(i, l)| parse_task_line(l, i))
            .collect();
        assert_eq!(tasks.len(), 4);

        assert_eq!(tasks[0].id, "t1");
        assert_eq!(tasks[0].title, "First task");
        assert_eq!(tasks[0].status, ' ');
        assert!(tasks[0].deps.is_empty());

        assert_eq!(tasks[1].id, "t2");
        assert_eq!(tasks[1].deps, vec!["t1"]);

        assert_eq!(tasks[2].status, 'x');

        assert_eq!(tasks[3].status, '~');
        assert_eq!(tasks[3].agent.as_deref(), Some("Alpha"));
    }

    #[test]
    fn parse_multiple_deps() {
        let task = parse_task_line("- [ ] t3: Update imports -> t1.1, t1.2", 0).unwrap();
        assert_eq!(task.deps, vec!["t1.1", "t1.2"]);
    }

    #[test]
    fn parse_indented_subtasks() {
        let lines = vec![
            "- [ ] t1: Parent task",
            "  - [ ] t1.1: Subtask one",
            "  - [ ] t1.2: Subtask two -> t1.1",
        ];
        let tasks: Vec<ParsedTask> = lines.iter().enumerate()
            .filter_map(|(i, l)| parse_task_line(l, i))
            .collect();
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[1].id, "t1.1");
        assert_eq!(tasks[2].deps, vec!["t1.1"]);
    }

    /// Helper: given ParsedTasks, find open tasks whose deps are all done.
    fn find_ready(tasks: &[ParsedTask]) -> Vec<&ParsedTask> {
        let done: std::collections::HashSet<&str> = tasks.iter()
            .filter(|t| t.status == 'x')
            .map(|t| t.id.as_str())
            .collect();
        tasks.iter()
            .filter(|t| t.status == ' ' && t.deps.iter().all(|d| done.contains(d.as_str())))
            .collect()
    }

    fn task(id: &str, status: char, deps: Vec<&str>) -> ParsedTask {
        ParsedTask {
            id: id.into(), title: "".into(), status, deps: deps.into_iter().map(|s| s.into()).collect(),
            agent: None, line_idx: 0, prefix: String::new(),
        }
    }

    #[test]
    fn find_ready_no_deps() {
        let tasks = vec![task("t1", ' ', vec![]), task("t2", ' ', vec!["t1"])];
        let ready = find_ready(&tasks);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "t1");
    }

    #[test]
    fn find_ready_after_completion() {
        let tasks = vec![
            task("t1", 'x', vec![]),
            task("t2", ' ', vec!["t1"]),
            task("t3", ' ', vec!["t1", "t2"]),
        ];
        let ready = find_ready(&tasks);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "t2");
    }

    #[test]
    fn find_ready_skips_in_progress() {
        let tasks = vec![task("t1", '~', vec![])];
        let ready = find_ready(&tasks);
        assert!(ready.is_empty());
    }

    #[test]
    fn parse_deps_with_agent_annotation() {
        let task = parse_task_line("- [~] t2: Task B -> t1 | agent: Bravo", 0).unwrap();
        assert_eq!(task.deps, vec!["t1"]);
        assert_eq!(task.agent.as_deref(), Some("Bravo"));
    }
}
