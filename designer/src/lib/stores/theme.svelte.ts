// Theme preference: 'system' defers to the OS (the default); an explicit
// 'light'/'dark' choice always wins over it (plan requirement). No
// server-side persistence — this is a pure per-browser convenience, so
// localStorage is the right (and only) place for it.

export type ThemePreference = 'system' | 'light' | 'dark';

const STORAGE_KEY = 'eieio-designer-theme';

function readInitial(): ThemePreference {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (stored === 'light' || stored === 'dark') return stored;
  } catch {
    // localStorage unavailable (private browsing, etc.) — fall through to
    // the OS-deferring default.
  }
  return 'system';
}

let preference = $state<ThemePreference>(readInitial());

function applyToDocument(): void {
  const root = document.documentElement;
  if (preference === 'system') root.removeAttribute('data-theme');
  else root.setAttribute('data-theme', preference);
}

export function getThemePreference(): ThemePreference {
  return preference;
}

export function setThemePreference(next: ThemePreference): void {
  preference = next;
  try {
    if (next === 'system') localStorage.removeItem(STORAGE_KEY);
    else localStorage.setItem(STORAGE_KEY, next);
  } catch {
    // Persistence is a nice-to-have; the toggle still works for the tab.
  }
  applyToDocument();
}

export function cycleThemePreference(): void {
  const order: ThemePreference[] = ['system', 'light', 'dark'];
  const next = order[(order.indexOf(preference) + 1) % order.length]!;
  setThemePreference(next);
}

applyToDocument();
