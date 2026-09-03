<script lang="ts">
  // eieio-m9s.34: the smallest possible form for `POST /api/systems` (DESIGNER §3.1) — a name,
  // nothing else. A System has no other field to ask for (§2's `systems` table is `(id, name)`),
  // so this is deliberately not styled as a multi-step wizard (SCOPE §6 is single-operator;
  // "a form is the right size").
  import type { SystemSummary } from '../api/types';

  interface Props {
    onSubmit: (name: string) => Promise<SystemSummary>;
    onCancel: () => void;
  }

  let { onSubmit, onCancel }: Props = $props();

  let name = $state('');
  let submitting = $state(false);
  let error = $state<string | null>(null);

  async function handleSubmit(event: SubmitEvent) {
    event.preventDefault();
    const trimmed = name.trim();
    if (submitting || trimmed.length === 0) return;
    submitting = true;
    error = null;
    try {
      await onSubmit(trimmed);
    } catch (err) {
      // A `SessionRequiredError` still surfaces here (never caught-and-dropped) — `App.svelte`'s
      // `onSessionRequired` subscriber reopens the login gate independently of this rendering,
      // and this modal simply shows whatever message the rejected call carried in the meantime.
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
    aria-label="New System"
    tabindex="-1"
    onclick={(e) => e.stopPropagation()}
    onkeydown={(e) => e.stopPropagation()}
  >
  <form onsubmit={handleSubmit}>
    <h2 class="onboard-modal__title">New System</h2>

    <label class="onboard-modal__label" for="eio-new-system-name">Name</label>
    <input
      id="eio-new-system-name"
      class="onboard-modal__input"
      type="text"
      autocomplete="off"
      bind:value={name}
      disabled={submitting}
    />

    {#if error}
      <p class="onboard-modal__error" role="alert">{error}</p>
    {/if}

    <div class="onboard-modal__actions">
      <button type="button" class="onboard-modal__button" onclick={onCancel} disabled={submitting}>Cancel</button>
      <button
        type="submit"
        class="onboard-modal__button onboard-modal__button--primary"
        disabled={submitting || name.trim().length === 0}
      >
        {submitting ? 'Creating…' : 'Create'}
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
    width: min(360px, 90vw);
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

  .onboard-modal__input,
  .onboard-modal :global(select) {
    padding: 8px 10px;
    border: 1px solid var(--chrome-border);
    border-radius: 4px;
    background: var(--chrome-bg);
    color: var(--chrome-text);
    font-size: 13px;
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
