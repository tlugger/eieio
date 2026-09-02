<script lang="ts">
  // DESIGNER §5 / §4: DAEMON §9.3 refuses a stale `PUT` with the current
  // text, and this shell's job is to render that refusal — "never
  // silent-overwrite... agents and humans editing the same file is the
  // expected condition, not an edge case."
  //
  // GUESS: DAEMON §9.3's `detail.diff` is a unified diff of TOML bytes; this
  // mock's stand-in "text" is JSON (mock.ts's header doc explains why), so a
  // line diff of it would show a rewritten-looking blob rather than the
  // small, readable delta a real unified diff over TOML lines would. This
  // shows the node's current text plainly instead of fabricating a diff
  // that would look precise but not be one.
  interface Props {
    current: string;
    onReloadLatest: () => void;
    onDismiss: () => void;
  }

  let { current, onReloadLatest, onDismiss }: Props = $props();
  let expanded = $state(false);
</script>

<div class="conflict" role="alert">
  <div class="conflict__row">
    <span class="conflict__message">
      This service changed on the node since you loaded it (DAEMON §9.3). Your edit was not applied.
    </span>
    <button type="button" class="conflict__link" onclick={() => (expanded = !expanded)}>
      {expanded ? 'hide' : 'show'} current version
    </button>
    <button type="button" class="conflict__button" onclick={onReloadLatest}>Reload latest</button>
    <button type="button" class="conflict__dismiss" aria-label="Dismiss" onclick={onDismiss}>✕</button>
  </div>
  {#if expanded}
    <pre class="conflict__current">{current}</pre>
  {/if}
</div>

<style>
  .conflict {
    background: var(--state-errored);
    color: #fff;
    font-size: 12px;
  }

  .conflict__row {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 8px 16px;
  }

  .conflict__message {
    flex: 1 1 auto;
  }

  .conflict__link,
  .conflict__button,
  .conflict__dismiss {
    border: 1px solid rgba(255, 255, 255, 0.6);
    background: rgba(255, 255, 255, 0.1);
    color: #fff;
    border-radius: 4px;
    padding: 2px 8px;
    cursor: pointer;
    font-size: 11px;
    flex: 0 0 auto;
  }

  .conflict__dismiss {
    border: none;
    background: none;
    padding: 2px 4px;
  }

  .conflict__current {
    margin: 0 16px 8px;
    padding: 8px;
    max-height: 240px;
    overflow: auto;
    background: rgba(0, 0, 0, 0.25);
    border-radius: 4px;
    font-family: var(--mono);
    font-size: 11px;
    white-space: pre-wrap;
    word-break: break-word;
  }
</style>
