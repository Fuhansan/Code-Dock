<script lang="ts">
  import { onMount } from 'svelte';
  import {
    getWorkshop,
    saveRole,
    deleteRole,
    availableTools,
    modelCatalog,
    type WorkshopDef,
    type RoleConfig,
    type ToolCatalogEntry
  } from '$lib/ipc';

  // 关闭 + 保存后回调（父组件据此刷新成员面板）。
  let { onClose, onChanged }: { onClose: () => void; onChanged?: () => void } = $props();

  let ws = $state<WorkshopDef | null>(null);
  let tools = $state<ToolCatalogEntry[]>([]);
  let models = $state<string[]>([]);
  let loadErr = $state('');

  // editing = 正在编辑的角色草稿（null = 列表视图）。isNew 决定 id 是否可改。
  let editing = $state<RoleConfig | null>(null);
  let isNew = $state(false);
  let showAdvanced = $state(false);
  let saving = $state(false);
  let formErr = $state('');

  onMount(load);
  async function load() {
    try {
      [ws, tools, models] = await Promise.all([getWorkshop(), availableTools(), modelCatalog()]);
    } catch (e) {
      loadErr = e instanceof Error ? e.message : String(e);
    }
  }

  const GROUP_ORDER = ['文件', '终端', '导航', '记忆', '协作'];
  const groupedTools = $derived(
    GROUP_ORDER.map((g) => ({ group: g, items: tools.filter((t) => t.group === g) })).filter(
      (x) => x.items.length > 0
    )
  );

  const SEC_LABEL: Record<string, string> = { strict: '严格', standard: '标准', permissive: '宽松' };

  function blankRole(): RoleConfig {
    return {
      id: '',
      display_name: '',
      description: '',
      is_coordinator: false,
      persona: '',
      model: {
        provider: 'bailian',
        primary: models[0] ?? 'qwen3.6-plus',
        fallback: null,
        temperature: 0.3,
        max_tokens: 4096,
        extended_thinking: false
      },
      budget: { per_call_tokens: 8000, per_session_tokens: 400000, per_session_calls: 200 },
      tools: [],
      teammates: [],
      loop_mode: 'single',
      max_history_tokens: 32000,
      security_level: 'standard',
      permission_rules: []
    };
  }

  function openEdit(role: RoleConfig) {
    editing = structuredClone($state.snapshot(role)) as RoleConfig;
    isNew = false;
    showAdvanced = false;
    formErr = '';
  }
  function openAdd() {
    editing = blankRole();
    isNew = true;
    showAdvanced = false;
    formErr = '';
  }
  function cancelEdit() {
    editing = null;
  }

  function toggleTool(name: string) {
    if (!editing) return;
    editing.tools = editing.tools.includes(name)
      ? editing.tools.filter((t) => t !== name)
      : [...editing.tools, name];
  }
  function toggleTeammate(id: string) {
    if (!editing) return;
    editing.teammates = editing.teammates.includes(id)
      ? editing.teammates.filter((t) => t !== id)
      : [...editing.teammates, id];
  }

  async function commit() {
    if (!editing) return;
    const r = editing;
    if (!r.display_name.trim()) {
      formErr = '请填写名称';
      return;
    }
    r.display_name = r.display_name.trim();
    // id：新角色留空 → 后端按名称自动生成；老角色保持原 id。
    saving = true;
    formErr = '';
    try {
      await saveRole(r);
      await load(); // 重读工作室（拿到规整后的协调者唯一性等）
      editing = null;
      onChanged?.();
    } catch (e) {
      formErr = e instanceof Error ? e.message : String(e);
    } finally {
      saving = false;
    }
  }

  async function removeRole(role: RoleConfig) {
    if (!confirmDelete(role)) return;
    try {
      await deleteRole(role.id);
      await load();
      onChanged?.();
    } catch (e) {
      loadErr = e instanceof Error ? e.message : String(e);
    }
  }
  // 用内联确认（Tauri 的 window.confirm 不可靠）。
  let pendingDelete = $state<string | null>(null);
  function confirmDelete(role: RoleConfig): boolean {
    // 两步：第一次点设置 pending，再点同一个才真删。
    if (pendingDelete === role.id) {
      pendingDelete = null;
      return true;
    }
    pendingDelete = role.id;
    setTimeout(() => {
      if (pendingDelete === role.id) pendingDelete = null;
    }, 2500);
    return false;
  }

  const otherRoles = $derived(ws && editing ? ws.roles.filter((r) => r.id !== editing!.id) : []);
</script>

<div class="ov" onclick={onClose} role="presentation"></div>
<div class="panel" role="dialog" aria-modal="true">
  <header class="hd">
    <h2>{editing ? (isNew ? '新增角色' : `编辑角色 · ${editing.display_name || editing.id}`) : '管理成员'}</h2>
    <button class="x" onclick={onClose} aria-label="关闭">✕</button>
  </header>

  {#if loadErr}
    <div class="err">{loadErr}</div>
  {/if}

  {#if !editing}
    <!-- ===== 列表视图 ===== -->
    {#if ws}
      <div class="ws-line">
        <span class="ws-ico">{ws.icon || '🧭'}</span>
        <strong>{ws.name}</strong>
        <span class="ws-sub">{ws.roles.length} 名成员</span>
      </div>
      <ul class="rl">
        {#each ws.roles as r (r.id)}
          <li class="ri">
            <span class="ri-meta">
              <strong>
                {r.display_name || r.id}
                {#if r.is_coordinator}<span class="chip">对接</span>{/if}
              </strong>
              <small>{r.description || r.id}</small>
            </span>
            <button class="btn ghost" onclick={() => openEdit(r)}>编辑</button>
            <button class="btn danger" class:armed={pendingDelete === r.id} onclick={() => removeRole(r)}>
              {pendingDelete === r.id ? '确认删除' : '删除'}
            </button>
          </li>
        {/each}
      </ul>
      <button class="btn add" onclick={openAdd}>＋ 添加角色</button>
      <p class="note">保存/删除会<strong>立即生效</strong>——正在跑的会话会用新配置重建（丢中途半圈，同续接）。</p>
    {:else}
      <div class="loading">加载中…</div>
    {/if}
  {:else}
    <!-- ===== 角色编辑表单 ===== -->
    <div class="form">
      {#if formErr}<div class="err">{formErr}</div>{/if}

      <h3 class="sec">基础</h3>
      <label class="f"><span>名称{#if isNew}（id 由系统生成）{/if}</span>
        <input bind:value={editing.display_name} placeholder="如 测试工程师 / Designer" /></label>
      <label class="f"><span>描述</span><input bind:value={editing.description} /></label>
      <label class="ck"><input type="checkbox" bind:checked={editing.is_coordinator} />
        设为协调者（承接用户对话，全室唯一——勾它会自动取消别人）</label>

      <label class="f col"><span>人设 Prompt（职责/风格；协作协议由系统注入，不在此编辑）</span>
        <textarea rows="8" bind:value={editing.persona}></textarea></label>

      <div class="row">
        <label class="f"><span>模型</span>
          <select bind:value={editing.model.primary}>
            {#each models as m (m)}<option value={m}>{m}</option>{/each}
          </select></label>
        <label class="f"><span>温度 {editing.model.temperature.toFixed(1)}</span>
          <input type="range" min="0" max="1" step="0.1" bind:value={editing.model.temperature} /></label>
        <label class="f"><span>max_tokens</span>
          <input type="number" bind:value={editing.model.max_tokens} /></label>
      </div>

      <div class="f col"><span>工具（能力）</span>
        {#each groupedTools as grp (grp.group)}
          <div class="tgrp">
            <em>{grp.group}</em>
            {#each grp.items as t (t.name)}
              <label class="tk" title={t.description}>
                {#if t.always_on}
                  <input type="checkbox" checked disabled /> {t.name} <small>始终开</small>
                {:else}
                  <input type="checkbox" checked={editing.tools.includes(t.name)} onchange={() => toggleTool(t.name)} /> {t.name}
                {/if}
              </label>
            {/each}
          </div>
        {/each}
      </div>

      <div class="f col"><span>安全级别</span>
        <div class="radios">
          {#each ['strict', 'standard', 'permissive'] as lv (lv)}
            <label class="rd"><input type="radio" value={lv} bind:group={editing.security_level} />{SEC_LABEL[lv]}</label>
          {/each}
        </div>
      </div>

      <button class="adv-toggle" onclick={() => (showAdvanced = !showAdvanced)}>
        {showAdvanced ? '▾' : '▸'} 高级
      </button>
      {#if showAdvanced}
        <div class="adv">
          <div class="row">
            <label class="f"><span>per_call_tokens</span><input type="number" bind:value={editing.budget.per_call_tokens} /></label>
            <label class="f"><span>per_session_tokens</span><input type="number" bind:value={editing.budget.per_session_tokens} /></label>
            <label class="f"><span>per_session_calls</span><input type="number" bind:value={editing.budget.per_session_calls} /></label>
          </div>
          <div class="row">
            <label class="f"><span>循环模式</span>
              <select bind:value={editing.loop_mode}>
                <option value="single">single</option>
                <option value="react">react</option>
                <option value="plan_execute">plan_execute</option>
              </select></label>
            <label class="f"><span>max_history_tokens</span><input type="number" bind:value={editing.max_history_tokens} /></label>
            <label class="ck"><input type="checkbox" bind:checked={editing.model.extended_thinking} /> extended_thinking</label>
          </div>
          <div class="f col"><span>协作伙伴（teammates）</span>
            <div class="tgrp">
              {#each otherRoles as o (o.id)}
                <label class="tk"><input type="checkbox" checked={editing.teammates.includes(o.id)} onchange={() => toggleTeammate(o.id)} /> {o.display_name || o.id}</label>
              {/each}
            </div>
          </div>
          <p class="note">权限规则（permission_rules）暂在编辑器外，按原值保留，不会丢。</p>
        </div>
      {/if}

      <div class="actions">
        <button class="btn ghost" onclick={cancelEdit} disabled={saving}>取消</button>
        <button class="btn primary" onclick={commit} disabled={saving}>{saving ? '保存中…' : '保存'}</button>
      </div>
    </div>
  {/if}
</div>

<style>
  .ov {
    position: fixed;
    inset: 0;
    z-index: 80;
    background: rgba(20, 22, 40, 0.32);
  }
  .panel {
    position: fixed;
    z-index: 81;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    width: min(680px, 94vw);
    max-height: 88vh;
    overflow: auto;
    background: #fff;
    border-radius: 18px;
    box-shadow: 0 24px 64px rgba(28, 30, 60, 0.28);
    padding: 20px 22px 22px;
  }
  .hd {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 14px;
  }
  .hd h2 {
    margin: 0;
    font-size: 17px;
    font-weight: 700;
  }
  .x {
    border: none;
    background: #f1f2f6;
    width: 30px;
    height: 30px;
    border-radius: 8px;
    cursor: pointer;
    font-size: 14px;
  }
  .err {
    background: #fdeceb;
    color: #ef4444;
    padding: 8px 12px;
    border-radius: 9px;
    font-size: 13px;
    margin-bottom: 12px;
  }
  .ws-line {
    display: flex;
    align-items: center;
    gap: 9px;
    margin-bottom: 12px;
  }
  .ws-line .ws-ico {
    font-size: 18px;
  }
  .ws-sub {
    color: #8a8fa3;
    font-size: 12.5px;
  }
  .rl {
    list-style: none;
    margin: 0 0 12px;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .ri {
    display: flex;
    align-items: center;
    gap: 10px;
    border: 1px solid #eef0f4;
    border-radius: 12px;
    padding: 10px 12px;
  }
  .ri-meta {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-width: 0;
  }
  .ri-meta strong {
    font-size: 14px;
  }
  .ri-meta small {
    color: #8a8fa3;
    font-size: 12px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .chip {
    display: inline-block;
    margin-left: 6px;
    padding: 1px 7px;
    border-radius: 999px;
    font-size: 10.5px;
    font-weight: 600;
    color: #fff;
    background: linear-gradient(135deg, #7b5cff, #5b8cff);
  }
  .btn {
    border: none;
    cursor: pointer;
    font: inherit;
    font-size: 13px;
    border-radius: 9px;
    padding: 7px 13px;
  }
  .btn.ghost {
    background: #f1f2f6;
    color: #2b2f45;
  }
  .btn.danger {
    background: #fdeceb;
    color: #ef4444;
  }
  .btn.danger.armed {
    background: #ef4444;
    color: #fff;
  }
  .btn.primary {
    background: linear-gradient(135deg, #7b5cff, #5b8cff);
    color: #fff;
  }
  .btn.add {
    width: 100%;
    background: #f5f3ff;
    color: #6b54ec;
    border: 1px dashed #c9bdff;
    padding: 10px;
  }
  .btn:disabled {
    opacity: 0.6;
    cursor: default;
  }
  .note {
    color: #8a8fa3;
    font-size: 12px;
    margin: 10px 0 0;
  }
  .loading {
    color: #8a8fa3;
    padding: 20px;
    text-align: center;
  }

  .form {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .sec {
    margin: 0;
    font-size: 13px;
    color: #8a8fa3;
    font-weight: 600;
  }
  .f {
    display: flex;
    flex-direction: column;
    gap: 5px;
    flex: 1;
    min-width: 0;
  }
  .f > span {
    font-size: 12.5px;
    color: #555b73;
    font-weight: 500;
  }
  .f input,
  .f select,
  .f textarea {
    border: 1px solid #e2e4ec;
    border-radius: 9px;
    padding: 8px 10px;
    font: inherit;
    font-size: 13.5px;
    outline: none;
    background: #fff;
  }
  .f input:focus,
  .f select:focus,
  .f textarea:focus {
    border-color: #7b5cff;
  }
  .f textarea {
    resize: vertical;
    line-height: 1.45;
  }
  .row {
    display: flex;
    gap: 12px;
  }
  .ck,
  .rd {
    display: flex;
    align-items: center;
    gap: 7px;
    font-size: 13px;
    color: #2b2f45;
    cursor: pointer;
  }
  .radios {
    display: flex;
    gap: 16px;
  }
  .tgrp {
    border: 1px solid #eef0f4;
    border-radius: 10px;
    padding: 8px 10px;
    display: flex;
    flex-wrap: wrap;
    gap: 6px 14px;
    align-items: center;
  }
  .tgrp em {
    font-style: normal;
    color: #8a8fa3;
    font-size: 12px;
    width: 100%;
  }
  .tk {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: 13px;
    cursor: pointer;
  }
  .tk small {
    color: #b6bacb;
    font-size: 11px;
  }
  .adv-toggle {
    align-self: flex-start;
    border: none;
    background: transparent;
    color: #6b54ec;
    cursor: pointer;
    font: inherit;
    font-size: 13px;
    padding: 2px 0;
  }
  .adv {
    display: flex;
    flex-direction: column;
    gap: 12px;
    border-left: 2px solid #eef0f4;
    padding-left: 12px;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 10px;
    margin-top: 6px;
  }
</style>
