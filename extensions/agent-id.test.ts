import { describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import agentIdExtension, {
  ACTIVITY_STATE_VALUES,
  AGENT_ID_CURRENT_COMMAND,
  AUTO_SUMMARY_ENTRY_TYPE,
  AUTO_SUMMARY_LIMIT,
  AUTO_SUMMARY_MAX_CHARS,
  buildAnnotateArgs,
  buildSummaryInput,
  injectIdentityForCurrent,
  latestExchange,
  normalizeAutoSummary,
  restoreAutoSummaryState,
  sessionFileExtension,
  shouldSummarize,
} from "./agent-id";

describe("activity state contract", () => {
  test("exposes stable lifecycle values", () => {
    expect(ACTIVITY_STATE_VALUES).toEqual([
      "working",
      "idle",
      "waiting",
      "blocked",
      "stopped",
    ]);
  });
});

describe("agent-id subprocess output and context", () => {
  test("does not leak lookup errors or publish context guidance", () => {
    const root = mkdtempSync(path.join(tmpdir(), "agent-id-extension-"));
    try {
      const extensionPath = path.join(import.meta.dir, "agent-id.ts");
      const script = `
        import agentIdExtension from ${JSON.stringify(extensionPath)};
        const handlers = {};
        let sentMessages = 0;
        const pi = {
          on(event, handler) { handlers[event] = handler; },
          appendEntry() {},
          sendMessage() { sentMessages += 1; },
        };
        agentIdExtension(pi);
        handlers.session_start?.({}, {
          cwd: "/tmp/context",
          sessionManager: {
            getSessionId: () => "fresh-session",
            getBranch: () => [],
          },
          models: { resolve: () => undefined },
          modelRegistry: { resolver: () => undefined },
        });
        if (sentMessages !== 0) throw new Error("unexpected context message");
      `;
      const result = Bun.spawnSync(["bun", "-e", script], {
        env: { ...process.env, AGENT_ID_HOME: root },
        stdout: "pipe",
        stderr: "pipe",
      });

      expect(result.exitCode).toBe(0);
      expect(new TextDecoder().decode(result.stderr)).not.toContain(
        "no identity found",
      );
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  test("does not let a late agent end overwrite stopped state", () => {
    const root = mkdtempSync(path.join(tmpdir(), "agent-id-shutdown-"));
    try {
      const extensionPath = path.join(import.meta.dir, "agent-id.ts");
      const script = `
        import agentIdExtension from ${JSON.stringify(extensionPath)};
        const handlers = {};
        const pi = {
          on(event, handler) { handlers[event] = handler; },
          appendEntry() {},
        };
        agentIdExtension(pi);
        const context = {
          cwd: "/tmp/shutdown",
          sessionManager: {
            getSessionId: () => "late-end-session",
            getSessionFile: () => "/tmp/late-end-session.jsonl",
            getBranch: () => [],
          },
          models: { resolve: () => undefined },
          modelRegistry: { resolver: () => undefined },
        };
        handlers.session_start?.({}, context);
        handlers.agent_start?.({}, context);
        handlers.session_shutdown?.({}, context);
        await handlers.agent_end?.(
          { type: "agent_end", messages: [], willContinue: false },
          context,
        );
      `;
      const result = Bun.spawnSync(["bun", "-e", script], {
        env: { ...process.env, AGENT_ID_HOME: root },
        stdout: "pipe",
        stderr: "pipe",
      });
      expect(result.exitCode).toBe(0);

      const lookup = Bun.spawnSync(
        ["agent-id", "lookup", "--session-id", "late-end-session", "--json"],
        {
          env: { ...process.env, AGENT_ID_HOME: root },
          stdout: "pipe",
          stderr: "pipe",
        },
      );
      expect(lookup.exitCode).toBe(0);
      const assignment = JSON.parse(new TextDecoder().decode(lookup.stdout));
      expect(assignment.state.value).toBe("stopped");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  test("publishes the session slug as a status and clears it on shutdown", () => {
    const root = mkdtempSync(path.join(tmpdir(), "agent-id-status-"));
    try {
      const extensionPath = path.join(import.meta.dir, "agent-id.ts");
      const script = `
        import agentIdExtension from ${JSON.stringify(extensionPath)};
        const handlers = {};
        const statuses = [];
        const pi = {
          on(event, handler) { handlers[event] = handler; },
          appendEntry() {},
        };
        agentIdExtension(pi);
        const context = {
          cwd: "/tmp/status",
          ui: { setStatus(key, text) { statuses.push([key, text ?? null]); } },
          sessionManager: {
            getSessionId: () => "status-session",
            getBranch: () => [],
          },
          models: { resolve: () => undefined },
          modelRegistry: { resolver: () => undefined },
        };
        handlers.session_start?.({}, context);
        handlers.agent_start?.({}, context);
        handlers.session_shutdown?.({}, context);
        console.log(JSON.stringify(statuses));
      `;
      const result = Bun.spawnSync(["bun", "-e", script], {
        env: { ...process.env, AGENT_ID_HOME: root },
        stdout: "pipe",
        stderr: "pipe",
      });
      expect(result.exitCode).toBe(0);

      const lookup = Bun.spawnSync(
        ["agent-id", "lookup", "--session-id", "status-session", "--json"],
        {
          env: { ...process.env, AGENT_ID_HOME: root },
          stdout: "pipe",
          stderr: "pipe",
        },
      );
      expect(lookup.exitCode).toBe(0);
      const { slug } = JSON.parse(new TextDecoder().decode(lookup.stdout));
      expect(JSON.parse(new TextDecoder().decode(result.stdout))).toEqual([
        ["agent-id", slug],
        ["agent-id", slug],
        ["agent-id", null],
      ]);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
});

describe("OMP session metadata", () => {
  test("reports an absolute session file through the omp namespace", () => {
    const context = {
      cwd: "/tmp/context",
      sessionManager: {
        getSessionId: () => "context-session",
        getSessionFile: () => "/tmp/sessions/context-session.jsonl",
        getBranch: () => [],
      },
      models: { resolve: () => undefined },
      modelRegistry: { resolver: () => undefined },
    };

    const extensions = sessionFileExtension(context);
    expect(extensions).toEqual({
      omp: { session_file: "/tmp/sessions/context-session.jsonl" },
    });
    const statefulExtensions = sessionFileExtension(context, "working");
    expect(statefulExtensions).toEqual({
      omp: {
        session_file: "/tmp/sessions/context-session.jsonl",
        state: "working",
      },
    });
    expect(
      buildAnnotateArgs("context-session", {
        cwd: "/tmp/context",
        extensions: statefulExtensions,
      }),
    ).toEqual([
      "annotate",
      "--session-id",
      "context-session",
      "--json",
      "--cwd",
      "/tmp/context",
      "--extension",
      'omp={\"session_file\":\"/tmp/sessions/context-session.jsonl\",\"state\":\"working\"}',
    ]);
  });

  test("ignores missing, relative, and failing session files", () => {
    const context = {
      cwd: "/tmp/context",
      sessionManager: {
        getSessionId: () => "context-session",
        getSessionFile: () => undefined as string | undefined,
        getBranch: () => [],
      },
      models: { resolve: () => undefined },
      modelRegistry: { resolver: () => undefined },
    };

    expect(sessionFileExtension(context)).toBeUndefined();
    expect(sessionFileExtension(context, "idle")).toEqual({
      omp: { state: "idle" },
    });
    context.sessionManager.getSessionFile = () => "relative/session.jsonl";
    expect(sessionFileExtension(context)).toBeUndefined();
    context.sessionManager.getSessionFile = () => {
      throw new Error("unavailable");
    };
    expect(sessionFileExtension(context)).toBeUndefined();
  });
});

describe("agent-id current command pattern", () => {
  test("matches various current invocations", () => {
    expect(AGENT_ID_CURRENT_COMMAND.test("agent-id current")).toBe(true);
    expect(AGENT_ID_CURRENT_COMMAND.test("agent-id current --json")).toBe(true);
    expect(
      AGENT_ID_CURRENT_COMMAND.test("/usr/local/bin/agent-id current --json"),
    ).toBe(true);
    expect(
      AGENT_ID_CURRENT_COMMAND.test("FOO=bar agent-id current --json"),
    ).toBe(true);
    expect(
      AGENT_ID_CURRENT_COMMAND.test("echo ok && agent-id current --json"),
    ).toBe(true);
    expect(
      AGENT_ID_CURRENT_COMMAND.test("agent-id current | jq .session_id"),
    ).toBe(true);
  });

  test("does not match unrelated commands", () => {
    expect(AGENT_ID_CURRENT_COMMAND.test("agent-id lookup session-1")).toBe(
      false,
    );
    expect(AGENT_ID_CURRENT_COMMAND.test("agent-id register --json")).toBe(
      false,
    );
    expect(AGENT_ID_CURRENT_COMMAND.test("agent-id discover")).toBe(false);
    expect(AGENT_ID_CURRENT_COMMAND.test("agent-id prime")).toBe(false);
    expect(
      AGENT_ID_CURRENT_COMMAND.test("agent-id annotate --state working"),
    ).toBe(false);
    expect(AGENT_ID_CURRENT_COMMAND.test("git status")).toBe(false);
    expect(AGENT_ID_CURRENT_COMMAND.test("agent-mail scan")).toBe(false);
  });
});

describe("tool-call session injection", () => {
  test("injects AGENT_ID_SESSION_ID only for matching agent-id current calls", () => {
    const handlers = new Map<
      string,
      (event: unknown, context: unknown) => unknown
    >();
    const pi = {
      on(event: string, handler: (event: unknown, context: unknown) => unknown) {
        handlers.set(event, handler);
      },
      appendEntry() {},
      sendMessage() {},
    };
    agentIdExtension(pi as Parameters<typeof agentIdExtension>[0]);

    const toolCall = handlers.get("tool_call");
    expect(toolCall).toBeDefined();

    const context = {
      cwd: "/tmp/context",
      sessionManager: {
        getSessionId: () => "session-xyz",
        getBranch: () => [],
      },
      models: { resolve: () => undefined },
      modelRegistry: { resolver: () => undefined },
    };

    const injected = toolCall?.(
      {
        toolName: "bash",
        input: { command: "agent-id current --json", env: { FOO: "bar" } },
      },
      context,
    );
    expect(injected).toEqual({
      input: {
        command: "agent-id current --json",
        env: {
          AGENT_ID_SESSION_ID: "session-xyz",
          FOO: "bar",
        },
      },
    });

    const callerOverride = toolCall?.(
      {
        toolName: "bash",
        input: {
          command: "agent-id current --json",
          env: { AGENT_ID_SESSION_ID: "explicit-session" },
        },
      },
      context,
    );
    expect(callerOverride).toEqual({
      input: {
        command: "agent-id current --json",
        env: {
          AGENT_ID_SESSION_ID: "explicit-session",
        },
      },
    });

    expect(
      toolCall?.(
        {
          toolName: "bash",
          input: { command: "agent-id lookup other-session --json" },
        },
        context,
      ),
    ).toBeUndefined();

    expect(
      toolCall?.(
        {
          toolName: "read",
          input: { path: "src/lib.rs" },
        },
        context,
      ),
    ).toBeUndefined();
  });
});

describe("latestExchange", () => {
  test("pairs the newest real request with the newest assistant reply", () => {
    const exchange = latestExchange([
      { role: "user", content: "first request", timestamp: 1 },
      { role: "assistant", content: [{ type: "text", text: "first reply" }] },
      { role: "user", content: "  second request  ", timestamp: 2 },
      {
        role: "assistant",
        content: [
          { type: "thinking", text: "hidden" },
          { type: "text", text: "second reply" },
        ],
      },
    ]);

    expect(exchange).toEqual({
      turnKey: "2",
      request: "second request",
      response: "second reply",
    });
  });

  test("ignores synthetic and steering user messages", () => {
    const exchange = latestExchange([
      { role: "user", content: "real request", timestamp: 7 },
      { role: "assistant", content: [{ type: "text", text: "reply" }] },
      { role: "user", content: "auto continue", timestamp: 8, synthetic: true },
      { role: "user", content: "steer", timestamp: 9, steering: true },
    ]);

    expect(exchange?.turnKey).toBe("7");
    expect(exchange?.request).toBe("real request");
  });

  test("returns null without a usable request", () => {
    expect(latestExchange([])).toBeNull();
    expect(
      latestExchange([
        { role: "user", content: "   ", timestamp: 1 },
        { role: "toolResult", content: "output" },
      ]),
    ).toBeNull();
  });
});

describe("normalizeAutoSummary", () => {
  test("keeps a single clean line", () => {
    expect(normalizeAutoSummary('  "Fixing checkout retries."\n\nextra ')).toBe(
      "Fixing checkout retries",
    );
    expect(normalizeAutoSummary("Reviewing\tindex   design")).toBe(
      "Reviewing index design",
    );
  });

  test("clips overlong output at a word boundary", () => {
    const summary = normalizeAutoSummary(`${"alpha ".repeat(30)}omega`);

    expect(summary).not.toBeNull();
    expect(summary?.length).toBeLessThanOrEqual(AUTO_SUMMARY_MAX_CHARS);
    expect(summary?.endsWith("alpha")).toBe(true);
  });

  test("rejects empty output", () => {
    expect(normalizeAutoSummary("")).toBeNull();
    expect(normalizeAutoSummary("\n  \n")).toBeNull();
    expect(normalizeAutoSummary('"..."')).toBeNull();
  });
});

describe("restoreAutoSummaryState", () => {
  test("returns the newest valid record", () => {
    const state = restoreAutoSummaryState([
      { type: "message" },
      {
        type: "custom",
        customType: AUTO_SUMMARY_ENTRY_TYPE,
        data: { version: 1, generations: 1, turnKey: "1", summary: "old" },
      },
      { type: "custom", customType: "other", data: { version: 1 } },
      {
        type: "custom",
        customType: AUTO_SUMMARY_ENTRY_TYPE,
        data: { version: 1, generations: 2, turnKey: "2", summary: "new" },
      },
    ]);

    expect(state).toEqual({
      version: 1,
      generations: 2,
      turnKey: "2",
      summary: "new",
    });
  });

  test("ignores malformed records", () => {
    expect(
      restoreAutoSummaryState([
        {
          type: "custom",
          customType: AUTO_SUMMARY_ENTRY_TYPE,
          data: { version: 2, generations: 1, turnKey: "1", summary: "x" },
        },
        {
          type: "custom",
          customType: AUTO_SUMMARY_ENTRY_TYPE,
          data: { version: 1, generations: "1", turnKey: "1" },
        },
      ]),
    ).toBeNull();
  });
});

describe("shouldSummarize", () => {
  test("generates until the limit, once per turn", () => {
    expect(shouldSummarize(null, "1")).toBe(true);

    const first = {
      version: 1 as const,
      generations: 1,
      turnKey: "1",
      summary: "first",
    };
    expect(shouldSummarize(first, "1")).toBe(false);
    expect(shouldSummarize(first, "2")).toBe(true);

    expect(
      shouldSummarize(
        { ...first, generations: AUTO_SUMMARY_LIMIT, turnKey: "3" },
        "4",
      ),
    ).toBe(false);
  });
});

describe("buildSummaryInput", () => {
  test("includes the previous summary and omits an empty reply", () => {
    const exchange = { turnKey: "1", request: "do the thing", response: "" };

    expect(buildSummaryInput(exchange, "earlier summary")).toBe(
      "Previous summary:\nearlier summary\n\nLatest request:\ndo the thing",
    );
    expect(buildSummaryInput({ ...exchange, response: "did it" }, null)).toBe(
      "Latest request:\ndo the thing\n\nLatest response:\ndid it",
    );
  });
});
