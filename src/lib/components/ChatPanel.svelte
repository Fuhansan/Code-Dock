<script lang="ts">
  import { sendChatMessage, type ChatMessage } from '$lib/ipc';
  import { tick } from 'svelte';

  // ---- Sprint 1: single-agent chat, no role/topic/state-machine yet ----

  const PROVIDER = 'bailian';
  const MODEL = 'qwen3.6-plus';

  let messages = $state<ChatMessage[]>([]);
  let input = $state('');
  let sending = $state(false);
  let error = $state<string | null>(null);

  let scrollRef = $state<HTMLDivElement | null>(null);

  async function scrollToBottom() {
    await tick();
    if (scrollRef) scrollRef.scrollTop = scrollRef.scrollHeight;
  }

  async function handleSend() {
    const text = input.trim();
    if (!text || sending) return;
    error = null;

    const userMsg: ChatMessage = { role: 'user', content: text };
    messages = [...messages, userMsg];
    input = '';
    sending = true;
    await scrollToBottom();

    try {
      const resp = await sendChatMessage({
        provider: PROVIDER,
        model: MODEL,
        messages: messages,
        temperature: 0.5,
        max_tokens: 4096
      });
      const assistantMsg: ChatMessage = {
        role: 'assistant',
        content: resp.content,
        tool_calls: resp.tool_calls
      };
      messages = [...messages, assistantMsg];
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      sending = false;
      await scrollToBottom();
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    // Enter sends, Shift+Enter inserts newline (chat-app convention).
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      handleSend();
    }
  }
</script>

<div class="chat-panel">
  <div class="chat-area" bind:this={scrollRef}>
    {#if messages.length === 0}
      <div class="empty">
        <p>跟 <strong>Qwen ({MODEL})</strong> 对话</p>
        <p class="muted">Sprint 1 单 Agent 模式，多 Agent 协作在 Sprint 2 引入</p>
      </div>
    {:else}
      {#each messages as msg, i (i)}
        <div class="bubble {msg.role}">
          <div class="role-label">
            {#if msg.role === 'user'}你{:else if msg.role === 'assistant'}Qwen{:else}{msg.role}{/if}
          </div>
          <div class="content">
            {#if 'content' in msg && msg.content}
              {msg.content}
            {:else}
              <em class="muted">(无文本内容)</em>
            {/if}
          </div>
        </div>
      {/each}
    {/if}

    {#if sending}
      <div class="bubble assistant pending">
        <div class="role-label">Qwen</div>
        <div class="content"><span class="dot"></span><span class="dot"></span><span class="dot"></span></div>
      </div>
    {/if}

    {#if error}
      <div class="error-box" role="alert">
        <strong>请求失败：</strong>{error}
      </div>
    {/if}
  </div>

  <div class="input-area">
    <textarea
      class="input"
      placeholder="输入消息，Enter 发送，Shift+Enter 换行…"
      bind:value={input}
      onkeydown={handleKeydown}
      disabled={sending}
    ></textarea>
    <button class="send" onclick={handleSend} disabled={sending || !input.trim()}>
      {sending ? '发送中…' : '发送'}
    </button>
  </div>
</div>

<style>
  .chat-panel {
    display: flex;
    flex-direction: column;
    height: 100%;
    overflow: hidden;
  }

  .chat-area {
    flex: 1;
    overflow-y: auto;
    padding: 24px;
    display: flex;
    flex-direction: column;
    gap: 16px;
  }

  .empty {
    margin: auto;
    text-align: center;
    color: #6e6e73;
  }
  .empty p {
    margin: 4px 0;
  }
  .muted {
    color: #8e8e93;
    font-size: 13px;
  }

  .bubble {
    max-width: 78%;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .bubble.user {
    align-self: flex-end;
    align-items: flex-end;
  }
  .bubble.assistant {
    align-self: flex-start;
  }

  .role-label {
    font-size: 11px;
    color: #8e8e93;
    font-weight: 500;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }

  .content {
    padding: 10px 14px;
    border-radius: 14px;
    font-size: 14px;
    line-height: 1.55;
    white-space: pre-wrap;
    word-break: break-word;
  }
  .bubble.user .content {
    background: #007aff;
    color: #ffffff;
  }
  .bubble.assistant .content {
    background: #f0f0f0;
    color: #1c1c1e;
  }

  .pending .content {
    display: flex;
    gap: 4px;
    align-items: center;
    height: 18px;
  }
  .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: #8e8e93;
    animation: pulse 1.2s ease-in-out infinite;
  }
  .dot:nth-child(2) {
    animation-delay: 0.2s;
  }
  .dot:nth-child(3) {
    animation-delay: 0.4s;
  }
  @keyframes pulse {
    0%, 60%, 100% { opacity: 0.3; }
    30% { opacity: 1; }
  }

  .error-box {
    background: #ffefee;
    color: #ff3b30;
    border: 1px solid #ffd0cc;
    border-radius: 8px;
    padding: 10px 12px;
    font-size: 13px;
    line-height: 1.5;
  }

  .input-area {
    border-top: 1px solid #e5e5e7;
    padding: 12px 16px;
    background: #ffffff;
    display: flex;
    gap: 8px;
    align-items: flex-end;
  }

  .input {
    flex: 1;
    min-height: 56px;
    max-height: 200px;
    border: 1px solid #e5e5e7;
    border-radius: 8px;
    padding: 10px 12px;
    font-family: inherit;
    font-size: 14px;
    resize: none;
    outline: none;
    background: #fafafa;
    box-sizing: border-box;
    transition: border-color 0.15s;
  }
  .input:focus {
    border-color: #007aff;
    box-shadow: 0 0 0 3px rgba(0, 122, 255, 0.12);
  }

  .send {
    background: #007aff;
    color: #ffffff;
    border: none;
    border-radius: 8px;
    padding: 0 18px;
    height: 36px;
    font-size: 14px;
    font-weight: 500;
    cursor: pointer;
    transition: background 0.15s;
    white-space: nowrap;
  }
  .send:hover:not(:disabled) {
    background: #0066d6;
  }
  .send:disabled {
    background: #c7c7cc;
    cursor: not-allowed;
  }

  @media (prefers-color-scheme: dark) {
    .bubble.assistant .content {
      background: #38383a;
      color: #f5f5f7;
    }
    .empty {
      color: #98989d;
    }
    .muted {
      color: #8e8e93;
    }
    .input-area {
      background: #2c2c2e;
      border-top-color: #38383a;
    }
    .input {
      background: #1c1c1e;
      border-color: #38383a;
      color: #f5f5f7;
    }
    .error-box {
      background: #3a1f1d;
      color: #ff6961;
      border-color: #5a2926;
    }
  }
</style>
