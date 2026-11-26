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

## Browser end-to-end tests

`e2e/` holds Playwright tests that drive the built console in headless
Chromium against `radio-devstack` (simulated scanners, agents, gateway and
the stub transcription service). They cover sign-in and roles, navigation,
the control lease (two users at once, admin revoke, a volume change read
back from the simulator), live WebRTC audio, a recorded and transcribed
transmission found by transcript search, Analytics, and audit chain
verification.

```bash
npx playwright install chromium   # once
npm run build && npm run e2e      # builds and starts radio-devstack on 127.0.0.1:8090
```

`E2E_BASE_URL=http://127.0.0.1:8080 npm run e2e` reuses a devstack you
already started instead (with `--ui-dir frontend/dist`). Tests run one at a
time because they share one backend. Failures keep a trace and screenshot
in `test-results/` (`npx playwright show-trace <file>`).

These tests exercise the console and the platform against simulators only;
they say nothing about real SDS200 behaviour.

## Layout

| Path | Contents |
|---|---|
| `src/api` | Typed client (no retries), live WebSocket stream, API types mirroring the gateway |
| `src/auth` | `/ui-config.json` loading, OIDC (code + PKCE, in-memory tokens) and the dev-stack role picker |
| `src/hooks` | Shared live state, the directory of sites, scanners and agents, leases, queries |
| `src/pages` | Dashboard, Scanners, Virtual scanner, Sites and agents, Transmissions, Events, Audit |
| `src/components` | Scanner display, status, signal bars, site map |
| `e2e` | Playwright browser tests against `radio-devstack` |

## Rules

- The UI hides what `/me` says the user cannot do, but the gateway
  authorizes every request. The UI is not a security boundary.
- State-changing requests are never retried. Sliders are debounced and
  serialized.
- Scanner-supplied text is rendered as text only; never use
  `dangerouslySetInnerHTML`.
- Values the scanner did not report show as `—`. Never fill them in.
