<script lang="ts">
  // eieio-m9s.34: `POST /api/nodes` (DESIGNER §3.1) as one form — name, address, class, token.
  //
  // **Class is asked for and defaults to `daemon`.** DESIGNER §3.1: a node's class "is stated,
  // not discovered, and it is the only field that could not be" — everything else about a node
  // comes back from a probe, but a leaf answers no probe at all (SCOPE §3.7, DESIGNER §7: its
  // services are compiled into firmware), so the operator has to say which kind this is. The
  // `<select>` below starts on `'daemon'` — never on a placeholder/empty option — because a form
  // submitted without touching this field must still produce the correct class, not an empty or
  // wrong one; leaving it unset here is exactly the "defaults to daemon" rule quietly not holding.
  //
  // **The token is write-only.** `type="password"` below, and this component never receives one
  // back to redisplay: `NodeSummary`/`GET /api/nodes` carries no `token` field at all (DESIGNER
  // §3.1 — "there is no serialization in which it can appear"), so there is nothing to echo even
  // if this modal wanted to. Submitting with the field empty is allowed on purpose: §3.1 "lets a
  // **The token is required, and the bead's contract said otherwise by mistake.** That contract
  // cited §3.1 as letting "a node be named before its token is known" — but that sentence is
  // about the CLI's `~/.config/eieio/nodes.toml`, a different config surface, where `token` is
  // genuinely an `Option`. The Designer's own `POST /api/nodes` takes `token: String`
  // (`crates/designer/src/api/nodes.rs`), validated non-empty, and §3.1's route table lists it
  // un-suffixed. So the field is required here and the form says so, rather than sending a
  // value the backend will reject and calling it "optional".
  import type { NodeSummary } from '../api/types';

  interface Props {
    systemId: number;
    onSubmit: (input: {
      system_id: number;
      name: string;
      address: string;
      token: string;
      class?: 'daemon' | 'leaf';
    }) => Promise<NodeSummary>;
    onCancel: () => void;
  }

  let { systemId, onSubmit, onCancel }: Props = $props();

  let name = $state('');
  let address = $state('');
  let nodeClass = $state<'daemon' | 'leaf'>('daemon');
  let token = $state('');
  let submitting = $state(false);
  let error = $state<string | null>(null);

  const canSubmit = $derived(name.trim().length > 0 && address.trim().length > 0 && !submitting);

  async function handleSubmit(event: SubmitEvent) {
    event.preventDefault();
    if (!canSubmit) return;
    submitting = true;
    error = null;
    const attemptedToken = token;
    try {
      await onSubmit({
        system_id: systemId,
        name: name.trim(),
        address: address.trim(),
        token: attemptedToken,
        class: nodeClass,
      });
      // Forget it immediately, win or lose, same posture as `LoginGate`'s password: nothing
      // here retains it past the one call it was collected for.
      token = '';
    } catch (err) {
      token = '';
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
    aria-label="Add node"
    tabindex="-1"
    onclick={(e) => e.stopPropagation()}
    onkeydown={(e) => e.stopPropagation()}
  >
  <form onsubmit={handleSubmit}>
    <h2 class="onboard-modal__title">Add node</h2>

    <label class="onboard-modal__label" for="eio-new-node-name">Name</label>
    <input
      id="eio-new-node-name"
      class="onboard-modal__input"
      type="text"
      autocomplete="off"
      bind:value={name}
      disabled={submitting}
    />

    <label class="onboard-modal__label" for="eio-new-node-address">Address</label>
    <input
      id="eio-new-node-address"
      class="onboard-modal__input"
      type="text"
      placeholder="http://10.0.0.5:7373"
      autocomplete="off"
      bind:value={address}
      disabled={submitting}
    />

    <label class="onboard-modal__label" for="eio-new-node-class">Class</label>
    <select id="eio-new-node-class" class="onboard-modal__input" bind:value={nodeClass} disabled={submitting}>
      <option value="daemon">daemon</option>
      <option value="leaf">leaf</option>
    </select>
    {#if nodeClass === 'leaf'}
      <p class="onboard-modal__hint">
        A leaf serves no management API (DESIGNER §3.1) — it will never be offered a probe, and its
        services live in firmware, not in a listing this Designer can fetch.
      </p>
    {/if}

    <label class="onboard-modal__label" for="eio-new-node-token">
      Token
    </label>
    <input
      id="eio-new-node-token"
      required
      class="onboard-modal__input"
      type="password"
      autocomplete="new-password"
      bind:value={token}
      disabled={submitting}
    />
    <p class="onboard-modal__hint">
      Write-only: once submitted, this Designer never shows a node's token back to you.
    </p>

    {#if error}
      <p class="onboard-modal__error" role="alert">{error}</p>
    {/if}

    <div class="onboard-modal__actions">
      <button type="button" class="onboard-modal__button" onclick={onCancel} disabled={submitting}>Cancel</button>
      <button type="submit" class="onboard-modal__button onboard-modal__button--primary" disabled={!canSubmit}>
        {submitting ? 'Adding…' : 'Add node'}
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
