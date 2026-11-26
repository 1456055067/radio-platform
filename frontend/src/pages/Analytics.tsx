import { useState } from 'react';
import Alert from '@cloudscape-design/components/alert';
import BarChart from '@cloudscape-design/components/bar-chart';
import Box from '@cloudscape-design/components/box';
import ColumnLayout from '@cloudscape-design/components/column-layout';
import Container from '@cloudscape-design/components/container';
import ContentLayout from '@cloudscape-design/components/content-layout';
import FormField from '@cloudscape-design/components/form-field';
import Header from '@cloudscape-design/components/header';
import SegmentedControl from '@cloudscape-design/components/segmented-control';
import Select, { type SelectProps } from '@cloudscape-design/components/select';
import SpaceBetween from '@cloudscape-design/components/space-between';
import Table from '@cloudscape-design/components/table';
import type { Activity, Bucket, Top, TopBy } from '../api/types';
import { useAuth } from '../auth/AuthContext';
import { RLink } from '../components/RLink';
import { useDirectory } from '../hooks/Directory';
import { useQuery } from '../hooks/useQuery';
import { formatDuration, formatFrequency } from '../lib/format';

interface Window {
  key: string;
  label: string;
  hours: number;
  bucket: Bucket;
}

export const WINDOWS: Window[] = [
  { key: '24h', label: 'Last 24 hours', hours: 24, bucket: 'hour' },
  { key: '7d', label: 'Last 7 days', hours: 24 * 7, bucket: 'hour' },
  { key: '30d', label: 'Last 30 days', hours: 24 * 30, bucket: 'day' },
  { key: '90d', label: 'Last 90 days', hours: 24 * 90, bucket: 'day' },
];

const BY: { id: TopBy; text: string }[] = [
  { id: 'talkgroup', text: 'Talkgroups' },
  { id: 'channel', text: 'Channels' },
  { id: 'frequency', text: 'Frequencies' },
  { id: 'scanner', text: 'Scanners' },
  { id: 'unit', text: 'Units' },
];

const ALL: SelectProps.Option = { value: '', label: 'All' };

/**
 * Chart category for a bucket. Labels must be unique: a 24-hour window
 * spans 25 hourly buckets, so hours carry the weekday.
 */
export function bucketLabel(iso: string, bucket: Bucket): string {
  const d = new Date(iso);
  return bucket === 'hour'
    ? d.toLocaleString([], { weekday: 'short', hour: '2-digit', minute: '2-digit' })
    : d.toLocaleDateString([], { month: 'short', day: 'numeric', timeZone: 'UTC' });
}

export function Analytics() {
  const { api } = useAuth();
  const dir = useDirectory();
  const [win, setWin] = useState<Window>(WINDOWS[0]!);
  const [site, setSite] = useState('');
  const [scanner, setScanner] = useState('');
  const [by, setBy] = useState<TopBy>('talkgroup');
  // Each fetch (including the 60 s refresh) ends the window at "now".
  const params = () => ({
    from: new Date(Date.now() - win.hours * 3600e3).toISOString(),
    to: new Date().toISOString(),
    bucket: win.bucket,
    site_id: site || undefined,
    scanner_id: scanner || undefined,
  });
  const activity = useQuery((signal) => api.get<Activity>('/analytics/activity', params(), signal), [api, win, site, scanner], 60_000);
  const top = useQuery((signal) => api.get<Top>('/analytics/top', { ...params(), by, limit: 15 }, signal), [api, win, site, scanner, by], 60_000);

  const points = activity.data?.points ?? [];
  const total = points.reduce((n, p) => n + p.transmissions, 0);
  const airtime = points.reduce((n, p) => n + p.airtime_ms, 0);
  const busiest = points.reduce<(typeof points)[number] | null>((b, p) => (!b || p.transmissions > b.transmissions ? p : b), null);
  const label = (iso: string) => bucketLabel(iso, win.bucket);
  const keyCell = (k: string) => {
    switch (by) {
      case 'scanner':
        return <RLink href={`/scanners/${k}`}>{dir.scannerById.get(k)?.name ?? 'Removed scanner'}</RLink>;
      case 'frequency':
        return formatFrequency(Number(k));
      default:
        return k;
    }
  };

  const siteOptions = [ALL, ...dir.sites.map((s) => ({ value: s.id, label: s.name }))];
  const scannerOptions = [ALL, ...dir.scanners.filter((s) => !site || s.site_id === site).map((s) => ({ value: s.id, label: s.name }))];

  return (
    <ContentLayout header={<Header variant="h1" description="Traffic per hour or day and the busiest talkgroups, channels and scanners, from transmission records.">Analytics</Header>}>
      <SpaceBetween size="l">
        <Container>
          <ColumnLayout columns={3} minColumnWidth={180}>
            <FormField label="Period">
              <Select
                selectedOption={{ value: win.key, label: win.label }}
                options={WINDOWS.map((w) => ({ value: w.key, label: w.label }))}
                onChange={({ detail }) => setWin(WINDOWS.find((w) => w.key === detail.selectedOption.value) ?? WINDOWS[0]!)}
              />
            </FormField>
            <FormField label="Site">
              <Select
                selectedOption={siteOptions.find((o) => o.value === site) ?? ALL}
                options={siteOptions}
                onChange={({ detail }) => {
                  setSite(detail.selectedOption.value ?? '');
                  setScanner('');
                }}
              />
            </FormField>
            <FormField label="Scanner">
              <Select selectedOption={scannerOptions.find((o) => o.value === scanner) ?? ALL} options={scannerOptions} onChange={({ detail }) => setScanner(detail.selectedOption.value ?? '')} />
            </FormField>
          </ColumnLayout>
        </Container>

        {(activity.error || top.error) && <Alert type="error">{activity.error ?? top.error}</Alert>}

        <Container>
          <ColumnLayout columns={3} variant="text-grid">
            <div>
              <Box variant="awsui-key-label">Transmissions</Box>
              <Box variant="awsui-value-large">{activity.data ? total : '—'}</Box>
            </div>
            <div>
              <Box variant="awsui-key-label">Airtime</Box>
              <Box variant="awsui-value-large">{activity.data ? formatDuration(airtime) : '—'}</Box>
            </div>
            <div>
              <Box variant="awsui-key-label">Busiest {win.bucket}</Box>
              <Box variant="awsui-value-large">{busiest && busiest.transmissions > 0 ? label(busiest.start) : '—'}</Box>
            </div>
          </ColumnLayout>
        </Container>

        <Container header={<Header description={`Transmissions per ${win.bucket} (UTC buckets).`}>Activity</Header>}>
          <BarChart
            series={[{ title: 'Transmissions', type: 'bar', data: points.map((p) => ({ x: label(p.start), y: p.transmissions })) }]}
            xScaleType="categorical"
            height={200}
            hideFilter
            hideLegend
            xTitle={win.bucket === 'hour' ? 'Hour' : 'Day'}
            yTitle="Transmissions"
            ariaLabel="Transmissions over time"
            statusType={activity.loading && !activity.data ? 'loading' : 'finished'}
            empty={<Box color="text-body-secondary">No transmissions in this period.</Box>}
          />
        </Container>

        <Table
          variant="container"
          header={
            <Header actions={<SegmentedControl selectedId={by} options={BY} onChange={({ detail }) => setBy(detail.selectedId as TopBy)} label="Rank by" />}>
              Busiest
            </Header>
          }
          items={top.data?.entries ?? []}
          trackBy="key"
          loading={top.loading && !top.data}
          loadingText="Loading"
          empty={<Box color="text-body-secondary">Nothing recorded for this period.</Box>}
          columnDefinitions={[
            { id: 'rank', header: '#', cell: (e) => (top.data?.entries.indexOf(e) ?? 0) + 1, width: 60 },
            { id: 'key', header: BY.find((b) => b.id === by)?.text.replace(/s$/, '') ?? 'Value', cell: (e) => keyCell(e.key) },
            { id: 'n', header: 'Transmissions', cell: (e) => e.transmissions },
            { id: 'air', header: 'Airtime', cell: (e) => formatDuration(e.airtime_ms) },
            { id: 'share', header: 'Share', cell: (e) => (total ? `${Math.round((e.transmissions / total) * 100)}%` : '—') },
          ]}
        />
      </SpaceBetween>
    </ContentLayout>
  );
}
