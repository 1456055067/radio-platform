import { useState } from 'react';
import Box from '@cloudscape-design/components/box';
import Button from '@cloudscape-design/components/button';
import ContentLayout from '@cloudscape-design/components/content-layout';
import Form from '@cloudscape-design/components/form';
import FormField from '@cloudscape-design/components/form-field';
import Header from '@cloudscape-design/components/header';
import Input from '@cloudscape-design/components/input';
import Modal from '@cloudscape-design/components/modal';
import Select from '@cloudscape-design/components/select';
import SpaceBetween from '@cloudscape-design/components/space-between';
import StatusIndicator from '@cloudscape-design/components/status-indicator';
import Table from '@cloudscape-design/components/table';
import Textarea from '@cloudscape-design/components/textarea';
import { describeError } from '../api/client';
import { useAuth } from '../auth/AuthContext';
import { useDirectory } from '../hooks/Directory';
import { useNotify } from '../hooks/Notifications';
import { dash, relativeTime } from '../lib/format';

const CIDR = /^([0-9.]+|[0-9a-fA-F:]+)\/(\d{1,3})$/;
const AGENT_ID = /^[A-Za-z0-9._-]{1,128}$/;

/** Splits the networks box into CIDRs and reports the first problem. */
export function parseNetworks(text: string): { nets: string[]; error?: string } {
  const nets = text
    .split(/[\s,]+/)
    .map((s) => s.trim())
    .filter(Boolean);
  for (const n of nets) {
    const m = CIDR.exec(n);
    if (!m) return { nets, error: `“${n}” is not CIDR notation (for example 192.168.10.0/24).` };
    if (Number(m[2]) === 0) return { nets, error: 'A network covering every address is not allowed.' };
  }
  if (nets.length > 64) return { nets, error: 'At most 64 networks.' };
  return { nets };
}

function coord(text: string, limit: number): number | null | 'bad' {
  if (!text.trim()) return null;
  const v = Number(text);
  return Number.isFinite(v) && Math.abs(v) <= limit ? v : 'bad';
}

function SiteModal({ onClose }: { onClose: (changed: boolean) => void }) {
  const { api } = useAuth();
  const notify = useNotify();
  const [name, setName] = useState('');
  const [description, setDescription] = useState('');
  const [lat, setLat] = useState('');
  const [lon, setLon] = useState('');
  const [networks, setNetworks] = useState('');
  const [submitted, setSubmitted] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const nets = parseNetworks(networks);
  const la = coord(lat, 90);
  const lo = coord(lon, 180);
  const errs = {
    name: !name.trim() || name.length > 128 ? 'Enter 1 to 128 characters.' : undefined,
    nets: nets.error ?? (nets.nets.length === 0 ? 'Add at least one network, or no scanner can be enrolled here.' : undefined),
    lat: la === 'bad' ? 'Latitude must be between -90 and 90.' : undefined,
    lon: lo === 'bad' ? 'Longitude must be between -180 and 180.' : undefined,
  };
  const save = async () => {
    setSubmitted(true);
    if (Object.values(errs).some(Boolean)) return;
    setSaving(true);
    setError(null);
    try {
      await api.request('/sites', {
        method: 'POST',
        body: {
          name: name.trim(),
          description: description.trim() || null,
          latitude: la,
          longitude: lo,
          allowed_networks: nets.nets,
        },
      });
      notify('success', `Created site ${name.trim()}.`);
      onClose(true);
    } catch (e) {
      setError(describeError(e));
    } finally {
      setSaving(false);
    }
  };
  const show = (e: string | undefined) => (submitted ? e : undefined);
  return (
    <Modal
      visible
      header="Create site"
      onDismiss={() => onClose(false)}
      footer={
        <Box float="right">
          <SpaceBetween direction="horizontal" size="xs">
            <Button variant="link" onClick={() => onClose(false)}>
              Cancel
            </Button>
            <Button variant="primary" loading={saving} onClick={() => void save()}>
              Create
            </Button>
          </SpaceBetween>
        </Box>
      }
    >
      <Form errorText={error}>
        <SpaceBetween size="m">
          <FormField label="Name" errorText={show(errs.name)}>
            <Input value={name} onChange={({ detail }) => setName(detail.value)} />
          </FormField>
          <FormField label="Description" constraintText="Optional.">
            <Input value={description} onChange={({ detail }) => setDescription(detail.value)} />
          </FormField>
          <SpaceBetween direction="horizontal" size="s">
            <FormField label="Latitude" errorText={show(errs.lat)} constraintText="Optional.">
              <Input value={lat} inputMode="decimal" onChange={({ detail }) => setLat(detail.value)} />
            </FormField>
            <FormField label="Longitude" errorText={show(errs.lon)} constraintText="Optional.">
              <Input value={lon} inputMode="decimal" onChange={({ detail }) => setLon(detail.value)} />
            </FormField>
          </SpaceBetween>
          <FormField
            label="Authorized scanner networks"
            description="Scanners can only be enrolled at addresses inside these networks, and agents refuse any other address. Keep them as narrow as possible."
            errorText={show(errs.nets)}
          >
            <Textarea value={networks} onChange={({ detail }) => setNetworks(detail.value)} placeholder="192.168.10.0/24" rows={3} />
          </FormField>
        </SpaceBetween>
      </Form>
    </Modal>
  );
}

function AgentModal({ onClose }: { onClose: (changed: boolean) => void }) {
  const { api } = useAuth();
  const dir = useDirectory();
  const notify = useNotify();
  const [agentId, setAgentId] = useState('');
  const [siteId, setSiteId] = useState(dir.sites[0]?.id ?? '');
  const [description, setDescription] = useState('');
  const [submitted, setSubmitted] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const idErr = AGENT_ID.test(agentId) ? undefined : 'Use 1 to 128 letters, digits, dots, dashes or underscores.';
  const options = dir.sites.map((s) => ({ value: s.id, label: s.name }));
  const save = async () => {
    setSubmitted(true);
    if (idErr || !siteId) return;
    setSaving(true);
    setError(null);
    try {
      await api.request('/agents', { method: 'POST', body: { agent_id: agentId, site_id: siteId, description: description.trim() || null } });
      notify('success', `Registered agent ${agentId}.`);
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
      header="Register agent"
      onDismiss={() => onClose(false)}
      footer={
        <Box float="right">
          <SpaceBetween direction="horizontal" size="xs">
            <Button variant="link" onClick={() => onClose(false)}>
              Cancel
            </Button>
            <Button variant="primary" loading={saving} onClick={() => void save()}>
              Register
            </Button>
          </SpaceBetween>
        </Box>
      }
    >
      <Form errorText={error}>
        <SpaceBetween size="m">
          <FormField
            label="Agent identity"
            description="Must equal the first DNS name in the agent's client certificate. The gateway rejects agents whose certificate identity is not registered."
            errorText={submitted ? idErr : undefined}
          >
            <Input value={agentId} onChange={({ detail }) => setAgentId(detail.value)} placeholder="agent-north-ridge" />
          </FormField>
          <FormField label="Site">
            <Select
              selectedOption={options.find((o) => o.value === siteId) ?? null}
              options={options}
              onChange={({ detail }) => setSiteId(detail.selectedOption.value ?? '')}
            />
          </FormField>
          <FormField label="Description" constraintText="Optional.">
            <Input value={description} onChange={({ detail }) => setDescription(detail.value)} />
          </FormField>
        </SpaceBetween>
      </Form>
    </Modal>
  );
}

export function Sites() {
  const { can } = useAuth();
  const dir = useDirectory();
  const [modal, setModal] = useState<'site' | 'agent' | null>(null);
  const manage = can('site_manage');
  const close = (changed: boolean) => {
    setModal(null);
    if (changed) dir.reload();
  };
  return (
    <ContentLayout header={<Header variant="h1" description="Sites define where scanners may live; agents connect each site to the gateway.">Sites and agents</Header>}>
      <SpaceBetween size="l">
        <Table
          variant="container"
          items={dir.sites}
          trackBy="id"
          loading={dir.loading}
          header={
            <Header counter={`(${dir.sites.length})`} actions={manage && <Button onClick={() => setModal('site')}>Create site</Button>}>
              Sites
            </Header>
          }
          empty={<Box color="text-body-secondary">No sites.</Box>}
          columnDefinitions={[
            { id: 'name', header: 'Name', cell: (s) => s.name },
            { id: 'desc', header: 'Description', cell: (s) => dash(s.description) },
            { id: 'nets', header: 'Authorized networks', cell: (s) => s.allowed_networks.join(', ') || '—' },
            {
              id: 'loc',
              header: 'Location',
              cell: (s) => (s.latitude !== null && s.longitude !== null ? `${s.latitude.toFixed(4)}, ${s.longitude.toFixed(4)}` : '—'),
            },
            { id: 'count', header: 'Scanners', cell: (s) => dir.scanners.filter((x) => x.site_id === s.id).length },
          ]}
        />
        <Table
          variant="container"
          items={dir.agents}
          trackBy="id"
          loading={dir.loading}
          header={
            <Header
              counter={`(${dir.agents.length})`}
              description="Connection state is refreshed every 15 seconds."
              actions={
                manage && (
                  <Button onClick={() => setModal('agent')} disabled={dir.sites.length === 0}>
                    Register agent
                  </Button>
                )
              }
            >
              Agents
            </Header>
          }
          empty={<Box color="text-body-secondary">No agents registered.</Box>}
          columnDefinitions={[
            { id: 'id', header: 'Identity', cell: (a) => a.id },
            { id: 'site', header: 'Site', cell: (a) => dash(dir.siteById.get(a.site_id)?.name) },
            { id: 'state', header: 'Connection', cell: (a) => <StatusIndicator type={a.connected ? 'success' : 'error'}>{a.connected ? 'Connected' : 'Offline'}</StatusIndicator> },
            { id: 'seen', header: 'Last connected', cell: (a) => relativeTime(a.last_seen_at) },
            { id: 'desc', header: 'Description', cell: (a) => dash(a.description) },
            { id: 'n', header: 'Scanners', cell: (a) => dir.scanners.filter((x) => x.agent_id === a.id).length },
          ]}
        />
      </SpaceBetween>
      {modal === 'site' && <SiteModal onClose={close} />}
      {modal === 'agent' && <AgentModal onClose={close} />}
    </ContentLayout>
  );
}
