<script lang="ts">
  // DESIGNER §5: "A narrow icon rail." The spec settles the shell's
  // layout (rail + one navigator + canvas) but does not enumerate what
  // lives on the rail itself. GUESS: this shell puts only what it has a
  // view for — the systems/navigator view (the only one this issue
  // builds) and the theme toggle — rather than inventing placeholder
  // destinations (a registries browser, node onboarding, etc.) that
  // would-be later issues' to define.
  import { cycleThemePreference, getThemePreference } from '../stores/theme.svelte';

  const theme = $derived(getThemePreference());

  function themeGlyph(pref: string): string {
    if (pref === 'light') return '☼';
    if (pref === 'dark') return '☾';
    return '◐';
  }

  function themeLabel(pref: string): string {
    if (pref === 'light') return 'Theme: light (click to switch to dark)';
    if (pref === 'dark') return 'Theme: dark (click to follow system)';
    return 'Theme: following system preference (click to switch to light)';
  }
</script>

<div class="rail">
  <button class="rail__button rail__button--active" title="Systems" aria-label="Systems" aria-current="true">
    <span aria-hidden="true">⬡</span>
  </button>

  <div class="rail__spacer"></div>

  <button
    class="rail__button"
    title={themeLabel(theme)}
    aria-label={themeLabel(theme)}
    onclick={cycleThemePreference}
  >
    <span aria-hidden="true">{themeGlyph(theme)}</span>
  </button>
</div>

<style>
  .rail {
    display: flex;
    flex-direction: column;
    align-items: center;
    width: 48px;
    padding: 10px 0;
    background: var(--chrome-bg);
    border-right: 1px solid var(--chrome-border);
  }

  .rail__spacer {
    flex: 1 1 auto;
  }

  .rail__button {
    width: 34px;
    height: 34px;
    margin: 3px 0;
    border: none;
    border-radius: 8px;
    background: none;
    color: var(--chrome-text-muted);
    font-size: 16px;
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .rail__button:hover {
    background: var(--chrome-bg-raised);
    color: var(--chrome-text);
  }

  .rail__button--active {
    background: var(--accent);
    color: var(--accent-contrast);
  }
</style>
