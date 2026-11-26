import { useState } from 'react';
import Alert from '@cloudscape-design/components/alert';
import Box from '@cloudscape-design/components/box';
import Button from '@cloudscape-design/components/button';
import ContentLayout from '@cloudscape-design/components/content-layout';
import FormField from '@cloudscape-design/components/form-field';
import Header from '@cloudscape-design/components/header';
import Select, { type SelectProps } from '@cloudscape-design/components/select';
import SpaceBetween from '@cloudscape-design/components/space-between';
import Table from '@cloudscape-design/components/table';
import type { EventRecord, ScannerEvent } from '../api/types';
import { useAuth } from '../auth/AuthContext';
import { RLink } from '../components/RLink';
import { useDirectory } from '../hooks/Directory';
import { usePaged } from '../hooks/usePaged';
import { formatDuration, formatFrequency, formatTime } from '../lib/format';

const KINDS: SelectProps.Option[] = [
  { value: '', label: 'All kinds' },
  { value: 'transmission_started', label: 'Transmission started' },
  { value: 'transmission_ended', label: 'Transmission ended' },
  { value: 'health_changed', label: 'Health changed' },
];

export function describeEvent(e: ScannerEvent): string {
  switch (e.type) {
    case 'transmission_started':
      return [formatFrequency(e.frequency_hz), e.channel, e.tgid ? `TG ${e.tgid}` : null, e.unit_id ? `UID ${e.unit_id}` : null]
        .filter(Boolean)
        .join(' · ');
    case 'transmission_ended':
      return `Lasted ${formatDuration(e.duration_ms)}`;
    case 'health_changed':
      return 'consecutive_failures' in e.health
        ? `${e.health.state} (${e.health.consecutive_failures} failures)`
        : e.health.state;
    default:
      return '';
  }
}

export function Events() {
  const { api } = useAuth();
  const dir = useDirectory();
  const [kind, setKind] = useState('');
  const [scanner, setScanner] = useState('');
  const q = usePaged<EventRecord>(api, '/events', { kind: kind || undefined, scanner_id: scanner || undefined });
  const scannerOptions = [{ value: '', label: 'All scanners' }, ...dir.scanners.map((s) => ({ value: s.id, label: s.name }))];

  return (
    <ContentLayout header={<Header variant="h1" description="Events reported by site agents, newest first.">Events</Header>}>
      <SpaceBetween size="l">
        {q.error && <Alert type="error">{q.error}</Alert>}
        <Table
          variant="container"
          items={q.items}
          trackBy="id"
          loading={q.loading && q.items.length === 0}
          header={
            <Header counter={`(${q.items.length}${q.hasMore ? '+' : ''})`} actions={<Button iconName="refresh" ariaLabel="Refresh" onClick={q.refresh} />}>
              Events
            </Header>
          }
          filter={
            <SpaceBetween direction="horizontal" size="s">
              <FormField label="Kind">
                <Select selectedOption={KINDS.find((k) => k.value === kind) ?? null} options={KINDS} onChange={({ detail }) => setKind(detail.selectedOption.value ?? '')} />
              </FormField>
              <FormField label="Scanner">
                <Select
                  selectedOption={scannerOptions.find((k) => k.value === scanner) ?? null}
                  options={scannerOptions}
                  onChange={({ detail }) => setScanner(detail.selectedOption.value ?? '')}
                />
              </FormField>
            </SpaceBetween>
          }
          empty={<Box color="text-body-secondary">No events.</Box>}
          footer={
            q.hasMore && (
              <Box textAlign="center">
                <Button onClick={q.loadMore} loading={q.loading}>
                  Load more
                </Button>
              </Box>
            )
          }
          columnDefinitions={[
            { id: 'at', header: 'Received', cell: (e) => formatTime(e.received_at) },
            { id: 'scanner', header: 'Scanner', cell: (e) => <RLink href={`/scanners/${e.scanner_id}`}>{dir.scannerById.get(e.scanner_id)?.name ?? 'Removed scanner'}</RLink> },
            { id: 'kind', header: 'Kind', cell: (e) => KINDS.find((k) => k.value === e.kind)?.label ?? e.kind },
            { id: 'desc', header: 'Details', cell: (e) => describeEvent(e.payload) },
            { id: 'agent', header: 'Agent', cell: (e) => e.agent_id },
          ]}
        />
      </SpaceBetween>
    </ContentLayout>
  );
}
