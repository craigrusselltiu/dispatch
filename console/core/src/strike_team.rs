// Strike Team: coordinated multi-agent execution.
//
// Pure logic module for managing a task dependency graph. No PTY, TUI, or async
// dependencies. The task file is a markdown document where each task block has
// status, dependencies, prompt, and agent fields parsed via line-by-line string
// matching.

use std::fmt;

use serde::{Deserialize, Serialize};

// ── Types ────────────────────────────────────────────────────────────────────

/// Current state of a single task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Pending,
    Active,
    Done,
    Failed,
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskStatus::Pending => write!(f, "pending"),
            TaskStatus::Active => write!(f, "active"),
            TaskStatus::Done => write!(f, "done"),
            TaskStatus::Failed => write!(f, "failed"),
        }
    }
}

/// A single task in the strike team plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    /// Short identifier like "T1", "T2".
    pub id: String,
    /// Human-readable title from the task heading.
    pub title: String,
    pub status: TaskStatus,
    /// Task IDs this task depends on (e.g. ["T1", "T3"]).
    pub dependencies: Vec<String>,
    /// Self-contained prompt for the agent.
    pub prompt: String,
    /// Callsign of the assigned agent, if any.
    pub agent: Option<String>,
}

/// Lifecycle phase of the strike team.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StrikeTeamPhase {
    Planning,
    Executing,
    Complete,
    Aborted,
}

/// Top-level state for an active strike team.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrikeTeamState {
    pub name: String,
    pub spec_file: String,
    pub repo: String,
    pub phase: StrikeTeamPhase,
    pub tasks: Vec<Task>,
    pub task_file_path: String,
    /// Callsign of the planner agent during the Planning phase.
    /// Persisted so tick_strike_team() can detect planner idle/exit
    /// without scanning slots by task_id (which races against the
    /// main loop clearing task_id before the tick runs).
    pub planner_callsign: Option<String>,
}

// ── Parsing ──────────────────────────────────────────────────────────────────

/// Parse a task file's text content into a list of tasks.
///
/// Expects markdown with `## T<N>: <title>` headings followed by `key: value`
/// lines for status, dependencies, prompt, and agent.
///
/// Prompts support multi-line values: lines indented with 2+ spaces after the
/// `prompt:` line are appended as continuation lines (trimmed, joined with `\n`).
pub fn parse_task_file(content: &str) -> Vec<Task> {
    let mut tasks = Vec::new();
    let mut current: Option<Task> = None;
    let mut in_prompt = false;

    for line in content.lines() {
        let trimmed = line.trim();

        // New task heading: ## T1: Some title
        if let Some(rest) = trimmed.strip_prefix("## ") {
            in_prompt = false;
            // Flush previous task.
            if let Some(task) = current.take() {
                tasks.push(task);
            }
            if let Some((id, title)) = rest.split_once(':') {
                let id = id.trim().to_string();
                let title = title.trim().to_string();
                current = Some(Task {
                    id,
                    title,
                    status: TaskStatus::Pending,
                    dependencies: Vec::new(),
                    prompt: String::new(),
                    agent: None,
                });
            }
            continue;
        }

        // Key-value lines within a task block.
        let Some(task) = current.as_mut() else { continue };

        // Prompt continuation: indented (2+ spaces) non-empty line after prompt:.
        if in_prompt && !trimmed.is_empty() && line.starts_with("  ") {
            task.prompt.push('\n');
            task.prompt.push_str(trimmed);
            continue;
        }
        in_prompt = false;

        if let Some(val) = trimmed.strip_prefix("status:") {
            let val = val.trim();
            task.status = match val {
                "active" => TaskStatus::Active,
                "done" => TaskStatus::Done,
                "failed" => TaskStatus::Failed,
                _ => TaskStatus::Pending,
            };
        } else if let Some(val) = trimmed.strip_prefix("dependencies:") {
            let val = val.trim();
            if val != "none" && !val.is_empty() {
                task.dependencies = val.split(',').map(|s| s.trim().to_string()).collect();
            }
        } else if let Some(val) = trimmed.strip_prefix("prompt:") {
            task.prompt = val.trim().to_string();
            in_prompt = true;
        } else if let Some(val) = trimmed.strip_prefix("agent:") {
            let val = val.trim();
            if !val.is_empty() {
                task.agent = Some(val.to_string());
            }
        }
    }

    // Flush last task.
    if let Some(task) = current {
        tasks.push(task);
    }

    tasks
}

/// Serialize a task list back to the markdown task file format.
pub fn write_task_file(tasks: &[Task]) -> String {
    let mut out = String::new();
    for (i, task) in tasks.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&format!("## {}: {}\n", task.id, task.title));
        out.push_str(&format!("status: {}\n", task.status));
        if task.dependencies.is_empty() {
            out.push_str("dependencies: none\n");
        } else {
            out.push_str(&format!("dependencies: {}\n", task.dependencies.join(", ")));
        }
        // Multi-line prompts: first line after "prompt:", rest indented with 2 spaces.
        let mut prompt_lines = task.prompt.lines();
        if let Some(first) = prompt_lines.next() {
            out.push_str(&format!("prompt: {}\n", first));
            for cont in prompt_lines {
                out.push_str(&format!("  {}\n", cont));
            }
        } else {
            out.push_str("prompt:\n");
        }
        if let Some(agent) = &task.agent {
            out.push_str(&format!("agent: {}\n", agent));
        }
    }
    out
}

// ── Queries ──────────────────────────────────────────────────────────────────

/// Return tasks that are ready to dispatch: status is Pending and all
/// dependencies have status Done.
pub fn ready_tasks(tasks: &[Task]) -> Vec<&Task> {
    tasks.iter().filter(|t| {
        t.status == TaskStatus::Pending && t.dependencies.iter().all(|dep| {
            tasks.iter().any(|d| d.id == *dep && d.status == TaskStatus::Done)
        })
    }).collect()
}

/// Find the task currently assigned to a given agent callsign.
pub fn task_for_agent<'a>(tasks: &'a [Task], callsign: &str) -> Option<&'a Task> {
    tasks.iter().find(|t| {
        t.status == TaskStatus::Active && t.agent.as_deref() == Some(callsign)
    })
}

/// True when every task is Done or Failed (nothing left to run).
pub fn is_complete(tasks: &[Task]) -> bool {
    !tasks.is_empty() && tasks.iter().all(|t| {
        t.status == TaskStatus::Done || t.status == TaskStatus::Failed
    })
}

/// Progress summary string, e.g. "3/7".
pub fn summary(tasks: &[Task]) -> String {
    let done = tasks.iter().filter(|t| t.status == TaskStatus::Done).count();
    format!("{}/{}", done, tasks.len())
}

// ── Mutations ────────────────────────────────────────────────────────────────

/// Mark a task as Active and assign it to an agent.
pub fn assign_task(tasks: &mut [Task], task_id: &str, callsign: &str) {
    if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
        task.status = TaskStatus::Active;
        task.agent = Some(callsign.to_string());
    }
}

/// Mark a task as Done.
pub fn complete_task(tasks: &mut [Task], task_id: &str) {
    if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
        task.status = TaskStatus::Done;
    }
}

/// Mark a task as Failed.
pub fn fail_task(tasks: &mut [Task], task_id: &str) {
    if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
        task.status = TaskStatus::Failed;
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_TASK_FILE: &str = "\
# Strike Team: auth-system
spec: docs/auth-spec.md

## T1: Implement user model
status: pending
dependencies: none
prompt: Create a User struct in src/models/user.rs with serde derives.

## T2: Add user API endpoints
status: pending
dependencies: T1
prompt: Create REST endpoints for CRUD operations on users.
  Add routes for GET /users, POST /users, GET /users/:id.
  Return JSON responses with proper status codes.

## T3: Add authentication middleware
status: pending
dependencies: T1
prompt: Implement JWT authentication middleware in src/middleware/auth.rs.

## T4: Wire auth into endpoints
status: pending
dependencies: T2, T3
prompt: Apply auth middleware to user endpoints. Add integration tests.
";

    #[test]
    fn parse_roundtrip() {
        let tasks = parse_task_file(SAMPLE_TASK_FILE);
        assert_eq!(tasks.len(), 4);

        assert_eq!(tasks[0].id, "T1");
        assert_eq!(tasks[0].title, "Implement user model");
        assert_eq!(tasks[0].status, TaskStatus::Pending);
        assert!(tasks[0].dependencies.is_empty());

        assert_eq!(tasks[1].id, "T2");
        assert_eq!(tasks[1].dependencies, vec!["T1"]);

        assert_eq!(tasks[3].id, "T4");
        assert_eq!(tasks[3].dependencies, vec!["T2", "T3"]);

        // Write and re-parse should preserve all data.
        let written = write_task_file(&tasks);
        let reparsed = parse_task_file(&written);
        assert_eq!(reparsed.len(), tasks.len());
        for (orig, re) in tasks.iter().zip(reparsed.iter()) {
            assert_eq!(orig.id, re.id);
            assert_eq!(orig.title, re.title);
            assert_eq!(orig.status, re.status);
            assert_eq!(orig.dependencies, re.dependencies);
            assert_eq!(orig.prompt, re.prompt);
            assert_eq!(orig.agent, re.agent);
        }
    }

    #[test]
    fn parse_with_agent_and_statuses() {
        let content = "\
## T1: Setup
status: done
dependencies: none
prompt: Do setup.
agent: Alpha

## T2: Build
status: active
dependencies: T1
prompt: Build the thing.
agent: Bravo

## T3: Test
status: pending
dependencies: T1
prompt: Test the thing.
";
        let tasks = parse_task_file(content);
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[0].status, TaskStatus::Done);
        assert_eq!(tasks[0].agent.as_deref(), Some("Alpha"));
        assert_eq!(tasks[1].status, TaskStatus::Active);
        assert_eq!(tasks[1].agent.as_deref(), Some("Bravo"));
        assert_eq!(tasks[2].status, TaskStatus::Pending);
        assert_eq!(tasks[2].agent, None);
    }

    #[test]
    fn write_preserves_agent_field() {
        let tasks = vec![
            Task {
                id: "T1".into(),
                title: "Setup".into(),
                status: TaskStatus::Done,
                dependencies: vec![],
                prompt: "Do setup.".into(),
                agent: Some("Alpha".into()),
            },
        ];
        let written = write_task_file(&tasks);
        assert!(written.contains("agent: Alpha"));
        let reparsed = parse_task_file(&written);
        assert_eq!(reparsed[0].agent.as_deref(), Some("Alpha"));
    }

    #[test]
    fn ready_tasks_no_deps() {
        let tasks = parse_task_file(SAMPLE_TASK_FILE);
        let ready = ready_tasks(&tasks);
        // Only T1 has no dependencies.
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "T1");
    }

    #[test]
    fn ready_tasks_after_completion() {
        let mut tasks = parse_task_file(SAMPLE_TASK_FILE);
        complete_task(&mut tasks, "T1");

        let ready = ready_tasks(&tasks);
        // T2 and T3 both depend only on T1 (now done).
        let ids: Vec<&str> = ready.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"T2"));
        assert!(ids.contains(&"T3"));
    }

    #[test]
    fn ready_tasks_blocked_by_partial_deps() {
        let mut tasks = parse_task_file(SAMPLE_TASK_FILE);
        complete_task(&mut tasks, "T1");
        complete_task(&mut tasks, "T2");

        let ready = ready_tasks(&tasks);
        // T3 is ready (depends on T1, done). T4 depends on T2+T3, T3 not done yet.
        let ids: Vec<&str> = ready.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"T3"));
        assert!(!ids.contains(&"T4"));
    }

    #[test]
    fn ready_tasks_all_deps_met() {
        let mut tasks = parse_task_file(SAMPLE_TASK_FILE);
        complete_task(&mut tasks, "T1");
        complete_task(&mut tasks, "T2");
        complete_task(&mut tasks, "T3");

        let ready = ready_tasks(&tasks);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "T4");
    }

    #[test]
    fn ready_tasks_skips_failed_deps() {
        let mut tasks = parse_task_file(SAMPLE_TASK_FILE);
        fail_task(&mut tasks, "T1");

        // T2, T3, T4 all transitively blocked by T1 being failed (not done).
        let ready = ready_tasks(&tasks);
        assert!(ready.is_empty());
    }

    #[test]
    fn assign_and_lookup() {
        let mut tasks = parse_task_file(SAMPLE_TASK_FILE);
        assign_task(&mut tasks, "T1", "Alpha");

        assert_eq!(tasks[0].status, TaskStatus::Active);
        assert_eq!(tasks[0].agent.as_deref(), Some("Alpha"));

        let found = task_for_agent(&tasks, "Alpha");
        assert!(found.is_some());
        assert_eq!(found.unwrap().id, "T1");

        assert!(task_for_agent(&tasks, "Bravo").is_none());
    }

    #[test]
    fn complete_and_fail() {
        let mut tasks = parse_task_file(SAMPLE_TASK_FILE);

        complete_task(&mut tasks, "T1");
        assert_eq!(tasks[0].status, TaskStatus::Done);

        fail_task(&mut tasks, "T2");
        assert_eq!(tasks[1].status, TaskStatus::Failed);
    }

    #[test]
    fn is_complete_checks_all() {
        let mut tasks = parse_task_file(SAMPLE_TASK_FILE);
        assert!(!is_complete(&tasks));

        for id in ["T1", "T2", "T3"] {
            complete_task(&mut tasks, id);
        }
        assert!(!is_complete(&tasks));

        fail_task(&mut tasks, "T4");
        assert!(is_complete(&tasks));
    }

    #[test]
    fn is_complete_empty() {
        assert!(!is_complete(&[]));
    }

    #[test]
    fn summary_format() {
        let mut tasks = parse_task_file(SAMPLE_TASK_FILE);
        assert_eq!(summary(&tasks), "0/4");

        complete_task(&mut tasks, "T1");
        complete_task(&mut tasks, "T2");
        assert_eq!(summary(&tasks), "2/4");

        fail_task(&mut tasks, "T3");
        assert_eq!(summary(&tasks), "2/4");

        complete_task(&mut tasks, "T4");
        assert_eq!(summary(&tasks), "3/4");
    }

    #[test]
    fn task_status_display() {
        assert_eq!(TaskStatus::Pending.to_string(), "pending");
        assert_eq!(TaskStatus::Active.to_string(), "active");
        assert_eq!(TaskStatus::Done.to_string(), "done");
        assert_eq!(TaskStatus::Failed.to_string(), "failed");
    }

    #[test]
    fn parse_empty_file() {
        let tasks = parse_task_file("");
        assert!(tasks.is_empty());
    }

    #[test]
    fn parse_header_only() {
        let tasks = parse_task_file("# Strike Team: test\nspec: foo.md\n");
        assert!(tasks.is_empty());
    }

    #[test]
    fn parse_multiline_prompt() {
        let content = "\
## T1: Setup database
status: pending
dependencies: none
prompt: Create the database schema in src/db/schema.rs.
  Add tables for users and sessions.
  Include indexes on email and session_token columns.
agent: Alpha

## T2: Simple task
status: pending
dependencies: T1
prompt: Do a simple thing.
";
        let tasks = parse_task_file(content);
        assert_eq!(tasks.len(), 2);

        // T1 has a multi-line prompt joined with newlines.
        assert_eq!(
            tasks[0].prompt,
            "Create the database schema in src/db/schema.rs.\n\
             Add tables for users and sessions.\n\
             Include indexes on email and session_token columns."
        );
        // Agent field is still parsed after multi-line prompt.
        assert_eq!(tasks[0].agent.as_deref(), Some("Alpha"));

        // T2 has a single-line prompt.
        assert_eq!(tasks[1].prompt, "Do a simple thing.");
    }

    #[test]
    fn multiline_prompt_roundtrip() {
        let tasks = vec![
            Task {
                id: "T1".into(),
                title: "Multi-line task".into(),
                status: TaskStatus::Pending,
                dependencies: vec![],
                prompt: "First line.\nSecond line.\nThird line.".into(),
                agent: None,
            },
            Task {
                id: "T2".into(),
                title: "Single-line task".into(),
                status: TaskStatus::Pending,
                dependencies: vec!["T1".into()],
                prompt: "Just one line.".into(),
                agent: None,
            },
        ];
        let written = write_task_file(&tasks);
        assert!(written.contains("prompt: First line.\n  Second line.\n  Third line.\n"));
        assert!(written.contains("prompt: Just one line.\n"));

        let reparsed = parse_task_file(&written);
        assert_eq!(reparsed.len(), 2);
        assert_eq!(reparsed[0].prompt, tasks[0].prompt);
        assert_eq!(reparsed[1].prompt, tasks[1].prompt);
    }

    #[test]
    fn multiline_prompt_in_sample() {
        // Verify the multi-line prompt in the sample task file is parsed correctly.
        let tasks = parse_task_file(SAMPLE_TASK_FILE);
        assert_eq!(
            tasks[1].prompt,
            "Create REST endpoints for CRUD operations on users.\n\
             Add routes for GET /users, POST /users, GET /users/:id.\n\
             Return JSON responses with proper status codes."
        );
        // Single-line prompts still work.
        assert_eq!(
            tasks[0].prompt,
            "Create a User struct in src/models/user.rs with serde derives."
        );
    }
}
