// eieio-m9s.45: pins DESIGNER §3.3's absence rule where it is visible — the config modal.
//
// The bug: `ensureFreshManifest` returned early for a reference with no cache entry at all
// (staleness and absence are different questions), so the modal could open on a block whose
// manifest the Designer had never seen. It then rendered the reference where a block name goes,
// no upstream fields, and the sentence "This block has no properties" — which is not something
// it knew. It knew nothing *about* the block's properties, while the instance's own `props` sat
// right there being neither shown nor editable.
//
// §3.3 now settles it: of the three act sites, the config modal is the one that *refuses*,
// because the modal **is** the manifest. `App.svelte` will not set `configuringInstanceId`
// without one and names the reference it lacks instead; this component's `manifest` prop is
// required, which is what makes that refusal something svelte-check enforces rather than
// something a caller remembers. So there is no absence render left in this file to assert —
// what is asserted here instead is the other half of the same rule: every claim this modal
// makes now comes from a manifest it actually has, and "This block has no properties" is only
// ever the manifest saying so.
//
// `vi.mock` on the linter: `ExpressionField` (one per property) initializes `crates/expr-wasm`
// through `init()`, which is wasm-bindgen's `--target web` build and fetches a `.wasm` URL —
// there is no such fetch in jsdom, and the rejection would surface as an unhandled rejection
// from the component's `$effect` rather than as anything this test is about. The linting itself
// is tested against the real module in `lib/expr/`; this file is about what the modal renders.
import { flushSync, mount, unmount } from 'svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import ConfigModal from './ConfigModal.svelte';
import type { BlockInstance, BlockManifest, PropertyDescriptor } from '../api/types';

vi.mock('../expr/lint', () => ({
  ensureLinterReady: () => Promise.resolve(),
  isLinterReady: () => true,
  lintExpression: () => ({ ok: true, signal_dependent: false, diagnostics: [], unbound: [] }),
}));

function manifest(properties: PropertyDescriptor[], overrides: Partial<BlockManifest> = {}): BlockManifest {
  return {
    block_ref: 'ghcr.io/tlugger/filter:1.2.0',
    name: 'filter',
    version: '1.2.0',
    abi: { major: 0, minor: 1 },
    capabilities: [],
    inputs: [],
    outputs: [],
    properties,
    targets: ['wasm32-unknown-unknown'],
    aot: [],
    ...overrides,
  };
}

function instance(props: Record<string, string> = {}): BlockInstance {
  return { id: 'b1', block: 'ghcr.io/tlugger/filter:1.2.0', props };
}

let mounted: Record<string, unknown> | null = null;
let target: HTMLElement | null = null;

function open(
  blockManifest: BlockManifest,
  blockInstance: BlockInstance = instance(),
  onAccept: (changed: Record<string, string | undefined>) => void = () => {},
): HTMLElement {
  target = document.createElement('div');
  document.body.appendChild(target);
  mounted = mount(ConfigModal, {
    target,
    props: {
      instance: blockInstance,
      manifest: blockManifest,
      manifests: [blockManifest],
      blocks: { [blockInstance.id]: blockInstance },
      connections: [],
      onAccept,
      onCancel: () => {},
    },
  });
  return target;
}

afterEach(() => {
  if (mounted) unmount(mounted);
  mounted = null;
  target?.remove();
  target = null;
});

describe('ConfigModal is the manifest — DESIGNER §3.3 absence rule (eieio-m9s.45)', () => {
  it('renders a field for every property the manifest declares', () => {
    const el = open(
      manifest([
        { name: 'threshold', type: 'float', required: true },
        { name: 'label', type: 'string', required: false },
      ]),
      instance({ threshold: '20' }),
    );
    expect(el.textContent).toContain('threshold');
    expect(el.textContent).toContain('label');
    expect(el.textContent).not.toContain('This block has no properties');
  });

  it('"This block has no properties" is now the manifest saying so, never a missing manifest', () => {
    // The distinction this whole bead is about. The sentence is unchanged; what changed is that
    // it can only be reached through a manifest that declares an empty property list, because
    // the `manifest` prop is required and `App.svelte` refuses to open the modal without one.
    const el = open(manifest([]));
    expect(el.textContent).toContain('This block has no properties');
  });

  it("the type line shows the manifest's own name, not the raw reference", () => {
    // Absence used to fall through to `instance.block` here — the modal captioning a block with
    // the string it failed to resolve, which reads as a successful render of a badly named block.
    const el = open(manifest([]));
    const type = el.querySelector('.modal__type');
    expect(type?.textContent).toContain('filter');
    expect(type?.textContent).not.toContain('ghcr.io/tlugger/filter:1.2.0');
  });

  it('the docs panel describes the manifest it was given', () => {
    // The `{:else} No manifest resolved for …` branch this replaces was the only place the modal
    // admitted it had nothing — in a panel behind a `?` button, while the rest of it rendered as
    // though everything were fine.
    const el = open(manifest([], { description: 'drops signals below a threshold' }));
    (el.querySelector('.modal__docs-toggle') as HTMLButtonElement).click();
    flushSync();
    expect(el.textContent).toContain('drops signals below a threshold');
  });

  it('an accept is computed against the manifest properties, so an override is not lost', () => {
    // The concrete harm of the old absence render: `handleAccept` iterates the manifest's
    // property list, so with no manifest it iterated nothing and an accept silently reduced the
    // operator's edit to the block's name. With the manifest required, a changed property is
    // always in the list to be compared.
    let seen: Record<string, string | undefined> | null = null;
    const el = open(
      manifest([{ name: 'threshold', type: 'float', required: true }]),
      instance({ threshold: '20' }),
      (changed) => {
        seen = changed;
      },
    );
    const input = el.querySelector('.modal__properties input, .modal__properties textarea') as
      | HTMLInputElement
      | HTMLTextAreaElement;
    input.value = '30';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();
    const accept = [...el.querySelectorAll('button')].find((b) => b.textContent?.trim() === 'Accept');
    (accept as HTMLButtonElement).click();
    expect(seen).toEqual({ threshold: '30' });
  });
});
