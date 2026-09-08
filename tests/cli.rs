use std::process::Command;

#[cfg(unix)]
use std::{fs, os::unix::fs::PermissionsExt};

use agent_id_cli::registry::Assignment;
use serde_json::Value;
use tempfile::TempDir;

fn command(root: &TempDir) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_agent-id"));
    command
        .env("AGENT_ID_HOME", root.path())
        .env("AGENT_REALM", "Darkwood")
        .env_remove("AGENT_ID_SESSION_ID")
        .env_remove("AGENT_SESSION_ID")
        .env_remove("OMP_SESSION_ID")
        .env_remove("PI_SESSION_ID")
        .env_remove("HERDR_ENV")
        .env_remove("HERDR_SOCKET_PATH")
        .env_remove("HERDR_BIN_PATH");
    command
}

#[cfg(unix)]
fn executable(path: &std::path::Path, contents: &str) {
    fs::write(path, contents).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

#[test]
fn register_and_lookup_print_the_same_name() {
    let root = TempDir::new().unwrap();
    let registered = command(&root)
        .args(["register", "session-1", "--family", "Oak"])
        .output()
        .unwrap();
    assert!(registered.status.success(), "{registered:?}");

    let name = String::from_utf8(registered.stdout).unwrap();
    assert!(name.ends_with(" Oak of Darkwood\n"), "{name:?}");

    let looked_up = command(&root)
        .args(["lookup", "session-1"])
        .output()
        .unwrap();
    assert!(looked_up.status.success(), "{looked_up:?}");
    assert_eq!(looked_up.stdout, name.as_bytes());
}

#[test]
fn json_contains_the_canonical_assignment() {
    let root = TempDir::new().unwrap();
    let output = command(&root)
        .args(["register", "session-2", "--family", "Oak", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");

    let assignment: Assignment = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(assignment.version, 1);
    assert_eq!(assignment.session_id, "session-2");
    assert_eq!(assignment.family_name, "Oak");
    assert_eq!(assignment.realm, "Darkwood");
    assert_eq!(assignment.state.value.to_string(), "unknown");
    let persisted: Value = serde_json::from_str(
        &std::fs::read_to_string(root.path().join("by-session/session-2.json")).unwrap(),
    )
    .unwrap();
    assert!(persisted.get("state").is_none());
    assert_eq!(
        assignment.slug,
        format!(
            "{}-oak-darkwood",
            assignment.first_name.to_ascii_lowercase()
        )
    );

    let lookup = command(&root)
        .args(["lookup", "session-2", "--json"])
        .output()
        .unwrap();
    let looked_up: Assignment = serde_json::from_slice(&lookup.stdout).unwrap();
    assert_eq!(looked_up, assignment);

    for identifier in [assignment.name.as_str(), assignment.slug.as_str()] {
        let lookup = command(&root)
            .args(["lookup", identifier, "--json"])
            .output()
            .unwrap();
        assert!(lookup.status.success(), "{lookup:?}");
        let looked_up: Assignment = serde_json::from_slice(&lookup.stdout).unwrap();
        assert_eq!(looked_up, assignment);
    }
}

#[test]
fn legacy_top_level_state_is_rejected() {
    let root = TempDir::new().unwrap();
    let registered = command(&root)
        .args(["register", "legacy-state", "--json"])
        .output()
        .unwrap();
    assert!(registered.status.success(), "{registered:?}");

    let path = root.path().join("by-session/legacy-state.json");
    let mut persisted: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    persisted["state"] = serde_json::json!({
        "value": "waiting",
        "updated_at": "2026-01-01T00:00:00Z",
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&persisted).unwrap()).unwrap();

    let lookup = command(&root)
        .args(["lookup", "legacy-state", "--json"])
        .output()
        .unwrap();
    assert!(!lookup.status.success(), "{lookup:?}");
    assert!(
        String::from_utf8_lossy(&lookup.stderr).contains("unknown field `state`"),
        "{lookup:?}"
    );
}

#[test]
fn annotate_updates_discovers_and_clears_summary() {
    let root = TempDir::new().unwrap();
    let registered = command(&root)
        .args(["register", "summary-session", "--json"])
        .output()
        .unwrap();
    assert!(registered.status.success(), "{registered:?}");
    let registered: Assignment = serde_json::from_slice(&registered.stdout).unwrap();

    let annotated = command(&root)
        .args([
            "annotate",
            "summary-session",
            "--summary",
            "  Implementing\n  activity summaries  ",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(annotated.status.success(), "{annotated:?}");
    let annotated: Assignment = serde_json::from_slice(&annotated.stdout).unwrap();
    let summary = annotated.summary.as_ref().unwrap();
    assert_eq!(summary.text, "Implementing activity summaries");
    assert!(summary.updated_at >= registered.updated_at);
    assert_eq!(annotated.name, registered.name);
    assert_eq!(annotated.created_at, registered.created_at);

    let lookup = command(&root)
        .args(["lookup", "summary-session", "--json"])
        .output()
        .unwrap();
    assert!(lookup.status.success(), "{lookup:?}");
    assert_eq!(
        serde_json::from_slice::<Assignment>(&lookup.stdout).unwrap(),
        annotated
    );

    let discovered = command(&root)
        .args(["discover", "--json"])
        .output()
        .unwrap();
    assert!(discovered.status.success(), "{discovered:?}");
    let discovered: Vec<Assignment> = serde_json::from_slice(&discovered.stdout).unwrap();
    assert_eq!(discovered, vec![annotated.clone()]);

    let human = command(&root).args(["discover"]).output().unwrap();
    assert!(human.status.success(), "{human:?}");
    assert_eq!(
        String::from_utf8(human.stdout).unwrap(),
        format!(
            "{}\t{}\tstate:unknown\tsummary:Implementing activity summaries\n",
            annotated.name, annotated.session_id
        )
    );

    let cleared = command(&root)
        .args(["annotate", "summary-session", "--clear-summary", "--json"])
        .output()
        .unwrap();
    assert!(cleared.status.success(), "{cleared:?}");
    let cleared: Assignment = serde_json::from_slice(&cleared.stdout).unwrap();
    assert_eq!(cleared.summary, None);
    assert_eq!(cleared.name, registered.name);
    assert_eq!(cleared.created_at, registered.created_at);

    let human = command(&root).args(["discover"]).output().unwrap();
    assert!(human.status.success(), "{human:?}");
    assert_eq!(
        String::from_utf8(human.stdout).unwrap(),
        format!("{}\t{}\tstate:unknown\n", cleared.name, cleared.session_id)
    );
}

#[test]
fn annotate_updates_state_independently_from_summary() {
    let root = TempDir::new().unwrap();
    let registered = command(&root)
        .args(["register", "state-session", "--json"])
        .output()
        .unwrap();
    assert!(registered.status.success(), "{registered:?}");
    let registered: Assignment = serde_json::from_slice(&registered.stdout).unwrap();

    let working = command(&root)
        .args(["annotate", "state-session", "--state", "working", "--json"])
        .output()
        .unwrap();
    assert!(working.status.success(), "{working:?}");
    let working: Assignment = serde_json::from_slice(&working.stdout).unwrap();
    assert_eq!(working.state.value.to_string(), "working");
    assert_eq!(working.extensions["omp"].data["state"]["value"], "working");
    let persisted: Value = serde_json::from_str(
        &std::fs::read_to_string(root.path().join("by-session/state-session.json")).unwrap(),
    )
    .unwrap();
    assert!(persisted.get("state").is_none());
    assert_eq!(
        persisted["extensions"]["omp"]["data"]["state"]["value"],
        "working"
    );
    assert_eq!(working.summary, None);
    assert_eq!(working.name, registered.name);
    assert_eq!(working.created_at, registered.created_at);
    assert!(working.updated_at >= registered.updated_at);

    let waiting = command(&root)
        .args([
            "annotate",
            "state-session",
            "--summary",
            "Waiting for review",
            "--state",
            "waiting",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(waiting.status.success(), "{waiting:?}");
    let waiting: Assignment = serde_json::from_slice(&waiting.stdout).unwrap();
    assert_eq!(waiting.summary.as_ref().unwrap().text, "Waiting for review");
    assert_eq!(waiting.state.value.to_string(), "waiting");

    let human = command(&root).args(["discover"]).output().unwrap();
    assert!(human.status.success(), "{human:?}");
    assert_eq!(
        String::from_utf8(human.stdout).unwrap(),
        format!(
            "{}\t{}\tstate:waiting\tsummary:Waiting for review\n",
            waiting.name, waiting.session_id
        )
    );

    let cleared = command(&root)
        .args(["annotate", "state-session", "--clear-state", "--json"])
        .output()
        .unwrap();
    assert!(cleared.status.success(), "{cleared:?}");
    let cleared: Assignment = serde_json::from_slice(&cleared.stdout).unwrap();
    assert_eq!(cleared.state.value.to_string(), "unknown");
    assert!(cleared.extensions["omp"].data.get("state").is_none());
    assert_eq!(cleared.summary.as_ref().unwrap().text, "Waiting for review");

    let invalid = command(&root)
        .args(["annotate", "state-session", "--state", "not-a-state"])
        .output()
        .unwrap();
    assert!(!invalid.status.success());
}

#[test]
fn annotate_updates_and_clears_cwd_metadata() {
    let root = TempDir::new().unwrap();
    let registered = command(&root)
        .args(["register", "cwd-session", "--json"])
        .output()
        .unwrap();
    assert!(registered.status.success(), "{registered:?}");
    let registered: Assignment = serde_json::from_slice(&registered.stdout).unwrap();

    let annotated = command(&root)
        .args([
            "annotate",
            "cwd-session",
            "--cwd",
            "  /work/agent-id  ",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(annotated.status.success(), "{annotated:?}");
    let annotated: Assignment = serde_json::from_slice(&annotated.stdout).unwrap();
    assert_eq!(annotated.cwd.as_deref(), Some("/work/agent-id"));
    assert_eq!(annotated.name, registered.name);
    assert_eq!(annotated.created_at, registered.created_at);

    let human = command(&root).args(["discover"]).output().unwrap();
    assert!(human.status.success(), "{human:?}");
    assert_eq!(
        String::from_utf8(human.stdout).unwrap(),
        format!(
            "{}\t{}\tstate:unknown\tcwd:/work/agent-id\n",
            annotated.name, annotated.session_id
        )
    );

    let stateful = command(&root)
        .args(["annotate", "cwd-session", "--state", "working", "--json"])
        .output()
        .unwrap();
    assert!(stateful.status.success(), "{stateful:?}");
    let stateful: Assignment = serde_json::from_slice(&stateful.stdout).unwrap();
    assert_eq!(stateful.cwd.as_deref(), Some("/work/agent-id"));
    assert_eq!(stateful.state.value.to_string(), "working");
    let cleared = command(&root)
        .args(["annotate", "cwd-session", "--clear-cwd", "--json"])
        .output()
        .unwrap();
    assert!(cleared.status.success(), "{cleared:?}");
    let cleared: Assignment = serde_json::from_slice(&cleared.stdout).unwrap();
    assert_eq!(cleared.cwd, None);
    assert_eq!(cleared.state.value.to_string(), "working");

    let invalid = command(&root)
        .args(["annotate", "cwd-session", "--cwd", "/work\nagent-id"])
        .output()
        .unwrap();
    assert!(!invalid.status.success());
}

#[test]
fn annotate_updates_and_clears_namespaced_extension_metadata() {
    let root = TempDir::new().unwrap();
    let registered = command(&root)
        .args(["register", "extension-session", "--json"])
        .output()
        .unwrap();
    assert!(registered.status.success(), "{registered:?}");

    let annotated = command(&root)
        .args([
            "annotate",
            "extension-session",
            "--extension",
            r#"omp={"session_file":"/tmp/extension-session.jsonl"}"#,
            "--extension",
            "counter=42",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(annotated.status.success(), "{annotated:?}");
    let annotated: Assignment = serde_json::from_slice(&annotated.stdout).unwrap();
    assert_eq!(
        annotated.extensions["omp"].data["session_file"],
        "/tmp/extension-session.jsonl"
    );
    assert_eq!(annotated.extensions["counter"].data, 42);

    let cleared = command(&root)
        .args([
            "annotate",
            "extension-session",
            "--clear-extension",
            "counter",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(cleared.status.success(), "{cleared:?}");
    let cleared: Assignment = serde_json::from_slice(&cleared.stdout).unwrap();
    assert!(!cleared.extensions.contains_key("counter"));
    assert!(cleared.extensions.contains_key("omp"));

    for extension in ["Missing={}", "missing-json", "omp={"] {
        let invalid = command(&root)
            .args(["annotate", "extension-session", "--extension", extension])
            .output()
            .unwrap();
        assert!(!invalid.status.success(), "{extension}: {invalid:?}");
    }

    let conflicting = command(&root)
        .args([
            "annotate",
            "extension-session",
            "--extension",
            "omp={}",
            "--clear-extension",
            "omp",
        ])
        .output()
        .unwrap();
    assert!(!conflicting.status.success(), "{conflicting:?}");
}

#[test]
fn register_returns_and_updates_an_existing_identity() {
    let root = TempDir::new().unwrap();
    let first = command(&root)
        .args(["register", "session-3", "--json"])
        .output()
        .unwrap();
    assert!(first.status.success(), "{first:?}");
    let first: Assignment = serde_json::from_slice(&first.stdout).unwrap();

    let second = command(&root)
        .args(["register", "session-3", "--json"])
        .output()
        .unwrap();
    assert!(second.status.success(), "{second:?}");
    let second: Assignment = serde_json::from_slice(&second.stdout).unwrap();

    assert_eq!(second.name, first.name);
    assert_eq!(second.created_at, first.created_at);
    assert!(second.updated_at >= first.updated_at);
}

#[test]
fn commands_discover_session_id_from_environment() {
    let root = TempDir::new().unwrap();
    let mut register = command(&root);
    register.env("AGENT_ID_SESSION_ID", "env-session");
    let registered = register
        .args(["register", "--family", "Oak"])
        .output()
        .unwrap();
    assert!(registered.status.success(), "{registered:?}");

    let mut lookup = command(&root);
    lookup.env("AGENT_ID_SESSION_ID", "env-session");
    let looked_up = lookup.args(["lookup"]).output().unwrap();
    assert!(looked_up.status.success(), "{looked_up:?}");
    assert_eq!(looked_up.stdout, registered.stdout);
}

#[test]
fn current_reads_the_session_id_from_environment() {
    let root = TempDir::new().unwrap();
    let mut register = command(&root);
    register.env("AGENT_ID_SESSION_ID", "current-session");
    let registered = register
        .args(["register", "--family", "Oak", "--json"])
        .output()
        .unwrap();
    assert!(registered.status.success(), "{registered:?}");
    let registered: Assignment = serde_json::from_slice(&registered.stdout).unwrap();

    let mut current = command(&root);
    current.env("AGENT_ID_SESSION_ID", "current-session");
    let output = current.args(["current", "--json"]).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let assignment: Assignment = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(assignment, registered);
}

#[test]
fn current_fails_without_a_session_environment() {
    let root = TempDir::new().unwrap();
    let output = command(&root).args(["current"]).output().unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("no session ID found"));
}

#[test]
fn legacy_session_environment_names_are_ignored() {
    let root = TempDir::new().unwrap();
    for (key, value) in [
        ("OMP_SESSION_ID", "omp-session"),
        ("PI_SESSION_ID", "pi-session"),
    ] {
        let mut command = command(&root);
        command.env(key, value);
        let output = command.args(["lookup"]).output().unwrap();

        assert!(!output.status.success());
        assert!(String::from_utf8(output.stderr)
            .unwrap()
            .contains("no session ID found"));
    }
}

#[test]
fn lookup_fails_before_registration() {
    let root = TempDir::new().unwrap();
    let output = command(&root).args(["lookup", "missing"]).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("no identity found"));
}

#[test]
fn prime_json_contains_the_agent_workflow_and_command_contract() {
    let root = TempDir::new().unwrap();
    let output = command(&root).args(["prime", "--json"]).output().unwrap();
    assert!(output.status.success(), "{output:?}");

    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let documentation = value["documentation"].as_str().unwrap();
    assert!(documentation.contains("agent-id register"));
    assert!(documentation.contains("agent-id lookup"));
    assert!(documentation.contains("agent-id annotate"));
    assert!(documentation.contains("agent-id current --json"));
    assert!(documentation.contains("agent-id discover"));
    assert!(documentation.contains("--all"));
    assert!(documentation.contains("--state VALUE"));
    assert!(documentation.contains("--extension OWNER=JSON"));
    assert!(documentation.contains("Inside Herdr"));
    assert!(documentation.contains("stopped"));
    assert!(documentation.contains("appropriate neighbor"));
    assert!(documentation.contains("Do not contact arbitrary agents"));
}

#[test]
fn discover_help_contains_neighbor_guardrails() {
    let root = TempDir::new().unwrap();
    let output = command(&root)
        .args(["discover", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("Do not contact arbitrary agents"), "{help}");
    assert!(help.contains("explicit recipient"), "{help}");
}

#[test]
fn discover_lists_recent_assignments() {
    let root = TempDir::new().unwrap();
    for session_id in ["discover-one", "discover-two"] {
        let output = command(&root)
            .args(["register", session_id, "--json"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }

    let output = command(&root)
        .args(["discover", "--json", "--realm", "Darkwood", "--limit", "1"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let assignments: Vec<Assignment> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(assignments.len(), 1);
    assert_eq!(assignments[0].realm, "Darkwood");

    let output = command(&root)
        .args(["discover", "--json", "--recent", "1"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let assignments: Vec<Assignment> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(assignments.len(), 2);
    assert!(assignments[0].updated_at >= assignments[1].updated_at);
}

#[test]
fn discover_excludes_only_stopped_assignments_by_default() {
    let root = TempDir::new().unwrap();
    for session_id in [
        "unset-session",
        "working-session",
        "idle-session",
        "waiting-session",
        "blocked-session",
        "stopped-session",
    ] {
        let output = command(&root)
            .args(["register", session_id])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }
    for (session_id, state) in [
        ("working-session", "working"),
        ("idle-session", "idle"),
        ("waiting-session", "waiting"),
        ("blocked-session", "blocked"),
        ("stopped-session", "stopped"),
    ] {
        let output = command(&root)
            .args(["annotate", session_id, "--state", state])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }

    let output = command(&root)
        .args(["discover", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let assignments: Vec<Assignment> = serde_json::from_slice(&output.stdout).unwrap();
    let mut session_ids: Vec<_> = assignments
        .iter()
        .map(|assignment| assignment.session_id.as_str())
        .collect();
    session_ids.sort_unstable();
    assert_eq!(
        session_ids,
        vec![
            "blocked-session",
            "idle-session",
            "unset-session",
            "waiting-session",
            "working-session",
        ]
    );

    let output = command(&root)
        .args(["discover", "--all", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let assignments: Vec<Assignment> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(assignments.len(), 6);
    assert!(assignments
        .iter()
        .any(|assignment| assignment.session_id == "stopped-session"));
}

#[cfg(unix)]
#[test]
fn discover_overlays_matching_herdr_runtime_without_persisting_it() {
    let root = TempDir::new().unwrap();
    let registered = command(&root)
        .args(["register", "runtime-session", "--json"])
        .output()
        .unwrap();
    assert!(registered.status.success(), "{registered:?}");
    let annotated = command(&root)
        .args([
            "annotate",
            "runtime-session",
            "--state",
            "idle",
            "--extension",
            r#"omp={"session_file":"/tmp/runtime-session.jsonl"}"#,
            "--json",
        ])
        .output()
        .unwrap();
    assert!(annotated.status.success(), "{annotated:?}");

    let unmatched = command(&root)
        .args(["register", "unmatched-session", "--json"])
        .output()
        .unwrap();
    assert!(unmatched.status.success(), "{unmatched:?}");

    let herdr = root.path().join("fake-herdr");
    executable(
        &herdr,
        r#"#!/bin/sh
cat <<'JSON'
{"id":"test","result":{"snapshot":{"agents":[{"agent":"omp","agent_session":{"agent":"omp","kind":"path","source":"herdr:omp","value":"/tmp/runtime-session.jsonl"},"agent_status":"working","cwd":"/work/agent-id","foreground_cwd":"/work/agent-id","pane_id":"w1:p1","tab_id":"w1:t1","workspace_id":"w1"}],"tabs":[{"tab_id":"w1:t1","label":"agents"}],"workspaces":[{"workspace_id":"w1","label":"agent-id","worktree":{"repo_key":"repo","repo_name":"agent-id","repo_root":"/work/agent-id","checkout_path":"/work/agent-id","is_linked_worktree":false}}]}}}
JSON
"#,
    );

    let mut discover = command(&root);
    discover
        .env("HERDR_ENV", "1")
        .env("HERDR_SOCKET_PATH", "/tmp/herdr.sock")
        .env("HERDR_BIN_PATH", &herdr);
    let output = discover.args(["discover", "--json"]).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let records: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(records.as_array().unwrap().len(), 1);
    let runtime = &records[0]["runtime"];
    assert_eq!(records[0]["state"]["value"], "working");
    assert_eq!(runtime["provider"], "herdr");
    assert_eq!(runtime["state"], "working");
    assert_eq!(runtime["locations"][0]["agent_status"], "working");
    assert_eq!(runtime["locations"][0]["pane_id"], "w1:p1");
    assert_eq!(runtime["locations"][0]["workspace_label"], "agent-id");
    assert_eq!(
        runtime["locations"][0]["worktree"]["checkout_path"],
        "/work/agent-id"
    );

    let mut human = command(&root);
    human
        .env("HERDR_ENV", "1")
        .env("HERDR_SOCKET_PATH", "/tmp/herdr.sock")
        .env("HERDR_BIN_PATH", &herdr);
    let human = human.args(["discover"]).output().unwrap();
    assert!(human.status.success(), "{human:?}");
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.contains("herdr:working pane:w1:p1 workspace:agent-id"));

    let lookup = command(&root)
        .args(["lookup", "runtime-session", "--json"])
        .output()
        .unwrap();
    assert!(lookup.status.success(), "{lookup:?}");
    let assignment: Value = serde_json::from_slice(&lookup.stdout).unwrap();
    assert_eq!(assignment["state"]["value"], "idle");
    assert!(assignment.get("runtime").is_none());

    executable(&herdr, "#!/bin/sh\nprintf 'unavailable\\n' >&2\nexit 1\n");
    let mut degraded = command(&root);
    degraded
        .env("HERDR_ENV", "1")
        .env("HERDR_SOCKET_PATH", "/tmp/herdr.sock")
        .env("HERDR_BIN_PATH", &herdr);
    let degraded = degraded.args(["discover", "--json"]).output().unwrap();
    assert!(degraded.status.success(), "{degraded:?}");
    let records: Value = serde_json::from_slice(&degraded.stdout).unwrap();
    let degraded_runtime = records
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["session_id"] == "runtime-session")
        .unwrap();
    assert_eq!(degraded_runtime["state"]["value"], "idle");
    assert!(degraded_runtime.get("runtime").is_none());
    assert!(String::from_utf8(degraded.stderr)
        .unwrap()
        .contains("unable to enrich discover from Herdr"));
}

#[test]
fn prune_defaults_to_apply_and_dry_run_previews() {
    let root = TempDir::new().unwrap();
    let registered = command(&root)
        .args(["register", "prune-session", "--json"])
        .output()
        .unwrap();
    assert!(registered.status.success(), "{registered:?}");
    let assignment: Assignment = serde_json::from_slice(&registered.stdout).unwrap();

    let cutoff = "2100-01-01T00:00:00Z";
    let dry_run = command(&root)
        .args(["prune", "--before", cutoff, "--dry-run", "--json"])
        .output()
        .unwrap();
    assert!(dry_run.status.success(), "{dry_run:?}");
    let report: Value = serde_json::from_slice(&dry_run.stdout).unwrap();
    assert_eq!(report["dry_run"], true);
    assert_eq!(report["candidates"].as_array().unwrap().len(), 1);

    let still_present = command(&root)
        .args(["lookup", assignment.session_id.as_str()])
        .output()
        .unwrap();
    assert!(still_present.status.success(), "{still_present:?}");

    let applied = command(&root)
        .args(["prune", "--before", cutoff, "--json"])
        .output()
        .unwrap();
    assert!(applied.status.success(), "{applied:?}");
    let report: Value = serde_json::from_slice(&applied.stdout).unwrap();
    assert_eq!(report["dry_run"], false);
    assert_eq!(report["removed"].as_array().unwrap().len(), 1);

    let removed = command(&root)
        .args(["lookup", assignment.session_id.as_str()])
        .output()
        .unwrap();
    assert!(!removed.status.success());
    assert!(!root
        .path()
        .join(format!("by-name/{}.json", assignment.slug))
        .exists());
    assert!(!root
        .path()
        .join(format!("by-name/{}", assignment.slug))
        .exists());
}

#[test]
fn registration_auto_creates_realm_when_missing() {
    let root = TempDir::new().unwrap();
    let config_dir = TempDir::new().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_agent-id"));
    command
        .env("AGENT_ID_HOME", root.path())
        .env("XDG_CONFIG_HOME", config_dir.path())
        .env_remove("AGENT_REALM")
        .env_remove("AGENT_ID_SESSION_ID");

    let output = command
        .args(["register", "session-auto-realm", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");

    let assignment: Assignment = serde_json::from_slice(&output.stdout).unwrap();
    let realm_file = config_dir.path().join("agent-id/realm");
    assert!(realm_file.is_file());
    let persisted_realm = std::fs::read_to_string(&realm_file).unwrap();
    assert_eq!(persisted_realm.trim(), assignment.realm);
}
