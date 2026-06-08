<script lang="ts">
  import { onMount } from 'svelte';
  import {
    listWorkshops,
    currentWorkshop,
    createWorkshop,
    enterWorkshop,
    deleteWorkshop,
    saveWorkshopSettings,
    pickDirectory,
    type WorkshopMeta
  } from '$lib/ipc';

  // 进入某工作室 → 父组件切到聊天视图。
  let { onEnter }: { onEnter: (id: string) => void } = $props();

  let workshops = $state<WorkshopMeta[]>([]);
  let activeId = $state('');
  let err = $state('');

  // 新建表单。
  let creating = $state(false);
  let newName = $state('');
  let newIcon = $state('🛠');

  // 设置弹窗（编辑某工作室的 name/icon/workspace）。
  let settingsFor = $state<WorkshopMeta | null>(null);
  let sName = $state('');
  let sIcon = $state('');
  let sWorkspace = $state('');
  let savingSettings = $state(false);

  let pendingDelete = $state<string | null>(null);

  onMount(refresh);
  async function refresh() {
    try {
      [workshops, activeId] = await Promise.all([listWorkshops(), currentWorkshop()]);
    } catch (e) {
      err = e instanceof Error ? e.message : String(e);
    }
  }

  // 点卡片即进入：已设工作区间 → 直接进；未设 → 弹窗（选信任目录 / 用默认目录）。
  let entryFor = $state<WorkshopMeta | null>(null);
  function onCardClick(w: WorkshopMeta) {
    if (w.workspace_path.trim()) doEnter(w.id);
    else entryFor = w;
  }
  async function doEnter(id: string) {
    try {
      await enterWorkshop(id);
      onEnter(id);
    } catch (e) {
      err = e instanceof Error ? e.message : String(e);
    }
  }
  // 弹窗：选一个可信目录，存为工作区间后进入。
  async function pickTrustedAndEnter(w: WorkshopMeta) {
    try {
      const dir = await pickDirectory();
      if (!dir) return; // 取消
      await saveWorkshopSettings(w.id, w.name, w.icon, dir);
      entryFor = null;
      await refresh();
      await doEnter(w.id);
    } catch (e) {
      err = e instanceof Error ? e.message : String(e);
    }
  }
  async function useDefaultAndEnter(w: WorkshopMeta) {
    entryFor = null;
    await doEnter(w.id); // 空工作区间 → 后端回落默认 {workshop}/workspace
  }
  // 原生目录选择器（设置弹窗用）。
  async function pickSettingsDir() {
    const dir = await pickDirectory();
    if (dir) sWorkspace = dir;
  }

  async function doCreate() {
    const name = newName.trim();
    if (!name) {
      creating = false;
      return;
    }
    try {
      await createWorkshop(name, newIcon.trim() || '🛠');
      newName = '';
      newIcon = '🛠';
      creating = false;
      await refresh();
    } catch (e) {
      err = e instanceof Error ? e.message : String(e);
    }
  }

  function openSettings(w: WorkshopMeta) {
    settingsFor = w;
    sName = w.name;
    sIcon = w.icon;
    sWorkspace = w.workspace_path;
  }
  async function saveSettings() {
    if (!settingsFor) return;
    savingSettings = true;
    err = '';
    try {
      await saveWorkshopSettings(settingsFor.id, sName, sIcon, sWorkspace);
      settingsFor = null;
      await refresh();
    } catch (e) {
      err = e instanceof Error ? e.message : String(e);
    } finally {
      savingSettings = false;
    }
  }

  async function doDelete(id: string) {
    if (pendingDelete !== id) {
      pendingDelete = id;
      setTimeout(() => {
        if (pendingDelete === id) pendingDelete = null;
      }, 2500);
      return;
    }
    pendingDelete = null;
    try {
      await deleteWorkshop(id);
      await refresh();
    } catch (e) {
      err = e instanceof Error ? e.message : String(e);
    }
  }
</script>

<div class="wb">
  <header class="wb-top">
    <div class="brand">
      <span class="logo">AiDock</span>
      <span class="tag">工作台</span>
    </div>
    <button class="primary" onclick={() => (creating = !creating)}>＋ 新建工作室</button>
  </header>

  {#if err}<div class="err">{err}</div>{/if}

  {#if creating}
    <div class="create">
      <input class="ic" bind:value={newIcon} maxlength="2" aria-label="图标" />
      <input
        class="nm"
        bind:value={newName}
        placeholder="工作室名称，如「Coding 工作室」"
        onkeydown={(e) => e.key === 'Enter' && doCreate()}
      />
      <button class="primary" onclick={doCreate}>创建</button>
      <button class="ghost" onclick={() => (creating = false)}>取消</button>
    </div>
  {/if}

  <h2 class="h">我的工作室</h2>
  <div class="wall">
    {#each workshops as w (w.id)}
      <div
        class="card"
        class:active={w.id === activeId}
        role="button"
        tabindex="0"
        onclick={() => onCardClick(w)}
        onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && onCardClick(w)}
      >
        <div class="ch">
          <span class="cico">{w.icon || '🧭'}</span>
          <span class="ch-r">
            {#if w.id === activeId}<span class="cur">当前</span>{/if}
            <button class="ic-btn" title="设置" onclick={(e) => { e.stopPropagation(); openSettings(w); }}>⚙</button>
            <button
              class="ic-btn danger"
              class:armed={pendingDelete === w.id}
              title="删除"
              onclick={(e) => { e.stopPropagation(); doDelete(w.id); }}
            >{pendingDelete === w.id ? '确认?' : '🗑'}</button>
          </span>
        </div>
        <strong class="cname">{w.name}</strong>
        <small class="cmeta">{w.member_count} 名成员 · 点击进入</small>
        {#if w.workspace_path}
          <small class="cws" title={w.workspace_path}>📁 {w.workspace_path}</small>
        {:else}
          <small class="cws muted">未设工作区间（进入时选）</small>
        {/if}
      </div>
    {/each}
  </div>
  <p class="note">编成员（角色）= 进入工作室后点右栏「管理成员」。</p>
</div>

{#if settingsFor}
  <div class="ov" onclick={() => (settingsFor = null)} role="presentation"></div>
  <div class="modal" role="dialog" aria-modal="true">
    <h3>工作室设置</h3>
    <div class="frow">
      <label class="f ico-f"><span>图标</span><input bind:value={sIcon} maxlength="2" /></label>
      <label class="f"><span>名称</span><input bind:value={sName} /></label>
    </div>
    <div class="f">
      <span>工作区间（④.d 可信目录；改/删文件不得越界）</span>
      <div class="ws-pick">
        <code class="ws-cur">{sWorkspace || '默认: ' + settingsFor.default_workspace}</code>
        <button class="ghost sm2" onclick={pickSettingsDir}>选择目录…</button>
        {#if sWorkspace}<button class="ghost sm2" onclick={() => (sWorkspace = '')}>用默认</button>{/if}
      </div>
    </div>
    <p class="hint">选一个可信目录；留空则用默认 <code>{settingsFor.default_workspace}</code>（全室共享）。</p>
    <div class="mact">
      <button class="ghost" onclick={() => (settingsFor = null)} disabled={savingSettings}>取消</button>
      <button class="primary" onclick={saveSettings} disabled={savingSettings}>{savingSettings ? '保存中…' : '保存'}</button>
    </div>
  </div>
{/if}

{#if entryFor}
  <div class="ov" onclick={() => (entryFor = null)} role="presentation"></div>
  <div class="modal" role="dialog" aria-modal="true">
    <h3>进入「{entryFor.name}」</h3>
    <p class="hint">这个工作室还没设工作区间（agent 改/删文件的可信目录）。选一个可信目录，或先用默认。</p>
    <div class="f"><span>默认目录</span><code class="ws-cur">{entryFor.default_workspace}</code></div>
    <div class="mact">
      <button class="ghost" onclick={() => (entryFor = null)}>取消</button>
      <button class="ghost" onclick={() => useDefaultAndEnter(entryFor!)}>用默认目录</button>
      <button class="primary" onclick={() => pickTrustedAndEnter(entryFor!)}>选择信任目录…</button>
    </div>
  </div>
{/if}

<style>
  .wb {
    min-height: 100vh;
    background:
      radial-gradient(1200px 600px at 80% -10%, #efeaff 0%, transparent 55%),
      radial-gradient(900px 500px at -10% 110%, #e8f0ff 0%, transparent 55%), #fafbfe;
    padding: 28px 40px 60px;
    box-sizing: border-box;
  }
  .wb-top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 24px;
  }
  .brand {
    display: flex;
    align-items: baseline;
    gap: 10px;
  }
  .logo {
    font-size: 22px;
    font-weight: 800;
    background: linear-gradient(135deg, #7b5cff, #5b8cff);
    -webkit-background-clip: text;
    background-clip: text;
    color: transparent;
  }
  .tag {
    color: #8a8fa3;
    font-size: 14px;
  }
  .err {
    background: #fdeceb;
    color: #ef4444;
    padding: 8px 12px;
    border-radius: 9px;
    font-size: 13px;
    margin-bottom: 14px;
  }
  .create {
    display: flex;
    gap: 8px;
    margin-bottom: 18px;
    align-items: center;
  }
  .create .ic {
    width: 46px;
    text-align: center;
  }
  .create .nm {
    flex: 1;
    max-width: 420px;
  }
  .create input {
    border: 1px solid #e2e4ec;
    border-radius: 10px;
    padding: 9px 12px;
    font: inherit;
    font-size: 14px;
    outline: none;
  }
  .create input:focus {
    border-color: #7b5cff;
  }
  .h {
    font-size: 15px;
    color: #555b73;
    margin: 0 0 14px;
  }
  .wall {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(240px, 1fr));
    gap: 16px;
    max-width: 1100px;
  }
  .card {
    background: #fff;
    border: 1px solid #eef0f4;
    border-radius: 16px;
    padding: 16px;
    box-shadow: 0 2px 10px rgba(28, 30, 60, 0.05);
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .card.active {
    border-color: #c9bdff;
    box-shadow: 0 6px 22px rgba(123, 92, 255, 0.14);
  }
  .ch {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .cico {
    font-size: 26px;
  }
  .cur {
    font-size: 11px;
    color: #6b54ec;
    background: #f1eeff;
    padding: 2px 8px;
    border-radius: 999px;
    font-weight: 600;
  }
  .cname {
    font-size: 15.5px;
    margin-top: 6px;
  }
  .cmeta {
    color: #8a8fa3;
    font-size: 12.5px;
  }
  .cws {
    color: #8a8fa3;
    font-size: 11.5px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .cws.muted {
    color: #b6bacb;
  }
  .card {
    cursor: pointer;
    transition:
      box-shadow 0.12s,
      transform 0.08s;
  }
  .card:hover {
    box-shadow: 0 8px 24px rgba(28, 30, 60, 0.12);
    transform: translateY(-1px);
  }
  .ch-r {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .ic-btn {
    border: none;
    background: #f1f2f6;
    border-radius: 8px;
    width: 28px;
    height: 26px;
    cursor: pointer;
    font-size: 13px;
    color: #555b73;
    padding: 0;
  }
  .ic-btn:hover {
    background: #e7e8ee;
  }
  .ic-btn.danger {
    color: #ef4444;
  }
  .ic-btn.danger.armed {
    background: #ef4444;
    color: #fff;
    width: auto;
    padding: 0 8px;
  }
  .ws-pick {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .ws-cur {
    flex: 1;
    min-width: 140px;
    background: #f5f6fa;
    border-radius: 8px;
    padding: 7px 10px;
    font-size: 12px;
    color: #555b73;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .sm2 {
    padding: 6px 12px;
    font-size: 12.5px;
    flex: none;
  }
  button {
    border: none;
    cursor: pointer;
    font: inherit;
    border-radius: 9px;
  }
  .primary {
    background: linear-gradient(135deg, #7b5cff, #5b8cff);
    color: #fff;
    padding: 8px 16px;
    font-size: 13.5px;
  }
  .ghost {
    background: #f1f2f6;
    color: #2b2f45;
    padding: 8px 14px;
    font-size: 13.5px;
  }
  button:disabled {
    opacity: 0.6;
    cursor: default;
  }
  .note {
    color: #8a8fa3;
    font-size: 12.5px;
    margin-top: 22px;
  }

  .ov {
    position: fixed;
    inset: 0;
    z-index: 80;
    background: rgba(20, 22, 40, 0.32);
  }
  .modal {
    position: fixed;
    z-index: 81;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    width: min(520px, 92vw);
    background: #fff;
    border-radius: 16px;
    box-shadow: 0 24px 64px rgba(28, 30, 60, 0.28);
    padding: 22px;
  }
  .modal h3 {
    margin: 0 0 14px;
    font-size: 16px;
  }
  .frow {
    display: flex;
    gap: 12px;
  }
  .ico-f {
    flex: 0 0 70px;
  }
  .f {
    display: flex;
    flex-direction: column;
    gap: 5px;
    flex: 1;
    margin-bottom: 12px;
  }
  .f > span {
    font-size: 12.5px;
    color: #555b73;
  }
  .f input {
    border: 1px solid #e2e4ec;
    border-radius: 9px;
    padding: 8px 10px;
    font: inherit;
    font-size: 13.5px;
    outline: none;
  }
  .f input:focus {
    border-color: #7b5cff;
  }
  .hint {
    color: #8a8fa3;
    font-size: 12px;
    margin: -4px 0 14px;
  }
  .mact {
    display: flex;
    justify-content: flex-end;
    gap: 10px;
  }
</style>
