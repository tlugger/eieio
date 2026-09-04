// eieio-m9s.40: the palette's install section — the UI half of DESIGNER §3.3's install flow.
//
// Three rules this component would break quietly rather than visibly, so each is pinned:
//
// 1. **Browsing is per node, and a leaf is refused by name.** DAEMON §9.8 makes browsing the
//    node's job because the node holds the registry credentials and enforces the signature
//    policy — there is no Designer-wide catalogue, and a leaf serves no management API at all
//    (DESIGNER §3.1; its blocks are compiled into firmware, SCOPE §3.7). A form that dialled a
//    leaf would produce a connection error indistinguishable from a node that is down, which is
//    the exact confusion §3.1 exists to prevent. So the controls are disabled and the reason is
//    written out, rather than being left to a failed request to explain.
// 2. **Preview and Install are different acts and must not read the same.** DAEMON §9.8: a
//    browse fetches a manifest and installs nothing; `POST /blocks/pull` is "a separate,
//    deliberate act". Two buttons, two callbacks, and this file asserts each one calls only its
//    own.
// 3. **A refusal is rendered, never swallowed.** A registry the node has no entry for, a
//    reference that did not resolve, a signature the node refused — all of them arrive as a
//    rejected promise from the callbacks this component is handed, and an operator who clicked
//    Install must be told why nothing happened.
//
// What this file deliberately does NOT test is the cache invalidation: it does not happen here.
// `client.ts`'s `pullBlock` composes the pull and the re-`PUT` into one call precisely so that
// no component — this one included — can install a block and skip it. `lib/api/blocks.test.ts`
// is where that is pinned, and this component's `onInstall` prop cannot observe the difference.
import { mount, unmount } from 'svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import BlockLibrary from './BlockLibrary.svelte';
import type { BlockManifest, NodeClass, NodeSummary } from '../api/types';

function node(nodeClass: NodeClass = 'daemon'): NodeSummary {
  return {
    id: 5,
    system_id: 1,
    name: 'porch',
    class: nodeClass,
    address: 'http://10.0.0.5:7373',
    capabilities: [],
  };
}

function manifest(blockRef: string, name: string): BlockManifest {
  return {
    block_ref: blockRef,
    name,
    version: '1.0.0',
    abi: { major: 1, minor: 0 },
    capabilities: [],
    inputs: [],
    outputs: [],
    properties: [],
    targets: ['wasm32-unknown-unknown'],
    aot: [],
  };
}

interface Handlers {
  onBrowseRegistry?: (repository: string) => Promise<string[]>;
  onPreview?: (reference: string) => Promise<void>;
  onInstall?: (reference: string) => Promise<void>;
}

function render(handlers: Handlers = {}, { nodeClass = 'daemon' as NodeClass | null, manifests = [] as BlockManifest[] } = {}) {
  const target = document.createElement('div');
  document.body.appendChild(target);
  const exports = mount(BlockLibrary, {
    target,
    props: {
      manifests,
      node: nodeClass === null ? null : node(nodeClass),
      onSelect: () => {},
      onClose: () => {},
      onBrowseRegistry: handlers.onBrowseRegistry ?? (() => Promise.resolve([])),
      onPreview: handlers.onPreview ?? (() => Promise.resolve()),
      onInstall: handlers.onInstall ?? (() => Promise.resolve()),
    },
  });
  return { target, exports };
}

function setValue(el: Element | null, value: string) {
  if (!el) throw new Error('element not found');
  (el as HTMLInputElement).value = value;
  el.dispatchEvent(new Event('input', { bubbles: true }));
}

async function browse(target: HTMLElement, repository: string) {
  setValue(target.querySelector('.library__registry-input'), repository);
  target.querySelector('.library__registry-form')?.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
  await vi.waitFor(() => {
    if (!target.querySelector('.library__offered') && !target.querySelector('.library__registry-error')) {
      throw new Error('the browse has not settled');
    }
  });
}

afterEach(() => {
  document.body.innerHTML = '';
});

describe('BlockLibrary — installing from a registry (DAEMON §9.8, §4.1)', () => {
  it('lists a repositorys tags on the node the palette is scoped to', async () => {
    const onBrowseRegistry = vi
      .fn()
      .mockResolvedValue(['ghcr.io/tlugger/threshold:2.1.0', 'ghcr.io/tlugger/threshold:2.0.0']);
    const { target, exports } = render({ onBrowseRegistry });
    await browse(target, 'ghcr.io/tlugger/threshold');

    expect(onBrowseRegistry).toHaveBeenCalledWith('ghcr.io/tlugger/threshold');
    const rows = [...target.querySelectorAll('.library__offered-ref')].map((el) => el.textContent);
    expect(rows).toEqual(['ghcr.io/tlugger/threshold:2.1.0', 'ghcr.io/tlugger/threshold:2.0.0']);
    unmount(exports);
  });

  it('Preview and Install are separate acts, each calling only its own handler', async () => {
    const onPreview = vi.fn().mockResolvedValue(undefined);
    const onInstall = vi.fn().mockResolvedValue(undefined);
    const { target, exports } = render({
      onBrowseRegistry: () => Promise.resolve(['ghcr.io/tlugger/threshold:2.1.0']),
      onPreview,
      onInstall,
    });
    await browse(target, 'ghcr.io/tlugger/threshold');

    const [preview, install] = [...target.querySelectorAll('.library__offered-action')] as HTMLButtonElement[];
    preview.click();
    await vi.waitFor(() => expect(onPreview).toHaveBeenCalledWith('ghcr.io/tlugger/threshold:2.1.0'));
    expect(onInstall).not.toHaveBeenCalled();

    // Both buttons are disabled while an act is in flight — one at a time, so a row can say
    // which of its two is working. Wait for that to clear before the second click, or the click
    // lands on a disabled button and is ignored.
    await vi.waitFor(() => expect(install.disabled).toBe(false));
    install.click();
    await vi.waitFor(() => expect(onInstall).toHaveBeenCalledWith('ghcr.io/tlugger/threshold:2.1.0'));
    expect(onPreview).toHaveBeenCalledTimes(1);
    unmount(exports);
  });

  it('renders a refused install rather than swallowing it', async () => {
    const { target, exports } = render({
      onBrowseRegistry: () => Promise.resolve(['ghcr.io/tlugger/threshold:2.1.0']),
      onInstall: () => Promise.reject(new Error('this artifact is not signed by a trusted key')),
    });
    await browse(target, 'ghcr.io/tlugger/threshold');

    const buttons = [...target.querySelectorAll('.library__offered-action')] as HTMLButtonElement[];
    buttons[1].click();
    await vi.waitFor(() => {
      const alert = target.querySelector('.library__registry-error');
      expect(alert?.textContent).toContain('not signed by a trusted key');
    });
    unmount(exports);
  });

  it('renders a refused browse the same way', async () => {
    const { target, exports } = render({
      onBrowseRegistry: () => Promise.reject(new Error('"example.invalid/x" names no registry this node is configured for')),
    });
    await browse(target, 'example.invalid/x');
    expect(target.querySelector('.library__registry-error')?.textContent).toContain('names no registry');
    expect(target.querySelector('.library__offered')).toBeNull();
    unmount(exports);
  });

  it('refuses a leaf by name rather than dialling it (DESIGNER §3.1, SCOPE §3.7)', () => {
    const onBrowseRegistry = vi.fn();
    const { target, exports } = render({ onBrowseRegistry }, { nodeClass: 'leaf' });
    const input = target.querySelector('.library__registry-input') as HTMLInputElement;
    const button = target.querySelector('.library__registry-browse') as HTMLButtonElement;
    expect(input.disabled).toBe(true);
    expect(button.disabled).toBe(true);
    expect(target.querySelector('.library__registry-note')?.textContent).toContain('compiled into firmware');
    expect(onBrowseRegistry).not.toHaveBeenCalled();
    unmount(exports);
  });

  it('says a node has to be selected before a registry can be browsed at all', () => {
    const { target, exports } = render({}, { nodeClass: null });
    expect((target.querySelector('.library__registry-input') as HTMLInputElement).disabled).toBe(true);
    expect(target.querySelector('.library__registry-note')?.textContent).toContain('per node');
    unmount(exports);
  });

  it('marks an offered reference the palette already describes, by whole reference', async () => {
    const { target, exports } = render(
      { onBrowseRegistry: () => Promise.resolve(['ghcr.io/tlugger/filter:1.3.0', 'ghcr.io/tlugger/filter:1.2.0']) },
      // Cached under a *different* whole reference that shares the name and the tag. DESIGNER
      // §3.3: a block is identified by its whole reference, never by its name — so this must not
      // be reported as already in the palette.
      { manifests: [manifest('ghcr.io/tlugger/filter:1.3.0', 'filter'), manifest('other/filter:1.2.0', 'filter')] },
    );
    await browse(target, 'ghcr.io/tlugger/filter');
    const rows = [...target.querySelectorAll('.library__offered-row')];
    expect(rows[0].querySelector('.library__offered-known')).not.toBeNull();
    expect(rows[1].querySelector('.library__offered-known')).toBeNull();
    unmount(exports);
  });
});
