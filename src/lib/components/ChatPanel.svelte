<script lang="ts">
  import { onMount, onDestroy, tick } from 'svelte';
  import { listen, type UnlistenFn } from '@tauri-apps/api/event';
  import {
    MESSAGE_EVENT,
    startSession,
    sendUserMessage,
    sessionStatus,
    type AgentMessage,
    type AgentMessageKind
  } from '$lib/ipc';

  // ---- Sprint 2: multi-agent group chat ----

  let messages = $state<AgentMessage[]>([]);
  let input = $state('');
  let sending = $state(false);
  let bootError = $state<string | null>(null);
  let sendError = $state<string | null>(null);
  let ready = $state(false);

  let scrollRef = $state<HTMLDivElement | null>(null);
  let unlistenFn: UnlistenFn | null = null;

  // Per-sender styling. Keys are role ids; unknown senders fall back.
  const SENDER_STYLES: Record<string, { label: string; color: string }> = {
    user: { label: '你', color: '#007aff' },
    PM: { label: 'PM', color: '#34c759' },
    frontend_dev: { label: '前端', color: '#ff9500' },
    backend_dev: { label: '后端', color: '#af52de' }
  };

  function styleFor(sender: string) {
    return SENDER_STYLES[sender] ?? { label: sender, color: '#8e8e93' };
  }

  async function scrollToBottom() {
    await tick();
    if (scrollRef) scrollRef.scrollTop = scrollRef.scrollHeight;
  }

  onMount(async () => {
    // Subscribe BEFORE start_session so we don't miss any events the
    // dispatcher emits during agent spin-up.
    try {
      unlistenFn = await listen<AgentMessage>(MESSAGE_EVENT, async (event) => {
        messages = [...messages, event.payload];
        await scrollToBottom();
      });
    } catch (e) {
      bootError = e instanceof Error ? e.message : String(e);
      return;
    }

    try {
      // start_session is idempotent; if a previous render already booted
      // the session this is a no-op.
      await startSession();
      ready = await sessionStatus();
    } catch (e) {
      bootError = e instanceof Error ? e.message : String(e);
    }
  });

  onDestroy(() => {
    if (unlistenFn) unlistenFn();
  });

  async function handleSend() {
    const text = input.trim();
    if (!text || sending || !ready) return;
    sendError = null;
    sending = true;
    input = '';
    try {
      await sendUserMessage(text);
    } catch (e) {
      sendError = e instanceof Error ? e.message : String(e);
    } finally {
      sending = false;
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      handleSend();
    }
  }

  // ---- Rendering helpers ----

  function kindTag(kind: AgentMessageKind): string {
    return kind.type;
  }

  function isInfoOnly(kind: AgentMessageKind): boolean {
    return (
      kind.type === 'WORK_START' ||
      kind.type === 'PROGRESS' ||
      kind.type === 'DONE' ||
      kind.type === 'SUMMARY'
    );
  }

  function primaryText(kind: AgentMessageKind): string {
    switch (kind.type) {
      case 'BROADCAST':
        return kind.content;
      case 'ASK_AGENT':
        return kind.content;
      case 'ANSWER':
        return kind.content;
      case 'WORK_START':
        return kind.task;
      case 'PROGRESS':
        return `${kind.task} — ${kind.percent}%${kind.note ? ' · ' + kind.note : ''}`;
      case 'DONE':
        return kind.summary;
      case 'SUMMARY':
        return kind.summary;
      case 'USER_INPUT':
        return kind.content;
    }
  }

  function decisionsOf(kind: AgentMessageKind): string[] {
    return kind.type === 'SUMMARY' ? kind.key_decisions : [];
  }
</script>

<div class="chat-panel">
  {#if bootError}
    <div class="banner error">
      <strong>启动失败：</strong>{bootError}
    </div>
  {:else if !ready}
    <div class="banner muted">正在启动工作室会话…</div>
  {/if}

  <div class="chat-area" bind:this={scrollRef}>
    {#if messages.length === 0 && ready}
      <div class="empty">
        <p>多 Agent 工作室已就绪</p>
        <p class="muted">默认成员：PM、前端、后端 · 输入一条需求开始</p>
      </div>
    {/if}

    {#each messages as msg (msg.id)}
      {@const s = styleFor(msg.sender)}
      {@const isUser = msg.sender === 'user'}
      {@const info = isInfoOnly(msg.kind)}

      {#if msg.kind.type === 'SUMMARY'}
        <div class="divider">
          <hr />
          <div class="divider-text">
            <span class="divider-label">SUMMARY</span>
            <span>{primaryText(msg.kind)}</span>
            {#if decisionsOf(msg.kind).length > 0}
              <ul class="decisions">
                {#each decisionsOf(msg.kind) as d}
                  <li>{d}</li>
                {/each}
              </ul>
            {/if}
          </div>
          <hr />
        </div>
      {:else if info}
        <div class="info-line" style:color={s.color}>
          <span class="info-sender">{s.label}</span>
          <span class="info-tag">{kindTag(msg.kind)}</span>
          <span class="info-text">{primaryText(msg.kind)}</span>
        </div>
      {:else}
        <div class="bubble" class:user={isUser}>
          <div class="role-label" style:color={s.color}>
            <span>{s.label}</span>
            {#if msg.kind.type === 'ASK_AGENT'}
              <span class="arrow">→ {styleFor(msg.kind.to).label}</span>
            {:else if msg.kind.type === 'ANSWER'}
              <span class="arrow">↩ 回复</span>
            {/if}
            <span class="kind-tag">{kindTag(msg.kind)}</span>
          </div>
          <div class="content" style:--accent={s.color}>
            {primaryText(msg.kind)}
          </div>
        </div>
      {/if}
    {/each}

    {#if sendError}
      <div class="banner error" role="alert">
        <strong>发送失败：</strong>{sendError}
      </div>
    {/if}
  </div>

  <div class="input-area">
    <textarea
      class="input"
      placeholder={ready ? '提一个需求 — 例如「做一个简单的 todo Web 应用」…' : '会话启动中…'}
      bind:value={input}
      onkeydown={handleKeydown}
      disabled={sending || !ready}
    ></textarea>
    <button class="send" onclick={handleSend} disabled={sending || !ready || !input.trim()}>
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

  .banner {
    padding: 8px 14px;
    font-size: 13px;
    border-bottom: 1px solid #e5e5e7;
  }
  .banner.muted {
    background: #f5f5f7;
    color: #6e6e73;
  }
  .banner.error {
    background: #ffefee;
    color: #ff3b30;
    border-color: #ffd0cc;
  }

  .chat-area {
    flex: 1;
    overflow-y: auto;
    padding: 20px 24px;
    display: flex;
    flex-direction: column;
    gap: 12px;
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

  /* Bubble layout */
  .bubble {
    max-width: 78%;
    display: flex;
    flex-direction: column;
    gap: 4px;
    align-self: flex-start;
  }
  .bubble.user {
    align-self: flex-end;
    align-items: flex-end;
  }

  .role-label {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .arrow {
    font-weight: 500;
    color: #8e8e93;
    text-transform: none;
    letter-spacing: 0;
  }
  .kind-tag {
    font-size: 10px;
    font-weight: 500;
    color: #8e8e93;
    background: #f0f0f0;
    padding: 1px 6px;
    border-radius: 3px;
    letter-spacing: 0;
    text-transform: none;
  }

  .content {
    padding: 10px 14px;
    border-radius: 14px;
    font-size: 14px;
    line-height: 1.55;
    white-space: pre-wrap;
    word-break: break-word;
    background: #f0f0f0;
    color: #1c1c1e;
    border-left: 3px solid var(--accent, #c7c7cc);
  }
  .bubble.user .content {
    background: #007aff;
    color: #ffffff;
    border-left: none;
  }

  /* Info line — WORK_START / PROGRESS / DONE compact rows */
  .info-line {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 12px;
    padding: 4px 0;
  }
  .info-sender {
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .info-tag {
    font-size: 10px;
    background: #f0f0f0;
    color: #6e6e73;
    padding: 1px 6px;
    border-radius: 3px;
  }
  .info-text {
    color: #6e6e73;
    flex: 1;
  }

  /* Divider — SUMMARY rendered as topic boundary */
  .divider {
    display: flex;
    align-items: center;
    gap: 10px;
    margin: 16px 0;
  }
  .divider hr {
    flex: 1;
    border: 0;
    border-top: 1px dashed #c7c7cc;
    margin: 0;
  }
  .divider-text {
    max-width: 60%;
    text-align: center;
    font-size: 12px;
    color: #6e6e73;
    display: flex;
    flex-direction: column;
    gap: 4px;
    align-items: center;
  }
  .divider-label {
    font-size: 10px;
    font-weight: 600;
    background: #34c759;
    color: #ffffff;
    padding: 2px 8px;
    border-radius: 4px;
    letter-spacing: 0.06em;
  }
  .decisions {
    margin: 4px 0 0 0;
    padding-left: 18px;
    text-align: left;
    font-size: 12px;
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
    .banner.muted {
      background: #2c2c2e;
      color: #98989d;
      border-color: #38383a;
    }
    .banner.error {
      background: #3a1f1d;
      color: #ff6961;
      border-color: #5a2926;
    }
    .empty {
      color: #98989d;
    }
    .muted {
      color: #8e8e93;
    }
    .content {
      background: #38383a;
      color: #f5f5f7;
    }
    .kind-tag,
    .info-tag {
      background: #38383a;
      color: #98989d;
    }
    .info-text {
      color: #98989d;
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
    .divider hr {
      border-top-color: #48484a;
    }
    .divider-text {
      color: #98989d;
    }
  }
</style>
