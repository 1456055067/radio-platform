import type { Site } from '../api/types';

/**
 * Relative positions of sites from their coordinates. No map tiles are
 * fetched, so this works air-gapped and under the strict CSP.
 */
export function SiteMap({ sites, problem }: { sites: Site[]; problem: (siteId: string) => boolean }) {
  const pts = sites.filter((s) => s.latitude !== null && s.longitude !== null);
  if (pts.length === 0) return null;
  const lats = pts.map((s) => s.latitude as number);
  const lons = pts.map((s) => s.longitude as number);
  const [minLat, maxLat] = [Math.min(...lats), Math.max(...lats)];
  const [minLon, maxLon] = [Math.min(...lons), Math.max(...lons)];
  const W = 600;
  const H = 220;
  const pad = 40;
  const sx = (lon: number) => (maxLon === minLon ? W / 2 : pad + ((lon - minLon) / (maxLon - minLon)) * (W - 2 * pad));
  const sy = (lat: number) => (maxLat === minLat ? H / 2 : H - pad - ((lat - minLat) / (maxLat - minLat)) * (H - 2 * pad));
  return (
    <svg className="rp-map" viewBox={`0 0 ${W} ${H}`} role="img" aria-label="Relative site locations">
      <rect className="rp-map-frame" x="1" y="1" width={W - 2} height={H - 2} rx="8" />
      {pts.map((s) => {
        const x = sx(s.longitude as number);
        const y = sy(s.latitude as number);
        return (
          <g key={s.id}>
            <circle className={problem(s.id) ? 'rp-map-dot bad' : 'rp-map-dot'} cx={x} cy={y} r="7" />
            <text className="rp-map-label" x={x > W * 0.6 ? x - 11 : x + 11} y={y + 4} textAnchor={x > W * 0.6 ? 'end' : 'start'}>
              {s.name}
            </text>
          </g>
        );
      })}
    </svg>
  );
}
