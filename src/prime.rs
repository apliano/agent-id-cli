use anyhow::Result;
use serde_json::json;

use crate::cli::PrimeArgs;

pub fn execute(args: &PrimeArgs) -> Result<()> {
    let documentation = generate(args.prelude);
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"documentation": documentation}))?
        );
    } else {
        println!("{documentation}");
    }
    Ok(())
}

pub fn generate(prelude_only: bool) -> String {
    let prelude = r#"# agent-id — Portable agent identity registry

Agent ID provides the permanent human-readable identity for a coding-agent session. The identity is keyed by the harness session ID; never invent a name or register a second name for the same session.

## OMP workflow

The OMP extension automatically looks up or registers the current session, records its working directory and session file, and publishes its lifecycle signal under the `extensions.omp` namespace.

Call `agent-id current --json` to inspect the complete current assignment. The command uses `AGENT_ID_SESSION_ID` and never registers a missing identity. Call `agent-id discover` directly when you need to find other identities. Discover lists non-stopped registry assignments by default; outside Herdr this includes all matching assignments, while inside Herdr it is limited to identities matched to live Herdr agents. Use `agent-id discover --all` to include stopped and historical registry assignments; inside Herdr, runtime projections are added where available.

The top-level state is materialized from Herdr runtime state first, then the OMP lifecycle signal, and otherwise `unknown`. Lifecycle hooks publish `working`, `idle`, and `stopped` automatically. Use `agent-id annotate` to publish `waiting` or `blocked`, or to set or clear a summary or namespaced extension value when an explicit update is needed. Automatic summaries use completed agent turns; explicit updates remain authoritative.

## Neighbor selection

When a request needs another agent, use `agent-id discover` to choose an appropriate neighbor before initiating agent-to-agent communication. Prefer an agent whose current work is related to the request; use summaries and Herdr context as evidence, not proof. Do not contact arbitrary agents for independent reviews or unrelated requests merely because they are idle or discoverable. If no suitable neighbor is apparent, do not broadcast; ask the user or report that no suitable neighbor was found. An explicit recipient chosen by the user takes precedence. This project helps identify recipients; the communication mechanism handles delivery.

## CLI fallback

Direct CLI use is normally unnecessary under OMP. If the extension is unavailable, the CLI resolves a session from:

1. An explicit `SESSION_ID` argument or `--session-id ID`.
2. `AGENT_ID_SESSION_ID`.

A missing session ID is an error. A missing lookup is an error; register the session first.

## Examples

```bash
agent-id discover
agent-id discover --recent 24 --json
agent-id discover --all
agent-id register "$AGENT_ID_SESSION_ID"
agent-id register --family Oak "$AGENT_ID_SESSION_ID"
agent-id lookup "$AGENT_ID_SESSION_ID"
agent-id annotate --summary "Implementing checkout retries" "$AGENT_ID_SESSION_ID"
agent-id annotate --clear-summary "$AGENT_ID_SESSION_ID"
agent-id annotate --state waiting "$AGENT_ID_SESSION_ID"
agent-id annotate --state blocked "$AGENT_ID_SESSION_ID"
agent-id annotate --clear-state "$AGENT_ID_SESSION_ID"
agent-id annotate --cwd "$PWD" "$AGENT_ID_SESSION_ID"
agent-id annotate --clear-cwd "$AGENT_ID_SESSION_ID"
```

The default CLI output is the full name. Use `--json` when a tool needs the session ID, name parts, realm, slug, optional timestamped summary, materialized state, working directory, and created/updated timestamps."#;

    if prelude_only {
        return prelude.to_string();
    }
    format!(
        "{prelude}\n\n## Commands\n\n### `agent-id register [SESSION_ID]` — Allocate a permanent identity\n\n```\n--family NAME       Prefer a family name, useful for child agents\n--realm NAME        Select a realm\n--session-id ID     Provide an explicit session ID\n--json              Print the complete assignment as JSON\n```\n\nRegistration is idempotent: an existing session keeps its identity and receives a new `updated_at`.\n\n### `agent-id lookup [IDENTIFIER]` — Read an existing identity\n\nIDENTIFIER may be a session ID, canonical name, or slug. Without an explicit identifier, lookup uses the session environment.\n\n```\n--session-id ID     Provide an explicit session ID\n--json              Print the complete assignment as JSON\n```\n\nFails if the identifier has not been registered.\n\n### `agent-id current` — Read the current session identity\n\n```\n--json              Print the complete assignment as JSON\n```\n\nUses `AGENT_ID_SESSION_ID` to identify the current session and never registers a missing identity.\n\n### `agent-id annotate [SESSION_ID]` — Update activity metadata\n\n```\n--summary TEXT         Set a concise summary (maximum 240 characters)\n--clear-summary        Remove the summary\n--state VALUE          Set working, idle, waiting, blocked, or stopped\n--clear-state          Remove the activity state\n--cwd PATH             Set the current working directory\n--clear-cwd            Remove the current working directory\n--extension OWNER=JSON Set namespaced unstructured JSON metadata; repeatable\n--clear-extension NAME Remove one extension namespace; repeatable\n--session-id ID        Provide the session ID explicitly\n--json                 Print the complete assignment as JSON\n```\n\nSummary, state, working-directory, and extension metadata updates are independent; omitted fields remain unchanged. Agent ID timestamps mutable fields. Extension updates atomically replace one owner's JSON value.\n\n### `agent-id discover` — List identities\n\n```\n--limit N           Maximum records (default 20; zero means all)\n--recent HOURS      Only include records updated within this many hours\n--realm NAME        Only include records in this realm\n--all               Include all registry assignments\n--json              Print assignments and available runtime projections as JSON\n```\n\nStopped assignments are excluded by default. Outside Herdr, discovery includes all matching non-stopped registry assignments. Inside Herdr, default discovery is limited to identities matched to live Herdr agents; use `--all` to include stopped and historical registry assignments. Results are sorted by `updated_at`, newest first. Inside Herdr, matching OMP session-file metadata adds a non-persistent live runtime projection.\n\n### `agent-id prune` — Remove old identity assignments\n\n```\n--before TIMESTAMP  Delete records updated before this timestamp\n--dry-run           Preview matching records without deleting\n--json              Print the prune report as JSON\n```\n\nPruning applies by default and removes both session records and matching name claims.\n\n### `agent-id prime` — Print this workflow manual\n\n```\n--prelude           Omit the command reference\n--json              Wrap the documentation in a JSON object\n```"
    )
}
