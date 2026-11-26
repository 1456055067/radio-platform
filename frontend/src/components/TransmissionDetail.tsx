import { useState } from 'react';
import Alert from '@cloudscape-design/components/alert';
import Box from '@cloudscape-design/components/box';
import Button from '@cloudscape-design/components/button';
import Container from '@cloudscape-design/components/container';
import Header from '@cloudscape-design/components/header';
import KeyValuePairs from '@cloudscape-design/components/key-value-pairs';
import Modal from '@cloudscape-design/components/modal';
import SpaceBetween from '@cloudscape-design/components/space-between';
import StatusIndicator from '@cloudscape-design/components/status-indicator';
import { describeError } from '../api/client';
import type { Transmission } from '../api/types';
import { useAuth } from '../auth/AuthContext';
import { useDirectory } from '../hooks/Directory';
import { useNotify } from '../hooks/Notifications';
import { dash, formatDuration, formatFrequency, formatTime } from '../lib/format';
import { RecordingPlayer } from './RecordingPlayer';

const SNIPPET = 90;

/** One-line transcript summary for tables. Text is rendered as text, never HTML. */
export function TranscriptCell({ t, canRead }: { t: Transmission; canRead: boolean }) {
  switch (t.transcription_state) {
    case null:
      return <Box color="text-status-inactive">—</Box>;
    case 'pending':
      return <StatusIndicator type="pending">Queued</StatusIndicator>;
    case 'failed':
      return <StatusIndicator type="error">Failed</StatusIndicator>;
    case 'skipped':
      return <StatusIndicator type="stopped">Skipped</StatusIndicator>;
    case 'done':
      if (!canRead) return <StatusIndicator type="success">Transcribed</StatusIndicator>;
      if (t.transcript === null) return <Box color="text-status-inactive">Removed</Box>;
      if (t.transcript === '') return <Box color="text-status-inactive">No speech</Box>;
      return <span title={t.transcript}>{t.transcript.length > SNIPPET ? `${t.transcript.slice(0, SNIPPET)}…` : t.transcript}</span>;
  }
}

const meta = (t: Transmission) =>
  [t.transcript_model, t.transcript_language && `language ${t.transcript_language}`, t.transcribed_at && formatTime(t.transcribed_at)]
    .filter(Boolean)
    .join(' · ');

function Transcript({ t, canRead }: { t: Transmission; canRead: boolean }) {
  switch (t.transcription_state) {
    case null:
      return <StatusIndicator type="stopped">Not transcribed. Transcription is off by default and enabled per scanner.</StatusIndicator>;
    case 'pending':
      return (
        <StatusIndicator type="pending">
          Queued for transcription{t.transcription_error ? ` (retrying after: ${t.transcription_error})` : ''}.
        </StatusIndicator>
      );
    case 'failed':
      return <StatusIndicator type="error">Transcription failed: {t.transcription_error ?? 'unknown error'}</StatusIndicator>;
    case 'skipped':
      return <StatusIndicator type="stopped">Not sent for transcription: {t.transcription_error ?? 'skipped'}.</StatusIndicator>;
    case 'done':
      if (!canRead) return <StatusIndicator type="info">Transcribed. Reading transcripts requires the audio_listen permission.</StatusIndicator>;
      if (t.transcript === null) return <StatusIndicator type="stopped">{t.transcription_error ?? 'Transcript removed.'}</StatusIndicator>;
      return (
        <SpaceBetween size="s">
          {t.transcript === '' ? (
            <Box color="text-body-secondary">The service found no speech.</Box>
          ) : (
            <Box variant="p">{t.transcript}</Box>
          )}
          <Box variant="small" color="text-body-secondary">
            Machine-generated and possibly wrong; check against the recording.
            {meta(t) && ` ${meta(t)}`}
          </Box>
        </SpaceBetween>
      );
  }
}

export function TransmissionDetail({
  t,
  onClose,
  onChange,
}: {
  t: Transmission;
  onClose: () => void;
  onChange: (t: Transmission) => void;
}) {
  const { api, can } = useAuth();
  const dir = useDirectory();
  const notify = useNotify();
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const canRead = can('audio_listen');
  const canRecords = can('recording_manage');

  // Each action runs once; nothing is retried automatically.
  const act = async (what: string, run: () => Promise<void>) => {
    setBusy(what);
    setError(null);
    try {
      await run();
      onChange(await api.get<Transmission>(`/transmissions/${t.id}`));
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(null);
    }
  };

  const retranscribe = () =>
    act('transcribe', async () => {
      await api.request(`/transmissions/${t.id}/transcribe`, { method: 'POST' });
      notify('success', 'Queued for transcription.');
    });
  const setHold = (hold: boolean) =>
    act('hold', async () => {
      await api.request(`/transmissions/${t.id}/hold`, { method: 'PUT', body: { hold } });
      notify('success', hold ? 'Hold placed: retention will keep this transmission.' : 'Hold released.');
    });
  const deleteRecording = () =>
    act('delete', async () => {
      await api.request(`/transmissions/${t.id}/audio`, { method: 'DELETE' });
      setConfirmDelete(false);
      notify('success', 'Recording and transcript deleted.');
    });

  return (
    <Modal
      visible
      size="large"
      header="Transmission"
      onDismiss={onClose}
      footer={
        canRecords && (
          <Box float="right">
            <SpaceBetween direction="horizontal" size="xs">
              <Button loading={busy === 'hold'} disabled={busy !== null} onClick={() => void setHold(!t.hold)}>
                {t.hold ? 'Release hold' : 'Place hold'}
              </Button>
              <Button disabled={!t.audio_object || t.hold || busy !== null} onClick={() => setConfirmDelete(true)}>
                Delete recording
              </Button>
            </SpaceBetween>
          </Box>
        )
      }
    >
      <SpaceBetween size="l">
        {error && <Alert type="error">{error}</Alert>}
        {t.hold && (
          <Alert type="info" header="On hold">
            Retention will not delete this transmission, its recording or transcript, and the recording cannot be deleted until the hold is released.
          </Alert>
        )}
        {confirmDelete && (
          <Alert
            type="warning"
            header="Delete this recording?"
            action={
              <SpaceBetween direction="horizontal" size="xs">
                <Button onClick={() => setConfirmDelete(false)}>Cancel</Button>
                <Button variant="primary" loading={busy === 'delete'} onClick={() => void deleteRecording()}>
                  Delete
                </Button>
              </SpaceBetween>
            }
          >
            The audio and its transcript are deleted permanently. The transmission record stays. This is audited.
          </Alert>
        )}
        <KeyValuePairs
          columns={3}
          items={[
            { label: 'Started', value: formatTime(t.started_at) },
            { label: 'Ended', value: formatTime(t.ended_at) },
            { label: 'Duration', value: formatDuration(t.duration_ms) },
            { label: 'Scanner', value: dir.scannerById.get(t.scanner_id)?.name ?? t.scanner_id },
            { label: 'Site', value: dash(dir.siteById.get(t.site_id)?.name) },
            { label: 'Frequency', value: formatFrequency(t.frequency_hz) },
            { label: 'Modulation', value: dash(t.modulation) },
            { label: 'System', value: dash(t.system_name) },
            { label: 'Department', value: dash(t.department_name) },
            { label: 'Channel', value: dash(t.channel_name) },
            { label: 'Talkgroup', value: dash(t.tgid) },
            { label: 'Unit ID', value: dash(t.unit_id) },
            { label: 'Signal', value: t.signal_bars === null ? '—' : `${t.signal_bars} / 4` },
            { label: 'RSSI', value: dash(t.rssi) },
            { label: 'Transmission ID', value: <Box variant="code">{t.id}</Box> },
          ]}
        />
        <Container header={<Header variant="h3">Audio</Header>}>
          {!t.audio_object ? (
            <StatusIndicator type="stopped">Not recorded. Recording is off by default and enabled per scanner.</StatusIndicator>
          ) : canRead ? (
            <RecordingPlayer transmissionId={t.id} />
          ) : (
            <StatusIndicator type="info">Recorded. Playing it requires the audio_listen permission.</StatusIndicator>
          )}
        </Container>
        <Container
          header={
            <Header
              variant="h3"
              actions={
                can('transcription_manage') &&
                t.audio_object && (
                  <Button loading={busy === 'transcribe'} disabled={busy !== null || t.transcription_state === 'pending'} onClick={() => void retranscribe()}>
                    {t.transcription_state ? 'Transcribe again' : 'Transcribe'}
                  </Button>
                )
              }
            >
              Transcript
            </Header>
          }
        >
          <Transcript t={t} canRead={canRead} />
        </Container>
      </SpaceBetween>
    </Modal>
  );
}
