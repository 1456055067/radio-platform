import StatusIndicator from '@cloudscape-design/components/status-indicator';
import type { Scanner, ScannerHealth } from '../api/types';
import { scannerStatus } from '../lib/format';

const TYPE = {
  success: 'success',
  warning: 'warning',
  error: 'error',
  stopped: 'stopped',
  pending: 'pending',
} as const;

/** Scanner status from the record, overridden by any newer live health. */
export function ScannerStatus({ scanner, liveHealth }: { scanner: Scanner; liveHealth?: ScannerHealth }) {
  const s = scannerStatus({ ...scanner, health: liveHealth ?? scanner.health });
  return <StatusIndicator type={TYPE[s.tone]}>{s.label}</StatusIndicator>;
}
