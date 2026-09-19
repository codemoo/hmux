import type { Terminal } from "@xterm/xterm";
import type { FitAddon } from "@xterm/addon-fit";
import type { createConnectionRecovery } from "./connection-recovery";
export type Identity = { id: string; created_at: number };
export type Session = Identity & {
  name: string;
  alias?: string;
  hidden?: boolean;
  runtime?: string;
  state?: string;
  current_path?: string;
  window_count: number;
  attached_clients: number;
  restored_from?: Identity;
};
export type Metrics = {
  observed_at: string;
  cpu_percent?: number;
  gpu_percent?: number;
  memory_used_bytes?: number;
  memory_total_bytes?: number;
  disk_used_bytes?: number;
  disk_total_bytes?: number;
};
export type Quota = { used_pct: number; resets_at?: string };
export type Account = {
  number: number;
  display_name?: string;
  email?: string;
  active: boolean;
  status: string;
  seven_day?: Quota;
  five_hour?: Quota;
};
export type Usage = {
  burn_state?: string;
  provider: string;
  generated_at_utc: string;
  weekly: Quota;
  weekly_observed: boolean;
  rolling_5h: Quota;
  rolling_5h_observed: boolean;
  accounts?: Account[];
  status: { stale: boolean; state: string; quota_observed_at?: string };
};
export type Snapshot = {
  online: boolean;
  catalog?: { sessions: Session[]; host_metrics?: Metrics };
  usage?: Record<string, Usage>;
};
export type Tab = {
  identity: Identity;
  term: Terminal;
  fit: FitAddon;
  host: HTMLElement;
  ws?: WebSocket;
  status: "connecting" | "connected" | "disconnected";
  generation: number;
  recovery: ReturnType<typeof createConnectionRecovery>;
  retryTimer?: number;
  openTimer?: number;
  nativeInput?: { flush(): void; cancel(): void; dispose(): void };
  disposeNativePaste?: () => void;
  interaction?: { hide(): void; dispose(): void };
};
