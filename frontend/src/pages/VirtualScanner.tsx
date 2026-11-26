import { useCallback, useEffect, useRef, useState } from 'react';
import { useParams } from 'react-router';
import Alert from '@cloudscape-design/components/alert';
import Badge from '@cloudscape-design/components/badge';
import Box from '@cloudscape-design/components/box';
import BreadcrumbGroup from '@cloudscape-design/components/breadcrumb-group';
import Button from '@cloudscape-design/components/button';
import ColumnLayout from '@cloudscape-design/components/column-layout';
import Container from '@cloudscape-design/components/container';
import ContentLayout from '@cloudscape-design/components/content-layout';
import FormField from '@cloudscape-design/components/form-field';
import Grid from '@cloudscape-design/components/grid';
import Header from '@cloudscape-design/components/header';
import Input from '@cloudscape-design/components/input';
import KeyValuePairs from '@cloudscape-design/components/key-value-pairs';
import Slider from '@cloudscape-design/components/slider';
import SpaceBetween from '@cloudscape-design/components/space-between';
import StatusIndicator from '@cloudscape-design/components/status-indicator';
import Table from '@cloudscape-design/components/table';
import Toggle from '@cloudscape-design/components/toggle';
import { useNavigate } from 'react-router';
import { describeError } from '../api/client';
import type { AudioStatus, HoldKind, Page, Scanner, ScannerCommand, TelemetryResponse, Transmission } from '../api/types';
import { LiveAudio } from '../components/LiveAudio';
import { useAuth } from '../auth/AuthContext';
import { ScannerDisplay } from '../components/ScannerDisplay';
import { ScannerStatus } from '../components/ScannerStatus';
import { isFresh } from '../hooks/activity';
import { useLive } from '../hooks/LiveContext';
import { useLease } from '../hooks/useLease';
import { useNotify } from '../hooks/Notifications';
import { useNow } from '../hooks/useNow';
import { useQuery } from '../hooks/useQuery';
import { useDirectory } from '../hooks/Directory';
import { dash, formatDuration, formatFrequency, formatTime, parseMhz, scannerStatus } from '../lib/format';

export const VOLUME_MAX = 29;
export const SQUELCH_MAX = 19;

/** Front-panel keys. Only `E` and `.` appear in the obtained spec; the rest are community-reported. */
export const KEYS: { code: string; label: string; documented?: boolean; unverified?: boolean }[] = [
  { code: 'M', label: 'Menu' },
  { code: 'F', label: 'Func' },
  { code: 'L', label: 'Avoid' },
  { code: 'Y', label: 'Replay' },
  { code: 'A', label: 'System' },
  { code: 'B', label: 'Dept' },
  { code: 'C', label: 'Channel' },
  { code: 'Z', label: 'ZIP' },
  { code: 'R', label: 'Range' },
  { code: 'T', label: 'Svc type', unverified: true },
  { code: '<', label: '◀ Knob' },
  { code: '^', label: 'Knob push' },
  { code: '>', label: 'Knob ▶' },
  { code: 'V', label: 'Vol push' },
  { code: 'Q', label: 'Sql push', unverified: true },
];
const KEYPAD = ['1', '2', '3', '4', '5', '6', '7', '8', '9', '.', '0', 'E'];

/**
 * Sends the latest requested level once input settles. Never retries; a
 * value changed while a command is in flight is sent after it completes.
 */
function useSettledCommand(send: (level: number) => Promise<void>, delayMs = 350) {
  const pending = useRef<number | null>(null);
  const inflight = useRef(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const sendRef = useRef(send);
  sendRef.current = send;
  const pump = useCallback(async () => {
    if (inflight.current || pending.current === null) return;
    const v = pending.current;
    pending.current = null;
    inflight.current = true;
    try {
      await sendRef.current(v);
    } finally {
      inflight.current = false;
      void pump();
    }
  }, []);
  useEffect(() => () => {
    if (timer.current) clearTimeout(timer.current);
  }, []);
  return useCallback(
    (v: number) => {
      pending.current = v;
      if (timer.current) clearTimeout(timer.current);
      timer.current = setTimeout(() => void pump(), delayMs);
    },
    [pump, delayMs],
  );
}

export function VirtualScanner() {
  const { id = '' } = useParams();
  const { api, me, can } = useAuth();
  const live = useLive();
  const dir = useDirectory();
  const notify = useNotify();
  const navigate = useNavigate();
  const now = useNow(1000);

  const [detailAt, setDetailAt] = useState(0);
  const detail = useQuery(
    async (signal) => {
      const d = await api.get<Scanner>(`/scanners/${id}`, undefined, signal);
      setDetailAt(Date.now());
      return d;
    },
    [api, id],
    10_000,
  );
  const audioStatus = useQuery((signal) => api.get<AudioStatus>(`/scanners/${id}/audio`, undefined, signal), [api, id]);
  const initial = useQuery((signal) => api.get<TelemetryResponse>(`/scanners/${id}/telemetry`, undefined, signal), [api, id]);
  const history = useQuery(
    (signal) =>
      can('event_view')
        ? api.get<Page<Transmission>>('/transmissions', { scanner_id: id, limit: 10 }, signal)
        : Promise.resolve<Page<Transmission>>({ items: [], next_cursor: null }),
    [api, id, live.recent.find((e) => e.scanner_id === id && e.event.type !== 'health_changed')?.at],
  );

  const scanner = detail.data;
  const lease = useLease(api, id, me.subject, scanner ? scanner.control : undefined, detailAt, (why) =>
    notify('warning', why, 'Scanner control'),
  );

  const liveSnap = live.telemetry.get(id);
  const snapshot = liveSnap ?? initial.data?.snapshot ?? null;
  const online = scanner ? scannerStatus({ ...scanner, health: live.health.get(id) ?? scanner.health }).online : false;
  const stale = !online || !isFresh(snapshot ?? undefined, now);

  const [volume, setVolume] = useState<number | null>(null);
  const [squelch, setSquelch] = useState<number | null>(null);
  const [longPress, setLongPress] = useState(false);
  const [freq, setFreq] = useState('');
  const [busy, setBusy] = useState<string | null>(null);

  // Follow the scanner's reported levels unless the user is adjusting them.
  const editing = useRef<{ vol: number; sql: number }>({ vol: 0, sql: 0 });
  useEffect(() => {
    if (snapshot?.volume !== null && snapshot?.volume !== undefined && Date.now() - editing.current.vol > 2000)
      setVolume(snapshot.volume);
    if (snapshot?.squelch !== null && snapshot?.squelch !== undefined && Date.now() - editing.current.sql > 2000)
      setSquelch(snapshot.squelch);
  }, [snapshot?.volume, snapshot?.squelch]);

  const canControl = can('scanner_control');
  const holding = !!lease.lease;
  const controlsEnabled = holding && !!scanner?.agent_connected;

  const run = useCallback(
    async (label: string, path: string, init: { method: 'POST' | 'PUT'; body?: unknown }) => {
      if (!lease.lease) return;
      setBusy(label);
      try {
        await api.request(path, { ...init, lease: lease.lease.id });
      } catch (e) {
        notify('error', describeError(e), `${label} failed`);
      } finally {
        setBusy(null);
      }
    },
    [api, lease.lease, notify],
  );
  const command = (label: string, c: ScannerCommand) => run(label, `/scanners/${id}/commands`, { method: 'POST', body: c });

  const sendVolume = useSettledCommand((level) => run('Volume', `/scanners/${id}/volume`, { method: 'PUT', body: { level } }));
  const sendSquelch = useSettledCommand((level) => run('Squelch', `/scanners/${id}/squelch`, { method: 'PUT', body: { level } }));

  const hold = (kind: HoldKind, index: number | null | undefined, label: string) =>
    index === null || index === undefined ? undefined : () => void run(`Hold ${label}`, `/scanners/${id}/hold`, { method: 'POST', body: { kind, index } });

  if (detail.error && !scanner) {
    return (
      <Box padding="l">
        <Alert type="error" header="Scanner unavailable">
          {detail.error}
        </Alert>
      </Box>
    );
  }

  const site = scanner ? dir.siteById.get(scanner.site_id) : undefined;
  const control = scanner?.control ?? null;
  const expiresIn = control ? Math.max(0, Math.round((new Date(control.expires_at).getTime() - now) / 1000)) : 0;
  const freqHz = parseMhz(freq);

  const recent = history.data?.items ?? [];

  return (
    <ContentLayout
      breadcrumbs={
        <BreadcrumbGroup
          items={[
            { text: 'Scanners', href: '/scanners' },
            { text: scanner?.name ?? '…', href: `/scanners/${id}` },
          ]}
          onFollow={(e) => {
            e.preventDefault();
            navigate(e.detail.href);
          }}
        />
      }
      header={
        <Header
          variant="h1"
          description={[site?.name, scanner ? `${scanner.host}:${scanner.port}` : null, scanner?.model, scanner?.firmware].filter(Boolean).join(' · ')}
          info={scanner && <ScannerStatus scanner={scanner} liveHealth={live.health.get(id)} />}
        >
          {scanner?.name ?? 'Scanner'}
        </Header>
      }
    >
      <Grid gridDefinition={[{ colspan: { default: 12, l: 7 } }, { colspan: { default: 12, l: 5 } }]}>
        <SpaceBetween size="l">
          <Container header={<Header description="Updates in real time from the scanner's status reports.">Display</Header>}>
            <ScannerDisplay snapshot={snapshot} stale={stale} />
          </Container>

          <Container header={<Header>Channel information</Header>}>
            <KeyValuePairs
              columns={3}
              items={[
                { label: 'Frequency', value: formatFrequency(snapshot?.frequency_hz) },
                { label: 'Modulation', value: dash(snapshot?.modulation) },
                { label: 'Talkgroup', value: dash(snapshot?.tgid) },
                { label: 'Unit ID', value: dash(snapshot?.unit_id) },
                { label: 'System', value: dash(snapshot?.system) },
                { label: 'System type', value: dash(snapshot?.system_type) },
                { label: 'Department', value: dash(snapshot?.department) },
                { label: 'Site', value: dash(snapshot?.site) },
                { label: 'Service type', value: dash(snapshot?.service_type) },
                {
                  label: 'Signal',
                  value:
                    snapshot?.signal_bars === null || snapshot?.signal_bars === undefined
                      ? '—'
                      : `${snapshot.signal_bars} / 4${snapshot.rssi !== null ? ` (RSSI ${snapshot.rssi})` : ''}`,
                },
                { label: 'Squelch', value: snapshot?.muted === false ? 'Open (receiving)' : snapshot?.muted ? 'Closed' : '—' },
                { label: 'Mode', value: dash(snapshot?.mode) },
              ]}
            />
          </Container>

          <Container header={<Header description="Opus over WebRTC from the site agent's single connection to the scanner.">Live audio</Header>}>
            {!scanner ? null : !scanner.audio_enabled ? (
              <StatusIndicator type="stopped">Audio is disabled for this scanner{can('scanner_manage') ? ' (enable it under Scanners)' : ''}.</StatusIndicator>
            ) : !can('audio_listen') ? (
              <Box variant="small" color="text-body-secondary">Your role cannot listen (audio_listen is required).</Box>
            ) : (
              <LiveAudio scannerId={id} state={live.audio.get(id) ?? audioStatus.data?.state} />
            )}
            {scanner?.recording_enabled && (
              <Box padding={{ top: 's' }}>
                <StatusIndicator type="info">Transmissions on this scanner are recorded.</StatusIndicator>
              </Box>
            )}
          </Container>

          {can('event_view') && (
            <Table
              variant="container"
              header={<Header counter={`(${recent.length})`}>Recent transmissions</Header>}
              items={recent}
              trackBy="id"
              loading={history.loading && !history.data}
              empty={<Box color="text-body-secondary">None recorded yet.</Box>}
              columnDefinitions={[
                { id: 't', header: 'Started', cell: (t) => formatTime(t.started_at) },
                { id: 'f', header: 'Frequency', cell: (t) => formatFrequency(t.frequency_hz) },
                { id: 'c', header: 'Channel', cell: (t) => dash(t.channel_name) },
                { id: 'g', header: 'Talkgroup', cell: (t) => dash(t.tgid) },
                { id: 'u', header: 'Unit', cell: (t) => dash(t.unit_id) },
                { id: 'd', header: 'Duration', cell: (t) => (t.ended_at ? formatDuration(t.duration_ms) : <StatusIndicator type="in-progress">Active</StatusIndicator>) },
              ]}
            />
          )}
        </SpaceBetween>

        <SpaceBetween size="l">
          <Container header={<Header>Control</Header>}>
            <SpaceBetween size="s">
              {control ? (
                holding ? (
                  <StatusIndicator type="success">You have control · expires in {expiresIn} s (renews automatically)</StatusIndicator>
                ) : (
                  <StatusIndicator type="warning">
                    Controlled by {control.holder} · expires in {expiresIn} s
                  </StatusIndicator>
                )
              ) : (
                <StatusIndicator type="info">Nobody has control. Viewing is always allowed.</StatusIndicator>
              )}
              {lease.error && <Alert type="error">{lease.error}</Alert>}
              {!canControl ? (
                <Box variant="small" color="text-body-secondary">
                  Your role is read-only (scanner_control is required to take control).
                </Box>
              ) : (
                <SpaceBetween direction="horizontal" size="xs">
                  {holding ? (
                    <Button onClick={() => void lease.release()} loading={lease.busy}>
                      Release control
                    </Button>
                  ) : (
                    <Button
                      variant="primary"
                      onClick={() => void lease.acquire().then(detail.reload)}
                      loading={lease.busy}
                      disabled={!!control && control.holder !== me.subject}
                    >
                      Take control
                    </Button>
                  )}
                  {can('scanner_admin') && control && !holding && control.lease_id && (
                    <Button onClick={() => void lease.revoke(control.lease_id ?? '').then(detail.reload)} loading={lease.busy}>
                      Revoke (admin)
                    </Button>
                  )}
                </SpaceBetween>
              )}
              {scanner && !scanner.agent_connected && <Alert type="warning">The site agent is offline; commands cannot be delivered.</Alert>}
            </SpaceBetween>
          </Container>

          <Container header={<Header description={holding ? undefined : 'Take control to change settings.'}>Audio levels</Header>}>
            <ColumnLayout columns={1}>
              <FormField label={`Volume${volume !== null ? `: ${volume}` : ''}`}>
                <Slider
                  ariaLabel="Volume"
                  min={0}
                  max={VOLUME_MAX}
                  value={volume ?? 0}
                  disabled={!controlsEnabled}
                  onChange={({ detail: d }) => {
                    editing.current.vol = Date.now();
                    setVolume(d.value);
                    sendVolume(d.value);
                  }}
                />
              </FormField>
              <FormField label={`Squelch${squelch !== null ? `: ${squelch}` : ''}`}>
                <Slider
                  ariaLabel="Squelch"
                  min={0}
                  max={SQUELCH_MAX}
                  value={squelch ?? 0}
                  disabled={!controlsEnabled}
                  onChange={({ detail: d }) => {
                    editing.current.sql = Date.now();
                    setSquelch(d.value);
                    sendSquelch(d.value);
                  }}
                />
              </FormField>
            </ColumnLayout>
          </Container>

          <Container
            header={
              <Header
                description="Resume restarts scanning from the top of the list (see protocol limitations)."
                info={snapshot?.holding ? <Badge color="blue">Holding</Badge> : undefined}
              >
                Scan and hold
              </Header>
            }
          >
            <SpaceBetween size="s">
              <SpaceBetween direction="horizontal" size="xs">
                {(
                  [
                    ['system', snapshot?.system_index, 'system'],
                    ['department', snapshot?.department_index, 'department'],
                    ['channel', snapshot?.channel_index, 'channel'],
                  ] as const
                ).map(([kind, index, label]) => {
                  const onClick = hold(kind, index, label);
                  return (
                    <Button key={kind} disabled={!controlsEnabled || !onClick} loading={busy === `Hold ${label}`} onClick={onClick}>
                      Hold {label}
                    </Button>
                  );
                })}
                <Button
                  variant="primary"
                  disabled={!controlsEnabled}
                  loading={busy === 'Resume'}
                  onClick={() => void run('Resume', `/scanners/${id}/resume`, { method: 'POST' })}
                >
                  Resume scan
                </Button>
              </SpaceBetween>
              <FormField
                label="Tune to frequency (quick search hold)"
                errorText={freq && freqHz === null ? 'Enter MHz between 25 and 1300, for example 851.0125' : undefined}
              >
                <SpaceBetween direction="horizontal" size="xs">
                  <Input value={freq} onChange={({ detail: d }) => setFreq(d.value)} placeholder="851.0125" inputMode="decimal" disabled={!controlsEnabled} ariaLabel="Frequency in MHz" />
                  <Button
                    disabled={!controlsEnabled || freqHz === null}
                    loading={busy === 'Tune'}
                    onClick={() => freqHz !== null && void command('Tune', { command: 'quick_search_hold', frequency_hz: freqHz })}
                  >
                    Tune
                  </Button>
                </SpaceBetween>
              </FormField>
            </SpaceBetween>
          </Container>

          <Container
            header={
              <Header
                description="Key codes other than E and . are community-reported. Keys marked unverified may not work on the SDS200."
                actions={<Toggle checked={longPress} onChange={({ detail: d }) => setLongPress(d.checked)}>Long press</Toggle>}
              >
                Front panel
              </Header>
            }
          >
            <SpaceBetween size="m">
              <div className="rp-keys">
                {KEYS.map((k) => (
                  <Button
                    key={k.code}
                    disabled={!controlsEnabled}
                    loading={busy === `Key ${k.code}`}
                    ariaLabel={`${k.label} key${k.unverified ? ' (unverified)' : ''}`}
                    onClick={() => void command(`Key ${k.code}`, { command: 'key', code: k.code, action: longPress ? 'long_press' : 'press' })}
                  >
                    {k.label}
                    {k.unverified ? ' *' : ''}
                  </Button>
                ))}
              </div>
              <div className="rp-keypad">
                {KEYPAD.map((c) => (
                  <Button
                    key={c}
                    disabled={!controlsEnabled}
                    ariaLabel={c === 'E' ? 'E / Yes key' : c === '.' ? 'Decimal / No key' : `Key ${c}`}
                    onClick={() => void command(`Key ${c}`, { command: 'key', code: c, action: longPress ? 'long_press' : 'press' })}
                  >
                    {c === 'E' ? 'E/Yes' : c === '.' ? './No' : c}
                  </Button>
                ))}
              </div>
              <Box variant="small" color="text-body-secondary">
                * Flagged unverified on SDS200 by community sources.
              </Box>
            </SpaceBetween>
          </Container>
        </SpaceBetween>
      </Grid>
    </ContentLayout>
  );
}
