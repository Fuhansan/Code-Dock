/**
 * Typed wrappers around Tauri IPC commands.
 *
 * Field names mirror the Rust side (snake_case) so this layer is one-to-one
 * with `src-tauri/src/commands.rs`. If a command's Rust signature changes,
 * change the wrapper here too — there is no other type bridge.
 */
import { invoke } from '@tauri-apps/api/core';

// ---------- Wire types (mirror Rust llm/mod.rs) ----------

export interface ToolCall {
  id: string;
  type: 'function';
  function: { name: string; arguments: string };
}

export type ChatMessage =
  | { role: 'system'; content: string }
  | { role: 'user'; content: string }
  | { role: 'assistant'; content: string | null; tool_calls?: ToolCall[] }
  | { role: 'tool'; content: string; tool_call_id: string };

export interface Tool {
  type: 'function';
  function: { name: string; description: string; parameters: unknown };
}

export interface Usage {
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
}

export interface ChatResponse {
  content: string | null;
  tool_calls: ToolCall[];
  finish_reason: string;
  usage?: Usage;
}

// ---------- Commands ----------

/** Sprint 0 IPC health check — keep until something better replaces it. */
export function greet(name: string): Promise<string> {
  return invoke('greet', { name });
}

/** True iff a key has been saved for this provider. Does not read the key. */
export function providerStatus(provider: string): Promise<boolean> {
  return invoke('provider_status', { provider });
}

/** Persist an API key into the OS keychain. */
export function saveApiKey(provider: string, key: string): Promise<void> {
  return invoke('save_api_key', { provider, key });
}

export interface SendChatParams {
  provider: string;
  model: string;
  messages: ChatMessage[];
  temperature?: number;
  max_tokens?: number;
  tools?: Tool[];
  tool_choice?: unknown;
}

/** One chat completion round-trip. Throws (rejected promise) on any error. */
export function sendChatMessage(params: SendChatParams): Promise<ChatResponse> {
  return invoke('send_chat_message', {
    provider: params.provider,
    model: params.model,
    messages: params.messages,
    temperature: params.temperature ?? null,
    max_tokens: params.max_tokens ?? null,
    tools: params.tools ?? null,
    tool_choice: params.tool_choice ?? null,
  });
}

// ---------- Multi-agent session (Sprint 2) ----------

/** Tauri event name the dispatcher emits for every routed message. */
export const MESSAGE_EVENT = 'aidock:message';

/** Tauri event name the agent runtime emits per executed tool call (step 5:
 *  generalized from MCP-only to ALL Call tools — file Read/Edit/Write/Glob/Grep,
 *  Bash, recall, MCP). One event per call, success or error. */
export const TOOL_CALL_EVENT = 'aidock:tool_call';

/** Sprint 4.6: emitted when the agent runtime needs the user's vote on
 *  a destructive tool call. Frontend should respond via `respondToApproval`. */
export const APPROVAL_REQUEST_EVENT = 'aidock:approval_request';

export type DangerLevel = 'read_only' | 'mutating' | 'destructive';
export type ApprovalDecision = 'allow_once' | 'allow_session' | 'reject';

export interface ApprovalRequest {
  id: string;
  agent: string;
  tool: string;
  args_preview: string;
  /** Unix milliseconds. */
  timestamp: number;
  danger: DangerLevel;
}

/** Resolve a pending approval request. */
export function respondToApproval(id: string, decision: ApprovalDecision): Promise<void> {
  return invoke('respond_to_approval', { id, decision });
}

/** Live MCP tool-call notification. Mirrors Rust `McpCallEvent`.
 *
 *  Sprint 4.7: `args` / `result` now carry the FULL payload. Per-tool
 *  formatters in the chat panel pick the one-line summary; the user
 *  expands the row to see the raw body. A 1 MiB cap on the Rust side
 *  is the only ceiling, present just as a runaway-tool safety net. */
export interface McpCallEvent {
  id: string;
  /** Unix milliseconds, same clock as AgentMessage so timelines sort cleanly. */
  timestamp: number;
  /** Role id that issued the call. */
  agent: string;
  /** Full tool name including `fs__` prefix. */
  tool: string;
  /** Full JSON args (or near-full — Rust caps at 1 MiB as a safety net). */
  args: string;
  /** Full result body from the MCP server. */
  result: string;
  /** False iff the result began with "ERROR:". */
  success: boolean;
}

/**
 * Typed kinds, mirroring `AgentMessageKind` on the Rust side. The wire tag
 * lives in `type` and uses SCREAMING_SNAKE_CASE.
 */
export type AgentMessageKind =
  | { type: 'BROADCAST'; content: string }
  | { type: 'ASK_AGENT'; to: string; content: string; expected_format?: string | null }
  | { type: 'ANSWER'; reply_to: string; content: string }
  | { type: 'WORK_START'; task: string }
  | { type: 'PROGRESS'; task: string; percent: number; note: string }
  | { type: 'DONE'; summary: string; artifact_id?: string | null }
  | { type: 'SUMMARY'; topic_id: string; summary: string; key_decisions: string[] }
  | { type: 'USER_INPUT'; content: string };

export interface AgentMessage {
  id: string;
  /** Role id of the sender, or `"user"` for human input. */
  sender: string;
  topic_id: string;
  /** Unix milliseconds. */
  timestamp: number;
  kind: AgentMessageKind;
  /**
   * Set on the message that opens a new topic. Sprint 2.6 — UI uses this
   * to show a real heading for the topic instead of just its id.
   */
  opens_topic_title?: string | null;
}

/** Start the default workshop session (idempotent). */
export function startSession(): Promise<void> {
  return invoke('start_session');
}

/** True iff a session is currently running. */
export function sessionStatus(): Promise<boolean> {
  return invoke('session_status');
}

/** Push one human-originated message into the session. */
export function sendUserMessage(content: string, topic_id?: string): Promise<void> {
  return invoke('send_user_message', {
    content,
    topic_id: topic_id ?? null,
  });
}

/** Load the persisted message history for the default session.
 *  Front-end calls this on mount so reloaded sessions show prior turns. */
export function loadMessageHistory(): Promise<AgentMessage[]> {
  return invoke('load_message_history');
}

/** Sprint 4 polish: like loadMessageHistory but for the MCP tool-call
 *  log, so reloaded sessions show prior tool-call cards too. */
export function loadMcpCallHistory(): Promise<McpCallEvent[]> {
  return invoke('load_mcp_call_history');
}

// ---------- 会话管理 (session lifecycle) ----------

/** One session in the history list. Mirrors Rust `session_store::SessionMeta`.
 *  Hierarchy: user → workshop → session; stored at
 *  ~/.aidock/users/{user}/workshops/{ws}/sessions/{id}/. */
export interface SessionMeta {
  /** `{unix_millis}_{rand}` — sortable, filesystem-safe. */
  id: string;
  /** Derived from the first user message; "新会话" when empty. */
  title: string;
  created_ms: number;
  last_active_ms: number;
  message_count: number;
}

/** Start a brand-new, empty session (switches away from the current one). */
export function newSession(): Promise<SessionMeta> {
  return invoke('new_session');
}

/** Resume an existing session by id — continues from its persisted history. */
export function resumeSession(id: string): Promise<void> {
  return invoke('resume_session', { id });
}

/** History: all sessions for the current user/workshop, newest first. */
export function listSessions(): Promise<SessionMeta[]> {
  return invoke('list_sessions');
}

/** The currently-active session, or null if none started yet. */
export function currentSession(): Promise<SessionMeta | null> {
  return invoke('current_session');
}

/** Rename a session (sets a custom title, overriding the LLM/derived one). */
export function renameSession(id: string, title: string): Promise<void> {
  return invoke('rename_session', { id, title });
}

/** Delete a session (and its data). If it was active, the backend switches to
 *  the most-recent remaining session (or creates a fresh one). */
export function deleteSession(id: string): Promise<void> {
  return invoke('delete_session', { id });
}

/** Emitted when a session's LLM-generated title is ready (after its first
 *  message). Frontend updates its session list on receipt. */
export const SESSION_TITLED_EVENT = 'aidock:session_titled';
export interface SessionTitled {
  id: string;
  title: string;
}
