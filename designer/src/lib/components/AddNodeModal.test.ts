// eieio-m9s.34: pins the two ways this form would be quietly wrong rather than broken.
//
// DESIGNER §3.1: a node's class "is stated, not discovered, and it is the only field that could
// not be" — every other fact about a node comes back from a probe, and a leaf answers none. So
// the `<select>` this form renders MUST start on `'daemon'`, not on an empty/placeholder option:
// an operator who fills in name/address and submits without touching the class field must still
// register a daemon, because that is what "defaults to daemon" means in practice. The first test
// below submits without touching the select and asserts the payload says `daemon`; flipping the
// component's own default to `'leaf'` (this bead's required negative proof) fails it — see the
// final report for the transcript.
//
// The token field is `type="password"` (§3.1: write-only, never re-displayed) — the second test
// checks the DOM attribute directly, the same "assert by attribute, not by vibes" posture
// `NavigatorTree.test.ts` already takes for its own two note classes.
//
// The third test is the harness's generic "a rejected call renders an error" case, applied here:
// a `SessionRequiredError` or any other rejection from `onSubmit` must show up in the form, never
// vanish silently.
import { mount, unmount } from 'svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import AddNodeModal from './AddNodeModal.svelte';
import type { NodeSummary } from '../api/types';

function renderModal(onSubmit: (input: unknown) => Promise<NodeSummary>) {
  const target = document.createElement('div');
  document.body.appendChild(target);
  const exports = mount(AddNodeModal, {
    target,
    props: {
      systemId: 1,
      onSubmit: onSubmit as never,
      onCancel: () => {},
    },
  });
  return { target, exports };
}

function setValue(el: Element | null, value: string) {
  if (!el) throw new Error('element not found');
  (el as HTMLInputElement).value = value;
  el.dispatchEvent(new Event('input', { bubbles: true }));
}

async function fillAndSubmit(target: HTMLElement, { name = 'node-a', address = 'http://10.0.0.1:7373' } = {}) {
  setValue(target.querySelector('#eio-new-node-name'), name);
  setValue(target.querySelector('#eio-new-node-address'), address);
  const form = target.querySelector('form')!;
  form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
  // Let the async `onSubmit` handler's microtasks (and Svelte's own reactive flush) settle.
  await Promise.resolve();
  await Promise.resolve();
}

describe('AddNodeModal — class defaults to daemon (eieio-m9s.34, DESIGNER §3.1)', () => {
  afterEach(() => {
    document.body.innerHTML = '';
  });

  it('submits class "daemon" when the operator never touches the class field', async () => {
    const onSubmit = vi.fn().mockResolvedValue({ id: 1, system_id: 1, name: 'node-a', class: 'daemon', address: 'x' });
    const { target, exports } = renderModal(onSubmit);

    await fillAndSubmit(target);

    expect(onSubmit).toHaveBeenCalledTimes(1);
    expect(onSubmit.mock.calls[0][0]).toMatchObject({ class: 'daemon' });

    unmount(exports);
  });

  it("the class <select> itself starts on 'daemon', not an unset/placeholder value", () => {
    const { target, exports } = renderModal(vi.fn());
    const select = target.querySelector('#eio-new-node-class') as HTMLSelectElement;
    expect(select.value).toBe('daemon');
    unmount(exports);
  });
});

describe('AddNodeModal — the token field is write-only (eieio-m9s.34, DESIGNER §3.1)', () => {
  afterEach(() => {
    document.body.innerHTML = '';
  });

  it('renders the token input as type="password"', () => {
    const { target, exports } = renderModal(vi.fn());
    const token = target.querySelector('#eio-new-node-token');
    expect(token?.getAttribute('type')).toBe('password');
    unmount(exports);
  });

  it('marks the token required, because the backend does', async () => {
    // eieio-m9s.34's contract had this optional, citing DESIGNER §3.1 as letting "a node be
    // named before its token is known". That sentence is about the CLI's nodes.toml — a
    // different config surface, where the field genuinely is an `Option`. The Designer's
    // `POST /api/nodes` takes `token: String` and validates it non-empty
    // (`crates/designer/src/api/nodes.rs`), and §3.1's table lists it un-suffixed. An
    // "optional" field the backend rejects is worse than a required one.
    const onSubmit = vi.fn().mockResolvedValue({ id: 1, system_id: 1, name: 'node-a', class: 'daemon', address: 'x' });
    const { target, exports } = renderModal(onSubmit);

    const token = target.querySelector<HTMLInputElement>('#eio-new-node-token');
    expect(token).not.toBeNull();
    expect(token!.required).toBe(true);
    unmount(exports);
  });
});

describe('AddNodeModal — a rejected submit renders an error (eieio-m9s.34)', () => {
  afterEach(() => {
    document.body.innerHTML = '';
  });

  it('shows the rejection message and does not swallow it', async () => {
    const onSubmit = vi.fn().mockRejectedValue(new Error('node already registered'));
    const { target, exports } = renderModal(onSubmit);

    await fillAndSubmit(target);

    const error = target.querySelector('.onboard-modal__error');
    expect(error).toBeTruthy();
    expect(error!.textContent).toContain('node already registered');

    unmount(exports);
  });
});
