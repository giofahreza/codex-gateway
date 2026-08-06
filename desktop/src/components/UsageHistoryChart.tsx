import { ContextHistory } from '../api/gateway';

type Props = {
  history: ContextHistory | null;
};

export function UsageHistoryChart({ history }: Props) {
  const buckets = normalizeBuckets(history);
  const width = 900;
  const height = 260;
  const padding = { top: 18, right: 18, bottom: 34, left: 54 };
  const plotWidth = width - padding.left - padding.right;
  const plotHeight = height - padding.top - padding.bottom;
  const maxTokens = Math.max(1, ...buckets.map((bucket) => bucket.totalTokens));
  const points = buckets.map((bucket, index) => {
    const x = padding.left + (buckets.length <= 1 ? 0 : (index / (buckets.length - 1)) * plotWidth);
    const y = padding.top + plotHeight - (bucket.totalTokens / maxTokens) * plotHeight;
    return { x, y, bucket };
  });
  const path = points.map((point, index) => `${index === 0 ? 'M' : 'L'} ${point.x.toFixed(2)} ${point.y.toFixed(2)}`).join(' ');
  const areaPath = points.length
    ? `${path} L ${points[points.length - 1].x.toFixed(2)} ${padding.top + plotHeight} L ${padding.left} ${padding.top + plotHeight} Z`
    : '';

  return (
    <section className="panel chart-panel">
      <div className="panel-header">
        <div>
          <h2>Context Usage</h2>
          <p>{history ? `${history.hours}h window, ${history.bucket_minutes}m buckets` : 'No history loaded'}</p>
        </div>
        <div className="chart-total">{formatNumber(buckets.reduce((sum, bucket) => sum + bucket.totalTokens, 0))} tokens</div>
      </div>
      <div className="chart-wrap">
        <svg viewBox={`0 0 ${width} ${height}`} role="img" aria-label="Token usage over time">
          <line className="axis" x1={padding.left} y1={padding.top} x2={padding.left} y2={padding.top + plotHeight} />
          <line className="axis" x1={padding.left} y1={padding.top + plotHeight} x2={width - padding.right} y2={padding.top + plotHeight} />
          {[0, 0.25, 0.5, 0.75, 1].map((tick) => {
            const y = padding.top + plotHeight - tick * plotHeight;
            return (
              <g key={tick}>
                <line className="grid-line" x1={padding.left} y1={y} x2={width - padding.right} y2={y} />
                <text className="axis-label" x={padding.left - 10} y={y + 4} textAnchor="end">
                  {formatShort(maxTokens * tick)}
                </text>
              </g>
            );
          })}
          {areaPath && <path className="area" d={areaPath} />}
          {path && <path className="line" d={path} />}
          {points.map((point, index) => (
            <circle key={`${point.bucket.label}-${index}`} className="point" cx={point.x} cy={point.y} r={2.8}>
              <title>
                {point.bucket.label} - {formatNumber(point.bucket.totalTokens)} tokens
              </title>
            </circle>
          ))}
          {points.length > 0 && (
            <>
              <text className="axis-label" x={padding.left} y={height - 8}>
                {points[0].bucket.label}
              </text>
              <text className="axis-label" x={width - padding.right} y={height - 8} textAnchor="end">
                {points[points.length - 1].bucket.label}
              </text>
            </>
          )}
        </svg>
        {points.length === 0 && <div className="empty-state">No usage buckets for the selected range.</div>}
      </div>
    </section>
  );
}

type ChartBucket = {
  label: string;
  totalTokens: number;
};

function normalizeBuckets(history: ContextHistory | null): ChartBucket[] {
  if (!history) return [];
  const labels = history.labels ?? [];
  if (history.buckets?.length) {
    return history.buckets.map((bucket, index) => ({
      label: labels[index] ?? `Bucket ${index + 1}`,
      totalTokens: bucket.total_tokens ?? 0,
    }));
  }
  if (history.models) {
    const length = Math.max(0, ...Object.values(history.models).map((rows) => rows.length));
    return Array.from({ length }, (_, index) => {
      const totalTokens = Object.values(history.models ?? {}).reduce((sum, rows) => {
        const row = rows[index];
        return sum + (row ? (row.input ?? 0) + (row.output ?? 0) + (row.cache ?? 0) + (row.reasoning ?? 0) : 0);
      }, 0);
      return {
        label: labels[index] ?? `Bucket ${index + 1}`,
        totalTokens,
      };
    });
  }
  return [];
}

function formatNumber(value: number) {
  return new Intl.NumberFormat().format(Math.round(value));
}

function formatShort(value: number) {
  return new Intl.NumberFormat(undefined, { notation: 'compact', maximumFractionDigits: 1 }).format(Math.round(value));
}
