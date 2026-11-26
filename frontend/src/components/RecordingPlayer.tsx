import { useEffect, useState } from 'react';
import Alert from '@cloudscape-design/components/alert';
import Spinner from '@cloudscape-design/components/spinner';
import { describeError } from '../api/client';
import { useAuth } from '../auth/AuthContext';

/**
 * Plays a recorded transmission. The audio is fetched with the bearer
 * token and played from a blob URL (the CSP allows `media-src blob:`).
 */
export function RecordingPlayer({ transmissionId }: { transmissionId: string }) {
  const { api } = useAuth();
  const [url, setUrl] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let created: string | null = null;
    let cancelled = false;
    api
      .raw(`/transmissions/${transmissionId}/audio`)
      .then((r) => r.blob())
      .then((b) => {
        if (cancelled) return;
        created = URL.createObjectURL(b);
        setUrl(created);
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(describeError(e));
      });
    return () => {
      cancelled = true;
      if (created) URL.revokeObjectURL(created);
    };
  }, [api, transmissionId]);
  if (error) return <Alert type="error">{error}</Alert>;
  if (!url) return <Spinner />;
  return <audio controls src={url} aria-label="Recorded transmission" style={{ width: "100%" }} />;
}
