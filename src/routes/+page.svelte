<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { listen, type UnlistenFn } from '@tauri-apps/api/event';
  import {
    greet,
    providerStatus,
    startSession,
    currentSession,
    listSessions,
    newSession,
    resumeSession,
    renameSession,
    deleteSession,
    listMembers,
    SESSION_TITLED_EVENT,
    MESSAGE_EVENT,
    TOOL_CALL_EVENT,
    type SessionMeta,
    type SessionTitled,
    type MemberInfo
  } from '$lib/ipc';
  import ApiKeySetup from '$lib/components/ApiKeySetup.svelte';
  import ChatPanel from '$lib/components/ChatPanel.svelte';
  import WorkshopEditor from '$lib/components/WorkshopEditor.svelte';

  // Sprint 0 health dot + Sprint 1 BYOK gate + chat panel mount.

  const PROVIDER = 'bailian';

  let backendOk = $state<boolean | null>(null);
  let keyConfigured = $state<boolean | null>(null); // null = unknown, before first check

  // ① 会话(session lifecycle)：列表 + 当前活跃。后端命令见 commands.rs/③。
  let sessions = $state<SessionMeta[]>([]);
  let activeSessionId = $state('');
  let sessionError = $state<string | null>(null);

  onMount(async () => {
    // Backend health (Sprint 0 contract)
    try {
      const reply = await greet('init');
      backendOk = reply.length > 0;
    } catch {
      backendOk = false;
    }
    // BYOK gate (Sprint 1 contract)
    await refreshKeyStatus();
    if (keyConfigured) await initSession();

    // 首条消息后：该会话从空变成有内容、标题就绪 → 重拉列表（拿到标题 + 让它进列表）。
    unlistenTitle = await listen<SessionTitled>(SESSION_TITLED_EVENT, () => {
      refreshSessions();
    });

    // 思考动画：活跃会话只要有内部活动(消息/工具调用)就在左栏点亮"…"，
    // 静默 ~4s 后熄灭。纯 UI 指示，不回灌任何上下文。
    // 消息流动会改变成员的工作状态（WORK_START/DONE/ASK/ANSWER）→ 顺手重拉成员。
    unlistenMsg = await listen(MESSAGE_EVENT, () => {
      bumpThinking();
      refreshMembers();
    });
    unlistenTool = await listen(TOOL_CALL_EVENT, () => bumpThinking());
  });

  // ---- 思考动画状态 ----
  let thinkingSessionId = $state<string | null>(null);
  let thinkTimer: ReturnType<typeof setTimeout> | null = null;
  function bumpThinking() {
    if (!activeSessionId) return;
    thinkingSessionId = activeSessionId;
    if (thinkTimer) clearTimeout(thinkTimer);
    thinkTimer = setTimeout(() => {
      thinkingSessionId = null;
      thinkTimer = null;
    }, 4000);
  }

  let unlistenTitle: UnlistenFn | null = null;
  let unlistenMsg: UnlistenFn | null = null;
  let unlistenTool: UnlistenFn | null = null;
  onDestroy(() => {
    if (unlistenTitle) unlistenTitle();
    if (unlistenMsg) unlistenMsg();
    if (unlistenTool) unlistenTool();
    if (thinkTimer) clearTimeout(thinkTimer);
  });

  async function refreshKeyStatus() {
    try {
      keyConfigured = await providerStatus(PROVIDER);
    } catch {
      keyConfigured = false;
    }
  }

  async function handleKeySaved() {
    keyConfigured = true;
    await initSession();
  }

  // 启动:续接最近活跃会话(无则新建),拉历史列表 + 标出当前。
  async function initSession() {
    try {
      await startSession();
      const cur = await currentSession();
      activeSessionId = cur?.id ?? '';
      await refreshSessions();
      await refreshMembers();
    } catch (e) {
      sessionError = e instanceof Error ? e.message : String(e);
    }
  }

  async function refreshSessions() {
    try {
      sessions = await listSessions();
    } catch (e) {
      console.warn('list sessions failed:', e);
    }
  }

  // 新建一个完全空的会话。
  async function onNewSession() {
    try {
      // 当前会话还是空的（没发过消息）就别再开新的——复用它，避免堆一堆空"新会话"。
      const cur = await currentSession();
      if (cur && cur.message_count === 0) {
        activeSessionId = cur.id;
        return;
      }
      const meta = await newSession();
      activeSessionId = meta.id;
      await refreshSessions();
      await refreshMembers();
    } catch (e) {
      sessionError = e instanceof Error ? e.message : String(e);
    }
  }

  // 续接某历史会话(接着上次跑)。
  async function onSwitchSession(id: string) {
    if (id === activeSessionId) return;
    try {
      await resumeSession(id);
      activeSessionId = id;
      await refreshSessions();
      await refreshMembers();
    } catch (e) {
      sessionError = e instanceof Error ? e.message : String(e);
    }
  }

  // ---- 会话项操作菜单（... → 分享/重命名/删除）----
  let openMenuId = $state<string | null>(null);
  let menuX = $state(0);
  let menuY = $state(0);
  let renamingId = $state<string | null>(null);
  let renameValue = $state('');
  let toast = $state('');

  function openMenu(e: MouseEvent, id: string) {
    e.stopPropagation();
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    menuX = r.left;
    menuY = r.bottom + 6;
    openMenuId = id;
  }
  function closeMenu() {
    openMenuId = null;
  }
  function flashToast(msg: string) {
    toast = msg;
    setTimeout(() => (toast = ''), 2200);
  }

  // 分享：占位，功能后续实现。
  function onShareSession() {
    closeMenu();
    flashToast('分享功能即将上线');
  }

  function startRename(id: string) {
    renameValue = sessions.find((s) => s.id === id)?.title ?? '';
    renamingId = id;
    closeMenu();
  }
  function cancelRename() {
    renamingId = null;
  }
  async function commitRename(id: string) {
    const t = renameValue.trim();
    renamingId = null;
    if (!t) return;
    try {
      await renameSession(id, t);
      sessions = sessions.map((s) => (s.id === id ? { ...s, title: t } : s));
    } catch (e) {
      sessionError = e instanceof Error ? e.message : String(e);
    }
  }

  // 删除走应用内确认弹窗（Tauri webview 里 window.confirm 不可靠/是 no-op，会直接
  // 跳过删除——所以自己做确认 UI）。
  let confirmDeleteId = $state<string | null>(null);
  function onDeleteSession(id: string) {
    closeMenu();
    confirmDeleteId = id;
  }
  async function confirmDelete() {
    const id = confirmDeleteId;
    confirmDeleteId = null;
    if (!id) return;
    try {
      await deleteSession(id);
      // 后端可能已切换活跃会话（删的是当前那个）——重新同步。
      const cur = await currentSession();
      activeSessionId = cur?.id ?? '';
      await refreshSessions();
      await refreshMembers();
    } catch (e) {
      sessionError = e instanceof Error ? e.message : String(e);
    }
  }

  // 重命名输入框自动聚焦 + 选中。
  function focusSelect(node: HTMLInputElement) {
    node.focus();
    node.select();
  }

  // ---- 工作室成员（真实数据 + 实时工作状态）----
  // 成员来自后端 roles::default_workshop()；状态从当前会话历史派生（list_members）。
  // 消息流动时重拉，得到接近实时的状态。
  type MemberStatus = 'idle' | 'working' | 'waiting_answer';
  const STATUS_META: Record<MemberStatus, { label: string; color: string }> = {
    idle: { label: '就绪', color: '#22c55e' },
    working: { label: '工作中', color: '#f59e0b' },
    waiting_answer: { label: '等待回复', color: '#3b82f6' }
  };
  // id → 中文展示（UI 装饰；后端 display_name 是英文，给 LLM 用的）。未知角色回落后端名。
  const ROLE_UI: Record<string, { name: string; desc: string; glyph: string; tint: string }> = {
    PM: { name: '项目经理（PM）', desc: '统筹规划，承接需求对接', glyph: '🧭', tint: '#efeaff' },
    frontend_dev: { name: '前端工程师', desc: '负责前端开发实现', glyph: '</>', tint: '#e8f0ff' },
    backend_dev: { name: '后端工程师', desc: '负责后端服务开发', glyph: '🗄', tint: '#e6f7ee' }
  };
  function roleUi(m: MemberInfo) {
    return ROLE_UI[m.id] ?? { name: m.display_name, desc: m.description, glyph: '🤖', tint: '#eef0f4' };
  }

  let showWorkshopEditor = $state(false);

  let members = $state<MemberInfo[]>([]);
  async function refreshMembers() {
    try {
      members = await listMembers();
    } catch (e) {
      console.warn('list members failed:', e);
    }
  }

  const legend = $derived(
    (['idle', 'working', 'waiting_answer'] as MemberStatus[]).map((s) => ({
      ...STATUS_META[s],
      count: members.filter((m) => m.status === s).length
    }))
  );
  // "就绪"=空闲、可接新任务。
  const readyCount = $derived(members.filter((m) => m.status === 'idle').length);
  const readyPct = $derived(members.length ? Math.round((readyCount / members.length) * 100) : 0);
</script>

<div class="app">
  <header class="topbar">
    <div class="brand">
      <span class="logo" aria-hidden="true">
        <svg viewBox="0 0 24 24" width="18" height="18" fill="none">
          <circle cx="9" cy="11" r="1.5" fill="#fff" />
          <circle cx="15" cy="11" r="1.5" fill="#fff" />
          <path d="M8.5 14.4c1 1.1 6 1.1 7 0" stroke="#fff" stroke-width="1.6" stroke-linecap="round" />
        </svg>
      </span>
      <span class="brand-name">AiDock</span>
    </div>
    <div class="topbar-right">
      <span class="online" title={backendOk === false ? '后端未连接' : '后端在线'}>
        <i class="dot" class:ok={backendOk === true} class:bad={backendOk === false}></i> 在线
      </span>
      <button class="avatar" aria-label="账户">🤖</button>
    </div>
  </header>

  <aside class="rail">
    <nav class="rail-nav">
      <section class="rail-sec">
        <h3 class="rail-title">我的工作室</h3>
        <button class="rail-item active">
          <span class="rail-ico grad">▦</span>
          <span class="rail-label">软件开发轻量版</span>
        </button>
      </section>

      <section class="rail-sec">
        <div class="rail-title-row">
          <h3 class="rail-title">会话</h3>
          <button class="pill" onclick={onNewSession}>＋ 新建</button>
        </div>
        <ul class="rail-list">
          {#each sessions as s (s.id)}
            <li class="session-li">
              {#if renamingId === s.id}
                <input
                  class="rename-input"
                  bind:value={renameValue}
                  use:focusSelect
                  onkeydown={(e) => {
                    if (e.key === 'Enter') commitRename(s.id);
                    else if (e.key === 'Escape') cancelRename();
                  }}
                  onblur={() => commitRename(s.id)}
                />
              {:else}
                <button
                  class="rail-item session"
                  class:active={s.id === activeSessionId}
                  onclick={() => onSwitchSession(s.id)}
                  title={s.title}
                >
                  <span class="rail-ico chat">💬</span>
                  <span class="rail-label">{s.title}</span>
                  {#if s.id === thinkingSessionId}
                    <span class="thinking-dots" aria-label="思考中"><i></i><i></i><i></i></span>
                  {/if}
                </button>
                <button
                  class="dots"
                  class:open={openMenuId === s.id}
                  onclick={(e) => openMenu(e, s.id)}
                  aria-label="会话操作"
                  title="更多"
                >⋯</button>
              {/if}
            </li>
          {/each}
          {#if sessions.length === 0}
            <li class="rail-empty">（暂无会话）</li>
          {/if}
        </ul>
      </section>

      <section class="rail-sec">
        <h3 class="rail-title">市场</h3>
        <button class="rail-item">
          <span class="rail-ico market">🛒</span>
          <span class="rail-label">浏览市场</span>
        </button>
      </section>
    </nav>

    <div class="rail-foot">
      <button class="pro-card">
        <span class="pro-ico">👑</span>
        <span class="pro-text">
          <strong>专业版</strong>
          <small>高效生产力，智能更进一步</small>
        </span>
        <span class="pro-chev">›</span>
      </button>
      <div class="foot-actions">
        <button class="ghost-ico active" title="浅色" aria-label="浅色">☀</button>
        <button class="ghost-ico" title="深色" aria-label="深色">☾</button>
        <button class="ghost-ico settings" title="设置" aria-label="设置">⚙</button>
      </div>
    </div>
  </aside>

  <main class="stage">
    {#if keyConfigured === null}
      <div class="loading">正在加载…</div>
    {:else if keyConfigured}
      <ChatPanel sessionId={activeSessionId} />
    {:else}
      <div class="loading muted">请先配置 LLM Provider</div>
    {/if}
  </main>

  <aside class="panel">
    <div class="panel-head">
      <h3 class="panel-h">工作室成员 <span class="info" title="本工作室的 agent 成员">ⓘ</span></h3>
      <button class="pill subtle" onclick={() => (showWorkshopEditor = true)}>管理成员</button>
    </div>

    <ul class="member-list">
      {#each members as m (m.id)}
        {@const ui = roleUi(m)}
        <li class="member" class:primary={m.is_primary}>
          <span class="member-ico" style="background:{ui.tint}">{ui.glyph}</span>
          <span class="member-meta">
            <strong>
              {ui.name}
              {#if m.is_primary}<span class="primary-chip">对接</span>{/if}
            </strong>
            <small>{m.status === 'working' && m.status_detail ? m.status_detail : ui.desc}</small>
          </span>
          <span class="badge" style="color:{STATUS_META[m.status].color}">
            <i class="bdot" style="background:{STATUS_META[m.status].color}"></i>{STATUS_META[m.status].label}
          </span>
        </li>
      {/each}
      {#if members.length === 0}
        <li class="member-empty">（成员加载中…）</li>
      {/if}
    </ul>

    <div class="legend">
      {#each legend as l (l.label)}
        <span class="leg"><i style="background:{l.color}"></i>{l.label} {l.count}</span>
      {/each}
    </div>

    <div class="status-block">
      <h3 class="panel-h">工作室状态</h3>
      <div class="status-row">
        <div class="ring" style="--pct:{readyPct}%"><span class="ring-hole"></span></div>
        <div class="status-info">
          <strong>{readyCount}/{members.length} 成员就绪</strong>
          <small>{readyCount} 位空闲可接新任务 · {members.length - readyCount} 位忙碌</small>
          <div class="bar"><i style="width:{readyPct}%"></i></div>
        </div>
      </div>
    </div>
  </aside>
</div>

{#if keyConfigured === false}
  <ApiKeySetup onSaved={handleKeySaved} />
{/if}

{#if showWorkshopEditor}
  <WorkshopEditor onClose={() => (showWorkshopEditor = false)} onChanged={refreshMembers} />
{/if}

{#if openMenuId}
  <div class="menu-overlay" onclick={closeMenu} role="presentation"></div>
  <div class="session-menu" style="left:{menuX}px; top:{menuY}px">
    <button class="menu-item" onclick={() => onShareSession()}>
      <svg class="mi-ico" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
        <path d="M22 2 11 13" /><path d="M22 2 15 22l-4-9-9-4 20-7z" />
      </svg>
      <span class="mi-label">分享</span>
    </button>
    <button class="menu-item" onclick={() => startRename(openMenuId!)}>
      <svg class="mi-ico" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
        <path d="M12 20h9" /><path d="M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4 12.5-12.5z" />
      </svg>
      <span class="mi-label">重命名</span>
    </button>
    <div class="menu-sep"></div>
    <button class="menu-item danger" onclick={() => onDeleteSession(openMenuId!)}>
      <svg class="mi-ico" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
        <path d="M3 6h18" /><path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" /><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6" /><path d="M10 11v6M14 11v6" />
      </svg>
      <span class="mi-label">删除</span>
    </button>
  </div>
{/if}

{#if confirmDeleteId}
  <div class="confirm-overlay" onclick={() => (confirmDeleteId = null)} role="presentation"></div>
  <div class="confirm-modal">
    <h4 class="confirm-title">删除这个会话？</h4>
    <p class="confirm-text">此操作不可恢复，该会话的对话与记忆都会被删除。</p>
    <div class="confirm-actions">
      <button class="cbtn ghost" onclick={() => (confirmDeleteId = null)}>取消</button>
      <button class="cbtn danger" onclick={confirmDelete}>删除</button>
    </div>
  </div>
{/if}

{#if toast}
  <div class="toast">{toast}</div>
{/if}

<style>
  :global(:root) {
    font-family: -apple-system, BlinkMacSystemFont, 'PingFang SC', 'Hiragino Sans GB',
      'Microsoft YaHei', 'Segoe UI', sans-serif;
    font-size: 14px;
    line-height: 1.5;
    color: #1e2233;
    -webkit-font-smoothing: antialiased;
    -moz-osx-font-smoothing: grayscale;

    --bg: #f3f4f8;
    --panel: #ffffff;
    --ink: #1e2233;
    --muted: #8a8fa3;
    --faint: #aeb2c4;
    --line: #ededf2;
    --brand1: #7b5cff;
    --brand2: #5b8cff;
    --brand-ink: #6b54ec;
    --brand-soft: #f1eeff;
    --radius: 16px;
    --radius-sm: 11px;
    --shadow-sm: 0 1px 2px rgba(28, 30, 60, 0.05);
    --shadow: 0 1px 2px rgba(28, 30, 60, 0.05), 0 10px 28px rgba(28, 30, 60, 0.06);
  }

  :global(body) {
    margin: 0;
    height: 100vh;
    overflow: hidden;
    background: var(--bg);
  }

  .app {
    display: grid;
    grid-template-columns: 248px 1fr 336px;
    grid-template-rows: 64px 1fr;
    grid-template-areas:
      'topbar topbar topbar'
      'rail stage panel';
    height: 100vh;
    width: 100vw;
    background: var(--bg);
  }

  /* ---------- top bar ---------- */
  .topbar {
    grid-area: topbar;
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 22px;
    background: var(--bg);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .logo {
    width: 30px;
    height: 30px;
    border-radius: 9px;
    background: linear-gradient(135deg, var(--brand1), var(--brand2));
    display: grid;
    place-items: center;
    box-shadow: 0 4px 12px rgba(123, 92, 255, 0.35);
  }
  .brand-name {
    font-size: 18px;
    font-weight: 700;
    letter-spacing: -0.01em;
  }
  .topbar-right {
    display: flex;
    align-items: center;
    gap: 14px;
  }
  .online {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 13px;
    color: var(--muted);
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: #c7c7cc;
  }
  .dot.ok {
    background: #22c55e;
    box-shadow: 0 0 0 3px rgba(34, 197, 94, 0.16);
  }
  .dot.bad {
    background: #ff3b30;
  }
  .avatar {
    width: 34px;
    height: 34px;
    border-radius: 50%;
    border: none;
    cursor: pointer;
    font-size: 16px;
    display: grid;
    place-items: center;
    background: linear-gradient(135deg, #aab4ff, #cdb9ff);
    box-shadow: var(--shadow-sm);
  }

  /* ---------- left rail ---------- */
  .rail {
    grid-area: rail;
    display: flex;
    flex-direction: column;
    justify-content: space-between;
    padding: 6px 14px 16px;
    overflow-y: auto;
  }
  .rail-sec {
    margin-bottom: 22px;
  }
  .rail-title {
    font-size: 12px;
    font-weight: 600;
    color: var(--faint);
    margin: 0 0 8px 8px;
  }
  .rail-title-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin: 0 4px 8px 0;
  }
  .rail-title-row .rail-title {
    margin: 0 0 0 8px;
  }
  .rail-list {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .rail-item {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    padding: 9px 10px;
    border: none;
    background: transparent;
    border-radius: var(--radius-sm);
    cursor: pointer;
    font: inherit;
    font-size: 13.5px;
    color: var(--ink);
    text-align: left;
    transition: background 0.15s;
  }
  .rail-item:hover {
    background: #ebecf2;
  }
  .rail-item.active {
    background: #fff;
    box-shadow: var(--shadow-sm);
    position: relative;
  }
  .rail-item.active::before {
    content: '';
    position: absolute;
    left: 0;
    top: 9px;
    bottom: 9px;
    width: 3px;
    border-radius: 2px;
    background: linear-gradient(var(--brand1), var(--brand2));
  }
  .rail-label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .rail-ico {
    width: 28px;
    height: 28px;
    flex: none;
    border-radius: 8px;
    display: grid;
    place-items: center;
    font-size: 14px;
    color: #fff;
  }
  .rail-ico.grad {
    background: linear-gradient(135deg, var(--brand1), var(--brand2));
  }
  .rail-ico.chat {
    background: #eceaff;
  }
  .rail-ico.market {
    background: #eef0f5;
  }
  .rail-empty {
    list-style: none;
    color: var(--faint);
    font-size: 12.5px;
    padding: 6px 10px;
  }

  /* 思考动画（活跃会话内部活动时左栏点亮的"…"） */
  .thinking-dots {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    flex: none;
    margin-left: 6px;
  }
  .thinking-dots i {
    width: 4px;
    height: 4px;
    border-radius: 50%;
    background: var(--brand1);
    display: block;
    animation: think 1.2s infinite ease-in-out both;
  }
  .thinking-dots i:nth-child(2) {
    animation-delay: 0.18s;
  }
  .thinking-dots i:nth-child(3) {
    animation-delay: 0.36s;
  }
  @keyframes think {
    0%,
    80%,
    100% {
      opacity: 0.25;
      transform: translateY(0);
    }
    40% {
      opacity: 1;
      transform: translateY(-2px);
    }
  }

  /* 会话项操作 ... + 内联重命名 + 下拉菜单 */
  .session-li {
    position: relative;
  }
  .session-li .session {
    padding-right: 30px;
  }
  .dots {
    position: absolute;
    right: 6px;
    top: 50%;
    transform: translateY(-50%);
    width: 24px;
    height: 24px;
    border: none;
    background: transparent;
    border-radius: 7px;
    cursor: pointer;
    color: var(--muted);
    font-size: 17px;
    line-height: 1;
    opacity: 0;
    transition:
      opacity 0.12s,
      background 0.12s;
  }
  .session-li:hover .dots,
  .dots.open {
    opacity: 1;
  }
  .dots:hover {
    background: #e1e2ea;
  }
  .rename-input {
    width: 100%;
    box-sizing: border-box;
    border: 1px solid var(--brand1);
    border-radius: var(--radius-sm);
    padding: 8px 10px;
    font: inherit;
    font-size: 13.5px;
    outline: none;
    background: #fff;
    box-shadow: 0 0 0 3px rgba(123, 92, 255, 0.12);
  }
  .menu-overlay {
    position: fixed;
    inset: 0;
    z-index: 40;
  }
  .session-menu {
    position: fixed;
    z-index: 41;
    min-width: 244px;
    background: #fff;
    border: 1px solid #eef0f4;
    border-radius: 18px;
    box-shadow:
      0 20px 50px rgba(28, 30, 60, 0.18),
      0 2px 8px rgba(28, 30, 60, 0.06);
    padding: 8px;
  }
  .menu-item {
    display: flex;
    align-items: center;
    gap: 14px;
    width: 100%;
    border: none;
    background: transparent;
    border-radius: 12px;
    padding: 12px 14px;
    font: inherit;
    font-size: 15px;
    color: #2b2f45;
    cursor: pointer;
    text-align: left;
    transition: background 0.12s;
  }
  .menu-item:hover {
    background: var(--brand-soft);
  }
  .mi-ico {
    width: 20px;
    height: 20px;
    flex: none;
    color: var(--brand-ink);
  }
  .mi-label {
    flex: 1;
  }
  .menu-item.danger {
    color: #ef4444;
  }
  .menu-item.danger .mi-ico {
    color: #ef4444;
  }
  .menu-item.danger:hover {
    background: #fdeceb;
  }
  .menu-sep {
    height: 1px;
    background: #eef0f4;
    margin: 8px 12px;
  }
  .confirm-overlay {
    position: fixed;
    inset: 0;
    z-index: 65;
    background: rgba(20, 22, 40, 0.28);
  }
  .confirm-modal {
    position: fixed;
    z-index: 66;
    left: 50%;
    top: 50%;
    transform: translate(-50%, -50%);
    width: 320px;
    background: #fff;
    border-radius: 16px;
    box-shadow: 0 20px 60px rgba(28, 30, 60, 0.25);
    padding: 22px;
  }
  .confirm-title {
    margin: 0 0 8px;
    font-size: 16px;
    font-weight: 700;
  }
  .confirm-text {
    margin: 0 0 18px;
    font-size: 13px;
    color: var(--muted);
    line-height: 1.5;
  }
  .confirm-actions {
    display: flex;
    justify-content: flex-end;
    gap: 10px;
  }
  .cbtn {
    border: none;
    cursor: pointer;
    font: inherit;
    font-size: 13.5px;
    border-radius: 10px;
    padding: 8px 16px;
  }
  .cbtn.ghost {
    background: #f1f2f6;
    color: var(--ink);
  }
  .cbtn.ghost:hover {
    background: #e7e8ee;
  }
  .cbtn.danger {
    background: #ef4444;
    color: #fff;
  }
  .cbtn.danger:hover {
    filter: brightness(1.05);
  }
  .toast {
    position: fixed;
    bottom: 28px;
    left: 50%;
    transform: translateX(-50%);
    z-index: 60;
    background: #1e2233;
    color: #fff;
    padding: 9px 16px;
    border-radius: 10px;
    font-size: 13px;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.2);
  }

  .pill {
    border: none;
    cursor: pointer;
    font: inherit;
    font-size: 12.5px;
    font-weight: 500;
    color: var(--brand-ink);
    background: var(--brand-soft);
    border-radius: 999px;
    padding: 5px 11px;
  }
  .pill:hover {
    background: #e7e2ff;
  }
  .pill.subtle {
    color: var(--muted);
    background: #f1f2f6;
  }

  .rail-foot {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .pro-card {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    border: 1px solid #f0e7cf;
    cursor: pointer;
    text-align: left;
    background: linear-gradient(135deg, #fff8e8, #fef3f1);
    border-radius: var(--radius);
    padding: 12px;
    font: inherit;
  }
  .pro-ico {
    font-size: 18px;
  }
  .pro-text {
    display: flex;
    flex-direction: column;
    line-height: 1.3;
    flex: 1;
  }
  .pro-text strong {
    font-size: 13.5px;
  }
  .pro-text small {
    font-size: 11.5px;
    color: var(--muted);
  }
  .pro-chev {
    color: var(--faint);
    font-size: 18px;
  }
  .foot-actions {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 2px;
  }
  .ghost-ico {
    width: 34px;
    height: 34px;
    border-radius: 10px;
    border: none;
    background: transparent;
    cursor: pointer;
    font-size: 15px;
    color: var(--muted);
  }
  .ghost-ico:hover {
    background: #ebecf2;
  }
  .ghost-ico.active {
    background: #fff;
    color: var(--brand-ink);
    box-shadow: var(--shadow-sm);
  }
  .ghost-ico.settings {
    margin-left: auto;
  }

  /* ---------- center stage ---------- */
  .stage {
    grid-area: stage;
    margin: 0 6px 14px;
    border-radius: 22px;
    background:
      radial-gradient(120% 80% at 50% -10%, rgba(123, 92, 255, 0.07), transparent 60%),
      var(--panel);
    box-shadow: var(--shadow);
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  .loading {
    margin: auto;
    color: var(--muted);
  }
  .loading.muted {
    opacity: 0.6;
  }

  /* ---------- right panel ---------- */
  .panel {
    grid-area: panel;
    padding: 6px 18px 16px;
    overflow-y: auto;
  }
  .panel-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 12px;
  }
  .panel-h {
    font-size: 14px;
    font-weight: 700;
    margin: 0;
  }
  .info {
    color: var(--faint);
    font-size: 12px;
    font-weight: 400;
  }

  .member-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .member {
    display: flex;
    align-items: center;
    gap: 11px;
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    padding: 11px 12px;
    box-shadow: var(--shadow-sm);
  }
  .member-ico {
    width: 38px;
    height: 38px;
    flex: none;
    border-radius: 11px;
    display: grid;
    place-items: center;
    font-size: 15px;
  }
  .member-meta {
    display: flex;
    flex-direction: column;
    line-height: 1.35;
    flex: 1;
    min-width: 0;
  }
  .member-meta strong {
    font-size: 13px;
    font-weight: 600;
  }
  .member-meta small {
    font-size: 11.5px;
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .badge {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    flex: none;
    font-size: 12px;
    font-weight: 500;
  }
  .bdot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
  }
  /* 主理/对接成员（PM）——进工作室后承接用户对话的激活角色，视觉上点出来。 */
  .member.primary {
    background: var(--brand-soft);
    border-color: #d9d2ff;
  }
  .member.primary .member-ico {
    box-shadow:
      0 0 0 2px #fff,
      0 0 0 4px var(--brand1);
  }
  .primary-chip {
    display: inline-block;
    margin-left: 6px;
    padding: 1px 7px;
    border-radius: 999px;
    font-size: 10.5px;
    font-weight: 600;
    color: #fff;
    background: linear-gradient(135deg, var(--brand1), var(--brand2));
    vertical-align: middle;
  }
  .member-empty {
    list-style: none;
    color: var(--faint);
    font-size: 12.5px;
    padding: 8px 4px;
  }

  .legend {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 14px;
    padding: 14px 4px 4px;
    font-size: 12px;
    color: var(--muted);
  }
  .leg {
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }
  .leg i {
    width: 7px;
    height: 7px;
    border-radius: 50%;
  }

  .status-block {
    margin-top: 18px;
    padding-top: 18px;
    border-top: 1px solid var(--line);
  }
  .status-row {
    display: flex;
    align-items: center;
    gap: 14px;
    margin-top: 12px;
  }
  .ring {
    width: 58px;
    height: 58px;
    flex: none;
    border-radius: 50%;
    background: conic-gradient(var(--brand1) 0 var(--pct), #e9ebf2 var(--pct) 100%);
    display: grid;
    place-items: center;
  }
  .ring-hole {
    width: 44px;
    height: 44px;
    border-radius: 50%;
    background: var(--bg);
  }
  .status-info {
    display: flex;
    flex-direction: column;
    gap: 3px;
    flex: 1;
  }
  .status-info strong {
    font-size: 13.5px;
  }
  .status-info small {
    font-size: 11.5px;
    color: var(--muted);
  }
  .bar {
    height: 6px;
    border-radius: 999px;
    background: #ebedf3;
    margin-top: 4px;
    overflow: hidden;
  }
  .bar i {
    display: block;
    height: 100%;
    border-radius: 999px;
    background: linear-gradient(90deg, var(--brand1), var(--brand2));
  }
</style>
