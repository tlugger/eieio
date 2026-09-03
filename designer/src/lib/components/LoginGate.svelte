<script lang="ts">
  // The login gate (DESIGNER §3.1 / §3.1's "v1-minimal" auth line, eieio-m9s.31): one password
  // field, `POST /api/session`, and a rendered error on a wrong password. Nothing else — no
  // user model, no "remember me", no registration. `App.svelte` renders this in place of the
  // whole app shell whenever no session is known to be live (`lib/api/client.ts`'s
  // `onSessionRequired`), which is what makes this the *only* place in the SPA that has to
  // know a login step exists at all.
  //
  // What it does not do, by design (this bead's own brief): it does not store the password —
  // `attempted` below is held only long enough to make the one `POST`, then the field is
  // cleared regardless of outcome; it never touches `document.cookie` — the session travels in
  // an `HttpOnly` cookie this page cannot read and should not try to (`session.rs`'s doc); and
  // it never renders anything of the app underneath it while a session is missing, so a wrong
  // password never has an empty canvas to fall back to.
  import { login } from '../api/client';
  import { WrongPasswordError } from '../api/backend';

  interface Props {
    onAuthenticated: () => void;
  }

  let { onAuthenticated }: Props = $props();

  let password = $state('');
  let submitting = $state(false);
  let error = $state<string | null>(null);

  async function handleSubmit(event: SubmitEvent) {
    event.preventDefault();
    if (submitting || password.length === 0) return;
    const attempted = password;
    // Post it and forget it: nothing here retains the password past this one call, win or lose.
    password = '';
    error = null;
    submitting = true;
    try {
      await login(attempted);
      onAuthenticated();
    } catch (err) {
      error =
        err instanceof WrongPasswordError
          ? 'Wrong password.'
          : err instanceof Error
            ? err.message
            : String(err);
    } finally {
      submitting = false;
    }
  }
</script>

<div class="gate">
  <form class="gate__card" onsubmit={handleSubmit}>
    <h1 class="gate__title">eieio Designer</h1>
    <p class="gate__subtitle">Enter the operator password to continue.</p>

    <label class="gate__label" for="eio-gate-password">Password</label>
    <input
      id="eio-gate-password"
      class="gate__input"
      type="password"
      autocomplete="current-password"
      bind:value={password}
      disabled={submitting}
    />

    {#if error}
      <p class="gate__error" role="alert">{error}</p>
    {/if}

    <button class="gate__submit" type="submit" disabled={submitting || password.length === 0}>
      {submitting ? 'Signing in…' : 'Sign in'}
    </button>
  </form>
</div>

<style>
  .gate {
    position: fixed;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    background: var(--canvas-bg);
    z-index: 1000;
  }

  .gate__card {
    display: flex;
    flex-direction: column;
    gap: 8px;
    width: min(320px, 90vw);
    padding: 28px 24px;
    background: var(--chrome-bg-raised);
    border: 1px solid var(--chrome-border);
    border-radius: 8px;
    box-shadow: var(--shadow-modal);
  }

  .gate__title {
    margin: 0;
    font-size: 18px;
    font-weight: 600;
    color: var(--chrome-text);
  }

  .gate__subtitle {
    margin: 0 0 12px;
    color: var(--chrome-text-muted);
    font-size: 12px;
  }

  .gate__label {
    font-size: 12px;
    color: var(--chrome-text-muted);
  }

  .gate__input {
    padding: 8px 10px;
    border: 1px solid var(--chrome-border);
    border-radius: 4px;
    background: var(--chrome-bg);
    color: var(--chrome-text);
    font-size: 13px;
  }

  .gate__error {
    margin: 0;
    color: var(--state-errored);
    font-size: 12px;
  }

  .gate__submit {
    margin-top: 8px;
    padding: 8px 10px;
    border: none;
    border-radius: 4px;
    background: var(--accent);
    color: var(--accent-contrast);
    font-size: 13px;
    font-weight: 600;
    cursor: pointer;
  }

  .gate__submit:disabled {
    opacity: 0.6;
    cursor: default;
  }
</style>
