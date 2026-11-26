import { LiveStream } from './live';

class FakeSocket {
  static all: FakeSocket[] = [];
  readyState = 0;
  onopen: (() => void) | null = null;
  onmessage: ((e: MessageEvent) => void) | null = null;
  onclose: ((e: CloseEvent) => void) | null = null;
  closed = false;
  constructor(
    readonly url: string,
    readonly protocols: string[],
  ) {
    FakeSocket.all.push(this);
  }
  close() {
    this.closed = true;
    this.readyState = 3;
  }
}

describe('LiveStream', () => {
  beforeEach(() => {
    FakeSocket.all = [];
    vi.useFakeTimers();
  });
  afterEach(() => vi.useRealTimers());

  it('authenticates with subprotocols, parses messages and reconnects', async () => {
    const msgs: unknown[] = [];
    const states: string[] = [];
    let n = 0;
    const s = new LiveStream({
      token: async () => `tok${++n}`,
      scannerId: 'abc',
      onMessage: (m) => msgs.push(m),
      onState: (st) => states.push(st),
      socketFactory: (u, p) => new FakeSocket(u, p) as unknown as WebSocket,
      location: { protocol: 'https:', host: 'console.example' },
    });
    s.start();
    await vi.advanceTimersByTimeAsync(0);
    const first = FakeSocket.all[0];
    expect(first?.url).toBe('wss://console.example/api/v1/events/stream?scanner_id=abc');
    expect(first?.protocols).toEqual(['radio-platform.v1', 'radio-platform.bearer.tok1']);
    first?.onopen?.();
    first?.onmessage?.({ data: '{"kind":"lagged","skipped":2}' } as MessageEvent);
    first?.onmessage?.({ data: 'not json' } as MessageEvent);
    expect(msgs).toEqual([{ kind: 'lagged', skipped: 2 }]);

    first?.onclose?.({ code: 4001 } as CloseEvent);
    expect(states).toContain('expired');
    await vi.advanceTimersByTimeAsync(1000);
    const second = FakeSocket.all[1];
    expect(second?.protocols[1]).toBe('radio-platform.bearer.tok2');

    s.stop();
    expect(second?.closed).toBe(true);
    await vi.advanceTimersByTimeAsync(60_000);
    expect(FakeSocket.all).toHaveLength(2);
  });
});
