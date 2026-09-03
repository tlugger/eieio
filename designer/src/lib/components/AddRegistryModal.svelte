<script lang="ts">
  // eieio-m9s.34: `POST /api/registries` (DESIGNER §3.1, §2) — a url and an optional credential.
  // `auth` is opaque to the backend (`crates/designer/src/api/registries.rs`'s own doc: "whatever
  // credential this registry needs... never inspected or validated here") and, per that module's
  // doc comment, "write-only, matching [the node token]'s own posture" — so this field gets the
  // same `type="password"` treatment as `AddNodeModal`'s token, for the same reason: nothing this
  // Designer answers back (`GET /api/registries` -> `{ id, url }`, no `auth`) could ever redisplay
  // it, so there is nothing to gain by rendering it as plain text.
  import type { RegistrySummary } from '../api/client';

  interface Props {
    onSubmit: (input: { url: string; auth?: string }) => Promise<RegistrySummary>;
    onCancel: () => void;
  }

  let { onSubmit, onCancel }: Props = $props();

  let url = $state('');
  let auth = $state('');
  let submitting = $state(false);
  let error = $state<string | null>(null);

  async function handleSubmit(event: SubmitEvent) {
    event.preventDefault();
    const trimmed = url.trim();
    if (submitting || trimmed.length === 0) return;
    submitting = true;
    error = null;
    const attemptedAuth = auth;
    try {
      await onSubmit({ url: trimmed, auth: attemptedAuth.length > 0 ? attemptedAuth : undefined });
      auth = '';
    } catch (err) {
      auth = '';
      error = err instanceof Error ? err.message : String(err);
    } finally {
      submitting = false;
    }
  }

  function handleKeydown(event: KeyboardEvent) {
    if (event.key === 'Escape') onCancel();
  }
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="modal-backdrop" role="presentation" onclick={onCancel}>
  <div
    class="onboard-modal"
    role="dialog"
    aria-modal="true"
    aria-label="Add registry"
    tabindex="-1"
    onclick={(e) => e.stopPropagation()}
    onkeydown={(e) => e.stopPropagation()}
  >
  <form onsubmit={handleSubmit}>
    <h2 class="onboard-modal__title">Add registry</h2>

    <label class="onboard-modal__label" for="eio-new-registry-url">URL</label>
    <input
      id="eio-new-registry-url"
      class="onboard-modal__input"
      type="text"
      placeholder="https://registry.example/v2"
      autocomplete="off"
      bind:value={url}
      disabled={submitting}
    />

    <label class="onboard-modal__label" for="eio-new-registry-auth">
      Credential <span class="onboard-modal__optional">(optional)</span>
    </label>
    <input
      id="eio-new-registry-auth"
      class="onboard-modal__input"
      type="password"
      autocomplete="new-password"
      bind:value={auth}
      disabled={submitting}
    />
    <p class="onboard-modal__hint">
      Write-only: this Designer never shows a registry's credential back to you.
    </p>

    {#if error}
      <p class="onboard-modal__error" role="alert">{error}</p>
    {/if}

    <div class="onboard-modal__actions">
      <button type="button" class="onboard-modal__button" onclick={onCancel} disabled={submitting}>Cancel</button>
      <button
        type="submit"
        class="onboard-modal__button onboard-modal__button--primary"
        disabled={submitting || url.trim().length === 0}
      >
        {submitting ? 'Adding…' : 'Add registry'}
      </button>
    </div>
  </form>
  </div>
</div>

<style>
  .modal-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(15, 23, 42, 0.35);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 500;
  }

  .onboard-modal {
    display: flex;
    flex-direction: column;
    gap: 8px;
    width: min(400px, 90vw);
    padding: 20px;
    background: var(--chrome-bg-raised);
    border: 1px solid var(--chrome-border);
    border-radius: 8px;
    box-shadow: var(--shadow-modal);
  }

  .onboard-modal__title {
    margin: 0 0 4px;
    font-size: 15px;
    font-weight: 600;
    color: var(--chrome-text);
  }

  .onboard-modal__label {
    font-size: 12px;
    color: var(--chrome-text-muted);
  }

  .onboard-modal__optional {
    font-weight: 400;
    color: var(--chrome-text-muted);
  }

  .onboard-modal__input {
    padding: 8px 10px;
    border: 1px solid var(--chrome-border);
    border-radius: 4px;
    background: var(--chrome-bg);
    color: var(--chrome-text);
    font-size: 13px;
  }

  .onboard-modal__hint {
    margin: 0;
    color: var(--chrome-text-muted);
    font-size: 11px;
  }

  .onboard-modal__error {
    margin: 0;
    color: var(--state-errored);
    font-size: 12px;
  }

  .onboard-modal__actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 8px;
  }

  .onboard-modal__button {
    padding: 7px 12px;
    border: 1px solid var(--chrome-border);
    border-radius: 4px;
    background: var(--chrome-bg-raised);
    color: var(--chrome-text);
    font-size: 12px;
    cursor: pointer;
  }

  .onboard-modal__button--primary {
    border-color: var(--accent);
    background: var(--accent);
    color: var(--accent-contrast);
    font-weight: 600;
  }

  .onboard-modal__button:disabled {
    opacity: 0.6;
    cursor: default;
  }
</style>
