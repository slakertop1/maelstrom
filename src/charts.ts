// SVG chart builders that return markup strings. Used both for live charts in
// the app (via dangerouslySetInnerHTML) and for the exported standalone HTML
// report, so they must not depend on React or external CSS.
import { tr } from "./i18n";

export interface Series {
  name: string;
  color: string;
  points: { x: number; y: number }[];
  fill?: boolean;
}

const TEXT = "#9aa0ab";
const GRID = "rgba(255,255,255,0.08)";
const AXIS = "rgba(255,255,255,0.18)";

let gradientCounter = 0;

function esc(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

export function fmtNum(v: number): string {
  if (!isFinite(v)) return "0";
  if (Math.abs(v) >= 1_000_000) return (v / 1_000_000).toFixed(1) + "M";
  if (Math.abs(v) >= 10_000) return (v / 1000).toFixed(1) + "k";
  if (Math.abs(v) >= 100) return Math.round(v).toString();
  if (Number.isInteger(v)) return v.toString();
  if (Math.abs(v) >= 10) return v.toFixed(1);
  return v.toFixed(2);
}

export function fmtMs(v: number): string {
  if (v >= 10_000) return (v / 1000).toFixed(1) + " " + tr("s");
  if (v >= 1000) return (v / 1000).toFixed(2) + " " + tr("s");
  return fmtNum(v) + " " + tr("ms");
}

function niceTicks(max: number, count = 5): number[] {
  if (!(max > 0)) max = 1;
  const rough = max / count;
  const mag = Math.pow(10, Math.floor(Math.log10(rough)));
  const norm = rough / mag;
  const step = (norm >= 5 ? 10 : norm >= 2 ? 5 : norm >= 1 ? 2 : 1) * mag;
  // Round the axis top UP to the next step so the highest data point always
  // fits inside the plot area — the y-axis expands to contain the peak.
  const niceMax = Math.ceil(max / step - 1e-9) * step;
  const ticks: number[] = [];
  for (let v = 0; v <= niceMax + step * 0.001; v += step) ticks.push(v);
  return ticks;
}

export interface LineChartOpts {
  series: Series[];
  width?: number;
  height?: number;
  yFormat?: (v: number) => string;
  xFormat?: (v: number) => string;
  xLabel?: string;
}

export function lineChart(opts: LineChartOpts): string {
  const W = opts.width ?? 760;
  const H = opts.height ?? 260;
  const padL = 56;
  const padR = 16;
  const padT = 30;
  const padB = 34;
  const iw = W - padL - padR;
  const ih = H - padT - padB;
  const yFmt = opts.yFormat ?? fmtNum;
  const xFmt = opts.xFormat ?? fmtNum;

  const allPoints = opts.series.flatMap((s) => s.points);
  // Fold instead of spreading into Math.min/max — a long run can have thousands
  // of points, and `Math.max(...bigArray)` overflows the argument/stack limit.
  const xMin = allPoints.length ? allPoints.reduce((m, p) => Math.min(m, p.x), Infinity) : 0;
  const xMax = allPoints.length ? allPoints.reduce((m, p) => Math.max(m, p.x), -Infinity) : 1;
  const yMaxRaw = allPoints.length ? allPoints.reduce((m, p) => Math.max(m, p.y), -Infinity) : 1;
  const ticks = niceTicks(yMaxRaw * 1.08 || 1);
  const yMax = ticks[ticks.length - 1] || 1;
  const xSpan = xMax - xMin || 1;

  const sx = (x: number) => padL + ((x - xMin) / xSpan) * iw;
  const sy = (y: number) => padT + ih - (y / yMax) * ih;

  let out = `<svg viewBox="0 0 ${W} ${H}" width="100%" xmlns="http://www.w3.org/2000/svg" font-family="Segoe UI, Arial, sans-serif">`;

  // grid + y labels
  for (const t of ticks) {
    const y = sy(t);
    out += `<line x1="${padL}" y1="${y}" x2="${W - padR}" y2="${y}" stroke="${GRID}" stroke-width="1"/>`;
    out += `<text x="${padL - 8}" y="${y + 4}" text-anchor="end" font-size="11" fill="${TEXT}">${esc(yFmt(t))}</text>`;
  }
  // x labels (up to 8)
  const xTickCount = Math.min(8, Math.max(2, Math.floor(iw / 90)));
  for (let i = 0; i <= xTickCount; i++) {
    const xv = xMin + (xSpan * i) / xTickCount;
    const x = sx(xv);
    out += `<text x="${x}" y="${H - 10}" text-anchor="middle" font-size="11" fill="${TEXT}">${esc(xFmt(xv))}</text>`;
  }
  out += `<line x1="${padL}" y1="${padT + ih}" x2="${W - padR}" y2="${padT + ih}" stroke="${AXIS}" stroke-width="1"/>`;

  // series
  for (const s of opts.series) {
    if (!s.points.length) continue;
    const pts = s.points
      .slice()
      .sort((a, b) => a.x - b.x)
      .map((p) => `${sx(p.x).toFixed(1)},${sy(p.y).toFixed(1)}`);
    if (s.fill) {
      const gid = `grad${gradientCounter++}`;
      out += `<defs><linearGradient id="${gid}" x1="0" y1="0" x2="0" y2="1">`;
      out += `<stop offset="0%" stop-color="${s.color}" stop-opacity="0.35"/>`;
      out += `<stop offset="100%" stop-color="${s.color}" stop-opacity="0.02"/>`;
      out += `</linearGradient></defs>`;
      const first = s.points.reduce((a, b) => (a.x < b.x ? a : b));
      const last = s.points.reduce((a, b) => (a.x > b.x ? a : b));
      out += `<polygon points="${sx(first.x).toFixed(1)},${(padT + ih).toFixed(1)} ${pts.join(" ")} ${sx(last.x).toFixed(1)},${(padT + ih).toFixed(1)}" fill="url(#${gid})"/>`;
    }
    out += `<polyline points="${pts.join(" ")}" fill="none" stroke="${s.color}" stroke-width="2" stroke-linejoin="round" stroke-linecap="round"/>`;
  }

  // legend
  let lx = padL;
  for (const s of opts.series) {
    out += `<circle cx="${lx + 5}" cy="14" r="5" fill="${s.color}"/>`;
    out += `<text x="${lx + 15}" y="18" font-size="12" fill="${TEXT}">${esc(s.name)}</text>`;
    lx += 15 + s.name.length * 7 + 22;
  }

  if (opts.xLabel) {
    out += `<text x="${W - padR}" y="${H - 10}" text-anchor="end" font-size="11" fill="${TEXT}" opacity="0.7">${esc(opts.xLabel)}</text>`;
  }

  out += `</svg>`;
  return out;
}

export function barChart(
  buckets: { label: string; count: number }[],
  opts?: { width?: number; height?: number; color?: string }
): string {
  const W = opts?.width ?? 760;
  const H = opts?.height ?? 260;
  const color = opts?.color ?? "#4c9df3";
  const padL = 56;
  const padR = 16;
  const padT = 16;
  const padB = 46;
  const iw = W - padL - padR;
  const ih = H - padT - padB;

  const maxCount = Math.max(1, ...buckets.map((b) => b.count));
  const ticks = niceTicks(maxCount * 1.05);
  const yMax = ticks[ticks.length - 1] || 1;
  const n = Math.max(1, buckets.length);
  const bw = iw / n;

  let out = `<svg viewBox="0 0 ${W} ${H}" width="100%" xmlns="http://www.w3.org/2000/svg" font-family="Segoe UI, Arial, sans-serif">`;
  for (const t of ticks) {
    const y = padT + ih - (t / yMax) * ih;
    out += `<line x1="${padL}" y1="${y}" x2="${W - padR}" y2="${y}" stroke="${GRID}"/>`;
    out += `<text x="${padL - 8}" y="${y + 4}" text-anchor="end" font-size="11" fill="${TEXT}">${esc(fmtNum(t))}</text>`;
  }
  buckets.forEach((b, i) => {
    const h = (b.count / yMax) * ih;
    const x = padL + i * bw + bw * 0.12;
    const y = padT + ih - h;
    out += `<rect x="${x.toFixed(1)}" y="${y.toFixed(1)}" width="${(bw * 0.76).toFixed(1)}" height="${Math.max(h, b.count > 0 ? 2 : 0).toFixed(1)}" rx="3" fill="${color}" opacity="0.9"><title>${esc(b.label)}: ${b.count}</title></rect>`;
    const every = Math.ceil(n / 8);
    if (i % every === 0) {
      out += `<text x="${(padL + i * bw + bw / 2).toFixed(1)}" y="${H - 26}" text-anchor="middle" font-size="10" fill="${TEXT}" transform="rotate(0)">${esc(b.label)}</text>`;
    }
  });
  out += `<line x1="${padL}" y1="${padT + ih}" x2="${W - padR}" y2="${padT + ih}" stroke="${AXIS}"/>`;
  out += `<text x="${(padL + iw / 2).toFixed(1)}" y="${H - 8}" text-anchor="middle" font-size="11" fill="${TEXT}" opacity="0.7">${esc(tr("response time"))}</text>`;
  out += `</svg>`;
  return out;
}

const DONUT_PALETTE = [
  "#3fb950",
  "#4c9df3",
  "#d29922",
  "#f0649a",
  "#a371f7",
  "#f85149",
  "#39c5cf",
  "#ff9d5c",
];

export function donutChart(
  slices: { label: string; value: number }[],
  opts?: { width?: number; height?: number }
): string {
  const W = opts?.width ?? 460;
  const H = opts?.height ?? 240;
  const cx = 118;
  const cy = H / 2;
  const rOuter = 88;
  const rInner = 56;
  const total = slices.reduce((a, s) => a + s.value, 0) || 1;

  const colorFor = (label: string, i: number): string => {
    if (/^2\d\d$/.test(label)) return "#3fb950";
    if (/^3\d\d$/.test(label)) return "#4c9df3";
    if (/^4\d\d$/.test(label)) return "#d29922";
    if (/^5\d\d$/.test(label)) return "#f85149";
    if (label.toLowerCase().includes("ошибка") || label.toLowerCase().includes("error"))
      return "#8b3a3a";
    return DONUT_PALETTE[i % DONUT_PALETTE.length];
  };

  let out = `<svg viewBox="0 0 ${W} ${H}" width="100%" xmlns="http://www.w3.org/2000/svg" font-family="Segoe UI, Arial, sans-serif">`;
  let angle = -Math.PI / 2;

  slices.forEach((s, i) => {
    const frac = s.value / total;
    const a0 = angle;
    const a1 = angle + frac * Math.PI * 2;
    angle = a1;
    const large = a1 - a0 > Math.PI ? 1 : 0;
    const color = colorFor(s.label, i);
    if (frac >= 0.999) {
      out += `<circle cx="${cx}" cy="${cy}" r="${(rOuter + rInner) / 2}" fill="none" stroke="${color}" stroke-width="${rOuter - rInner}"/>`;
    } else if (frac > 0.0005) {
      const x0o = cx + rOuter * Math.cos(a0);
      const y0o = cy + rOuter * Math.sin(a0);
      const x1o = cx + rOuter * Math.cos(a1);
      const y1o = cy + rOuter * Math.sin(a1);
      const x0i = cx + rInner * Math.cos(a1);
      const y0i = cy + rInner * Math.sin(a1);
      const x1i = cx + rInner * Math.cos(a0);
      const y1i = cy + rInner * Math.sin(a0);
      out += `<path d="M ${x0o.toFixed(2)} ${y0o.toFixed(2)} A ${rOuter} ${rOuter} 0 ${large} 1 ${x1o.toFixed(2)} ${y1o.toFixed(2)} L ${x0i.toFixed(2)} ${y0i.toFixed(2)} A ${rInner} ${rInner} 0 ${large} 0 ${x1i.toFixed(2)} ${y1i.toFixed(2)} Z" fill="${color}" opacity="0.92"><title>${esc(s.label)}: ${s.value}</title></path>`;
    }
  });

  out += `<text x="${cx}" y="${cy - 2}" text-anchor="middle" font-size="22" font-weight="600" fill="#e6e8ec">${esc(fmtNum(total))}</text>`;
  out += `<text x="${cx}" y="${cy + 18}" text-anchor="middle" font-size="11" fill="${TEXT}">${esc(tr("requests"))}</text>`;

  // legend
  let ly = cy - slices.length * 12 + 4;
  slices.forEach((s, i) => {
    const color = colorFor(s.label, i);
    const pct = ((s.value / total) * 100).toFixed(1);
    out += `<rect x="240" y="${ly - 9}" width="11" height="11" rx="3" fill="${color}"/>`;
    out += `<text x="258" y="${ly + 1}" font-size="12" fill="#e6e8ec">${esc(s.label)}</text>`;
    out += `<text x="${W - 12}" y="${ly + 1}" text-anchor="end" font-size="12" fill="${TEXT}">${esc(fmtNum(s.value))} · ${pct}%</text>`;
    ly += 24;
  });

  out += `</svg>`;
  return out;
}
