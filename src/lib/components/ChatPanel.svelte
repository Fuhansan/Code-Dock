<script lang="ts">
  import { onMount, onDestroy, tick } from 'svelte';
  import { listen, type UnlistenFn } from '@tauri-apps/api/event';
  import {
    MESSAGE_EVENT,
    MCP_CALL_EVENT,
    startSession,
    sendUserMessage,
    sessionStatus,
    loadMessageHistory,
    type AgentMessage,
    type AgentMessageKind,
    type McpCallEvent
  } from '$lib/ipc';

  type TimelineItem =
    | { kind: 'message'; ts: number; data: AgentMessage }
    | { kind: 'mcp'; ts: number; data: McpCallEvent };

  // ---- Sprint 2: multi-agent group chat ----

  // Sprint 4.5: unified timeline. AgentMessages and McpCallEvents both
  // land here, sorted by their backend timestamp so the user sees the
  // chat in causal order even when MCP events arrive between messages.
  let timeline = $state<TimelineItem[]>([]);
  // Derived "messages only" view for backward-compat checks (empty test,
  // input enabled, etc.).
  let messages = $derived(timeline.filter((i) => i.kind === 'message') as Array<{
    kind: 'message';
    ts: number;
    data: AgentMessage;
  }>);

  let input = $state('');
  let sending = $state(false);
  let bootError = $state<string | null>(null);
  let sendError = $state<string | null>(null);
  let ready = $state(false);

  // Sprint 2.6: topic_id → human title. Populated as we see opens_topic_title
  // on incoming messages. Used for SUMMARY divider headings + topic break
  // labels in the stream.
  let topicTitles = $state<Record<string, string>>({});

  let scrollRef = $state<HTMLDivElement | null>(null);
  let unlistenMsg: UnlistenFn | null = null;
  let unlistenMcp: UnlistenFn | null = null;

  function insertItem(item: TimelineItem) {
    // Append-and-sort. Sort is stable + cheap for the message volumes V0.1
    // sees, and prevents reordering when events arrive in slightly
    // surprising orders (an MCP call dispatched before its surrounding
    // BROADCAST emit, etc).
    const next = [...timeline, item];
    next.sort((a, b) => a.ts - b.ts);
    timeline = next;
  }

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
    // Sprint 3: load persisted history FIRST so the user sees prior turns
    // immediately after a restart, before any live events arrive.
    try {
      const history = await loadMessageHistory();
      const initial: TimelineItem[] = [];
      for (const m of history) {
        if (m.opens_topic_title && !topicTitles[m.topic_id]) {
          topicTitles[m.topic_id] = m.opens_topic_title;
        }
        initial.push({ kind: 'message', ts: m.timestamp, data: m });
      }
      initial.sort((a, b) => a.ts - b.ts);
      timeline = initial;
      await scrollToBottom();
    } catch (e) {
      // Non-fatal — fresh session simply returns []. A real load error
      // surfaces here but shouldn't block subscribe + start.
      console.warn('load history failed:', e);
    }

    // Subscribe BEFORE start_session so we don't miss any events the
    // dispatcher emits during agent spin-up.
    try {
      unlistenMsg = await listen<AgentMessage>(MESSAGE_EVENT, async (event) => {
        const m = event.payload;
        if (m.opens_topic_title && !topicTitles[m.topic_id]) {
          topicTitles = { ...topicTitles, [m.topic_id]: m.opens_topic_title };
        }
        insertItem({ kind: 'message', ts: m.timestamp, data: m });
        await scrollToBottom();
      });
      // Sprint 4.5: MCP tool calls flow on a separate Tauri event.
      unlistenMcp = await listen<McpCallEvent>(MCP_CALL_EVENT, async (event) => {
        const e = event.payload;
        insertItem({ kind: 'mcp', ts: e.timestamp, data: e });
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
    if (unlistenMsg) unlistenMsg();
    if (unlistenMcp) unlistenMcp();
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
    {#if timeline.length === 0 && ready}
      <div class="empty">
        <p>多 Agent 工作室已就绪</p>
        <p class="muted">默认成员：PM、前端、后端 · 输入一条需求开始</p>
      </div>
    {/if}

    {#each timeline as item (item.kind === 'message' ? item.data.id : item.data.id)}
      {#if item.kind === 'mcp'}
        {@const e = item.data}
        {@const s = styleFor(e.agent)}
        <div class="mcp-card" class:err={!e.success} style:--accent={s.color}>
          <div class="mcp-head">
            <span class="mcp-tool">🔧 {e.tool}</span>
            <span class="mcp-agent" style:color={s.color}>{s.label}</span>
            {#if !e.success}<span class="mcp-badge">ERR</span>{/if}
          </div>
          <div class="mcp-args">{e.args_preview}</div>
          <div class="mcp-result">{e.result_preview}</div>
        </div>
      {:else}
        {@const msg = item.data}
        {@const s = styleFor(msg.sender)}
        {@const isUser = msg.sender === 'user'}
        {@const info = isInfoOnly(msg.kind)}

        {#if msg.kind.type === 'SUMMARY'}
        {@const summaryTopicId = msg.kind.topic_id}
        {@const summaryTopicTitle = topicTitles[summaryTopicId]}
        <div class="divider">
          <hr />
          <div class="divider-text">
            <span class="divider-label">SUMMARY</span>
            {#if summaryTopicTitle}
              <span class="topic-title">「{summaryTopicTitle}」</span>
            {/if}
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
  .topic-title {
    font-weight: 500;
    color: #1c1c1e;
  }

  /* Sprint 4.5 — MCP tool-call cards */
  .mcp-card {
    align-self: stretch;
    border-left: 3px solid var(--accent, #c7c7cc);
    background: #f9f9fb;
    border-radius: 0 8px 8px 0;
    padding: 8px 12px;
    font-size: 12px;
    line-height: 1.4;
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin-left: 4px;
  }
  .mcp-card.err {
    background: #fff7f6;
    border-left-color: #ff3b30;
  }
  .mcp-head {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .mcp-tool {
    font-family: 'SF Mono', Menlo, monospace;
    font-size: 12px;
    font-weight: 500;
  }
  .mcp-agent {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .mcp-badge {
    font-size: 10px;
    background: #ff3b30;
    color: #fff;
    padding: 1px 6px;
    border-radius: 3px;
    letter-spacing: 0.04em;
  }
  .mcp-args,
  .mcp-result {
    font-family: 'SF Mono', Menlo, monospace;
    font-size: 11px;
    color: #6e6e73;
    white-space: pre-wrap;
    word-break: break-word;
  }
  .mcp-result {
    color: #1c1c1e;
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
    .mcp-card {
      background: #1c1c1e;
    }
    .mcp-card.err {
      background: #3a1f1d;
    }
    .mcp-args {
      color: #8e8e93;
    }
    .mcp-result {
      color: #d1d1d6;
    }
    .topic-title {
      color: #f5f5f7;
    }
  }
</style>
