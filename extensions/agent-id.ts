import { execFileSync } from "node:child_process";
import path from "node:path";

type MessageContent = string | Array<{ type: string; text?: string }>;

type AgentMessageLike = {
  role: string;
  content?: MessageContent;
  synthetic?: boolean;
  steering?: boolean;
  timestamp?: number;
};

type SessionEntryLike = {
  type: string;
  customType?: string;
  data?: unknown;
};

type SummaryModel = { provider: string; id: string; baseUrl?: string };

type SessionContext = {
  cwd: string;
  sessionManager: {
    getSessionId(): string | undefined;
    getSessionFile?(): string | undefined;
    getBranch(): SessionEntryLike[];
  };
  models: { resolve(spec: string): SummaryModel | undefined };
  modelRegistry: { resolver(model: SummaryModel, sessionId?: string): unknown };
};

type AgentEndEvent = {
  type: "agent_end";
  messages: AgentMessageLike[];
  willContinue?: boolean;
};

export type ToolCallEvent = {
  toolName: string;
  input: Record<string, unknown>;
};

export const AGENT_ID_CURRENT_COMMAND =
  /(?:^|[;&|`$()]\s*)(?:(?:[A-Za-z_][A-Za-z0-9_]*=(?:"[^"]*"|'[^']*'|\S+)\s+)*)(?:\S*\/)?agent-id\s+(?:--[A-Za-z0-9_-]+(?:=(?:"[^"]*"|'[^']*'|\S+))?\s+|-[A-Za-z0-9]\s+)*current(?=\s|$)/;
export const ACTIVITY_STATE_VALUES = [
  "working",
  "idle",
  "waiting",
  "blocked",
  "stopped",
] as const;
type ActivityStateValue = (typeof ACTIVITY_STATE_VALUES)[number];
type ActivityUpdate = {
  summary?: string;
  clear_summary?: boolean;
  cwd?: string;
  clear_cwd?: boolean;
  extensions?: Record<string, unknown>;
};

type ActivityState = {
  value: ActivityStateValue;
  updated_at: string;
};

type ExtensionAPI = {
  on(
    event:
      | "session_before_switch"
      | "session_before_branch"
      | "session_before_tree"
      | "session_start"
      | "session_switch"
      | "session_branch"
      | "session_tree"
      | "session_shutdown"
      | "agent_start",
    handler: (event: unknown, context: SessionContext) => void,
  ): void;
  on(
    event: "tool_call",
    handler: (event: unknown, context: SessionContext) => unknown,
  ): void;
  on(
    event: "agent_end",
    handler: (event: AgentEndEvent, context: SessionContext) => Promise<void>,
  ): void;
  appendEntry(customType: string, data: unknown): void;
};

type Assignment = {
  session_id: string;
  name: string;
  slug: string;
  first_name: string;
  family_name: string;
  realm: string;
  summary?: { text: string; updated_at: string } | null;
  state?: ActivityState | null;
  cwd?: string | null;
};

type IdentityResult = {
  output: string;
  assignment: Assignment;
  registered: boolean;
};

let currentSessionId: string | undefined;

function requiredString(value: Record<string, unknown>, key: string): string {
  const result = value[key];
  if (typeof result !== "string" || result.length === 0) {
    throw new Error(`agent-id JSON field ${key} is missing or invalid`);
  }
  return result;
}

function parseAssignment(output: string): Assignment {
  const value: unknown = JSON.parse(output);
  if (typeof value !== "object" || value === null) {
    throw new Error("agent-id JSON output is not an object");
  }
  const record = value as Record<string, unknown>;
  return {
    session_id: requiredString(record, "session_id"),
    name: requiredString(record, "name"),
    slug: requiredString(record, "slug"),
    first_name: requiredString(record, "first_name"),
    family_name: requiredString(record, "family_name"),
    realm: requiredString(record, "realm"),
  };
}

function runAgentId(args: string[]): string {
  return execFileSync("agent-id", args, {
    env: { ...process.env },
    encoding: "utf8",
    stdio: ["pipe", "pipe", "pipe"],
    timeout: 5000,
  });
}

function lookupIdentity(sessionId: string): IdentityResult {
  const output = runAgentId(["lookup", "--session-id", sessionId, "--json"]);
  return { output, assignment: parseAssignment(output), registered: false };
}

function registerIdentity(sessionId: string): IdentityResult {
  const output = runAgentId(["register", "--session-id", sessionId, "--json"]);
  return { output, assignment: parseAssignment(output), registered: true };
}

export function buildAnnotateArgs(
  sessionId: string,
  update: ActivityUpdate,
): string[] {
  const args = ["annotate", "--session-id", sessionId, "--json"];
  if (update.summary !== undefined) {
    args.push("--summary", update.summary);
  }
  if (update.clear_summary) {
    args.push("--clear-summary");
  }
  if (update.cwd !== undefined) {
    args.push("--cwd", update.cwd);
  }
  if (update.clear_cwd) {
    args.push("--clear-cwd");
  }
  for (const [owner, data] of Object.entries(update.extensions ?? {})) {
    args.push("--extension", `${owner}=${JSON.stringify(data)}`);
  }
  return args;
}

function annotateIdentity(
  sessionId: string,
  update: ActivityUpdate,
): IdentityResult {
  const output = runAgentId(buildAnnotateArgs(sessionId, update));
  return { output, assignment: parseAssignment(output), registered: false };
}

function ensureIdentity(sessionId: string): IdentityResult {
  try {
    return lookupIdentity(sessionId);
  } catch (lookupError) {
    try {
      return registerIdentity(sessionId);
    } catch (registerError) {
      // A second process may have registered the identity between lookup and
      // register. Retry lookup before reporting a real failure.
      try {
        return lookupIdentity(sessionId);
      } catch (retryError) {
        throw new Error(
          `lookup failed: ${lookupError instanceof Error ? lookupError.message : String(lookupError)}; register failed: ${registerError instanceof Error ? registerError.message : String(registerError)}; retry failed: ${retryError instanceof Error ? retryError.message : String(retryError)}`,
        );
      }
    }
  }
}

export function injectIdentityForCurrent(
  event: unknown,
  context?: SessionContext,
): { input: Record<string, unknown> } | undefined {
  const sessionId = context?.sessionManager?.getSessionId?.() ?? currentSessionId;
  if (!sessionId || typeof event !== "object" || event === null) return;
  const toolEvent = event as Partial<ToolCallEvent>;
  if (toolEvent.toolName !== "bash" || !toolEvent.input) return;

  const command = toolEvent.input.command;
  if (typeof command !== "string" || !AGENT_ID_CURRENT_COMMAND.test(command)) return;

  const existingEnv = toolEvent.input.env;
  const callerEnv =
    typeof existingEnv === "object" && existingEnv !== null && !Array.isArray(existingEnv)
      ? (existingEnv as Record<string, unknown>)
      : {};
  return {
    input: {
      ...toolEvent.input,
      env: { AGENT_ID_SESSION_ID: sessionId, ...callerEnv },
    },
  };
}

export function sessionFileExtension(
  context: SessionContext,
  state?: ActivityStateValue,
): Record<string, unknown> | undefined {
  let sessionFile: string | undefined;
  try {
    sessionFile = context.sessionManager.getSessionFile?.();
  } catch {
    sessionFile = undefined;
  }
  const data: Record<string, string> = {};
  if (
    typeof sessionFile === "string" &&
    (path.posix.isAbsolute(sessionFile) || path.win32.isAbsolute(sessionFile))
  ) {
    data.session_file = sessionFile;
  }
  if (state !== undefined) data.state = state;
  return Object.keys(data).length > 0 ? { omp: data } : undefined;
}

function updateActivityState(
  context: SessionContext,
  value: ActivityStateValue,
): void {
  const sessionId = context.sessionManager.getSessionId();
  if (!sessionId) return;
  try {
    ensureIdentity(sessionId);
    annotateIdentity(sessionId, {
      cwd: context.cwd,
      extensions: sessionFileExtension(context, value),
    });
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    console.warn(`agent-id: unable to update the activity state: ${detail}`);
  }
}

export const AUTO_SUMMARY_ENTRY_TYPE = "dev.derekstride.agent-id.auto-summary";
export const AUTO_SUMMARY_LIMIT = 3;
export const AUTO_SUMMARY_MAX_CHARS = 80;
const AUTO_SUMMARY_MAX_TOKENS = 64;
const AUTO_SUMMARY_INPUT_CHARS = 2000;
const AUTO_SUMMARY_TIMEOUT_MS = 20_000;
const AUTO_SUMMARY_MODEL_ROLES = ["@tiny", "@smol"] as const;

const AUTO_SUMMARY_PROMPT = [
  "Write a short present-tense phrase naming the concrete work of a coding session.",
  "",
  "Rules:",
  `- At most ${AUTO_SUMMARY_MAX_CHARS} characters.`,
  "- Describe the work, not the conversation.",
  "- Never mention the user, the assistant, or an agent identity.",
  "- Never describe progress or completion state.",
  '- Never begin with "Working on".',
  "- Return the phrase alone, without quotes or a trailing period.",
].join("\n");

export type AutoSummaryState = {
  version: 1;
  generations: number;
  turnKey: string;
  summary: string;
};

export type SummaryExchange = {
  turnKey: string;
  request: string;
  response: string;
};

type CompleteSimple = (
  model: SummaryModel,
  context: {
    systemPrompt: string[];
    messages: Array<{ role: "user"; content: string; timestamp: number }>;
  },
  options: {
    apiKey: unknown;
    maxTokens: number;
    disableReasoning: boolean;
    signal: AbortSignal;
  },
) => Promise<{
  content: Array<{ type: string; text?: string }>;
  stopReason: string;
}>;

type SummarySession = {
  state: AutoSummaryState | null;
  queue: Promise<void>;
  abort: AbortController;
  epoch: number;
};

const summarySessions = new Map<string, SummarySession>();
let completionModule: Promise<CompleteSimple | null> | undefined;

function messageText(content: MessageContent | undefined): string {
  if (typeof content === "string") return content.trim();
  if (!Array.isArray(content)) return "";
  return content
    .filter((block) => block.type === "text" && typeof block.text === "string")
    .map((block) => block.text)
    .join("\n")
    .trim();
}

export function latestExchange(
  messages: readonly AgentMessageLike[],
): SummaryExchange | null {
  let response = "";
  for (let index = messages.length - 1; index >= 0; index--) {
    const message = messages[index];
    if (!message) continue;
    if (message.role === "assistant") {
      if (!response) response = messageText(message.content);
      continue;
    }
    if (message.role !== "user" || message.synthetic || message.steering) {
      continue;
    }
    const request = messageText(message.content);
    if (!request) continue;
    return {
      turnKey: String(message.timestamp ?? index),
      request: request.slice(0, AUTO_SUMMARY_INPUT_CHARS),
      response: response.slice(0, AUTO_SUMMARY_INPUT_CHARS),
    };
  }
  return null;
}

export function buildSummaryInput(
  exchange: SummaryExchange,
  previous: string | null,
): string {
  const sections: string[] = [];
  if (previous) sections.push(`Previous summary:\n${previous}`);
  sections.push(`Latest request:\n${exchange.request}`);
  if (exchange.response) {
    sections.push(`Latest response:\n${exchange.response}`);
  }
  return sections.join("\n\n");
}

export function normalizeAutoSummary(raw: string): string | null {
  const firstLine = raw.split(/\r?\n/).find((line) => line.trim().length > 0);
  if (!firstLine) return null;
  const collapsed = firstLine.trim().replace(/\s+/g, " ");
  const unquoted = collapsed.replace(/^["'`]+|["'`]+$/g, "");
  const trimmed = unquoted.replace(/\.+$/, "").trim();
  if (!trimmed) return null;
  if (trimmed.length <= AUTO_SUMMARY_MAX_CHARS) return trimmed;
  const cut = trimmed.slice(0, AUTO_SUMMARY_MAX_CHARS);
  const boundary = cut.lastIndexOf(" ");
  return (boundary > 0 ? cut.slice(0, boundary) : cut).trim() || null;
}

function isAutoSummaryState(value: unknown): value is AutoSummaryState {
  if (typeof value !== "object" || value === null) return false;
  return (
    "version" in value &&
    value.version === 1 &&
    "generations" in value &&
    typeof value.generations === "number" &&
    "turnKey" in value &&
    typeof value.turnKey === "string" &&
    "summary" in value &&
    typeof value.summary === "string"
  );
}

export function restoreAutoSummaryState(
  entries: readonly SessionEntryLike[],
): AutoSummaryState | null {
  for (let index = entries.length - 1; index >= 0; index--) {
    const entry = entries[index];
    if (!entry || entry.type !== "custom") continue;
    if (entry.customType !== AUTO_SUMMARY_ENTRY_TYPE) continue;
    if (isAutoSummaryState(entry.data)) return entry.data;
  }
  return null;
}

export function shouldSummarize(
  state: AutoSummaryState | null,
  turnKey: string,
): boolean {
  if (!state) return true;
  if (state.generations >= AUTO_SUMMARY_LIMIT) return false;
  return state.turnKey !== turnKey;
}

// The host rewrites this specifier onto its own bundled pi-ai copy. Importing
// lazily keeps identity registration working when that resolution fails.
function loadCompletion(): Promise<CompleteSimple | null> {
  completionModule ??= import("@oh-my-pi/pi-ai")
    .then((module: unknown) => {
      if (typeof module !== "object" || module === null) return null;
      if (!("completeSimple" in module)) return null;
      if (typeof module.completeSimple !== "function") return null;
      // Host-bundled pi-ai export; its call signature cannot be checked here.
      const complete = module.completeSimple as CompleteSimple;
      return complete;
    })
    .catch(() => null);
  return completionModule;
}

function summarySession(sessionId: string): SummarySession {
  const existing = summarySessions.get(sessionId);
  if (existing) return existing;
  const created: SummarySession = {
    state: null,
    queue: Promise.resolve(),
    abort: new AbortController(),
    epoch: 0,
  };
  summarySessions.set(sessionId, created);
  return created;
}

function restoreSummarySession(context: SessionContext): void {
  const sessionId = context.sessionManager.getSessionId();
  if (!sessionId) return;
  const session = summarySession(sessionId);
  session.abort.abort();
  session.abort = new AbortController();
  session.epoch += 1;
  try {
    session.state = restoreAutoSummaryState(context.sessionManager.getBranch());
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    console.warn(`agent-id: unable to restore the summary state: ${detail}`);
    session.state = null;
  }
}

function abortSummarySession(context: SessionContext): void {
  const sessionId = context.sessionManager.getSessionId();
  if (!sessionId) return;
  const session = summarySessions.get(sessionId);
  if (!session) return;
  session.abort.abort();
  session.epoch += 1;
}

async function summarizeExchange(
  context: SessionContext,
  sessionId: string,
  input: string,
  signal: AbortSignal,
): Promise<string | null> {
  const complete = await loadCompletion();
  if (!complete) return null;

  let model: SummaryModel | undefined;
  for (const role of AUTO_SUMMARY_MODEL_ROLES) {
    model = context.models.resolve(role);
    if (model) break;
  }
  if (!model) return null;

  const response = await complete(
    model,
    {
      systemPrompt: [AUTO_SUMMARY_PROMPT],
      messages: [{ role: "user", content: input, timestamp: Date.now() }],
    },
    {
      apiKey: context.modelRegistry.resolver(model, sessionId),
      maxTokens: AUTO_SUMMARY_MAX_TOKENS,
      disableReasoning: true,
      signal,
    },
  );
  if (response.stopReason === "error" || response.stopReason === "aborted") {
    return null;
  }
  return normalizeAutoSummary(messageText(response.content));
}

async function maintainAutoSummary(
  pi: ExtensionAPI,
  context: SessionContext,
  sessionId: string,
  session: SummarySession,
  event: AgentEndEvent,
  controller: AbortController,
  epoch: number,
): Promise<void> {
  try {
    if (session.epoch !== epoch || controller.signal.aborted) return;
    const exchange = latestExchange(event.messages);
    if (!exchange) return;
    if (!shouldSummarize(session.state, exchange.turnKey)) return;

    const signal = AbortSignal.any([
      controller.signal,
      AbortSignal.timeout(AUTO_SUMMARY_TIMEOUT_MS),
    ]);
    const summary = await summarizeExchange(
      context,
      sessionId,
      buildSummaryInput(exchange, session.state?.summary ?? null),
      signal,
    );
    if (
      !summary ||
      signal.aborted ||
      session.epoch !== epoch ||
      controller.signal.aborted
    ) {
      return;
    }
    if (context.sessionManager.getSessionId() !== sessionId) return;

    ensureIdentity(sessionId);
    if (session.epoch !== epoch || controller.signal.aborted) return;
    annotateIdentity(sessionId, { summary });

    const state: AutoSummaryState = {
      version: 1,
      generations: (session.state?.generations ?? 0) + 1,
      turnKey: exchange.turnKey,
      summary,
    };
    session.state = state;
    pi.appendEntry(AUTO_SUMMARY_ENTRY_TYPE, state);
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    console.warn(`agent-id: unable to update the session summary: ${detail}`);
  }
}

export default function agentIdExtension(pi: ExtensionAPI): void {
  pi.on("session_before_switch", (_event, context) => abortSummarySession(context));
  pi.on("session_before_branch", (_event, context) => abortSummarySession(context));
  pi.on("session_before_tree", (_event, context) => abortSummarySession(context));
  pi.on("tool_call", (event, context) => injectIdentityForCurrent(event, context));
  pi.on("session_start", (_event, context) => {
    currentSessionId = context.sessionManager.getSessionId();
    updateActivityState(context, "idle");
    restoreSummarySession(context);
  });
  pi.on("session_switch", (_event, context) => {
    currentSessionId = context.sessionManager.getSessionId();
    updateActivityState(context, "idle");
    restoreSummarySession(context);
  });
  pi.on("session_branch", (_event, context) => {
    currentSessionId = context.sessionManager.getSessionId();
    updateActivityState(context, "idle");
    restoreSummarySession(context);
  });
  pi.on("session_tree", (_event, context) => {
    currentSessionId = context.sessionManager.getSessionId();
    updateActivityState(context, "idle");
    restoreSummarySession(context);
  });
  pi.on("agent_start", (_event, context) => {
    currentSessionId = context.sessionManager.getSessionId();
    updateActivityState(context, "working");
  });
  pi.on("session_shutdown", (_event, context) => {
    currentSessionId = undefined;
    updateActivityState(context, "stopped");
    for (const session of summarySessions.values()) session.abort.abort();
    summarySessions.clear();
  });

  pi.on("agent_end", async (event, context) => {
    if (event.willContinue) return;
    const sessionId = context.sessionManager.getSessionId();
    if (!sessionId || sessionId !== currentSessionId) return;
    const session = summarySession(sessionId);
    const controller = session.abort;
    const epoch = session.epoch;
    session.queue = session.queue.then(async () => {
      if (
        session.epoch !== epoch ||
        controller.signal.aborted ||
        currentSessionId !== sessionId
      ) {
        return;
      }
      updateActivityState(context, "idle");
      await maintainAutoSummary(
        pi,
        context,
        sessionId,
        session,
        event,
        controller,
        epoch,
      );
    });
    await session.queue;
  });
}
