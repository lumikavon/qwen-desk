export type ModelKind = 'generation' | 'embedding';
export interface ModelSpec { id: string; name: string; kind: ModelKind; source_repo: string; filename: string; path: string; compatibility: string }
export interface Settings {
  models: ModelSpec[];
  inference: { context_size: number; batch_size: number; threads: number; gpu_layers: number; gpu_memory_mb: number; cache_type: string };
  service: { host: string; port: number; concurrency: number; timeout_secs: number; auto_start: boolean };
  app: { autostart: boolean; silent_start: boolean; close_to_tray: boolean; tray_enabled: boolean; log_level: string };
}
export interface EngineState { status?: string; model?: string | Partial<ModelSpec> | null; mode?: string; error?: unknown; active_requests?: number }
export interface Snapshot {
  settings: Settings;
  engine: { generation?: EngineState; embedding?: EngineState; last_inference?: Record<string, unknown> | null };
  inventory: { id: string; path: string; size: number; exists: boolean; managed: boolean }[];
  downloads: { id: string; status: string; downloaded?: number; total?: number; error?: unknown; path?: string }[];
  service: { running: boolean; address?: string };
  hf: { status?: string; message?: string };
  paths: { models?: string; logs?: string };
  version?: string; dev_mode?: boolean; startup_error?: unknown;
}
export function record(value: unknown): Record<string, any> { return value !== null && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, any> : {}; }
export function errorMessage(error: unknown): string { const object = record(error); return typeof object.message === 'string' ? object.message : typeof error === 'string' ? error : '操作失败，请检查运行状态或日志。'; }
