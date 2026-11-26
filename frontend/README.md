# frontend: the Radio Platform web console

React 19, TypeScript, Vite and the Cloudscape Design System. The gateway
serves the built files (`[http.ui]`), so the console and API share an
origin.

**Status:** Phases 3 and 4. Live audio and recording playback work against
the dev stack. Transcripts are shown as "not available" until Phase 5. Only
simulated scanners have been used.

## Develop

```bash
npm ci                                       # .npmrc disables install scripts
cargo run -p radio-devstack                  # in another terminal; simulated stack on 127.0.0.1:8080
npm run dev                                  # http://127.0.0.1:5173, proxies /api, /dev and /ui-config.json
```

`RADIO_BACKEND=http://host:port npm run dev` points the proxy at another
backend.

## Check

```bash
npm audit && npm run typecheck && npm test && npm run build
```

## Layout

| Path | Contents |
|---|---|
| `src/api` | Typed client (no retries), live WebSocket stream, API types mirroring the gateway |
| `src/auth` | `/ui-config.json` loading, OIDC (code + PKCE, in-memory tokens) and the dev-stack role picker |
| `src/hooks` | Shared live state, the directory of sites, scanners and agents, leases, queries |
| `src/pages` | Dashboard, Scanners, Virtual scanner, Sites and agents, Transmissions, Events, Audit |
| `src/components` | Scanner display, status, signal bars, site map |

## Rules

- The UI hides what `/me` says the user cannot do, but the gateway
  authorizes every request. The UI is not a security boundary.
- State-changing requests are never retried. Sliders are debounced and
  serialized.
- Scanner-supplied text is rendered as text only; never use
  `dangerouslySetInnerHTML`.
- Values the scanner did not report show as `—`. Never fill them in.
