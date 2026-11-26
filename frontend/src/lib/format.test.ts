import type { Transmission } from '../api/types';
import { csvField, formatDuration, formatFrequency, parseMhz, scannerStatus, timeline, transmissionsCsv } from './format';

describe('frequency formatting', () => {
  it('formats at the scanner’s 100 Hz resolution', () => {
    expect(formatFrequency(851_012_500)).toBe('851.0125 MHz');
    expect(formatFrequency(null)).toBe('—');
  });
  it('parses MHz input strictly', () => {
    expect(parseMhz('851.0125')).toBe(851_012_500);
    expect(parseMhz(' 162.55 ')).toBe(162_550_000);
    expect(parseMhz('8')).toBeNull(); // below 25 MHz
    expect(parseMhz('851,0125')).toBeNull();
    expect(parseMhz('1e3')).toBeNull();
    expect(parseMhz('')).toBeNull();
  });
});

describe('durations', () => {
  it('scales units', () => {
    expect(formatDuration(450)).toBe('450 ms');
    expect(formatDuration(2500)).toBe('2.5 s');
    expect(formatDuration(125_000)).toBe('2 min 5 s');
  });
});

describe('scanner status', () => {
  const base = { enabled: true, agent_connected: true, health: { state: 'healthy' as const } };
  it('prefers the most limiting condition', () => {
    expect(scannerStatus(base)).toMatchObject({ online: true, tone: 'success' });
    expect(scannerStatus({ ...base, enabled: false })).toMatchObject({ online: false, label: 'Disabled' });
    expect(scannerStatus({ ...base, agent_connected: false })).toMatchObject({ online: false, label: 'Agent offline' });
    expect(scannerStatus({ ...base, health: { state: 'unreachable', consecutive_failures: 4 } })).toMatchObject({ online: false, tone: 'error' });
    expect(scannerStatus({ ...base, health: null })).toMatchObject({ online: false, tone: 'pending' });
  });
});

describe('CSV export', () => {
  it('quotes and neutralises formulas', () => {
    expect(csvField('a,b')).toBe('"a,b"');
    expect(csvField('say "hi"')).toBe('"say ""hi"""');
    expect(csvField('=HYPERLINK("x")')).toBe(`"'=HYPERLINK(""x"")"`);
    expect(csvField('@cmd')).toBe("'@cmd");
    expect(csvField(-74)).toBe('-74');
    expect(csvField(null)).toBe('');
  });
  it('writes a header and one line per row', () => {
    const t = { id: 'x', started_at: '2026-11-25T00:00:00Z', channel_name: '+evil' } as Transmission;
    const csv = transmissionsCsv([t]);
    const lines = csv.trim().split('\r\n');
    expect(lines).toHaveLength(2);
    expect(lines[0]).toMatch(/^id,started_at,/);
    expect(lines[1]).toContain("'+evil");
  });
});

describe('timeline', () => {
  it('buckets by start time and ignores out-of-range rows', () => {
    const at = (ms: number) => ({ started_at: new Date(ms).toISOString() }) as Transmission;
    const b = timeline([at(0), at(10), at(55), at(99), at(500)], 0, 100, 2);
    expect(b.map((x) => x.y)).toEqual([2, 2]);
  });
});
