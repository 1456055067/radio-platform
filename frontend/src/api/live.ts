import type { LiveMessage } from './types';

export type LiveState = 'connecting' | 'open' | 'closed' | 'expired';

export interface LiveOptions {
  token: () => Promise<string>;
  scannerId?: string;
  onMessage: (m: LiveMessage) => void;
  onState?: (s: LiveState) => void;
  /** For tests. */
  socketFactory?: (url: string, protocols: string[]) => WebSocket;
  location?: Pick<Location, 'protocol' | 'host'>;
}

const MAX_BACKOFF_MS = 30_000;

/**
 * The gateway's live stream. Browsers cannot set an Authorization header
 * on a WebSocket, so the token travels as a subprotocol
 * (`radio-platform.bearer.<token>`) next to `radio-platform.v1`.
 * Reconnects with capped exponential backoff; close code 4001 (session
 * expired) fetches a fresh token before reconnecting.
 */
export class LiveStream {
  private ws: WebSocket | null = null;
  private stopped = false;
  private attempt = 0;
  private timer: ReturnType<typeof setTimeout> | null = null;

  constructor(private readonly opts: LiveOptions) {}

  start(): void {
    this.stopped = false;
    void this.connect();
  }

  stop(): void {
    this.stopped = true;
    if (this.timer) clearTimeout(this.timer);
    this.timer = null;
    const ws = this.ws;
    this.ws = null;
    if (ws && ws.readyState <= 1) ws.close(1000, 'client closing');
  }

  private url(): string {
    const loc = this.opts.location ?? window.location;
    const scheme = loc.protocol === 'https:' ? 'wss:' : 'ws:';
    const q = this.opts.scannerId ? `?scanner_id=${encodeURIComponent(this.opts.scannerId)}` : '';
    return `${scheme}//${loc.host}/api/v1/events/stream${q}`;
  }

  private async connect(): Promise<void> {
    if (this.stopped) return;
    this.opts.onState?.('connecting');
    let token: string;
    try {
      token = await this.opts.token();
    } catch {
      this.schedule();
      return;
    }
    if (this.stopped) return;
    const make = this.opts.socketFactory ?? ((u, p) => new WebSocket(u, p));
    const ws = make(this.url(), ['radio-platform.v1', `radio-platform.bearer.${token}`]);
    this.ws = ws;
    ws.onopen = () => {
      this.attempt = 0;
      this.opts.onState?.('open');
    };
    ws.onmessage = (ev: MessageEvent) => {
      if (typeof ev.data !== 'string') return;
      try {
        this.opts.onMessage(JSON.parse(ev.data) as LiveMessage);
      } catch {
        // Ignore malformed frames; the stream continues.
      }
    };
    ws.onclose = (ev: CloseEvent) => {
      if (this.ws !== ws) return;
      this.ws = null;
      if (this.stopped) return;
      this.opts.onState?.(ev.code === 4001 ? 'expired' : 'closed');
      this.schedule();
    };
  }

  private schedule(): void {
    if (this.stopped) return;
    const delay = Math.min(MAX_BACKOFF_MS, 500 * 2 ** this.attempt) * (0.75 + Math.random() * 0.5);
    this.attempt = Math.min(this.attempt + 1, 10);
    this.timer = setTimeout(() => void this.connect(), delay);
  }
}
