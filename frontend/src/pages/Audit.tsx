import { useState } from 'react';
import Alert from '@cloudscape-design/components/alert';
import Box from '@cloudscape-design/components/box';
import Button from '@cloudscape-design/components/button';
import ButtonDropdown from '@cloudscape-design/components/button-dropdown';
import ColumnLayout from '@cloudscape-design/components/column-layout';
import Container from '@cloudscape-design/components/container';
import ContentLayout from '@cloudscape-design/components/content-layout';
import FormField from '@cloudscape-design/components/form-field';
import Header from '@cloudscape-design/components/header';
import Input from '@cloudscape-design/components/input';
import Select, { type SelectProps } from '@cloudscape-design/components/select';
import SpaceBetween from '@cloudscape-design/components/space-between';
import StatusIndicator from '@cloudscape-design/components/status-indicator';
import Table from '@cloudscape-design/components/table';
import { describeError } from '../api/client';
import type { AuditRecord, AuditVerification } from '../api/types';
import { useAuth } from '../auth/AuthContext';
import { usePaged } from '../hooks/usePaged';
import { dash, download, formatTime } from '../lib/format';

const OUTCOME = {
  success: <StatusIndicator type="success">Success</StatusIndicator>,
  denied: <StatusIndicator type="warning">Denied</StatusIndicator>,
  failure: <StatusIndicator type="error">Failed</StatusIndicator>,
} as const;

const OUTCOMES: SelectProps.Option[] = [
  { value: '', label: 'Any' },
  { value: 'success', label: 'Success' },
  { value: 'denied', label: 'Denied' },
  { value: 'failure', label: 'Failed' },
];

interface Filters {
  actor: string;
  action: string;
  outcome: string;
}

const EMPTY: Filters = { actor: '', action: '', outcome: '' };

function query(f: Filters): Record<string, string | undefined> {
  return { actor: f.actor.trim() || undefined, action: f.action.trim() || undefined, outcome: f.outcome || undefined };
}

export function Audit() {
  const { api, can } = useAuth();
  const [draft, setDraft] = useState<Filters>(EMPTY);
  const [applied, setApplied] = useState<Filters>(EMPTY);
  const q = usePaged<AuditRecord>(api, '/audit', query(applied));
  const [verification, setVerification] = useState<AuditVerification | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  if (!can('audit_view')) {
    return (
      <Box padding="l">
        <Alert type="info">The audit log requires the audit_view permission.</Alert>
      </Box>
    );
  }

  const verify = async () => {
    setBusy('verify');
    setError(null);
    try {
      setVerification(await api.get<AuditVerification>('/audit/verify'));
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(null);
    }
  };

  // Downloads run once; the server audits each export.
  const exportAs = async (format: string) => {
    setBusy('export');
    setError(null);
    try {
      const params = new URLSearchParams({ format });
      for (const [k, v] of Object.entries(query(applied))) if (v) params.set(k, v);
      const r = await api.raw(`/audit/export?${params.toString()}`);
      const body = await r.text();
      const stamp = new Date().toISOString().replace(/[:.]/g, '-');
      download(`audit-${stamp}.${format}`, format === 'csv' ? 'text/csv;charset=utf-8' : 'application/jsonl', body);
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(null);
    }
  };

  return (
    <ContentLayout header={<Header variant="h1" description="Every control action, configuration change and denied attempt, in a tamper-evident hash chain.">Audit log</Header>}>
      <SpaceBetween size="l">
        {(q.error || error) && <Alert type="error">{q.error ?? error}</Alert>}
        <Container
          header={
            <Header
              description="Recomputes every record's hash and compares the end of the chain with the stored head."
              actions={
                <Button loading={busy === 'verify'} onClick={() => void verify()}>
                  Verify chain
                </Button>
              }
            >
              Integrity
            </Header>
          }
        >
          {!verification ? (
            <Box color="text-body-secondary">Not verified in this session.</Box>
          ) : verification.ok ? (
            <SpaceBetween size="xs">
              <StatusIndicator type="success">
                Intact: {verification.verified} records verified
                {verification.unchained ? `; ${verification.unchained} older records predate the chain` : ''}.
              </StatusIndicator>
              <Box variant="small" color="text-body-secondary">
                Head #{verification.head_seq} <Box variant="code" fontSize="body-s">{verification.head_hash}</Box>. Keep a copy of the head outside the database to detect a rewritten chain.
              </Box>
            </SpaceBetween>
          ) : (
            <Alert type="error" header="The audit chain is broken">
              Record #{verification.failure?.seq}: {verification.failure?.reason}. {verification.verified} records before it verified. Investigate before trusting later records.
            </Alert>
          )}
        </Container>

        <Container header={<Header>Filters</Header>}>
          <SpaceBetween size="m">
            <ColumnLayout columns={3} minColumnWidth={180}>
              <FormField label="Actor" constraintText="Exact subject.">
                <Input value={draft.actor} onChange={({ detail }) => setDraft({ ...draft, actor: detail.value })} />
              </FormField>
              <FormField label="Action" constraintText="Prefix, for example scanner.">
                <Input value={draft.action} placeholder="scanner." onChange={({ detail }) => setDraft({ ...draft, action: detail.value })} />
              </FormField>
              <FormField label="Outcome">
                <Select
                  selectedOption={OUTCOMES.find((o) => o.value === draft.outcome) ?? OUTCOMES[0]!}
                  options={OUTCOMES}
                  onChange={({ detail }) => setDraft({ ...draft, outcome: detail.selectedOption.value ?? '' })}
                />
              </FormField>
            </ColumnLayout>
            <SpaceBetween direction="horizontal" size="xs">
              <Button variant="primary" onClick={() => setApplied({ ...draft })}>
                Search
              </Button>
              <Button
                onClick={() => {
                  setDraft(EMPTY);
                  setApplied(EMPTY);
                }}
              >
                Clear
              </Button>
            </SpaceBetween>
          </SpaceBetween>
        </Container>

        <Table
          variant="container"
          items={q.items}
          trackBy="id"
          loading={q.loading && q.items.length === 0}
          header={
            <Header
              counter={`(${q.items.length}${q.hasMore ? '+' : ''})`}
              actions={
                <SpaceBetween direction="horizontal" size="xs">
                  <Button iconName="refresh" ariaLabel="Refresh" onClick={q.refresh} />
                  {can('export') && (
                    <ButtonDropdown
                      loading={busy === 'export'}
                      items={[
                        { id: 'jsonl', text: 'JSON Lines (with hashes)' },
                        { id: 'csv', text: 'CSV' },
                      ]}
                      onItemClick={({ detail }) => void exportAs(detail.id)}
                    >
                      Export matching records
                    </ButtonDropdown>
                  )}
                </SpaceBetween>
              }
            >
              Records
            </Header>
          }
          empty={<Box color="text-body-secondary">No audit records.</Box>}
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
            { id: 'seq', header: '#', cell: (a) => dash(a.seq) },
            { id: 'at', header: 'Time', cell: (a) => formatTime(a.at) },
            { id: 'actor', header: 'Actor', cell: (a) => a.actor },
            { id: 'action', header: 'Action', cell: (a) => a.action },
            { id: 'res', header: 'Resource', cell: (a) => `${a.resource_type}${a.resource_id ? ` ${a.resource_id.slice(0, 8)}…` : ''}` },
            { id: 'outcome', header: 'Outcome', cell: (a) => OUTCOME[a.outcome] ?? a.outcome },
            { id: 'ip', header: 'Source IP', cell: (a) => dash(a.source_ip) },
            { id: 'details', header: 'Details', cell: (a) => <Box variant="code" fontSize="body-s">{JSON.stringify(a.details)}</Box> },
          ]}
        />
      </SpaceBetween>
    </ContentLayout>
  );
}
