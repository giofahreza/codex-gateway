import { CalendarClock, Database, Gauge, Zap } from 'lucide-react';
import { AccountUsage, UsageTotals } from '../api/gateway';

type Props = {
  totals: UsageTotals | null;
  accounts: AccountUsage[];
  lastRefresh: Date | null;
};

export function UsageOverview({ totals, accounts, lastRefresh }: Props) {
  const requests = totals?.requests ?? 0;
  const errors = totals?.errors ?? 0;
  const totalTokens = totals?.total_tokens ?? 0;
  const activeAccounts = accounts.filter((account) => account.requests > 0 || account.totalTokens > 0).length;
  const errorRate = requests > 0 ? (errors / requests) * 100 : 0;

  return (
    <section className="overview-grid">
      <Metric icon={<Gauge size={20} />} label="Requests" value={formatNumber(requests)} detail={`${formatNumber(errors)} errors`} />
      <Metric icon={<Zap size={20} />} label="Tokens" value={formatNumber(totalTokens)} detail={`${formatNumber(totals?.input_tokens ?? 0)} in / ${formatNumber(totals?.output_tokens ?? 0)} out`} />
      <Metric icon={<Database size={20} />} label="Accounts" value={formatNumber(activeAccounts)} detail={`${formatNumber(accounts.length)} tracked`} />
      <Metric icon={<CalendarClock size={20} />} label="Error Rate" value={`${errorRate.toFixed(1)}%`} detail={lastRefresh ? `Updated ${lastRefresh.toLocaleTimeString()}` : 'Waiting for data'} />
    </section>
  );
}

function Metric({ icon, label, value, detail }: { icon: React.ReactNode; label: string; value: string; detail: string }) {
  return (
    <div className="metric-card">
      <div className="metric-icon">{icon}</div>
      <div>
        <div className="metric-label">{label}</div>
        <div className="metric-value">{value}</div>
        <div className="metric-detail">{detail}</div>
      </div>
    </div>
  );
}

function formatNumber(value: number) {
  return new Intl.NumberFormat().format(value);
}
