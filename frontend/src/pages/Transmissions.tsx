import { useCallback, useEffect, useMemo, useState } from 'react';
import Alert from '@cloudscape-design/components/alert';
import Badge from '@cloudscape-design/components/badge';
import BarChart from '@cloudscape-design/components/bar-chart';
import Box from '@cloudscape-design/components/box';
import Button from '@cloudscape-design/components/button';
import ButtonDropdown from '@cloudscape-design/components/button-dropdown';
import ColumnLayout from '@cloudscape-design/components/column-layout';
import Container from '@cloudscape-design/components/container';
import ContentLayout from '@cloudscape-design/components/content-layout';
import DateRangePicker, { type DateRangePickerProps } from '@cloudscape-design/components/date-range-picker';
import FormField from '@cloudscape-design/components/form-field';
import Header from '@cloudscape-design/components/header';
import Input from '@cloudscape-design/components/input';
import Select, { type SelectProps } from '@cloudscape-design/components/select';
import SpaceBetween from '@cloudscape-design/components/space-between';
import StatusIndicator from '@cloudscape-design/components/status-indicator';
import Table from '@cloudscape-design/components/table';
import { describeError } from '../api/client';
import type { Page, Transmission } from '../api/types';
import { TransmissionDetail, TranscriptCell } from '../components/TransmissionDetail';
import { useAuth } from '../auth/AuthContext';
import { RLink } from '../components/RLink';
import { useDirectory } from '../hooks/Directory';
import { useLive } from '../hooks/LiveContext';
import { dash, download, formatDuration, formatFrequency, formatTime, parseMhz, timeline, transmissionsCsv } from '../lib/format';

const UNIT_MS: Record<DateRangePickerProps.TimeUnit, number> = {
  second: 1e3,
  minute: 60e3,
  hour: 3600e3,
  day: 86400e3,
  week: 7 * 86400e3,
  month: 30 * 86400e3,
  year: 365 * 86400e3,
};

/** Resolves a picker value to absolute bounds (relative ranges end now). */
export function resolveRange(v: DateRangePickerProps.Value | null, now = Date.now()): { from: number; to: number } | null {
  if (!v) return null;
  if (v.type === 'relative') return { from: now - v.amount * UNIT_MS[v.unit], to: now };
  const from = new Date(v.startDate).getTime();
  const to = new Date(v.endDate).getTime();
  return Number.isNaN(from) || Number.isNaN(to) ? null : { from, to };
}

const MAX_ROWS = 5000;
const ALL: SelectProps.Option = { value: '', label: 'All' };

const RECORDED: SelectProps.Option[] = [
  { value: '', label: 'Any' },
  { value: 'true', label: 'Recorded' },
  { value: 'false', label: 'Not recorded' },
];

interface Filters {
  site: string;
  scanner: string;
  freq: string;
  tgid: string;
  text: string;
  recorded: string;
  range: DateRangePickerProps.Value | null;
}

const CLEARED: Filters = { site: '', scanner: '', freq: '', tgid: '', text: '', recorded: '', range: null };

export function Transmissions() {
  const { api, can } = useAuth();
  const dir = useDirectory();
  const live = useLive();
  const [draft, setDraft] = useState<Filters>({
    ...CLEARED,
    range: { type: 'relative', amount: 1, unit: 'hour', key: 'previous-1-hour' },
  });
  const canRead = can('audio_listen');
  const [applied, setApplied] = useState(draft);
  const [rows, setRows] = useState<Transmission[]>([]);
  const [cursor, setCursor] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<Transmission | null>(null);
  const [bounds, setBounds] = useState<{ from: number; to: number } | null>(null);
  const [searchedAt, setSearchedAt] = useState(() => Date.now());

  const freqHz = draft.freq ? parseMhz(draft.freq) : null;
  const freqError = draft.freq && freqHz === null ? 'Enter MHz, for example 851.0125' : undefined;

  const query = useCallback(
    (f: Filters, r: { from: number; to: number } | null) => ({
      site_id: f.site || undefined,
      scanner_id: f.scanner || undefined,
      frequency_hz: f.freq ? (parseMhz(f.freq) ?? undefined) : undefined,
      tgid: f.tgid.trim() || undefined,
      q: f.text.trim() || undefined,
      recorded: f.recorded || undefined,
      from: r ? new Date(r.from).toISOString() : undefined,
      to: r ? new Date(r.to).toISOString() : undefined,
      limit: 200,
    }),
    [],
  );

  const load = useCallback(
    async (f: Filters, after: string | null) => {
      setLoading(true);
      setError(null);
      const r = resolveRange(f.range);
      if (!after) {
        setBounds(r);
        setSearchedAt(Date.now());
      }
      try {
        const page = await api.get<Page<Transmission>>('/transmissions', { ...query(f, after ? bounds : r), cursor: after });
        setRows((prev) => (after ? [...prev, ...page.items] : page.items).slice(0, MAX_ROWS));
        setCursor(page.next_cursor);
      } catch (e) {
        setError(describeError(e));
      } finally {
        setLoading(false);
      }
    },
    [api, query, bounds],
  );

  useEffect(() => {
    void load(applied, null);
  }, [applied]);

  // New transmissions matching the current view: offer a refresh rather than reshuffling rows.
  const latestEnd = live.recent.find((e) => e.event.type === 'transmission_ended')?.at ?? 0;
  const fresh = latestEnd > searchedAt && applied.range?.type === 'relative';

  const siteOptions = [ALL, ...dir.sites.map((s) => ({ value: s.id, label: s.name }))];
  const scannerOptions = [
    ALL,
    ...dir.scanners.filter((s) => !draft.site || s.site_id === draft.site).map((s) => ({ value: s.id, label: s.name })),
  ];

  const chart = useMemo(() => {
    if (!bounds) return [];
    const span = bounds.to - bounds.from;
    const buckets = span <= 3600e3 ? 30 : span <= 86400e3 ? 24 : 30;
    return timeline(rows, bounds.from, bounds.to, buckets);
  }, [rows, bounds]);
  const fmtTick = (d: Date) =>
    bounds && bounds.to - bounds.from <= 86400e3
      ? d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
      : d.toLocaleDateString([], { month: 'short', day: 'numeric' });

  const exportAs = (kind: string) => {
    const stamp = new Date().toISOString().replace(/[:.]/g, '-');
    if (kind === 'csv') download(`transmissions-${stamp}.csv`, 'text/csv;charset=utf-8', transmissionsCsv(rows));
    else download(`transmissions-${stamp}.json`, 'application/json', JSON.stringify(rows, null, 2));
  };

  return (
    <ContentLayout
      header={
        <Header variant="h1" description="Search transmissions, play recordings and read machine transcripts.">
          Transmissions
        </Header>
      }
    >
      <SpaceBetween size="l">
        <Container header={<Header>Filters</Header>}>
          <SpaceBetween size="m">
            <ColumnLayout columns={4} minColumnWidth={180}>
              <FormField label="Site">
                <Select
                  selectedOption={siteOptions.find((o) => o.value === draft.site) ?? ALL}
                  options={siteOptions}
                  onChange={({ detail }) => setDraft({ ...draft, site: detail.selectedOption.value ?? '', scanner: '' })}
                />
              </FormField>
              <FormField label="Scanner">
                <Select
                  selectedOption={scannerOptions.find((o) => o.value === draft.scanner) ?? ALL}
                  options={scannerOptions}
                  onChange={({ detail }) => setDraft({ ...draft, scanner: detail.selectedOption.value ?? '' })}
                />
              </FormField>
              <FormField label="Frequency (MHz)" errorText={freqError} constraintText="Exact match.">
                <Input value={draft.freq} inputMode="decimal" placeholder="851.0125" onChange={({ detail }) => setDraft({ ...draft, freq: detail.value })} />
              </FormField>
              <FormField label="Talkgroup">
                <Input value={draft.tgid} placeholder="1201" onChange={({ detail }) => setDraft({ ...draft, tgid: detail.value })} />
              </FormField>
            </ColumnLayout>
            <ColumnLayout columns={2} minColumnWidth={220}>
              {canRead ? (
                <FormField label="Transcript contains" constraintText='Whole words; "quoted phrase", or, -exclude. Searches are audited.'>
                  <Input
                    type="search"
                    value={draft.text}
                    placeholder="engine 5"
                    onChange={({ detail }) => setDraft({ ...draft, text: detail.value })}
                    onKeyDown={({ detail }) => {
                      if (detail.key === 'Enter' && !freqError) setApplied({ ...draft });
                    }}
                  />
                </FormField>
              ) : (
                <FormField label="Transcript contains" description="Searching transcripts requires the audio_listen permission.">
                  <Input value="" disabled onChange={() => {}} />
                </FormField>
              )}
              <FormField label="Recording">
                <Select
                  selectedOption={RECORDED.find((o) => o.value === draft.recorded) ?? RECORDED[0]!}
                  options={RECORDED}
                  onChange={({ detail }) => setDraft({ ...draft, recorded: detail.selectedOption.value ?? '' })}
                />
              </FormField>
            </ColumnLayout>
            <FormField label="Time range">
              <DateRangePicker
                value={draft.range}
                onChange={({ detail }) => setDraft({ ...draft, range: detail.value })}
                relativeOptions={[
                  { key: 'previous-15-minutes', amount: 15, unit: 'minute', type: 'relative' },
                  { key: 'previous-1-hour', amount: 1, unit: 'hour', type: 'relative' },
                  { key: 'previous-24-hours', amount: 24, unit: 'hour', type: 'relative' },
                  { key: 'previous-7-days', amount: 7, unit: 'day', type: 'relative' },
                ]}
                isValidRange={(r) => {
                  if (r?.type === 'absolute') {
                    const ok = r.startDate && r.endDate && new Date(r.startDate) <= new Date(r.endDate);
                    return ok ? { valid: true } : { valid: false, errorMessage: 'Choose a start before the end.' };
                  }
                  return { valid: true };
                }}
                placeholder="Any time"
              />
            </FormField>
            <SpaceBetween direction="horizontal" size="xs">
              <Button variant="primary" disabled={!!freqError} onClick={() => setApplied({ ...draft })} loading={loading && rows.length === 0}>
                Search
              </Button>
              <Button
                onClick={() => {
                  setDraft(CLEARED);
                  setApplied(CLEARED);
                }}
              >
                Clear
              </Button>
            </SpaceBetween>
          </SpaceBetween>
        </Container>

        {error && <Alert type="error">{error}</Alert>}
        {fresh && (
          <Alert type="info" action={<Button onClick={() => setApplied({ ...applied })}>Refresh</Button>}>
            New transmissions have been recorded since this search.
          </Alert>
        )}

        <Container header={<Header description={bounds ? `${new Date(bounds.from).toLocaleString()} – ${new Date(bounds.to).toLocaleString()}` : 'All loaded results'}>Timeline</Header>}>
          {bounds ? (
            <BarChart
              series={[{ title: 'Transmissions', type: 'bar', data: chart.map((p) => ({ x: fmtTick(p.x), y: p.y })) }]}
              xScaleType="categorical"
              height={160}
              hideFilter
              hideLegend
              xTitle="Start time"
              yTitle="Count"
              ariaLabel="Transmissions over time"
              empty={<Box color="text-body-secondary">No transmissions in range.</Box>}
            />
          ) : (
            <Box color="text-body-secondary">Choose a time range to see the timeline.</Box>
          )}
        </Container>

        <Table
          variant="container"
          items={rows}
          trackBy="id"
          loading={loading && rows.length === 0}
          loadingText="Searching"
          onRowClick={({ detail }) => setSelected(detail.item)}
          header={
            <Header
              counter={`(${rows.length}${cursor ? '+' : ''})`}
              actions={
                can('export') && (
                  <ButtonDropdown
                    disabled={rows.length === 0}
                    items={[
                      { id: 'csv', text: 'CSV' },
                      { id: 'json', text: 'JSON' },
                    ]}
                    onItemClick={({ detail }) => exportAs(detail.id)}
                  >
                    Export loaded results
                  </ButtonDropdown>
                )
              }
            >
              Results
            </Header>
          }
          empty={<Box color="text-body-secondary">No transmissions match.</Box>}
          footer={
            cursor && (
              <Box textAlign="center">
                <Button onClick={() => void load(applied, cursor)} loading={loading} disabled={rows.length >= MAX_ROWS}>
                  {rows.length >= MAX_ROWS ? `Showing the first ${MAX_ROWS}; narrow the search` : 'Load more'}
                </Button>
              </Box>
            )
          }
          columnDefinitions={[
            { id: 'start', header: 'Started', cell: (t) => formatTime(t.started_at) },
            { id: 'dur', header: 'Duration', cell: (t) => (t.ended_at ? formatDuration(t.duration_ms) : <StatusIndicator type="in-progress">Active</StatusIndicator>) },
            { id: 'scanner', header: 'Scanner', cell: (t) => <RLink href={`/scanners/${t.scanner_id}`}>{dir.scannerById.get(t.scanner_id)?.name ?? 'Removed scanner'}</RLink> },
            { id: 'site', header: 'Site', cell: (t) => dash(dir.siteById.get(t.site_id)?.name) },
            { id: 'freq', header: 'Frequency', cell: (t) => formatFrequency(t.frequency_hz) },
            { id: 'sys', header: 'System', cell: (t) => dash(t.system_name) },
            { id: 'dept', header: 'Department', cell: (t) => dash(t.department_name) },
            { id: 'chan', header: 'Channel', cell: (t) => dash(t.channel_name) },
            { id: 'tg', header: 'Talkgroup', cell: (t) => dash(t.tgid) },
            { id: 'uid', header: 'Unit', cell: (t) => dash(t.unit_id) },
            { id: 'transcript', header: 'Transcript', maxWidth: 360, cell: (t) => <TranscriptCell t={t} canRead={canRead} /> },
            {
              id: 'audio',
              header: 'Audio',
              cell: (t) =>
                t.audio_object ? (
                  can('audio_listen') ? (
                    <Button variant="inline-link" iconName="caret-right-filled" onClick={() => setSelected(t)} ariaLabel="Play recording">
                      Play
                    </Button>
                  ) : (
                    'Recorded'
                  )
                ) : (
                  <Box color="text-status-inactive">Not recorded</Box>
                ),
            },
            { id: 'hold', header: '', cell: (t) => (t.hold ? <Badge color="blue">Hold</Badge> : null) },
            { id: 'details', header: '', minWidth: 90, cell: (t) => <Button variant="inline-link" onClick={() => setSelected(t)} ariaLabel="Details">Details</Button> },
          ]}
        />
      </SpaceBetween>

      {selected && (
        <TransmissionDetail
          t={selected}
          onClose={() => setSelected(null)}
          onChange={(t) => {
            setSelected(t);
            setRows((prev) => prev.map((r) => (r.id === t.id ? t : r)));
          }}
        />
      )}
    </ContentLayout>
  );
}
