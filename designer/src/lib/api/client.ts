// The single seam between the shell and the backend.
//
// Today every export below is re-exported straight from mock.ts. Swapping
// to the real backend (crates/designer's axum binary, DESIGNER §3.1) means
// rewriting the bodies of these functions to call `fetch('/api/...')` and
// leaving every call site elsewhere in `src/` untouched — this file is the
// only one that is allowed to know the mock exists.
//
// DESIGNER §3.1's split matters here: systems/nodes/manifests are the
// backend's own small REST surface; anything service- or block-shaped is
// reached through the one catch-all proxy at
// `/api/nodes/{id}/daemon/{*path}`, forwarded verbatim to that node's
// daemon (DAEMON-SPEC §9). Nothing in this file, or anywhere else in this
// SPA, ever holds a node's bearer token — DESIGNER §3.1 is explicit that
// it never reaches the browser, and that stays true regardless of what
// this file's bodies end up doing.

export {
  listSystems,
  listNodes,
  listBlockManifests,
  listServices,
  getService,
  startService,
  stopService,
  reloadService,
  serviceEdit,
  putService,
} from './mock';

export type * from './types';
