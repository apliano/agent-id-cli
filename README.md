# agent-id

Portable identity registry and OMP companion extension for coding-agent sessions.

The `agent-id` binary assigns a permanent human-readable name to a stable session ID. The registry is durable, realm-aware, and independent of any particular agent harness.

In OMP, the companion extension makes identity automatic: it registers sessions, records the OMP lifecycle signal, and derives a concise current-work summary from completed agent turns.

The Rust crate is named `agent-id-cli`; the installed binary remains `agent-id`.

## Installation

### Homebrew

```bash
brew install derekstride/tap/agent-id-cli
```

### Cargo

```bash
cargo install agent-id-cli
```

### OMP plugin

Install the binary first, then install the OMP extension from this repository:

```bash
omp plugin install https://github.com/DerekStride/agent-id-cli
```

Once installed, the extension handles the normal identity workflow automatically. The plugin also bundles an on-demand skill for identity inspection and selecting relevant neighbors when coordination is needed.

## Usage

With the OMP plugin, each session receives a stable identity and current lifecycle information without manual setup. Inspect recent identities from a terminal:

```bash
agent-id discover
agent-id discover --recent 24
agent-id discover --all
agent-id lookup "Spring Oak of Darkwood"
```

Discovery shows non-stopped sessions by default, including available summaries, materialized states, and working directories. When run inside Herdr, default discovery is limited to identities matched to live Herdr agents; use `--all` to include historical registry records. Herdr state takes precedence when runtime information is available, followed by the OMP extension state, with `unknown` when neither signal exists.

For standalone use without OMP, register a harness session ID once and look it up later:

```bash
agent-id register SESSION_ID
agent-id lookup SESSION_ID
```

Run `agent-id --help` or `agent-id <command> --help` for all commands and options.

## Origin and companion project

`agent-id` is based on Josh Beckman's [design for coordinating dozens of coding agents](https://gist.github.com/joshbeckman/d21dbd6c566470e4d012392fd3cb8ed8). It carries forward the central identity decisions from that work: externally assigned human-readable names keyed to stable harness session IDs, machine realms that partition allocation, and durable lookup independent of model self-report. This project extracts the identity registry and OMP lifecycle integration; agent-to-agent communication, workspaces, and other coordination concerns remain separate.

As a companion project, [AgentMail](https://github.com/DerekStride/agent-mail) provides agent-to-agent messaging that uses these human-readable identities while keeping delivery and read state separate from the identity registry. `agent-id` remains transport-agnostic, so other communication and coordination tools can integrate with the same identities.
