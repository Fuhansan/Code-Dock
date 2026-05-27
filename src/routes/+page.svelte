<script lang="ts">
  import { onMount } from 'svelte';
  import { greet, providerStatus } from '$lib/ipc';
  import ApiKeySetup from '$lib/components/ApiKeySetup.svelte';
  import ChatPanel from '$lib/components/ChatPanel.svelte';

  // Sprint 0 health dot + Sprint 1 BYOK gate + chat panel mount.

  const PROVIDER = 'bailian';

  let backendOk = $state<boolean | null>(null);
  let keyConfigured = $state<boolean | null>(null); // null = unknown, before first check

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
  });

  async function refreshKeyStatus() {
    try {
      keyConfigured = await providerStatus(PROVIDER);
    } catch {
      keyConfigured = false;
    }
  }

  function handleKeySaved() {
    keyConfigured = true;
  }
</script>

<div class="app">
  <header class="header">
    <h1 class="brand">AiDock</h1>
    <div class="header-actions">
      <span
        class="health-dot"
        class:ok={backendOk === true}
        class:bad={backendOk === false}
        title={backendOk === null
          ? 'Backend: connecting...'
          : backendOk
            ? 'Backend: OK'
            : 'Backend: unreachable'}
      ></span>
      <button class="icon-btn" title="Settings" aria-label="Settings">⚙</button>
    </div>
  </header>

  <aside class="left">
    <nav>
      <section class="nav-section">
        <h3 class="nav-title">我的工作室</h3>
        <ul class="nav-list">
          <li class="nav-item active">软件开发轻量版</li>
        </ul>
      </section>

      <section class="nav-section">
        <h3 class="nav-title">会话</h3>
        <ul class="nav-list muted">
          <li class="nav-item">（首版单会话）</li>
        </ul>
      </section>

      <section class="nav-section">
        <h3 class="nav-title">市场</h3>
        <ul class="nav-list muted">
          <li class="nav-item">浏览市场</li>
        </ul>
      </section>
    </nav>
  </aside>

  <main class="center">
    {#if keyConfigured === null}
      <div class="loading">正在加载…</div>
    {:else if keyConfigured}
      <ChatPanel />
    {:else}
      <div class="loading muted">请先配置 LLM Provider</div>
    {/if}
  </main>

  <aside class="right">
    <section class="panel-section">
      <h3 class="panel-title">阶段进度</h3>
      <p class="placeholder small">Sprint 2 接入</p>
    </section>

    <section class="panel-section">
      <h3 class="panel-title">工件</h3>
      <p class="placeholder small">Sprint 1+ 接入</p>
    </section>

    <section class="panel-section">
      <h3 class="panel-title">成员状态</h3>
      <p class="placeholder small">Sprint 2 接入</p>
    </section>
  </aside>
</div>

{#if keyConfigured === false}
  <ApiKeySetup onSaved={handleKeySaved} />
{/if}

<style>
  :global(:root) {
    font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Inter, sans-serif;
    font-size: 14px;
    line-height: 1.5;
    color: #1c1c1e;
    background-color: #fafafa;
    -webkit-font-smoothing: antialiased;
    -moz-osx-font-smoothing: grayscale;
  }

  :global(body) {
    margin: 0;
    height: 100vh;
    overflow: hidden;
  }

  .app {
    display: grid;
    grid-template-columns: 240px 1fr 320px;
    grid-template-rows: 48px 1fr;
    grid-template-areas:
      'header header header'
      'left center right';
    height: 100vh;
    width: 100vw;
  }

  .header {
    grid-area: header;
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 16px;
    border-bottom: 1px solid #e5e5e7;
    background-color: #ffffff;
  }

  .brand {
    font-size: 16px;
    font-weight: 600;
    margin: 0;
    letter-spacing: -0.01em;
  }

  .header-actions {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .health-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #c7c7cc;
    transition: background 0.3s;
  }
  .health-dot.ok {
    background: #34c759;
    box-shadow: 0 0 0 2px rgba(52, 199, 89, 0.18);
  }
  .health-dot.bad {
    background: #ff3b30;
    box-shadow: 0 0 0 2px rgba(255, 59, 48, 0.18);
  }

  .icon-btn {
    background: transparent;
    border: 1px solid transparent;
    border-radius: 6px;
    width: 32px;
    height: 32px;
    cursor: pointer;
    font-size: 16px;
    color: #6e6e73;
  }
  .icon-btn:hover {
    background: #f0f0f0;
  }

  .left {
    grid-area: left;
    border-right: 1px solid #e5e5e7;
    background: #f5f5f7;
    overflow-y: auto;
    padding: 16px 0;
  }

  .nav-section {
    margin-bottom: 24px;
  }

  .nav-title {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: #6e6e73;
    margin: 0 0 6px 0;
    padding: 0 16px;
  }

  .nav-list {
    list-style: none;
    padding: 0;
    margin: 0;
  }

  .nav-list.muted {
    opacity: 0.55;
  }

  .nav-item {
    padding: 6px 16px;
    font-size: 13px;
    cursor: pointer;
    border-left: 2px solid transparent;
  }
  .nav-item:hover {
    background: #ebebed;
  }
  .nav-item.active {
    background: #e8e8eb;
    border-left-color: #007aff;
    font-weight: 500;
  }

  .center {
    grid-area: center;
    display: flex;
    flex-direction: column;
    background: #ffffff;
    overflow: hidden;
  }

  .loading {
    margin: auto;
    color: #8e8e93;
    font-size: 14px;
  }
  .loading.muted {
    opacity: 0.6;
  }

  .right {
    grid-area: right;
    border-left: 1px solid #e5e5e7;
    background: #f5f5f7;
    overflow-y: auto;
    padding: 16px;
  }

  .panel-section {
    margin-bottom: 20px;
  }

  .panel-title {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: #6e6e73;
    margin: 0 0 8px 0;
  }

  .placeholder {
    color: #999;
    font-size: 13px;
    margin: 0;
    text-align: center;
  }

  .placeholder.small {
    font-size: 12px;
    text-align: left;
  }

  @media (prefers-color-scheme: dark) {
    :global(:root) {
      color: #f5f5f7;
      background-color: #1c1c1e;
    }
    .header {
      background-color: #2c2c2e;
      border-bottom-color: #38383a;
    }
    .left,
    .right {
      background: #1c1c1e;
      border-color: #38383a;
    }
    .center {
      background: #2c2c2e;
    }
    .icon-btn {
      color: #98989d;
    }
    .icon-btn:hover {
      background: #38383a;
    }
    .nav-item:hover {
      background: #2c2c2e;
    }
    .nav-item.active {
      background: #2c2c2e;
    }
  }
</style>
