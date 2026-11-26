import { useMemo, useState } from 'react';
import { useCollection } from '@cloudscape-design/collection-hooks';
import Box from '@cloudscape-design/components/box';
import Button from '@cloudscape-design/components/button';
import ButtonDropdown from '@cloudscape-design/components/button-dropdown';
import ContentLayout from '@cloudscape-design/components/content-layout';
import Form from '@cloudscape-design/components/form';
import FormField from '@cloudscape-design/components/form-field';
import Header from '@cloudscape-design/components/header';
import Input from '@cloudscape-design/components/input';
import Modal from '@cloudscape-design/components/modal';
import Pagination from '@cloudscape-design/components/pagination';
import Select, { type SelectProps } from '@cloudscape-design/components/select';
import SpaceBetween from '@cloudscape-design/components/space-between';
import Table from '@cloudscape-design/components/table';
import TextFilter from '@cloudscape-design/components/text-filter';
import Toggle from '@cloudscape-design/components/toggle';
import Alert from '@cloudscape-design/components/alert';
import { describeError } from '../api/client';
import type { Scanner } from '../api/types';
import { useAuth } from '../auth/AuthContext';
import { RLink } from '../components/RLink';
import { ScannerStatus } from '../components/ScannerStatus';
import { useDirectory } from '../hooks/Directory';
import { useLive } from '../hooks/LiveContext';
import { useNotify } from '../hooks/Notifications';
import { dash, relativeTime } from '../lib/format';
import { parseNetworks } from './Sites';

const IPV4 = /^(25[0-5]|2[0-4]\d|1?\d?\d)(\.(25[0-5]|2[0-4]\d|1?\d?\d)){3}$/;
const IPV6 = /^[0-9a-fA-F:]+$/;

export interface ScannerForm {
  site_id: string;
  agent_id: string;
  name: string;
  host: string;
  port: string;
  enabled: boolean;
}

/** Client-side checks; the gateway validates everything again. */
export function validateScanner(f: ScannerForm, creating: boolean): Partial<Record<keyof ScannerForm, string>> {
  const e: Partial<Record<keyof ScannerForm, string>> = {};
  if (creating && !f.site_id) e.site_id = 'Choose a site.';
  if (creating && !f.agent_id) e.agent_id = 'Choose the agent that manages this scanner.';
  const name = f.name.trim();
  if (!name || name.length > 128) e.name = 'Enter 1 to 128 characters.';
  const host = f.host.trim();
  if (!IPV4.test(host) && !(host.includes(':') && IPV6.test(host))) e.host = 'Enter the scanner’s IP address (hostnames are not accepted).';
  const port = Number(f.port);
  if (!Number.isInteger(port) || port < 1 || port > 65535) e.port = 'Enter a port from 1 to 65535.';
  return e;
}

function ScannerModal({
  initial,
  prefill,
  onClose,
}: {
  initial: Scanner | null;
  prefill?: Partial<ScannerForm>;
  onClose: (changed: boolean) => void;
}) {
  const { api, can } = useAuth();
  const dir = useDirectory();
  const notify = useNotify();
  const creating = initial === null;
  const [audio, setAudio] = useState({
    audio_enabled: initial?.audio_enabled ?? false,
    recording_enabled: initial?.recording_enabled ?? false,
    transcription_enabled: initial?.transcription_enabled ?? false,
    rtsp_port: String(initial?.rtsp_port ?? 554),
  });
  const rtspOk = /^\d{1,5}$/.test(audio.rtsp_port) && Number(audio.rtsp_port) > 0 && Number(audio.rtsp_port) < 65536;
  const [f, setF] = useState<ScannerForm>(
    initial
      ? { site_id: initial.site_id, agent_id: initial.agent_id, name: initial.name, host: initial.host, port: String(initial.port), enabled: initial.enabled }
      : { site_id: dir.sites[0]?.id ?? '', agent_id: '', name: '', host: '', port: '50536', enabled: true, ...prefill },
  );
  const [submitted, setSubmitted] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const errors = validateScanner(f, creating);
  const show = (k: keyof ScannerForm) => (submitted ? errors[k] : undefined);

  const siteOptions: SelectProps.Option[] = dir.sites.map((s) => ({ value: s.id, label: s.name, description: s.allowed_networks.join(', ') }));
  const agentOptions: SelectProps.Option[] = dir.agents
    .filter((a) => a.site_id === f.site_id)
    .map((a) => ({ value: a.id, label: a.id, description: a.connected ? 'Connected' : 'Offline' }));
  const site = dir.siteById.get(f.site_id);

  const save = async () => {
    setSubmitted(true);
    if (Object.keys(errors).length) return;
    setSaving(true);
    setError(null);
    try {
      const body = { name: f.name.trim(), host: f.host.trim(), port: Number(f.port), enabled: f.enabled };
      if (creating) {
        await api.request('/scanners', { method: 'POST', body: { ...body, site_id: f.site_id, agent_id: f.agent_id } });
        notify('success', `Enrolled ${body.name}.`);
      } else {
        if (!rtspOk) {
          setError('Enter an RTSP port from 1 to 65535.');
          return;
        }
        const audioBody: Record<string, unknown> = {
          audio_enabled: audio.audio_enabled,
          rtsp_port: Number(audio.rtsp_port),
        };
        if (can('recording_manage') && audio.recording_enabled !== initial.recording_enabled) {
          audioBody.recording_enabled = audio.recording_enabled && audio.audio_enabled;
        }
        if (can('transcription_manage') && audio.transcription_enabled !== initial.transcription_enabled) {
          audioBody.transcription_enabled = audio.transcription_enabled && audio.recording_enabled && audio.audio_enabled;
        }
        await api.request(`/scanners/${initial.id}`, { method: 'PATCH', body: { ...body, ...audioBody } });
        notify('success', `Saved ${body.name}.`);
      }
      onClose(true);
    } catch (e) {
      setError(describeError(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <Modal
      visible
      onDismiss={() => onClose(false)}
      header={creating ? 'Enroll scanner' : `Edit ${initial.name}`}
      footer={
        <Box float="right">
          <SpaceBetween direction="horizontal" size="xs">
            <Button variant="link" onClick={() => onClose(false)}>
              Cancel
            </Button>
            <Button variant="primary" loading={saving} onClick={() => void save()}>
              {creating ? 'Enroll' : 'Save'}
            </Button>
          </SpaceBetween>
        </Box>
      }
    >
      <Form errorText={error}>
        <SpaceBetween size="m">
          {creating && (
            <>
              <FormField label="Site" errorText={show('site_id')}>
                <Select
                  selectedOption={siteOptions.find((o) => o.value === f.site_id) ?? null}
                  options={siteOptions}
                  onChange={({ detail }) => setF({ ...f, site_id: detail.selectedOption.value ?? '', agent_id: '' })}
                  placeholder="Choose a site"
                  empty="No sites. Create one under Sites and agents."
                />
              </FormField>
              <FormField label="Managing agent" errorText={show('agent_id')} description="The site agent that talks to this scanner on the local network.">
                <Select
                  selectedOption={agentOptions.find((o) => o.value === f.agent_id) ?? null}
                  options={agentOptions}
                  onChange={({ detail }) => setF({ ...f, agent_id: detail.selectedOption.value ?? '' })}
                  placeholder="Choose an agent"
                  empty="No agents registered at this site."
                />
              </FormField>
            </>
          )}
          <FormField label="Name" errorText={show('name')}>
            <Input value={f.name} onChange={({ detail }) => setF({ ...f, name: detail.value })} />
          </FormField>
          <FormField
            label="IP address"
            errorText={show('host')}
            description={site ? `Must be inside this site's authorized networks: ${site.allowed_networks.join(', ') || 'none configured'}.` : undefined}
          >
            <Input value={f.host} onChange={({ detail }) => setF({ ...f, host: detail.value })} placeholder="192.168.1.50" />
          </FormField>
          <FormField label="UDP port" errorText={show('port')} description="The SDS200 control port is 50536.">
            <Input value={f.port} inputMode="numeric" onChange={({ detail }) => setF({ ...f, port: detail.value })} />
          </FormField>
          <Toggle checked={f.enabled} onChange={({ detail }) => setF({ ...f, enabled: detail.checked })}>
            Managed (the agent connects to it)
          </Toggle>
          {!creating && (
            <SpaceBetween size="s">
              <Toggle
                checked={audio.audio_enabled}
                onChange={({ detail }) =>
                  setAudio({
                    ...audio,
                    audio_enabled: detail.checked,
                    recording_enabled: detail.checked && audio.recording_enabled,
                    transcription_enabled: detail.checked && audio.transcription_enabled,
                  })
                }
                description="The site agent becomes the scanner's single RTSP client while someone listens or recording is on."
              >
                Live audio
              </Toggle>
              <FormField label="RTSP port" description="554 on the SDS200. Must be an allowed audio port." errorText={rtspOk ? undefined : 'Enter a port'}>
                <Input value={audio.rtsp_port} inputMode="numeric" disabled={!audio.audio_enabled} onChange={({ detail }) => setAudio({ ...audio, rtsp_port: detail.value })} />
              </FormField>
              {can('recording_manage') && (
                <Toggle
                  checked={audio.recording_enabled}
                  disabled={!audio.audio_enabled}
                  onChange={({ detail }) => setAudio({ ...audio, recording_enabled: detail.checked, transcription_enabled: detail.checked && audio.transcription_enabled })}
                  description="Stores every transmission's audio. Only record communications you are authorized to record."
                >
                  Record transmissions
                </Toggle>
              )}
              {can('transcription_manage') && (
                <Toggle
                  checked={audio.transcription_enabled}
                  disabled={!audio.recording_enabled || !audio.audio_enabled}
                  onChange={({ detail }) => setAudio({ ...audio, transcription_enabled: detail.checked })}
                  description="Sends each recording to the gateway's configured transcription service. Transcripts are machine-generated and can be wrong."
                >
                  Transcribe recordings
                </Toggle>
              )}
            </SpaceBetween>
          )}
        </SpaceBetween>
      </Form>
    </Modal>
  );
}

interface Discovered {
  host: string;
  port: number;
  model: string;
}

/** Probes a site's networks through its agent; nothing is enrolled automatically. */
function DiscoverModal({ onClose, onEnroll }: { onClose: () => void; onEnroll: (p: Partial<ScannerForm>) => void }) {
  const { api } = useAuth();
  const dir = useDirectory();
  const [siteId, setSiteId] = useState(dir.agents[0]?.site_id ?? '');
  const site = dir.siteById.get(siteId);
  const agents = dir.agents.filter((a) => a.site_id === siteId);
  const [agentId, setAgentId] = useState(agents[0]?.id ?? '');
  const [networks, setNetworks] = useState(site?.allowed_networks.join(', ') ?? '');
  const [port, setPort] = useState('50536');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<{ candidates: Discovered[]; skipped_enrolled: number } | null>(null);
  const parsed = parseNetworks(networks);
  const portNum = Number(port);
  const portOk = Number.isInteger(portNum) && portNum > 0 && portNum < 65536;

  const run = async () => {
    setBusy(true);
    setError(null);
    setResult(null);
    try {
      setResult(
        await api.request(`/sites/${siteId}/discover`, {
          method: 'POST',
          body: { agent_id: agentId, networks: parsed.nets, port: portNum },
        }),
      );
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(false);
    }
  };

  const siteOptions = dir.sites.filter((s) => dir.agents.some((a) => a.site_id === s.id)).map((s) => ({ value: s.id, label: s.name }));
  const agentOptions = agents.map((a) => ({ value: a.id, label: a.id, description: a.connected ? 'Connected' : 'Offline' }));
  return (
    <Modal
      visible
      size="large"
      header="Discover scanners"
      onDismiss={onClose}
      footer={
        <Box float="right">
          <SpaceBetween direction="horizontal" size="xs">
            <Button variant="link" onClick={onClose}>
              Close
            </Button>
            <Button variant="primary" loading={busy} disabled={!agentId || !!parsed.error || parsed.nets.length === 0 || !portOk} onClick={() => void run()}>
              Probe
            </Button>
          </SpaceBetween>
        </Box>
      }
    >
      <SpaceBetween size="m">
        <Alert type="info">
          The site agent sends the read-only <code>MDL</code> (model) command to each address, at most 1024 per run.
          Enrolled scanners are never probed. How a real SDS200 reacts to probes has not been verified on hardware.
        </Alert>
        <SpaceBetween direction="horizontal" size="s">
          <FormField label="Site">
            <Select
              selectedOption={siteOptions.find((o) => o.value === siteId) ?? null}
              options={siteOptions}
              onChange={({ detail }) => {
                const id = detail.selectedOption.value ?? '';
                setSiteId(id);
                setAgentId(dir.agents.find((a) => a.site_id === id)?.id ?? '');
                setNetworks(dir.siteById.get(id)?.allowed_networks.join(', ') ?? '');
                setResult(null);
              }}
            />
          </FormField>
          <FormField label="Agent">
            <Select selectedOption={agentOptions.find((o) => o.value === agentId) ?? null} options={agentOptions} onChange={({ detail }) => setAgentId(detail.selectedOption.value ?? '')} />
          </FormField>
          <FormField label="UDP port" errorText={portOk ? undefined : 'Enter a port'}>
            <Input value={port} inputMode="numeric" onChange={({ detail }) => setPort(detail.value)} />
          </FormField>
        </SpaceBetween>
        <FormField label="Networks" description="Must lie inside the site's authorized networks and the agent's own policy." errorText={parsed.error}>
          <Input value={networks} onChange={({ detail }) => setNetworks(detail.value)} />
        </FormField>
        {error && <Alert type="error">{error}</Alert>}
        {result && (
          <Table
            variant="embedded"
            items={result.candidates}
            trackBy={(d) => `${d.host}:${d.port}`}
            header={<Header counter={`(${result.candidates.length})`} description={`${result.skipped_enrolled} enrolled endpoint(s) skipped.`}>Found</Header>}
            empty={<Box color="text-body-secondary">No unenrolled scanner answered.</Box>}
            columnDefinitions={[
              { id: 'host', header: 'Address', cell: (d) => `${d.host}:${d.port}` },
              { id: 'model', header: 'Model (as reported)', cell: (d) => d.model },
              {
                id: 'enroll',
                header: '',
                cell: (d) => (
                  <Button variant="inline-link" onClick={() => onEnroll({ site_id: siteId, agent_id: agentId, host: d.host, port: String(d.port), name: `${d.model} ${d.host}` })}>
                    Enroll…
                  </Button>
                ),
              },
            ]}
          />
        )}
      </SpaceBetween>
    </Modal>
  );
}

export function Scanners() {
  const { api, can } = useAuth();
  const dir = useDirectory();
  const live = useLive();
  const notify = useNotify();
  const [editing, setEditing] = useState<Scanner | null | 'new'>(null);
  const [prefill, setPrefill] = useState<Partial<ScannerForm> | undefined>();
  const [discovering, setDiscovering] = useState(false);
  const [deleting, setDeleting] = useState<Scanner | null>(null);
  const [busy, setBusy] = useState(false);
  const manage = can('scanner_manage');

  const rows = useMemo(
    () => dir.scanners.map((s) => ({ ...s, site_name: dir.siteById.get(s.site_id)?.name ?? '' })),
    [dir.scanners, dir.siteById],
  );
  const { items, collectionProps, filterProps, paginationProps, filteredItemsCount } = useCollection(rows, {
    filtering: {
      empty: <Box color="text-body-secondary">No scanners enrolled.</Box>,
      noMatch: <Box color="text-body-secondary">No scanners match.</Box>,
    },
    sorting: { defaultState: { sortingColumn: { sortingField: 'name' } } },
    pagination: { pageSize: 25 },
  });

  const toggle = async (s: Scanner) => {
    try {
      await api.request(`/scanners/${s.id}`, { method: 'PATCH', body: { enabled: !s.enabled } });
      notify('success', `${s.name} ${s.enabled ? 'disabled' : 'enabled'}.`);
      dir.reload();
    } catch (e) {
      notify('error', describeError(e));
    }
  };

  const remove = async () => {
    if (!deleting) return;
    setBusy(true);
    try {
      await api.request(`/scanners/${deleting.id}`, { method: 'DELETE' });
      notify('success', `Removed ${deleting.name}.`);
      setDeleting(null);
      dir.reload();
    } catch (e) {
      notify('error', describeError(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <ContentLayout header={<Header variant="h1" description="Enrolled SDS200 scanners and their managing agents.">Scanners</Header>}>
      <Table
        {...collectionProps}
        variant="full-page"
        items={items}
        trackBy="id"
        loading={dir.loading}
        loadingText="Loading scanners"
        header={
          <Header
            counter={`(${dir.scanners.length})`}
            actions={
              manage && (
                <SpaceBetween direction="horizontal" size="xs">
                  <Button onClick={() => setDiscovering(true)} disabled={dir.agents.length === 0}>
                    Discover
                  </Button>
                  <Button
                    variant="primary"
                    onClick={() => {
                      setPrefill(undefined);
                      setEditing('new');
                    }}
                    disabled={dir.sites.length === 0}
                  >
                    Enroll scanner
                  </Button>
                </SpaceBetween>
              )
            }
          >
            Scanners
          </Header>
        }
        filter={<TextFilter {...filterProps} filteringPlaceholder="Find scanners" countText={`${filteredItemsCount ?? 0} matches`} />}
        pagination={<Pagination {...paginationProps} />}
        columnDefinitions={[
          { id: 'name', header: 'Name', sortingField: 'name', cell: (s) => <RLink href={`/scanners/${s.id}`}>{s.name}</RLink> },
          { id: 'site', header: 'Site', sortingField: 'site_name', cell: (s) => dash(s.site_name) },
          { id: 'status', header: 'Status', cell: (s) => <ScannerStatus scanner={s} liveHealth={live.health.get(s.id)} /> },
          { id: 'addr', header: 'Address', cell: (s) => `${s.host}:${s.port}` },
          { id: 'agent', header: 'Agent', sortingField: 'agent_id', cell: (s) => s.agent_id },
          { id: 'model', header: 'Model', cell: (s) => dash(s.model) },
          { id: 'fw', header: 'Firmware', cell: (s) => dash(s.firmware) },
          { id: 'upd', header: 'Updated', sortingField: 'updated_at', cell: (s) => relativeTime(s.updated_at) },
          ...(manage
            ? [
                {
                  id: 'actions',
                  header: 'Actions',
                  cell: (s: Scanner) => (
                    <ButtonDropdown
                      variant="inline-icon"
                      ariaLabel={`Actions for ${s.name}`}
                      items={[
                        { id: 'edit', text: 'Edit' },
                        { id: 'toggle', text: s.enabled ? 'Disable' : 'Enable' },
                        { id: 'delete', text: 'Remove' },
                      ]}
                      onItemClick={({ detail }) => {
                        if (detail.id === 'edit') setEditing(s);
                        if (detail.id === 'toggle') void toggle(s);
                        if (detail.id === 'delete') setDeleting(s);
                      }}
                    />
                  ),
                },
              ]
            : []),
        ]}
      />
      {editing && (
        <ScannerModal
          initial={editing === 'new' ? null : editing}
          prefill={editing === 'new' ? prefill : undefined}
          onClose={(changed) => {
            setEditing(null);
            if (changed) dir.reload();
          }}
        />
      )}
      {discovering && (
        <DiscoverModal
          onClose={() => setDiscovering(false)}
          onEnroll={(p) => {
            setDiscovering(false);
            setPrefill(p);
            setEditing('new');
          }}
        />
      )}
      {deleting && (
        <Modal
          visible
          header={`Remove ${deleting.name}?`}
          onDismiss={() => setDeleting(null)}
          footer={
            <Box float="right">
              <SpaceBetween direction="horizontal" size="xs">
                <Button variant="link" onClick={() => setDeleting(null)}>
                  Cancel
                </Button>
                <Button variant="primary" loading={busy} onClick={() => void remove()}>
                  Remove
                </Button>
              </SpaceBetween>
            </Box>
          }
        >
          <Alert type="warning">
            The agent stops managing this scanner immediately, and its recorded transmissions, events and telemetry are
            permanently deleted. To keep the history, disable the scanner instead.
          </Alert>
        </Modal>
      )}
    </ContentLayout>
  );
}
