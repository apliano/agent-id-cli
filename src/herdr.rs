use std::{collections::HashMap, env, ffi::OsString, process::Command};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::activity::ActivityState;
use crate::registry::Assignment;

const OMP_EXTENSION_OWNER: &str = "omp";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DiscoveredAssignment {
    #[serde(flatten)]
    pub assignment: Assignment,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<RuntimeProjection>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RuntimeProjection {
    pub provider: &'static str,
    pub state: String,
    pub observed_at: DateTime<Utc>,
    pub locations: Vec<HerdrLocation>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HerdrLocation {
    pub agent_status: String,
    pub pane_id: String,
    pub tab_id: String,
    pub workspace_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub foreground_cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<HerdrWorktree>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HerdrWorktree {
    pub repo_key: String,
    pub repo_name: String,
    pub repo_root: String,
    pub checkout_path: String,
    pub is_linked_worktree: bool,
}

#[derive(Debug, Deserialize)]
struct ApiResponse {
    result: ApiResult,
}

#[derive(Debug, Deserialize)]
struct ApiResult {
    snapshot: Snapshot,
}

#[derive(Debug, Deserialize)]
struct Snapshot {
    agents: Vec<AgentInfo>,
    tabs: Vec<TabInfo>,
    workspaces: Vec<WorkspaceInfo>,
}

#[derive(Debug, Deserialize)]
struct AgentInfo {
    agent: Option<String>,
    agent_session: Option<AgentSession>,
    agent_status: String,
    cwd: Option<String>,
    foreground_cwd: Option<String>,
    pane_id: String,
    tab_id: String,
    workspace_id: String,
}

#[derive(Debug, Deserialize)]
struct AgentSession {
    agent: String,
    kind: String,
    source: String,
    value: String,
}

#[derive(Debug, Deserialize)]
struct TabInfo {
    tab_id: String,
    label: String,
}

#[derive(Debug, Deserialize)]
struct WorkspaceInfo {
    workspace_id: String,
    label: String,
    worktree: Option<HerdrWorktree>,
}

pub fn augment_discovery(
    assignments: Vec<Assignment>,
    include_all: bool,
) -> Vec<DiscoveredAssignment> {
    if !herdr_environment()
        || !assignments
            .iter()
            .any(|assignment| omp_session_file(assignment).is_some())
    {
        return base_records(assignments);
    }

    match load_snapshot() {
        Ok(snapshot) => {
            let records = join_snapshot(assignments, snapshot, Utc::now());
            if include_all {
                records
            } else {
                records
                    .into_iter()
                    .filter(|record| record.runtime.is_some())
                    .collect()
            }
        }
        Err(error) => {
            eprintln!("agent-id: unable to enrich discover from Herdr: {error:#}");
            base_records(assignments)
        }
    }
}

pub fn base_records(assignments: Vec<Assignment>) -> Vec<DiscoveredAssignment> {
    assignments
        .into_iter()
        .map(|assignment| DiscoveredAssignment {
            assignment,
            runtime: None,
        })
        .collect()
}

fn herdr_environment() -> bool {
    env::var_os("HERDR_ENV").as_deref() == Some(std::ffi::OsStr::new("1"))
        && env::var_os("HERDR_SOCKET_PATH").is_some_and(|value| !value.is_empty())
}

fn herdr_binary() -> OsString {
    env::var_os("HERDR_BIN_PATH")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| OsString::from("herdr"))
}

fn load_snapshot() -> Result<Snapshot> {
    let output = Command::new(herdr_binary())
        .args(["api", "snapshot"])
        .output()
        .context("run `herdr api snapshot`")?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        bail!("`herdr api snapshot` failed: {}", detail.trim());
    }
    let response: ApiResponse =
        serde_json::from_slice(&output.stdout).context("parse Herdr session snapshot")?;
    Ok(response.result.snapshot)
}

fn join_snapshot(
    assignments: Vec<Assignment>,
    snapshot: Snapshot,
    observed_at: DateTime<Utc>,
) -> Vec<DiscoveredAssignment> {
    let mut by_session_id = HashMap::new();
    let mut by_session_file = HashMap::new();
    for (index, assignment) in assignments.iter().enumerate() {
        by_session_id.insert(assignment.session_id.as_str(), index);
        let Some(session_file) = omp_session_file(assignment) else {
            continue;
        };
        by_session_file
            .entry(session_file)
            .and_modify(|index: &mut Option<usize>| *index = None)
            .or_insert(Some(index));
    }

    let tabs: HashMap<_, _> = snapshot
        .tabs
        .into_iter()
        .map(|tab| (tab.tab_id, tab.label))
        .collect();
    let workspaces: HashMap<_, _> = snapshot
        .workspaces
        .into_iter()
        .map(|workspace| {
            (
                workspace.workspace_id,
                (workspace.label, workspace.worktree),
            )
        })
        .collect();
    let mut locations = vec![Vec::new(); assignments.len()];

    for agent in snapshot.agents {
        let Some(session) = agent.agent_session.as_ref() else {
            continue;
        };
        if agent.agent.as_deref() != Some("omp")
            || session.agent != "omp"
            || session.source != "herdr:omp"
        {
            continue;
        }
        let index = match session.kind.as_str() {
            "id" => by_session_id.get(session.value.as_str()).copied(),
            "path" => by_session_file
                .get(session.value.as_str())
                .copied()
                .flatten(),
            _ => None,
        };
        let Some(index) = index else {
            continue;
        };
        let (workspace_label, worktree) = workspaces
            .get(&agent.workspace_id)
            .map(|(label, worktree)| (Some(label.clone()), worktree.clone()))
            .unwrap_or((None, None));
        locations[index].push(HerdrLocation {
            agent_status: agent.agent_status,
            pane_id: agent.pane_id,
            tab_label: tabs.get(&agent.tab_id).cloned(),
            tab_id: agent.tab_id,
            workspace_label,
            workspace_id: agent.workspace_id,
            cwd: agent.cwd,
            foreground_cwd: agent.foreground_cwd,
            worktree,
        });
    }

    assignments
        .into_iter()
        .zip(locations)
        .map(|(mut assignment, mut locations)| {
            locations.sort_by(|left, right| {
                (&left.workspace_id, &left.tab_id, &left.pane_id).cmp(&(
                    &right.workspace_id,
                    &right.tab_id,
                    &right.pane_id,
                ))
            });
            let runtime = (!locations.is_empty()).then(|| {
                let state = locations[0].agent_status.clone();
                assignment.state = ActivityState::from_external(&state, observed_at);
                RuntimeProjection {
                    provider: "herdr",
                    state,
                    observed_at,
                    locations,
                }
            });
            DiscoveredAssignment {
                assignment,
                runtime,
            }
        })
        .collect()
}

fn omp_session_file(assignment: &Assignment) -> Option<&str> {
    assignment
        .extensions
        .get(OMP_EXTENSION_OWNER)?
        .data
        .get("session_file")?
        .as_str()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::TimeZone;
    use serde_json::json;

    use super::*;
    use crate::registry::ExtensionMetadata;

    fn assignment(session_id: &str, session_file: Option<&str>) -> Assignment {
        let now = Utc.timestamp_opt(0, 0).single().unwrap();
        let mut extensions = BTreeMap::new();
        if let Some(session_file) = session_file {
            extensions.insert(
                "omp".to_string(),
                ExtensionMetadata {
                    data: json!({ "session_file": session_file }),
                    updated_at: now,
                },
            );
        }
        Assignment {
            version: 1,
            session_id: session_id.to_string(),
            name: format!("{session_id} Agent of Test"),
            slug: format!("{session_id}-agent-test"),
            first_name: session_id.to_string(),
            family_name: "Agent".to_string(),
            realm: "Test".to_string(),
            summary: None,
            state: ActivityState::unknown(now),
            cwd: None,
            extensions,
            created_at: now,
            updated_at: now,
        }
    }

    fn agent(kind: &str, value: &str, pane_id: &str) -> AgentInfo {
        AgentInfo {
            agent: Some("omp".to_string()),
            agent_session: Some(AgentSession {
                agent: "omp".to_string(),
                kind: kind.to_string(),
                source: "herdr:omp".to_string(),
                value: value.to_string(),
            }),
            agent_status: "working".to_string(),
            cwd: Some("/work".to_string()),
            foreground_cwd: Some("/work".to_string()),
            pane_id: pane_id.to_string(),
            tab_id: "w1:t1".to_string(),
            workspace_id: "w1".to_string(),
        }
    }

    #[test]
    fn joins_path_and_id_references_without_guessing() {
        let now = Utc.timestamp_opt(1, 0).single().unwrap();
        let assignments = vec![
            assignment("path-session", Some("/tmp/path-session.jsonl")),
            assignment("id-session", None),
        ];
        let snapshot = Snapshot {
            agents: vec![
                agent("path", "/tmp/path-session.jsonl", "w1:p1"),
                agent("id", "id-session", "w1:p2"),
                agent("path", "/tmp/unmatched.jsonl", "w1:p3"),
            ],
            tabs: vec![TabInfo {
                tab_id: "w1:t1".to_string(),
                label: "agents".to_string(),
            }],
            workspaces: vec![WorkspaceInfo {
                workspace_id: "w1".to_string(),
                label: "project".to_string(),
                worktree: None,
            }],
        };

        let records = join_snapshot(assignments, snapshot, now);

        assert_eq!(
            records[0].runtime.as_ref().unwrap().locations[0].pane_id,
            "w1:p1"
        );
        assert_eq!(records[0].runtime.as_ref().unwrap().state, "working");
        assert_eq!(records[0].assignment.state.value.to_string(), "working");
        assert_eq!(records[0].assignment.state.updated_at, now);
        assert_eq!(
            records[1].runtime.as_ref().unwrap().locations[0].pane_id,
            "w1:p2"
        );
        assert_eq!(records[0].runtime.as_ref().unwrap().observed_at, now);
    }
}
