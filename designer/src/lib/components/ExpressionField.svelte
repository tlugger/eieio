<script lang="ts">
  // DESIGNER §5: "Every property input is an expression editor... with
  // WASM-`expr` linting on keystroke, a signal-dependence badge (constant
  // vs per-signal), and the manifest-declared type shown as the expected
  // result type." ABI §11: "Every property is an expression" — there is no
  // separate literal-field kind, only this editor rendering a trivial
  // expression when that's all a value is.
  import { ensureLinterReady, isLinterReady, lintExpression, type LintResult } from '../expr/lint';
  import type { PropertyType } from '../api/types';

  interface Props {
    name: string;
    type: PropertyType;
    description?: string;
    required?: boolean;
    /** `undefined` renders as "using the manifest default" rather than an
     * empty override — the property's own default (if any) is shown as
     * placeholder text so the field never has to invent a starting value. */
    value: string | undefined;
    default?: string;
    onInput: (value: string) => void;
    onReset?: () => void;
  }

  let { name, type, description, required, value, default: defaultValue, onInput, onReset }: Props = $props();

  let lintResult = $state<LintResult | null>(null);
  let ready = $state(isLinterReady());

  $effect(() => {
    if (ready) return;
    ensureLinterReady().then(() => {
      ready = true;
    });
  });

  // Lint the effective text — the override when there is one, else the
  // manifest's own default expression, so a field showing only placeholder
  // text still reports whether *that* expression is sound.
  const effectiveSource = $derived(value ?? defaultValue ?? '');

  $effect(() => {
    const source = effectiveSource;
    if (!ready) {
      lintResult = null;
      return;
    }
    lintResult = lintExpression(source);
  });

  function handleInput(event: Event) {
    onInput((event.target as HTMLTextAreaElement).value);
  }

  const firstDiagnostic = $derived(lintResult?.diagnostics[0] ?? null);
  const spanText = $derived.by(() => {
    if (!firstDiagnostic) return null;
    const { start, end } = firstDiagnostic.span;
    return effectiveSource.slice(start, Math.max(start, end));
  });
</script>

<div class="expr-field">
  <div class="expr-field__header">
    <label class="expr-field__label" for={`expr-${name}`}>
      {name}
      {#if required}<span class="expr-field__required" title="Required">*</span>{/if}
    </label>
    <span class="expr-field__type" title="Manifest-declared result type (ABI §11.1)">{type}</span>
    {#if lintResult}
      <span
        class={`expr-field__badge ${lintResult.signal_dependent ? 'expr-field__badge--signal' : 'expr-field__badge--constant'}`}
        title={lintResult.signal_dependent
          ? 'Evaluated per signal (EXPR §10)'
          : 'Constant — evaluated once, at configure time (EXPR §10)'}
      >
        {lintResult.signal_dependent ? 'per signal' : 'constant'}
      </span>
    {/if}
    {#if value !== undefined && onReset}
      <button type="button" class="expr-field__reset" onclick={onReset} title="Revert to the manifest default">
        reset
      </button>
    {/if}
  </div>

  {#if description}
    <p class="expr-field__description">{description}</p>
  {/if}

  <textarea
    id={`expr-${name}`}
    class="expr-field__input"
    class:expr-field__input--invalid={lintResult && !lintResult.ok}
    rows="1"
    spellcheck="false"
    placeholder={defaultValue ?? ''}
    value={value ?? ''}
    oninput={handleInput}
  ></textarea>

  {#if lintResult && !lintResult.ok && firstDiagnostic}
    <p class="expr-field__diagnostic" role="alert">
      <span class="expr-field__diagnostic-code">{firstDiagnostic.code}</span>
      {firstDiagnostic.message}
      {#if spanText}
        <code class="expr-field__diagnostic-span">at "{spanText}" ({firstDiagnostic.span.start}–{firstDiagnostic.span.end})</code>
      {/if}
      {#if lintResult.diagnostics.length > 1}
        <span class="expr-field__diagnostic-more">(+{lintResult.diagnostics.length - 1} more)</span>
      {/if}
    </p>
  {/if}
</div>

<style>
  .expr-field {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 8px 0;
    border-bottom: 1px solid var(--chrome-border);
  }

  .expr-field__header {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .expr-field__label {
    font-weight: 600;
    font-size: 12px;
    font-family: var(--mono);
  }

  .expr-field__required {
    color: var(--state-errored);
    margin-left: 2px;
  }

  .expr-field__type {
    font-size: 10px;
    color: var(--chrome-text-muted);
    padding: 1px 6px;
    border: 1px solid var(--chrome-border);
    border-radius: 8px;
  }

  .expr-field__badge {
    font-size: 10px;
    padding: 1px 6px;
    border-radius: 8px;
  }

  .expr-field__badge--constant {
    background: var(--chrome-border);
    color: var(--chrome-text);
  }

  .expr-field__badge--signal {
    background: var(--accent);
    color: var(--accent-contrast);
  }

  .expr-field__reset {
    margin-left: auto;
    font-size: 10px;
    border: none;
    background: none;
    color: var(--chrome-text-muted);
    cursor: pointer;
    text-decoration: underline;
  }

  .expr-field__description {
    margin: 0;
    font-size: 11px;
    color: var(--chrome-text-muted);
  }

  .expr-field__input {
    font-family: var(--mono);
    font-size: 12px;
    padding: 6px 8px;
    border: 1px solid var(--chrome-border);
    border-radius: 6px;
    background: var(--chrome-bg);
    color: var(--chrome-text);
    resize: vertical;
    min-height: 30px;
  }

  .expr-field__input:focus {
    outline: 2px solid var(--focus-ring);
    outline-offset: -1px;
  }

  .expr-field__input--invalid {
    border-color: var(--canvas-edge-error);
  }

  .expr-field__diagnostic {
    margin: 0;
    font-size: 11px;
    color: var(--canvas-edge-error);
  }

  .expr-field__diagnostic-code {
    font-family: var(--mono);
    font-weight: 700;
    margin-right: 4px;
  }

  .expr-field__diagnostic-span {
    display: block;
    margin-top: 2px;
    font-family: var(--mono);
    color: var(--chrome-text-muted);
  }

  .expr-field__diagnostic-more {
    color: var(--chrome-text-muted);
    margin-left: 4px;
  }
</style>
