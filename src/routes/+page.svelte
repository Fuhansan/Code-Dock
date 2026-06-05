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
    SESSION_TITLED_EVENT,
    type SessionMeta,
    type SessionTitled
  } from '$lib/ipc';
  import ApiKeySetup from '$lib/components/ApiKeySetup.svelte';
  import ChatPanel from '$lib/components/ChatPanel.svelte';

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

    // 会话标题就绪(首条消息后 LLM 生成) → 刷新列表显示新标题。
    unlistenTitle = await listen<SessionTitled>(SESSION_TITLED_EVENT, (e) => {
      const { id, title } = e.payload;
      sessions = sessions.map((s) => (s.id === id ? { ...s, title } : s));
    });
  });

  let unlistenTitle: UnlistenFn | null = null;
  onDestroy(() => {
    if (unlistenTitle) unlistenTitle();
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
    } catch (e) {
      sessionError = e instanceof Error ? e.message : String(e);
    }
  }

  // 工作室成员面板。V0.1 静态：6 个角色原型——本工作室在用的 3 个(PM/前端/后端)=
  // 就绪/忙碌，其余=离线/休息。TODO：接后端实时状态(AgentState) + 动态工作室成员。
  type MemberStatus = 'ready' | 'busy' | 'rest' | 'offline';
  const STATUS_META: Record<MemberStatus, { label: string; color: string }> = {
    ready: { label: '就绪', color: '#22c55e' },
    busy: { label: '忙碌', color: '#f59e0b' },
    rest: { label: '休息中', color: '#3b82f6' },
    offline: { label: '离线', color: '#9aa0b4' }
  };
  const members: { name: string; desc: string; glyph: string; tint: string; status: MemberStatus }[] = [
    { name: '项目经理（PM）', desc: '统筹规划，分解任务', glyph: '🧭', tint: '#efeaff', status: 'ready' },
    { name: '前端工程师', desc: '负责前端开发实现', glyph: '</>', tint: '#e8f0ff', status: 'ready' },
    { name: '后端工程师', desc: '负责后端服务开发', glyph: '🗄', tint: '#e6f7ee', status: 'busy' },
    { name: '测试工程师', desc: '负责测试与质量保障', glyph: '🧪', tint: '#fff1e0', status: 'offline' },
    { name: 'UI/UX 设计师', desc: '负责界面与用户体验', glyph: '🎨', tint: '#f1eaff', status: 'rest' },
    { name: '文档工程师', desc: '负责文档编写与维护', glyph: '📄', tint: '#fff6da', status: 'offline' }
  ];
  const legend = $derived(
    (['ready', 'busy', 'rest', 'offline'] as MemberStatus[]).map((s) => ({
      ...STATUS_META[s],
      count: members.filter((m) => m.status === s).length
    }))
  );
  const availableCount = $derived(members.filter((m) => m.status === 'ready' || m.status === 'busy').length);
  const availablePct = $derived(Math.round((availableCount / members.length) * 100));
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
            <li>
              <button
                class="rail-item session"
                class:active={s.id === activeSessionId}
                onclick={() => onSwitchSession(s.id)}
                title={s.title}
              >
                <span class="rail-ico chat">💬</span>
                <span class="rail-label">{s.title}</span>
              </button>
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
      <button class="pill subtle">管理成员</button>
    </div>

    <ul class="member-list">
      {#each members as m (m.name)}
        <li class="member">
          <span class="member-ico" style="background:{m.tint}">{m.glyph}</span>
          <span class="member-meta">
            <strong>{m.name}</strong>
            <small>{m.desc}</small>
          </span>
          <span class="badge" style="color:{STATUS_META[m.status].color}">
            <i class="bdot" style="background:{STATUS_META[m.status].color}"></i>{STATUS_META[m.status].label}
          </span>
        </li>
      {/each}
    </ul>

    <div class="legend">
      {#each legend as l (l.label)}
        <span class="leg"><i style="background:{l.color}"></i>{l.label} {l.count}</span>
      {/each}
    </div>

    <div class="status-block">
      <h3 class="panel-h">工作室状态</h3>
      <div class="status-row">
        <div class="ring" style="--pct:{availablePct}%"><span class="ring-hole"></span></div>
        <div class="status-info">
          <strong>{availableCount}/{members.length} 成员可用</strong>
          <small>当前有 {availableCount} 位成员可响应任务</small>
          <div class="bar"><i style="width:{availablePct}%"></i></div>
        </div>
      </div>
    </div>
  </aside>
</div>

{#if keyConfigured === false}
  <ApiKeySetup onSaved={handleKeySaved} />
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
