// Mirrors the gateway's v1 JSON shapes (see /api/v1/openapi.json).
// Every telemetry field is optional: a value the scanner did not report is
// null, never invented.

export type Permission =
  | 'scanner_view'
  | 'scanner_control'
  | 'scanner_admin'
  | 'scanner_manage'
  | 'site_manage'
  | 'event_view'
  | 'audit_view'
  | 'audio_listen'
  | 'recording_manage'
  | 'transcription_manage'
  | 'export';

export interface Me {
  subject: string;
  name: string | null;
  tenant_id: string;
  /** null = every site in the tenant */
  sites: string[] | null;
  permissions: Permission[];
  expires_at: string;
}

export interface Page<T> {
  items: T[];
  next_cursor: string | null;
}

export interface Site {
  id: string;
  tenant_id: string;
  name: string;
  description: string | null;
  latitude: number | null;
  longitude: number | null;
  allowed_networks: string[];
  created_at: string;
}

export interface Agent {
  id: string;
  tenant_id: string;
  site_id: string;
  description: string | null;
  last_seen_at: string | null;
  connected: boolean;
}

export type ScannerHealth =
  | { state: 'unknown' }
  | { state: 'healthy' }
  | { state: 'degraded'; consecutive_failures: number }
  | { state: 'unreachable'; consecutive_failures: number };

export interface ControlView {
  lease_id: string | null;
  holder: string;
  expires_at: string;
}

export interface Scanner {
  id: string;
  tenant_id: string;
  site_id: string;
  agent_id: string;
  name: string;
  host: string;
  port: number;
  enabled: boolean;
  model: string | null;
  firmware: string | null;
  health: ScannerHealth | null;
  audio_enabled: boolean;
  recording_enabled: boolean;
  transcription_enabled: boolean;
  rtsp_port: number;
  created_at: string;
  updated_at: string;
  agent_connected: boolean;
  control: ControlView | null;
}

export interface DisplayText {
  info_area1: string | null;
  info_area2: string | null;
  overwrite: string | null;
  popup: string | null;
}

export interface TelemetrySnapshot {
  observed_at: string;
  mode: string | null;
  screen: string | null;
  system: string | null;
  system_index: number | null;
  system_type: string | null;
  department: string | null;
  department_index: number | null;
  site: string | null;
  channel: string | null;
  channel_index: number | null;
  channel_kind: string | null;
  frequency_hz: number | null;
  tgid: string | null;
  unit_id: string | null;
  modulation: string | null;
  service_type: string | null;
  signal_bars: number | null;
  rssi: number | null;
  muted: boolean | null;
  volume: number | null;
  squelch: number | null;
  holding: boolean;
  recording: boolean | null;
  display: DisplayText;
}

export interface TelemetryResponse {
  scanner_id: string;
  snapshot: TelemetrySnapshot | null;
  agent_connected: boolean;
}

export interface Transmission {
  id: string;
  tenant_id: string;
  site_id: string;
  scanner_id: string;
  started_at: string;
  ended_at: string | null;
  duration_ms: number | null;
  frequency_hz: number | null;
  tgid: string | null;
  unit_id: string | null;
  system_name: string | null;
  department_name: string | null;
  channel_name: string | null;
  modulation: string | null;
  signal_bars: number | null;
  rssi: number | null;
  audio_object: string | null;
  /** Machine transcript; null without audio_listen. */
  transcript: string | null;
  transcription_state: TranscriptionState | null;
  transcription_error: string | null;
  transcript_language: string | null;
  transcript_model: string | null;
  transcribed_at: string | null;
  /** Exempt from retention and deletion. */
  hold: boolean;
}

export type TranscriptionState = 'pending' | 'done' | 'failed' | 'skipped';

export interface TranscriptionStatus {
  enabled: boolean;
  /** Request style and endpoint host; only for transcription_manage. */
  service: string | null;
  /** done/failed: transmissions from the last 24 hours. */
  queue: { pending: number; done: number; failed: number };
  worker: { completed: number; retried: number; failed: number; skipped: number; in_flight: number } | null;
}

export interface SweepReport {
  at: string | null;
  recordings: number;
  transcripts: number;
  transmissions: number;
  events: number;
  errors: number;
}

export interface RetentionStatus {
  enabled: boolean;
  recordings_days: number | null;
  transcripts_days: number | null;
  transmissions_days: number | null;
  events_days: number | null;
  /** Only for audit_view. */
  last_sweep: SweepReport | null;
}

export type Bucket = 'hour' | 'day';
export type TopBy = 'talkgroup' | 'channel' | 'frequency' | 'scanner' | 'unit';

export interface Activity {
  bucket: Bucket;
  points: { start: string; transmissions: number; airtime_ms: number }[];
}

export interface Top {
  by: TopBy;
  entries: { key: string; transmissions: number; airtime_ms: number }[];
}

export interface TransmissionStart {
  transmission_id: string;
  scanner_id: string;
  site_id: string | null;
  started_at: string;
  frequency_hz: number | null;
  tgid: string | null;
  unit_id: string | null;
  system: string | null;
  department: string | null;
  channel: string | null;
  modulation: string | null;
  signal_bars: number | null;
  rssi: number | null;
}

export type ScannerEvent =
  | ({ type: 'transmission_started' } & TransmissionStart)
  | { type: 'transmission_ended'; transmission_id: string; ended_at: string; duration_ms: number }
  | { type: 'health_changed'; health: ScannerHealth };

export interface EventRecord {
  id: number;
  tenant_id: string;
  site_id: string;
  scanner_id: string;
  agent_id: string;
  kind: string;
  payload: ScannerEvent;
  received_at: string;
}

export type AuditOutcome = 'success' | 'denied' | 'failure';

export interface AuditRecord {
  id: number;
  at: string;
  tenant_id: string;
  actor: string;
  action: string;
  resource_type: string;
  resource_id: string | null;
  outcome: AuditOutcome;
  details: Record<string, unknown>;
  source_ip: string | null;
  /** Position in the tenant's hash chain; null before the chain existed. */
  seq: number | null;
  prev_hash: string | null;
  hash: string | null;
}

export interface AuditVerification {
  ok: boolean;
  verified: number;
  unchained: number;
  head_seq: number;
  head_hash: string;
  failure: { seq: number; reason: string } | null;
}

export type AudioState =
  | { state: 'idle' }
  | { state: 'cooldown'; retry_in_secs: number }
  | { state: 'starting' }
  | { state: 'streaming'; received: number; lost: number }
  | { state: 'failed'; message: string };

export interface AudioStatus {
  audio_enabled: boolean;
  recording_enabled: boolean;
  live_available: boolean;
  state: AudioState | null;
  listeners: number;
  receiving: boolean;
}

export interface MediaStatus {
  live_enabled: boolean;
  sessions: number;
  streams: number;
  recording_enabled: boolean;
  recording_backend: string | null;
  recordings: { stored: number; failed: number; in_progress: number } | null;
}

export type LiveMessage =
  | { kind: 'telemetry'; site_id: string; scanner_id: string; data: TelemetrySnapshot }
  | { kind: 'event'; site_id: string; scanner_id: string; data: ScannerEvent }
  | { kind: 'audio'; site_id: string; scanner_id: string; data: AudioState }
  | { kind: 'lagged'; skipped: number };

export type HoldKind = 'system' | 'department' | 'site' | 'channel' | 'talkgroup_channel';
export type KeyAction = 'press' | 'long_press';

export type ScannerCommand =
  | { command: 'get_status' }
  | { command: 'probe_capabilities' }
  | { command: 'get_volume' }
  | { command: 'set_volume'; level: number }
  | { command: 'get_squelch' }
  | { command: 'set_squelch'; level: number }
  | { command: 'hold'; kind: HoldKind; index: number }
  | { command: 'resume' }
  | { command: 'key'; code: string; action: KeyAction }
  | { command: 'quick_search_hold'; frequency_hz: number };

export interface Problem {
  type?: string;
  title?: string;
  status: number;
  detail?: string;
}
