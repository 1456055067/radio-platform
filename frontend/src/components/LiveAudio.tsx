import { useEffect, useRef, useState } from 'react';
import Alert from '@cloudscape-design/components/alert';
import Box from '@cloudscape-design/components/box';
import Button from '@cloudscape-design/components/button';
import SpaceBetween from '@cloudscape-design/components/space-between';
import StatusIndicator from '@cloudscape-design/components/status-indicator';
import { describeError } from '../api/client';
import type { AudioState } from '../api/types';
import { useAuth } from '../auth/AuthContext';

type Phase = 'idle' | 'connecting' | 'playing' | 'error';

/** Waits for ICE gathering (non-trickle WHEP), at most `ms`. */
function gathered(pc: RTCPeerConnection, ms = 2000): Promise<void> {
  if (pc.iceGatheringState === 'complete') return Promise.resolve();
  return new Promise((resolve) => {
    const done = () => {
      pc.removeEventListener('icegatheringstatechange', check);
      resolve();
    };
    const check = () => {
      if (pc.iceGatheringState === 'complete') done();
    };
    pc.addEventListener('icegatheringstatechange', check);
    setTimeout(done, ms);
  });
}

export function describeAudioState(s: AudioState | null | undefined): { type: 'success' | 'pending' | 'stopped' | 'warning' | 'error'; text: string } {
  switch (s?.state) {
    case 'streaming':
      return { type: 'success', text: `Streaming (${s.lost} of ${s.received + s.lost} packets lost)` };
    case 'starting':
      return { type: 'pending', text: 'Connecting to the scanner' };
    case 'cooldown':
      return { type: 'pending', text: `Waiting ${s.retry_in_secs} s before reconnecting (protects the scanner)` };
    case 'failed':
      return { type: 'error', text: `Audio failed: ${s.message}` };
    case 'idle':
      return { type: 'stopped', text: 'Idle (starts when someone listens or records)' };
    default:
      return { type: 'stopped', text: 'No audio status yet' };
  }
}

/**
 * Listens to a scanner over WebRTC (WHEP). The browser offers a
 * receive-only audio transceiver; the gateway answers and streams Opus.
 */
export function LiveAudio({ scannerId, state }: { scannerId: string; state: AudioState | null | undefined }) {
  const { api } = useAuth();
  const [phase, setPhase] = useState<Phase>('idle');
  const [error, setError] = useState<string | null>(null);
  const pc = useRef<RTCPeerConnection | null>(null);
  const session = useRef<string | null>(null);
  const audio = useRef<HTMLAudioElement | null>(null);

  const stop = async () => {
    pc.current?.close();
    pc.current = null;
    const loc = session.current;
    session.current = null;
    if (audio.current) audio.current.srcObject = null;
    setPhase('idle');
    if (loc) await api.raw(loc, { method: 'DELETE', keepalive: true }).catch(() => undefined);
  };

  useEffect(() => {
    return () => {
      void stop();
    };
  }, [scannerId]);

  const start = async () => {
    setError(null);
    setPhase('connecting');
    try {
      const conn = new RTCPeerConnection();
      pc.current = conn;
      conn.addTransceiver('audio', { direction: 'recvonly' });
      conn.ontrack = (e) => {
        if (audio.current) {
          audio.current.srcObject = e.streams[0] ?? new MediaStream([e.track]);
          void audio.current.play().catch(() => undefined);
        }
        setPhase('playing');
      };
      conn.onconnectionstatechange = () => {
        if (conn.connectionState === 'failed') {
          setError('The audio connection failed. Check that the gateway media port is reachable.');
          void stop().then(() => setPhase('error'));
        }
      };
      await conn.setLocalDescription(await conn.createOffer());
      await gathered(conn);
      const resp = await api.raw(`/scanners/${scannerId}/audio/whep`, {
        method: 'POST',
        body: conn.localDescription?.sdp ?? '',
        contentType: 'application/sdp',
      });
      session.current = resp.headers.get('Location');
      await conn.setRemoteDescription({ type: 'answer', sdp: await resp.text() });
    } catch (e) {
      setError(describeError(e));
      await stop();
      setPhase('error');
    }
  };

  const st = describeAudioState(state);
  return (
    <SpaceBetween size="s">
      <Box variant="small" color="text-body-secondary">
        Only listen to communications you are authorized to receive. Listening is recorded in the audit log.
      </Box>
      <SpaceBetween direction="horizontal" size="xs" alignItems="center">
        {phase === 'idle' || phase === 'error' ? (
          <Button iconName="caret-right-filled" variant="primary" onClick={() => void start()}>
            Listen live
          </Button>
        ) : (
          <Button iconName="close" onClick={() => void stop()} loading={phase === 'connecting'}>
            Stop
          </Button>
        )}
        {phase === 'playing' && <StatusIndicator type="success">Playing</StatusIndicator>}
        {phase === 'connecting' && <StatusIndicator type="pending">Connecting</StatusIndicator>}
      </SpaceBetween>
      <audio ref={audio} controls hidden={phase !== 'playing'} aria-label="Live scanner audio" />
      <StatusIndicator type={st.type}>{st.text}</StatusIndicator>
      {error && <Alert type="error">{error}</Alert>}
    </SpaceBetween>
  );
}
