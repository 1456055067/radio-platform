import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { Permission } from '../api/types';
import { authValue, fakeFetch, renderWith, transmission } from '../test/fixtures';
import { TranscriptCell, TransmissionDetail } from './TransmissionDetail';

const done = transmission({
  audio_object: 'k',
  transcription_state: 'done',
  transcript: 'Engine 5 responding <b>Main</b> Street',
  transcript_model: 'whisper-1',
  transcript_language: 'en',
  transcribed_at: '2026-11-25T10:01:00Z',
});

function show(perms: Permission[], t = done, routes: Record<string, unknown> = {}) {
  const f = fakeFetch(routes);
  const onChange = vi.fn();
  render(renderWith(<TransmissionDetail t={t} onClose={() => {}} onChange={onChange} />, { auth: authValue(['event_view', ...perms], f) }));
  return { f, onChange };
}

describe('transcripts', () => {
  it('shows the transcript as text, labelled machine-generated', () => {
    show(['audio_listen']);
    expect(screen.getByText('Engine 5 responding <b>Main</b> Street')).toBeInTheDocument();
    expect(screen.getByText(/Machine-generated and possibly wrong/)).toHaveTextContent('whisper-1 · language en');
    expect(screen.queryByRole('button', { name: 'Transcribe again' })).not.toBeInTheDocument();
  });

  it('explains what a user without audio_listen is missing', () => {
    show([], { ...done, transcript: null });
    expect(screen.getByText(/Reading transcripts requires the audio_listen permission/)).toBeInTheDocument();
  });

  it('describes every state in the table cell', () => {
    const cell = (t: Parameters<typeof TranscriptCell>[0]['t'], canRead = true) => render(<TranscriptCell t={t} canRead={canRead} />).container.textContent;
    expect(cell(transmission())).toBe('—');
    expect(cell(transmission({ transcription_state: 'pending' }))).toBe('Queued');
    expect(cell(transmission({ transcription_state: 'failed' }))).toBe('Failed');
    expect(cell(transmission({ transcription_state: 'done', transcript: '' }))).toBe('No speech');
    expect(cell(done, false)).toBe('Transcribed');
    expect(cell({ ...done, transcript: 'x'.repeat(200) })).toHaveLength(91);
  });

  it('re-transcribes once and reloads the record', async () => {
    const user = userEvent.setup();
    const { f, onChange } = show(['audio_listen', 'transcription_manage'], done, {
      'POST /api/v1/transmissions/tx-1/transcribe': null,
      'GET /api/v1/transmissions/tx-1': { ...done, transcription_state: 'pending' },
    });
    await user.click(screen.getByRole('button', { name: 'Transcribe again' }));
    await waitFor(() => expect(onChange).toHaveBeenCalledWith(expect.objectContaining({ transcription_state: 'pending' })));
    expect(f.mock.calls.filter((c) => c[1]?.method === 'POST')).toHaveLength(1);
  });
});

describe('records', () => {
  it('confirms before deleting a recording, and holds block deletion', async () => {
    const user = userEvent.setup();
    const { f } = show(['recording_manage'], done, {
      'DELETE /api/v1/transmissions/tx-1/audio': null,
      'GET /api/v1/transmissions/tx-1': { ...done, audio_object: null, transcript: null, transcription_state: null },
    });
    await user.click(screen.getByRole('button', { name: 'Delete recording' }));
    expect(f).not.toHaveBeenCalled();
    await user.click(screen.getByRole('button', { name: 'Delete' }));
    await waitFor(() => expect(f.mock.calls.some((c) => c[1]?.method === 'DELETE')).toBe(true));
  });

  it('places a hold', async () => {
    const user = userEvent.setup();
    const { f, onChange } = show(['recording_manage'], { ...done, hold: false }, {
      'PUT /api/v1/transmissions/tx-1/hold': { ...done, hold: true },
      'GET /api/v1/transmissions/tx-1': { ...done, hold: true },
    });
    await user.click(screen.getByRole('button', { name: 'Place hold' }));
    await waitFor(() => expect(onChange).toHaveBeenCalledWith(expect.objectContaining({ hold: true })));
    const put = f.mock.calls.find((c) => c[1]?.method === 'PUT');
    expect(JSON.parse(String(put?.[1]?.body))).toEqual({ hold: true });
  });

  it('disables deletion while held', () => {
    show(['recording_manage'], { ...done, hold: true });
    expect(screen.getByRole('button', { name: 'Delete recording' })).toBeDisabled();
    expect(screen.getByText(/Retention will not delete this transmission/)).toBeInTheDocument();
  });
});
