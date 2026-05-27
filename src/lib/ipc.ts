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
