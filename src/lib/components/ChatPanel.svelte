<script lang="ts">
  import { onMount, onDestroy, tick } from 'svelte';
  import { listen, type UnlistenFn } from '@tauri-apps/api/event';
  import {
    MESSAGE_EVENT,
    TOOL_CALL_EVENT,
    APPROVAL_REQUEST_EVENT,
    sendUserMessage,
    loadMessageHistory,
    loadMcpCallHistory,
    respondToApproval,
    type AgentMessage,
    type AgentMessageKind,
    type McpCallEvent,
    type ApprovalRequest,
    type ApprovalDecision
  } from '$lib/ipc';

  type TimelineItem =
    | { kind: 'message'; ts: number; data: AgentMessage }
    | { kind: 'mcp'; ts: number; data: McpCallEvent };

  type RenderItem =
    | { kind: 'message'; data: AgentMessage }
    | { kind: 'mcp_group'; agent: string; calls: McpCallEvent[] };

  // ---- Sprint 2: multi-agent group chat ----

  // Sprint 4.5: unified timeline. AgentMessages and McpCallEvents both
  // land here, sorted by their backend timestamp so the user sees the
  // chat in causal order even when MCP events arrive between messages.
  let timeline = $state<TimelineItem[]>([]);

  // Sprint 4.5 refinement: collapse consecutive MCP events from the same
  // agent into one group, rendered as a single "work card" with sub-lines.
  // Stops the chat from being drowned by 4–8 cards per agent activity.
  // Re-runs whenever timeline changes — cheap for V0.1 message volumes.
  const renderItems = $derived.by<RenderItem[]>(() => {
    const out: RenderItem[] = [];
    for (const it of timeline) {
      if (it.kind === 'message') {
        out.push({ kind: 'message', data: it.data });
        continue;
      }
      const last = out[out.length - 1];
      if (last && last.kind === 'mcp_group' && last.agent === it.data.agent) {
        last.calls.push(it.data);
      } else {
        out.push({ kind: 'mcp_group', agent: it.data.agent, calls: [it.data] });
      }
    }
    return out;
  });
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
  // ① 当前会话 id（由 +page 传入）。变化即重载历史；非空即可输入。
  let { sessionId = '' }: { sessionId?: string } = $props();
  const ready = $derived(!!sessionId);

  // Sprint 2.6: topic_id → human title. Populated as we see opens_topic_title
  // on incoming messages. Used for SUMMARY divider headings + topic break
  // labels in the stream.
  let topicTitles = $state<Record<string, string>>({});

  let scrollRef = $state<HTMLDivElement | null>(null);
  let unlistenMsg: UnlistenFn | null = null;
  let unlistenMcp: UnlistenFn | null = null;
  let unlistenApproval: UnlistenFn | null = null;

  // Sprint 4.6: queue of pending approval prompts. Show one at a time as
  // a banner; queue the rest so multiple agents racing for approval
  // don't pile up overlapping modals.
  let approvalQueue = $state<ApprovalRequest[]>([]);
  let approvalInFlight = $state<string | null>(null);
  const currentApproval = $derived(approvalQueue[0] ?? null);

  async function decide(decision: ApprovalDecision) {
    if (!currentApproval || approvalInFlight) return;
    approvalInFlight = currentApproval.id;
    try {
      await respondToApproval(currentApproval.id, decision);
    } catch (e) {
      console.warn('respond_to_approval failed:', e);
    } finally {
      // Drop the head whether the command succeeded or not — the runtime
      // either got our decision or has already timed out.
      approvalQueue = approvalQueue.slice(1);
      approvalInFlight = null;
    }
  }

  function dangerLabel(d: ApprovalRequest['danger']): string {
    return d === 'destructive' ? '高风险' : d === 'mutating' ? '修改文件' : '只读';
  }

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

  /** Load (or reload) the ACTIVE session's persisted streams into the timeline.
   *  Clears first so switching sessions never mixes two sessions' streams.
   *  (Session lifecycle — start/resume/new — is owned by +page; this component
   *  just shows whichever session is active and reloads when `sessionId` changes.) */
  async function loadHistory() {
    try {
      const [history, mcpHistory] = await Promise.all([
        loadMessageHistory(),
        loadMcpCallHistory().catch((e) => {
          console.warn('load mcp history failed:', e);
          return [] as McpCallEvent[];
        })
      ]);
      const initial: TimelineItem[] = [];
      const titles: Record<string, string> = {};
      for (const m of history) {
        if (m.opens_topic_title) titles[m.topic_id] = m.opens_topic_title;
        initial.push({ kind: 'message', ts: m.timestamp, data: m });
      }
      for (const e of mcpHistory) {
        initial.push({ kind: 'mcp', ts: e.timestamp, data: e });
      }
      initial.sort((a, b) => a.ts - b.ts);
      topicTitles = titles;
      timeline = initial;
      await scrollToBottom();
    } catch (e) {
      console.warn('load history failed:', e);
    }
  }

  onMount(async () => {
    // Subscribe once. Events flow from whichever session is currently active,
    // so no re-subscription is needed on switch.
    try {
      unlistenMsg = await listen<AgentMessage>(MESSAGE_EVENT, async (event) => {
        const m = event.payload;
        if (m.opens_topic_title && !topicTitles[m.topic_id]) {
          topicTitles = { ...topicTitles, [m.topic_id]: m.opens_topic_title };
        }
        insertItem({ kind: 'message', ts: m.timestamp, data: m });
        await scrollToBottom();
      });
      // step 5: ALL tool calls (file/Bash/recall/MCP) flow on this Tauri event.
      unlistenMcp = await listen<McpCallEvent>(TOOL_CALL_EVENT, async (event) => {
        const e = event.payload;
        insertItem({ kind: 'mcp', ts: e.timestamp, data: e });
        await scrollToBottom();
      });
      // Sprint 4.6: approval prompts.
      unlistenApproval = await listen<ApprovalRequest>(
        APPROVAL_REQUEST_EVENT,
        (event) => {
          approvalQueue = [...approvalQueue, event.payload];
        }
      );
    } catch (e) {
      bootError = e instanceof Error ? e.message : String(e);
    }
  });

  // Reload history whenever the active session changes (incl. the first set).
  $effect(() => {
    const sid = sessionId;
    if (!sid) return;
    loadHistory();
  });

  onDestroy(() => {
    if (unlistenMsg) unlistenMsg();
    if (unlistenMcp) unlistenMcp();
    if (unlistenApproval) unlistenApproval();
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

  // Sprint 4.7: per-tool formatters. Backend now ships full args/result
  // in the event; UI picks a Claude-Code-style one-liner here and lets
  // the user click to expand the row for the raw body. Each `case` is
  // intentionally small so adding a new MCP tool is a one-place change.

  function shortTool(t: string): string {
    return t.replace(/^fs__/, '');
  }

  function basename(p: string): string {
    const idx = p.lastIndexOf('/');
    const name = idx >= 0 ? p.slice(idx + 1) : p;
    return name || p; // handle trailing slash
  }

  function parseArgs(raw: string): Record<string, unknown> {
    try {
      const v = JSON.parse(raw);
      return v && typeof v === 'object' && !Array.isArray(v)
        ? (v as Record<string, unknown>)
        : {};
    } catch {
      return {};
    }
  }

  function str(v: unknown): string {
    return typeof v === 'string' ? v : '';
  }

  function clip(s: string, max = 200): string {
    return s.length > max ? s.slice(0, max) + '…' : s;
  }

  /** Per-row display state. `head` lands next to the tool name as
   *  `tool(head)`; `summary` is the indented `⎿` line. */
  type Formatted = { head: string; summary: string };

  function formatCall(c: McpCallEvent): Formatted {
    const tool = shortTool(c.tool);
    const args = parseArgs(c.args);
    const result = c.result.trim();

    // Failure paths bypass per-tool formatting — the user wants to see
    // *why* it failed, not "+12 lines".
    if (result.startsWith('DENIED:')) {
      return { head: argLabel(args), summary: clip(result) };
    }
    if (result.startsWith('ERROR:')) {
      const tail = result.slice('ERROR:'.length).trim();
      return { head: argLabel(args), summary: clip(tail.split('\n')[0]) };
    }

    switch (tool) {
      case 'write_file': {
        // Create/overwrite — never dump file body, just confirm.
        const path = str(args.path);
        const content = str(args.content);
        const lines = content ? content.split('\n').length : 0;
        return {
          head: basename(path),
          summary: lines > 0 ? `wrote ${path} · ${lines} lines` : `wrote ${path}`
        };
      }
      case 'edit_file': {
        // Pull +/- from the diff result if we got one; else fall back
        // to the edits[] count from args.
        const path = str(args.path);
        let added = 0;
        let removed = 0;
        for (const line of result.split('\n')) {
          if (line.startsWith('+') && !line.startsWith('+++')) added++;
          else if (line.startsWith('-') && !line.startsWith('---')) removed++;
        }
        const editsCount = Array.isArray(args.edits) ? args.edits.length : null;
        const summary =
          added + removed > 0
            ? `${path} · +${added} −${removed} lines`
            : editsCount != null
              ? `${path} · ${editsCount} edit${editsCount === 1 ? '' : 's'}`
              : `${path} · edited`;
        return { head: basename(path), summary };
      }
      case 'read_text_file':
      case 'read_file': {
        const path = str(args.path);
        const lines = result.split('\n').length;
        return { head: basename(path), summary: `${path} · ${lines} lines` };
      }
      case 'read_media_file': {
        const path = str(args.path);
        return { head: basename(path), summary: `${path} · binary content` };
      }
      case 'read_multiple_files': {
        const paths = Array.isArray(args.paths) ? args.paths.length : 0;
        return { head: `${paths} files`, summary: `read ${paths} files` };
      }
      case 'list_directory':
      case 'list_directory_with_sizes': {
        const path = str(args.path);
        const entries = result.split('\n').filter((l) => l.trim()).length;
        return { head: basename(path) || '.', summary: `${path || '.'} · ${entries} entries` };
      }
      case 'directory_tree': {
        const path = str(args.path);
        const lines = result.split('\n').filter((l) => l.trim()).length;
        return { head: basename(path) || '.', summary: `${path || '.'} · ${lines} entries (tree)` };
      }
      case 'search_files': {
        const pattern = str(args.pattern);
        const matches = result.split('\n').filter((l) => l.trim()).length;
        return {
          head: pattern,
          summary: matches > 0 ? `${matches} matches` : 'no matches'
        };
      }
      case 'create_directory': {
        const path = str(args.path);
        return { head: basename(path), summary: `created ${path}` };
      }
      case 'move_file': {
        const src = str(args.source);
        const dst = str(args.destination);
        return {
          head: `${basename(src)} → ${basename(dst)}`,
          summary: `${src} → ${dst}`
        };
      }
      case 'get_file_info': {
        const path = str(args.path);
        const first = result.split('\n')[0] ?? '';
        return { head: basename(path), summary: clip(first, 120) };
      }
      case 'list_allowed_directories':
        return { head: '', summary: 'allowed directories listed' };
      default: {
        // Unknown tool — show a generic key arg + first line of result.
        const first = result.split('\n')[0] ?? '';
        return { head: argLabel(args), summary: clip(first, 120) };
      }
    }
  }

  function argLabel(args: Record<string, unknown>): string {
    if (typeof args.path === 'string') return basename(args.path);
    if (typeof args.query === 'string') return args.query;
    if (typeof args.pattern === 'string') return args.pattern;
    if (typeof args.source === 'string') return basename(args.source);
    return '';
  }

  /** Pretty-print args JSON for the expand panel. Falls back to the
   *  raw string when args isn't parseable (already-truncated tail). */
  function prettyArgs(raw: string): string {
    try {
      return JSON.stringify(JSON.parse(raw), null, 2);
    } catch {
      return raw;
    }
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

  {#if currentApproval}
    {@const s = styleFor(currentApproval.agent)}
    <div class="approval-banner" style:--accent={s.color}>
      <div class="approval-head">
        <span class="approval-label">⚠ 需要批准</span>
        <span class="approval-agent" style:color={s.color}>{s.label}</span>
        <span class="approval-danger">{dangerLabel(currentApproval.danger)}</span>
        {#if approvalQueue.length > 1}
          <span class="approval-queue">+{approvalQueue.length - 1} 个排队</span>
        {/if}
      </div>
      <div class="approval-tool">🔧 {currentApproval.tool}</div>
      <div class="approval-args">{currentApproval.args_preview}</div>
      <div class="approval-actions">
        <button
          class="appr-btn deny"
          onclick={() => decide('reject')}
          disabled={!!approvalInFlight}
        >拒绝</button>
        <button
          class="appr-btn once"
          onclick={() => decide('allow_once')}
          disabled={!!approvalInFlight}
        >仅本次允许</button>
        <button
          class="appr-btn session"
          onclick={() => decide('allow_session')}
          disabled={!!approvalInFlight}
        >本会话允许</button>
      </div>
    </div>
  {/if}

  <div class="chat-area" bind:this={scrollRef}>
    {#if timeline.length === 0 && ready}
      <div class="empty">
        <p>多 Agent 工作室已就绪</p>
        <p class="muted">默认成员：PM、前端、后端 · 输入一条需求开始</p>
      </div>
    {/if}

    {#each renderItems as item, idx (idx)}
      {#if item.kind === 'mcp_group'}
        {@const s = styleFor(item.agent)}
        {@const okCount = item.calls.filter((c) => c.success).length}
        {@const errCount = item.calls.length - okCount}
        <div class="mcp-card" style:--accent={s.color}>
          <div class="mcp-head">
            <span class="mcp-agent" style:color={s.color}>{s.label}</span>
            <span class="mcp-headline">
              {item.calls.length} 次工具调用{#if errCount > 0} · <span class="mcp-err-count">{errCount} 失败</span>{/if}
            </span>
          </div>
          <ul class="mcp-list">
            {#each item.calls as c (c.id)}
              {@const f = formatCall(c)}
              <li class="mcp-row" class:err={!c.success}>
                <details class="mcp-detail">
                  <summary class="mcp-summary">
                    <span class="mcp-caret">▸</span>
                    <span class="mcp-line">
                      <span class="mcp-bullet">●</span>
                      <span class="mcp-name">{shortTool(c.tool)}</span>
                      {#if f.head}<span class="mcp-arg">({f.head})</span>{/if}
                    </span>
                    <span class="mcp-line mcp-result-line">
                      <span class="mcp-elbow">⎿</span>
                      <span class="mcp-result">{f.summary}</span>
                    </span>
                  </summary>
                  <div class="mcp-detail-body">
                    <div class="mcp-detail-label">args</div>
                    <pre class="mcp-detail-pre">{prettyArgs(c.args)}</pre>
                    <div class="mcp-detail-label">result</div>
                    <pre class="mcp-detail-pre">{c.result || '(empty)'}</pre>
                  </div>
                </details>
              </li>
            {/each}
          </ul>
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

  /* Sprint 4.6 — approval banner */
  .approval-banner {
    background: #fff8e6;
    border: 1px solid #ffcc66;
    border-left: 4px solid var(--accent, #ff9500);
    padding: 12px 16px;
    margin: 8px 16px 0;
    border-radius: 8px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    font-size: 13px;
  }
  .approval-head {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
  }
  .approval-label {
    font-weight: 600;
    color: #b35a00;
  }
  .approval-agent {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .approval-danger {
    background: #ff9500;
    color: #ffffff;
    font-size: 11px;
    padding: 2px 8px;
    border-radius: 4px;
    font-weight: 500;
  }
  .approval-queue {
    margin-left: auto;
    font-size: 11px;
    color: #8e8e93;
  }
  .approval-tool {
    font-family: 'SF Mono', Menlo, monospace;
    font-size: 13px;
    color: #1c1c1e;
  }
  .approval-args {
    font-family: 'SF Mono', Menlo, monospace;
    font-size: 11px;
    color: #6e6e73;
    background: rgba(255, 255, 255, 0.6);
    padding: 8px;
    border-radius: 4px;
    white-space: pre-wrap;
    word-break: break-word;
    max-height: 120px;
    overflow-y: auto;
  }
  .approval-actions {
    display: flex;
    gap: 8px;
    justify-content: flex-end;
  }
  .appr-btn {
    padding: 6px 14px;
    font-size: 13px;
    font-weight: 500;
    border-radius: 6px;
    border: 1px solid transparent;
    cursor: pointer;
    transition: background 0.15s, opacity 0.15s;
  }
  .appr-btn:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }
  .appr-btn.deny {
    background: #ff3b30;
    color: #ffffff;
  }
  .appr-btn.deny:hover:not(:disabled) {
    background: #d62b22;
  }
  .appr-btn.once {
    background: #ffffff;
    border-color: #d2d2d7;
    color: #1c1c1e;
  }
  .appr-btn.once:hover:not(:disabled) {
    background: #f5f5f7;
  }
  .appr-btn.session {
    background: #34c759;
    color: #ffffff;
  }
  .appr-btn.session:hover:not(:disabled) {
    background: #2aa849;
  }

  /* Sprint 4.5 — MCP tool-call cards. Claude-Code-ish compact rows:
     "● tool(arg)" + indented "⎿ result". Verbose args available on
     hover via the row's title attribute. */
  .mcp-card {
    align-self: stretch;
    border-left: 3px solid var(--accent, #c7c7cc);
    background: #f9f9fb;
    border-radius: 0 8px 8px 0;
    padding: 8px 12px;
    font-size: 12px;
    line-height: 1.45;
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin-left: 4px;
  }
  .mcp-head {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 2px;
  }
  .mcp-agent {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .mcp-headline {
    font-size: 11px;
    color: #6e6e73;
  }
  .mcp-err-count {
    color: #ff3b30;
    font-weight: 500;
  }
  .mcp-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .mcp-row {
    padding: 2px 0;
  }
  .mcp-detail {
    display: block;
  }
  .mcp-summary {
    list-style: none;
    cursor: pointer;
    display: grid;
    grid-template-columns: 14px 1fr;
    column-gap: 4px;
    row-gap: 0;
    align-items: start;
    padding: 2px 4px 2px 0;
    border-radius: 4px;
    transition: background 0.1s;
  }
  .mcp-summary::-webkit-details-marker {
    display: none;
  }
  .mcp-summary:hover {
    background: rgba(0, 0, 0, 0.03);
  }
  .mcp-caret {
    grid-row: 1 / span 2;
    color: #c7c7cc;
    font-size: 10px;
    line-height: 1.5;
    transition: transform 0.15s;
    user-select: none;
  }
  .mcp-detail[open] .mcp-caret {
    transform: rotate(90deg);
  }
  .mcp-line {
    display: flex;
    align-items: baseline;
    gap: 6px;
    font-family: 'SF Mono', Menlo, monospace;
    font-size: 12px;
    line-height: 1.5;
    color: #1c1c1e;
    min-width: 0;
  }
  .mcp-bullet {
    color: var(--accent, #6e6e73);
    font-size: 10px;
    line-height: 1;
    align-self: center;
    flex-shrink: 0;
  }
  .mcp-row.err .mcp-bullet {
    color: #ff3b30;
  }
  .mcp-name {
    font-weight: 500;
    flex-shrink: 0;
  }
  .mcp-arg {
    color: #6e6e73;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }
  .mcp-result-line {
    color: #6e6e73;
    padding-left: 10px;
  }
  .mcp-elbow {
    color: #c7c7cc;
    margin-right: 2px;
    flex-shrink: 0;
  }
  .mcp-result {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }
  .mcp-row.err .mcp-result {
    color: #ff3b30;
  }
  .mcp-detail-body {
    margin: 4px 0 6px 18px;
    padding: 8px 10px;
    background: rgba(0, 0, 0, 0.04);
    border-radius: 6px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .mcp-detail-label {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: #8e8e93;
    font-weight: 600;
  }
  .mcp-detail-pre {
    margin: 0 0 4px 0;
    font-family: 'SF Mono', Menlo, monospace;
    font-size: 11px;
    line-height: 1.45;
    color: #1c1c1e;
    background: rgba(255, 255, 255, 0.6);
    padding: 6px 8px;
    border-radius: 4px;
    white-space: pre-wrap;
    word-break: break-word;
    max-height: 320px;
    overflow: auto;
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

  /* 自动深色已停用——固定浅色，避免界面随系统深色模式"乱变色"（用户要求）。
     条件 max-width:0 永不匹配 = 整块失效；将来要深色/做切换时去掉这个条件即可。 */
  @media (prefers-color-scheme: dark) and (max-width: 0px) {
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
      background: #2c2c2e;
    }
    .mcp-line {
      color: #f5f5f7;
    }
    .mcp-arg {
      color: #8e8e93;
    }
    .mcp-result-line {
      color: #8e8e93;
    }
    .mcp-elbow {
      color: #48484a;
    }
    .mcp-headline {
      color: #98989d;
    }
    .mcp-row.err .mcp-result {
      color: #ff6961;
    }
    .mcp-summary:hover {
      background: rgba(255, 255, 255, 0.04);
    }
    .mcp-detail-body {
      background: rgba(255, 255, 255, 0.04);
    }
    .mcp-detail-pre {
      background: rgba(0, 0, 0, 0.3);
      color: #f5f5f7;
    }
    .mcp-detail-label {
      color: #98989d;
    }
    .mcp-caret {
      color: #48484a;
    }
    .topic-title {
      color: #f5f5f7;
    }
    .approval-banner {
      background: #2c2410;
      border-color: #6e5318;
    }
    .approval-tool {
      color: #f5f5f7;
    }
    .approval-args {
      background: rgba(0, 0, 0, 0.3);
      color: #d1d1d6;
    }
    .appr-btn.once {
      background: #38383a;
      border-color: #48484a;
      color: #f5f5f7;
    }
    .appr-btn.once:hover:not(:disabled) {
      background: #48484a;
    }
  }
</style>
