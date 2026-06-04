<script lang="ts">
  import { saveApiKey } from '$lib/ipc';

  interface Props {
    onSaved: () => void;
  }
  let { onSaved }: Props = $props();

  let provider = $state('bailian');
  let apiKey = $state('');
  let saving = $state(false);
  let error = $state<string | null>(null);

  // Providers available in V0.1. Dropdown is one-item now; the seam is here.
  const PROVIDERS = [
    {
      id: 'bailian',
      label: '阿里百炼 (Qwen)',
      hint: '在 bailian.console.aliyun.com 控制台获取，格式 sk-xxx',
      consoleUrl: 'https://bailian.console.aliyun.com'
    }
  ];

  $effect(() => {
    if (apiKey || error) error = null;
  });

  async function handleSubmit(e: Event) {
    e.preventDefault();
    const trimmed = apiKey.trim();
    if (!trimmed) {
      error = 'API Key 不能为空';
      return;
    }
    saving = true;
    try {
      await saveApiKey(provider, trimmed);
      apiKey = '';
      onSaved();
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      saving = false;
    }
  }

  const currentHint = $derived(PROVIDERS.find((p) => p.id === provider));
</script>

<div class="overlay">
  <div class="modal">
    <header class="modal-head">
      <h2>配置 LLM Provider</h2>
      <p class="lead">
        首次启动需要配置一个 LLM Provider。API Key 会加密存到系统 keychain，**不**写入任何文件。
      </p>
    </header>

    <form onsubmit={handleSubmit}>
      <label class="field">
        <span class="label">Provider</span>
        <select bind:value={provider} disabled={saving}>
          {#each PROVIDERS as p (p.id)}
            <option value={p.id}>{p.label}</option>
          {/each}
        </select>
      </label>

      <label class="field">
        <span class="label">API Key</span>
        <input
          type="password"
          autocomplete="off"
          spellcheck="false"
          placeholder="sk-..."
          bind:value={apiKey}
          disabled={saving}
        />
        {#if currentHint}
          <span class="hint">
            {currentHint.hint}
            <a href={currentHint.consoleUrl} target="_blank" rel="noreferrer">控制台 ↗</a>
          </span>
        {/if}
      </label>

      {#if error}
        <p class="error" role="alert">{error}</p>
      {/if}

      <div class="actions">
        <button type="submit" class="primary" disabled={saving || !apiKey.trim()}>
          {saving ? '保存中…' : '保存并继续'}
        </button>
      </div>
    </form>
  </div>
</div>

<style>
  .overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.4);
    backdrop-filter: blur(4px);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
  }

  .modal {
    background: #ffffff;
    border-radius: 12px;
    padding: 28px;
    width: 440px;
    max-width: 92vw;
    box-shadow: 0 20px 60px rgba(0, 0, 0, 0.18);
  }

  .modal-head h2 {
    margin: 0 0 8px 0;
    font-size: 18px;
    font-weight: 600;
    letter-spacing: -0.01em;
  }

  .lead {
    color: #6e6e73;
    font-size: 13px;
    line-height: 1.55;
    margin: 0 0 24px 0;
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-bottom: 18px;
  }

  .label {
    font-size: 12px;
    font-weight: 500;
    color: #6e6e73;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }

  select,
  input {
    border: 1px solid #d2d2d7;
    border-radius: 8px;
    padding: 9px 12px;
    font-size: 14px;
    font-family: inherit;
    background: #ffffff;
    color: inherit;
    outline: none;
    transition: border-color 0.15s;
  }
  select:focus,
  input:focus {
    border-color: #007aff;
    box-shadow: 0 0 0 3px rgba(0, 122, 255, 0.15);
  }

  .hint {
    font-size: 12px;
    color: #8e8e93;
    line-height: 1.4;
  }
  .hint a {
    color: #007aff;
    text-decoration: none;
    margin-left: 4px;
  }

  .error {
    color: #ff3b30;
    font-size: 13px;
    margin: 0 0 16px 0;
  }

  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 8px;
  }

  .primary {
    background: #007aff;
    color: #ffffff;
    border: none;
    border-radius: 8px;
    padding: 9px 18px;
    font-size: 14px;
    font-weight: 500;
    cursor: pointer;
    transition: background 0.15s;
  }
  .primary:hover:not(:disabled) {
    background: #0066d6;
  }
  .primary:disabled {
    background: #c7c7cc;
    cursor: not-allowed;
  }

  /* 自动深色已停用（用户要求，固定浅色）。max-width:0 永不匹配 = 整块失效。 */
  @media (prefers-color-scheme: dark) and (max-width: 0px) {
    .modal {
      background: #2c2c2e;
      box-shadow: 0 20px 60px rgba(0, 0, 0, 0.5);
    }
    select,
    input {
      background: #1c1c1e;
      border-color: #48484a;
      color: #f5f5f7;
    }
    .lead {
      color: #98989d;
    }
    .label {
      color: #98989d;
    }
    .hint {
      color: #8e8e93;
    }
  }
</style>
