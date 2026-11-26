/** 0–4 signal bars as reported by the scanner (`Property@Sig`). */
export function SignalBars({ bars, rssi }: { bars: number | null | undefined; rssi?: number | null }) {
  const n = bars ?? 0;
  const label =
    bars === null || bars === undefined
      ? 'Signal not reported'
      : `Signal ${bars} of 4${rssi !== null && rssi !== undefined ? `, RSSI ${rssi}` : ''}`;
  return (
    <span role="img" aria-label={label} title={label} className="rp-bars">
      {[1, 2, 3, 4].map((i) => (
        <span key={i} className={i <= n ? 'rp-bar on' : 'rp-bar'} style={{ height: `${4 + i * 3}px` }} />
      ))}
    </span>
  );
}
