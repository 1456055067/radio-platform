import { useMemo } from 'react';
import Box from '@cloudscape-design/components/box';
import Cards from '@cloudscape-design/components/cards';
import ColumnLayout from '@cloudscape-design/components/column-layout';
import Container from '@cloudscape-design/components/container';
import ContentLayout from '@cloudscape-design/components/content-layout';
import Header from '@cloudscape-design/components/header';
import KeyValuePairs from '@cloudscape-design/components/key-value-pairs';
import SpaceBetween from '@cloudscape-design/components/space-between';
import StatusIndicator from '@cloudscape-design/components/status-indicator';
import Table from '@cloudscape-design/components/table';
import Alert from '@cloudscape-design/components/alert';
import { RLink } from '../components/RLink';
import { ScannerStatus } from '../components/ScannerStatus';
import { SignalBars } from '../components/SignalBars';
import { SiteMap } from '../components/SiteMap';
import { currentActivity, isFresh } from '../hooks/activity';
import { useDirectory } from '../hooks/Directory';
import { useAuth } from '../auth/AuthContext';
import { useQuery } from '../hooks/useQuery';
import type { MediaStatus, RetentionStatus, TranscriptionStatus } from '../api/types';
import { useLive } from '../hooks/LiveContext';
import { useNow } from '../hooks/useNow';
import { dash, formatFrequency, relativeTime, scannerStatus } from '../lib/format';

function Counter({ label, value, tone }: { label: string; value: number | string; tone?: 'error' | 'success' }) {
  return (
    <div>
      <Box variant="awsui-key-label">{label}</Box>
      <Box variant="awsui-value-large" color={tone === 'error' ? 'text-status-error' : tone === 'success' ? 'text-status-success' : undefined}>
        {value}
      </Box>
    </div>
  );
}

export function retentionSummary(r: RetentionStatus): string {
  const parts = [
    r.recordings_days && `recordings ${r.recordings_days} d`,
    r.transcripts_days && `transcripts ${r.transcripts_days} d`,
    r.transmissions_days && `transmissions ${r.transmissions_days} d`,
    r.events_days && `events ${r.events_days} d`,
  ].filter(Boolean);
  return parts.join(' · ');
}

export function Dashboard() {
  const dir = useDirectory();
  const live = useLive();
  const now = useNow(1000);
  const { api } = useAuth();
  const media = useQuery((signal) => api.get<MediaStatus>('/media/status', undefined, signal), [api], 10_000);
  const transcription = useQuery((signal) => api.get<TranscriptionStatus>('/transcription/status', undefined, signal), [api], 15_000);
  const retention = useQuery((signal) => api.get<RetentionStatus>('/retention', undefined, signal), [api], 60_000);

  const statuses = useMemo(
    () =>
      new Map(
        dir.scanners.map((s) => [s.id, scannerStatus({ ...s, health: live.health.get(s.id) ?? s.health })] as const),
      ),
    [dir.scanners, live.health],
  );
  const online = [...statuses.values()].filter((s) => s.online).length;
  const enabled = dir.scanners.filter((s) => s.enabled).length;
  const activity = currentActivity(dir.scanners, live.active, live.telemetry, now);
  const agentsUp = dir.agents.filter((a) => a.connected).length;

  return (
    <ContentLayout header={<Header variant="h1" description="Fleet status, updated live.">Dashboard</Header>}>
      <SpaceBetween size="l">
        {dir.error && <Alert type="error">{dir.error}</Alert>}
        <Container>
          <ColumnLayout columns={5} variant="text-grid" minColumnWidth={140}>
            <Counter label="Total scanners" value={dir.scanners.length} />
            <Counter label="Online" value={online} tone="success" />
            <Counter label="Offline or disabled" value={dir.scanners.length - online} tone={enabled - online > 0 ? 'error' : undefined} />
            <Counter label="Receiving now" value={activity.length} />
            <Counter label="Agents connected" value={`${agentsUp} / ${dir.agents.length}`} />
          </ColumnLayout>
        </Container>

        <Table
          header={<Header counter={`(${activity.length})`} description="Squelch open right now, from live telemetry and transmission events.">Active transmissions</Header>}
          variant="container"
          items={activity}
          trackBy="scanner_id"
          empty={<Box color="text-body-secondary">No scanner is receiving.</Box>}
          columnDefinitions={[
            { id: 'scanner', header: 'Scanner', cell: (a) => <RLink href={`/scanners/${a.scanner_id}`}>{dir.scannerById.get(a.scanner_id)?.name ?? a.scanner_id}</RLink> },
            { id: 'site', header: 'Site', cell: (a) => dash(dir.siteById.get(a.site_id)?.name) },
            { id: 'freq', header: 'Frequency', cell: (a) => formatFrequency(a.frequency_hz) },
            { id: 'sys', header: 'System / department', cell: (a) => [a.system, a.department].filter(Boolean).join(' / ') || '—' },
            { id: 'chan', header: 'Channel', cell: (a) => dash(a.channel) },
            { id: 'tg', header: 'Talkgroup', cell: (a) => dash(a.tgid) },
            { id: 'uid', header: 'Unit', cell: (a) => dash(a.unit_id) },
            { id: 'sig', header: 'Signal', cell: (a) => <SignalBars bars={a.signal_bars} /> },
            { id: 'since', header: 'Started', cell: (a) => (a.started_at ? relativeTime(a.started_at, now) : '—') },
          ]}
        />

        <Table
          header={<Header counter={`(${dir.scanners.length})`}>Scanner health</Header>}
          variant="container"
          loading={dir.loading}
          loadingText="Loading scanners"
          items={dir.scanners}
          trackBy="id"
          empty={
            <Box textAlign="center" color="text-body-secondary">
              No scanners enrolled. <RLink href="/scanners">Enroll one</RLink>.
            </Box>
          }
          columnDefinitions={[
            { id: 'name', header: 'Name', cell: (s) => <RLink href={`/scanners/${s.id}`}>{s.name}</RLink> },
            { id: 'site', header: 'Site', cell: (s) => dash(dir.siteById.get(s.site_id)?.name) },
            { id: 'status', header: 'Status', cell: (s) => <ScannerStatus scanner={s} liveHealth={live.health.get(s.id)} /> },
            {
              id: 'chan',
              header: 'Current channel',
              cell: (s) => {
                const t = live.telemetry.get(s.id);
                return isFresh(t, now) ? dash(t.channel ?? t.system) : '—';
              },
            },
            {
              id: 'sig',
              header: 'Signal',
              cell: (s) => {
                const t = live.telemetry.get(s.id);
                return isFresh(t, now) ? <SignalBars bars={t.signal_bars} rssi={t.rssi} /> : '—';
              },
            },
            { id: 'model', header: 'Model / firmware', cell: (s) => [s.model, s.firmware].filter(Boolean).join(' · ') || '—' },
            { id: 'control', header: 'Control', cell: (s) => (s.control ? s.control.holder : '—') },
          ]}
        />

        <Container header={<Header description="Positions are relative, drawn from each site's coordinates.">Sites</Header>}>
          <SpaceBetween size="m">
            <SiteMap
              sites={dir.sites}
              problem={(id) => dir.scanners.some((s) => s.site_id === id && s.enabled && !statuses.get(s.id)?.online)}
            />
            <Cards
              items={dir.sites}
              trackBy="id"
              cardsPerRow={[{ cards: 1 }, { minWidth: 500, cards: 2 }, { minWidth: 900, cards: 3 }]}
              empty={<Box color="text-body-secondary">No sites.</Box>}
              cardDefinition={{
                header: (site) => site.name,
                sections: [
                  { id: 'desc', content: (site) => dash(site.description) },
                  {
                    id: 'scanners',
                    header: 'Scanners',
                    content: (site) => {
                      const mine = dir.scanners.filter((s) => s.site_id === site.id);
                      const up = mine.filter((s) => statuses.get(s.id)?.online).length;
                      return (
                        <StatusIndicator type={mine.length === 0 ? 'info' : up === mine.length ? 'success' : up === 0 ? 'error' : 'warning'}>
                          {up} of {mine.length} online
                        </StatusIndicator>
                      );
                    },
                  },
                  {
                    id: 'agents',
                    header: 'Agents',
                    content: (site) => {
                      const mine = dir.agents.filter((a) => a.site_id === site.id);
                      return mine.length === 0
                        ? '—'
                        : mine.map((a) => (
                            <div key={a.id}>
                              <StatusIndicator type={a.connected ? 'success' : 'error'}>{a.id}</StatusIndicator>
                            </div>
                          ));
                    },
                  },
                  {
                    id: 'coords',
                    header: 'Location',
                    content: (site) =>
                      site.latitude !== null && site.longitude !== null
                        ? `${site.latitude.toFixed(4)}, ${site.longitude.toFixed(4)}`
                        : 'Not set',
                  },
                ],
              }}
            />
          </SpaceBetween>
        </Container>

        <Container header={<Header description="Audio, recording, transcription and retention are off until configured.">Platform services</Header>}>
          <KeyValuePairs
            columns={3}
            items={[
              {
                label: 'Live telemetry stream',
                value:
                  live.state === 'open' ? (
                    <StatusIndicator type="success">Connected{live.lagged ? ` (${live.lagged} updates skipped)` : ''}</StatusIndicator>
                  ) : (
                    <StatusIndicator type={live.state === 'connecting' ? 'pending' : 'error'}>{live.state}</StatusIndicator>
                  ),
              },
              {
                label: 'Audio streaming',
                value: !media.data ? (
                  '—'
                ) : media.data.live_enabled ? (
                  <StatusIndicator type="success">
                    {media.data.streams} streaming · {media.data.sessions} listening
                  </StatusIndicator>
                ) : (
                  <StatusIndicator type="stopped">Not configured</StatusIndicator>
                ),
              },
              {
                label: 'Recording storage',
                value: !media.data ? (
                  '—'
                ) : media.data.recording_enabled && media.data.recordings ? (
                  <StatusIndicator type={media.data.recordings.failed > 0 ? 'warning' : 'success'}>
                    {media.data.recordings.stored} stored{media.data.recordings.failed ? `, ${media.data.recordings.failed} failed` : ''} · {media.data.recordings.in_progress} in progress
                  </StatusIndicator>
                ) : (
                  <StatusIndicator type="stopped">Off (default)</StatusIndicator>
                ),
              },
              {
                label: 'Transcription',
                value: !transcription.data ? (
                  '—'
                ) : transcription.data.enabled ? (
                  <StatusIndicator type={transcription.data.queue.failed > 0 ? 'warning' : 'success'}>
                    {transcription.data.queue.pending} queued · {transcription.data.queue.done} done
                    {transcription.data.queue.failed ? `, ${transcription.data.queue.failed} failed` : ''} (24 h)
                  </StatusIndicator>
                ) : (
                  <StatusIndicator type="stopped">Off (default)</StatusIndicator>
                ),
              },
              {
                label: 'Retention',
                value: !retention.data ? (
                  '—'
                ) : retention.data.enabled ? (
                  <StatusIndicator type="success">{retentionSummary(retention.data)}</StatusIndicator>
                ) : (
                  <StatusIndicator type="stopped">Keep everything (no limits set)</StatusIndicator>
                ),
              },
            ]}
          />
        </Container>
      </SpaceBetween>
    </ContentLayout>
  );
}
