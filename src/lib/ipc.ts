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
