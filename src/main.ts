import { invoke, isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { SSEParser } from './sse';
import { badge, button, bytes, escape as h, field, icon, select, toggle } from './ui';
import { settingsForm, validateSettings, isLoopback } from './settings';
import { errorMessage, record, type EngineState, type ModelKind, type ModelSpec, type Settings, type Snapshot } from './types';
import './style.css';

type Route = 'dashboard' | 'models' | 'inference' | 'service' | 'app';
const routes: { id: Route; title: string; icon: string; subtitle: string }[] = [
  { id: 'dashboard', title: '仪表盘', icon: 'dashboard', subtitle: '本地模型与服务，一目了然。' },
  { id: 'models', title: '模型管理', icon: 'models', subtitle: '准备模型，开始本地推理。' },
  { id: 'inference', title: '推理测试', icon: 'inference', subtitle: '从一段文字开始，验证模型的实际表现。' },
  { id: 'service', title: '服务设置', icon: 'service', subtitle: '连接你的应用，分配推理资源。' },
  { id: 'app', title: '应用设置', icon: 'settings', subtitle: '启动方式、桌面行为与诊断。' },
];
const native = isTauri();
let route: Route = routes.some(r => r.id === location.hash.slice(1)) ? location.hash.slice(1) as Route : 'dashboard';
let snapshot: Snapshot | null = null;
let connectionError = '';
let connecting = native;
let pollBusy = false;
let draft: Settings | null = null;
let draftBaseline: Settings | null = null;
let dirty = false;
let pendingRestart = false;
let pendingReload = false;
const busy = new Set<string>();
const signatures = new Map<string, string>();
let currentInference: { request_id: string; parser: SSEParser; start: number; cancelled: boolean } | null = null;
let eventReady = false;
let inferenceKind: ModelKind = 'generation';
let output = '';
let reasoning = '';
let vectors: { index: number; values: number[] }[] = [];
let usage: Record<string, unknown> = {};
let finishReason = '';
let inferenceError = '';
let elapsed = 0;
let inferenceStatus = '等待输入';
let inferenceForm: Record<string, string | boolean> = { mode: 'chat', prompt: '', system: '你是一个有帮助的助手，请用中文回答。', target: '英语', temperature: '0.7', top_p: '0.9', max_tokens: '512', stream: true, texts: '' };
let lastFocus: HTMLElement | null = null;

async function dispatch<T = unknown>(op: string, payload: unknown = {}): Promise<T> {
  if (!native) throw new Error('连接不可用：请在 Qwen Desk 桌面应用中使用此功能。');
  return invoke<T>('dispatch', { op, payload });
}
const app = document.querySelector<HTMLDivElement>('#app')!;
app.innerHTML = `
  <header class="titlebar"><div class="titlebar-brand" data-tauri-drag-region>${icon('logo')}<span data-tauri-drag-region>Qwen Desk</span></div><div class="drag-space" data-tauri-drag-region><span data-tauri-drag-region>本地推理工作台</span></div><div class="window-controls"><button id="devtools" type="button" aria-label="打开开发者工具" title="开发者工具" hidden data-action="window" data-window="devtools">${icon('terminal')}</button><button type="button" aria-label="最小化窗口" title="最小化" data-action="window" data-window="minimize" ${!native ? 'disabled' : ''}>${icon('minus')}</button><button type="button" aria-label="最大化或还原窗口" title="最大化 / 还原" data-action="window" data-window="maximize" ${!native ? 'disabled' : ''}>${icon('maximize')}</button><button type="button" class="window-close" aria-label="关闭窗口" title="关闭" data-action="window" data-window="close" ${!native ? 'disabled' : ''}>${icon('close')}</button></div></header>
  <div class="app-layout"><aside class="sidebar"><div class="workspace-label">工作空间</div><nav aria-label="主导航">${routes.map(r => `<a href="#${r.id}" data-route="${r.id}" aria-label="${r.title}" title="${r.title}">${icon(r.icon)}<span>${r.title}</span></a>`).join('')}</nav><div class="sidebar-bottom"><div class="local-mark">${icon('logo')}<span>在本地，保持掌控</span></div><div id="sidebar-status" class="muted">正在连接…</div><small id="version">Qwen Desk</small></div></aside>
  <main id="main" tabindex="-1"><div id="connection"></div><div id="notice" role="status" aria-live="polite" aria-atomic="true"></div><div class="page-heading"><div><h1 id="page-title"></h1><p id="page-subtitle"></p></div><div id="page-actions" class="actions"></div></div><div id="page"></div></main></div>
  <dialog id="dialog" aria-labelledby="dialog-title"><div id="dialog-content"></div></dialog>`;
const $ = <T extends HTMLElement = HTMLElement>(selector: string) => document.querySelector<T>(selector);

function patch(id: string, html: string): void {
  const el = document.getElementById(id);
  if (!el || signatures.get(id) === html) return;
  const active = document.activeElement as HTMLElement | null;
  const contained = !!active && el.contains(active);
  const focusKey = active?.getAttribute('data-focus-key');
  el.innerHTML = html; signatures.set(id, html);
  if (contained && focusKey) el.querySelector<HTMLElement>(`[data-focus-key="${CSS.escape(focusKey)}"]`)?.focus({ preventScroll: true });
}
function notice(message: string, error = false): void {
  const element = $('#notice')!;
  element.className = `notice ${error ? 'error' : 'success'}`;
  element.setAttribute('role', error ? 'alert' : 'status');
  element.innerHTML = `${icon(error ? 'alert' : 'check')}<span>${h(message)}</span><button type="button" class="icon-button" aria-label="关闭提示" data-action="dismiss-notice">${icon('close')}</button>`;
  if (error) { element.tabIndex = -1; element.focus(); }
}
function clearNotice(): void { $('#notice')!.innerHTML = ''; $('#notice')!.className = ''; }
function online(): boolean { return native && !!snapshot && !connectionError; }
function statusLabel(value: unknown): string {
  const labels: Record<string, string> = { loaded: '已加载', ready: '已就绪', loading: '加载中', unloading: '卸载中', unloaded: '未加载', idle: '未加载', empty: '未加载', error: '错误', failed: '失败', downloading: '下载中', pending: '等待下载', queued: '等待下载', verifying: '校验中', completed: '已下载', complete: '已下载', available: '可用', cancelled: '已取消', canceled: '已取消', missing: '未配置', valid: '认证已配置', configured: '认证已配置', public: '公开访问', running: '运行中', success: '成功', done: '完成' };
  return typeof value === 'string' ? labels[value] ?? value : '状态未知';
}
function stateBadge(state: EngineState | undefined): string {
  const value = state?.status;
  return badge(statusLabel(value), ['ready', 'loaded'].includes(value ?? '') ? 'good' : ['error', 'failed'].includes(value ?? '') ? 'bad' : ['loading', 'unloading'].includes(value ?? '') ? 'working' : 'neutral');
}
function modelIdentity(state?: EngineState): string { return typeof state?.model === 'string' ? state.model : state?.model?.id ?? state?.model?.name ?? ''; }
function modelName(state?: EngineState): string {
  const value = modelIdentity(state);
  return snapshot?.settings.models.find(m => m.id === value || m.name === value)?.name ?? (typeof state?.model === 'object' ? state?.model?.name : value) ?? '未加载模型';
}
function isLoaded(kind: ModelKind): boolean { return ['ready', 'loaded'].includes(snapshot?.engine[kind]?.status ?? ''); }
function isCurrent(model: ModelSpec): boolean { const id = modelIdentity(snapshot?.engine[model.kind]); return id === model.id || id === model.name; }
function transitioning(kind: ModelKind): boolean { return ['loading', 'unloading'].includes(snapshot?.engine[kind]?.status ?? ''); }
function disabled(): boolean { return !online(); }

function renderPage(): void {
  signatures.clear();
  const current = routes.find(r => r.id === route)!;
  $('#page-title')!.textContent = current.title;
  $('#page-subtitle')!.textContent = current.subtitle;
  document.title = `${current.title} · Qwen Desk`;
  document.querySelectorAll<HTMLAnchorElement>('nav a').forEach(a => { a.classList.toggle('active', a.dataset.route === route); if (a.dataset.route === route) a.setAttribute('aria-current', 'page'); else a.removeAttribute('aria-current'); });
  $('#page-actions')!.innerHTML = route === 'models' ? button('添加模型', 'add-model', { icon: 'plus', class: 'primary', disabled: disabled() }) : route === 'dashboard' ? button('刷新状态', 'refresh', { icon: 'refresh', disabled: !native }) : '';
  const page = $('#page')!;
  if (route === 'dashboard') page.innerHTML = '<div id="dashboard-live"></div>';
  if (route === 'models') page.innerHTML = `<div class="callout">${icon('models')}<div><strong>兼容格式：GGUF</strong><p>默认生成模型保留 AWQ 模型的 API 别名，实际运行 unsloth 的 Q4_K_M GGUF。两者为不同量化版本，不会直接加载 AWQ 权重。向量模型使用 Qwen 官方 Q8_0 GGUF，末 token 池化并归一化。</p></div></div><div id="model-list"></div><div class="storage-footer">${icon('folder')}<span>模型存储目录</span><code id="models-path">等待连接</code></div>`;
  if (route === 'inference') renderInference();
  if (route === 'service' || route === 'app') {
    if (!draft && snapshot) { draft = structuredClone(snapshot.settings); draftBaseline = structuredClone(snapshot.settings); }
    page.innerHTML = draft ? settingsForm(draft, route) : '<div class="empty panel"><h2>配置尚不可用</h2><p>连接桌面运行时后，可以读取并编辑已保存的设置。</p></div>';
  }
  updateLive();
}
function connectionView(): void {
  const message = !native ? '连接不可用 · 当前为浏览器预览。请启动 Qwen Desk 桌面应用，以读取真实模型、运行推理和管理服务。' : connectionError ? `桌面连接中断 · ${connectionError} 正在自动重试。` : connecting ? '正在连接本地运行时…' : '';
  patch('connection', (message ? `<div class="connection-banner ${connectionError || !native ? 'error' : ''}" role="status">${icon(connectionError || !native ? 'alert' : 'clock')}<span>${h(message)}</span>${native ? button('重新连接', 'refresh', { disabled: pollBusy }) : ''}</div>` : '') + (snapshot?.startup_error ? `<div class="connection-banner error" role="alert">${icon('alert')}<span>${h(errorMessage(snapshot.startup_error))}</span></div>` : ''));
  patch('sidebar-status', badge(!native ? '未连接桌面运行时' : connectionError ? '连接中断' : snapshot ? '本地运行时已连接' : '正在连接', online() ? 'good' : 'neutral'));
  $('#version')!.textContent = snapshot?.version ? `Qwen Desk ${snapshot.version}` : 'Qwen Desk';
  $('#devtools')!.hidden = !(import.meta.env.DEV && snapshot?.dev_mode === true);
}
function updateLive(): void {
  connectionView();
  if (route === 'dashboard') updateDashboard();
  if (route === 'models') updateModels();
  if (route === 'inference') updateInference();
  if (route === 'service' || route === 'app') {
    const form = $('#settings-form');
    form?.querySelectorAll<HTMLInputElement | HTMLSelectElement | HTMLButtonElement>('input,select,button').forEach(el => { el.disabled = disabled() || busy.has('settings'); });
    patch('settings-dirty', busy.has('settings') ? '正在保存…' : dirty ? '有未保存的修改' : '配置已同步');
    updateNetworkWarning();
    if (snapshot) {
      patch('hf-status', `${badge(snapshot.hf.status === 'ready' ? '凭据已配置' : statusLabel(snapshot.hf.status), ['ready', 'valid', 'configured', 'available'].includes(snapshot.hf.status ?? '') ? 'good' : ['invalid', 'error', 'forbidden'].includes(snapshot.hf.status ?? '') ? 'bad' : 'neutral')}<p>${h(snapshot.hf.message ?? '未收到认证状态。')}</p>`);
      patch('log-path', h(snapshot.paths.logs ?? '未提供日志目录'));
    }
    patch('restart-feedback', pendingRestart || pendingReload ? `<div class="callout"><div><strong>设置已保存</strong><p>${pendingRestart ? '服务参数已更改，请重启正在运行的 API 服务。' : ''}${pendingReload ? '推理参数或模型配置已更改，请卸载并重新加载模型后生效。' : ''}</p></div>${pendingRestart ? button('重启服务', 'service', { data: 'data-service="restart"', disabled: disabled() || busy.has('service') }) : ''}</div>` : '');
  }
  $('#page-actions [data-action="add-model"]')?.toggleAttribute('disabled', disabled());
}
function engineCard(kind: ModelKind): string {
  const state = snapshot?.engine[kind];
  return `<section class="panel engine-card"><div class="section-heading"><span class="category">${icon(kind === 'generation' ? 'inference' : 'models')}${kind === 'generation' ? '生成模型' : '向量模型'}</span>${stateBadge(state)}</div><h2>${h(modelName(state) || '未加载模型')}</h2><p>${kind === 'generation' ? '文本生成 / 翻译 / 对话' : '语义检索 / 文本向量化'}</p><div class="engine-meta"><span>运行模式 <strong>${h(state?.mode || '—')}</strong></span><span>活动请求 <strong>${typeof state?.active_requests === 'number' ? state.active_requests : '—'}</strong></span></div>${state?.error ? `<p class="inline-error">${h(errorMessage(state.error))}</p>` : ''}<a class="text-link" href="#${isLoaded(kind) ? 'inference' : 'models'}">${isLoaded(kind) ? '打开推理测试' : '前往模型管理'}${icon('arrow')}</a></section>`;
}
function serviceButtons(): string {
  const running = snapshot?.service.running;
  return `${button(running ? '停止服务' : '启动服务', 'service', { icon: running ? 'stop' : 'play', class: running ? '' : 'primary', data: `data-service="${running ? 'stop' : 'start'}" data-focus-key="service-toggle"`, disabled: disabled() || busy.has('service') })}${running ? button('重启', 'service', { icon: 'refresh', data: 'data-service="restart" data-focus-key="service-restart"', disabled: disabled() || busy.has('service') }) : ''}`;
}
function updateDashboard(): void {
  if (!snapshot) {
    patch('dashboard-live', `<div class="panel empty">${icon('service')}<h2>${connecting ? '正在读取工作空间' : '等待桌面运行时'}</h2><p>${connecting ? '连接后将显示模型、服务和推理状态。' : '此工作台只展示真实运行数据。请在桌面应用中启动本地工作空间。'}</p>${native ? button('重新连接', 'refresh', { icon: 'refresh' }) : ''}</div>`); return;
  }
  const recent = snapshot.engine.last_inference;
  const address = snapshot.service.address || `http://${snapshot.settings.service.host}:${snapshot.settings.service.port}`;
  patch('dashboard-live', `<section class="panel service-overview"><div><div class="section-heading"><h2>本地 API 服务</h2>${badge(snapshot.service.running ? '运行中' : '已停止', snapshot.service.running ? 'good' : 'neutral')}</div><code class="endpoint">${h(address)}</code><p>OpenAI 兼容 · Chat Completions / Embeddings</p></div><div class="actions">${serviceButtons()}</div></section><div class="engine-grid">${engineCard('generation')}${engineCard('embedding')}</div><div class="dashboard-bottom"><section class="panel"><div class="section-heading"><h2>最近一次推理</h2>${icon('clock')}</div>${recent ? `<dl class="details">${Object.entries(recent).filter(([, val]) => val !== null && typeof val !== 'object').map(([key, value]) => `<div><dt>${h(({ status: '状态', model: '模型', kind: '任务类型', elapsed_ms: '耗时（毫秒）', duration_ms: '耗时（毫秒）', prompt_tokens: '输入 Token', completion_tokens: '输出 Token', total_tokens: '总 Token', tokens_per_second: 'Token / 秒', request_id: '请求 ID', error: '错误', finish_reason: '结束原因' } as Record<string, string>)[key] ?? key)}</dt><dd>${h(value)}</dd></div>`).join('')}</dl>` : '<div class="empty compact"><p>还没有推理记录</p><span>加载一个模型后，在推理测试中发送首个请求。</span></div>'}</section><section class="panel getting-started"><h2>开始使用</h2><ol><li><a href="#models">准备模型</a><span>下载默认 GGUF 或导入本地文件</span></li><li><a href="#inference">验证推理</a><span>测试生成、翻译与文本向量化</span></li><li><a href="#service">连接应用</a><span>设置 API 地址并启动服务</span></li></ol></section></div>`);
}
function updateModels(): void {
  if (!snapshot) { patch('model-list', '<div class="panel empty"><h2>模型列表尚不可用</h2><p>连接桌面应用后读取已注册的模型。</p></div>'); return; }
  patch('models-path', h(snapshot.paths.models ?? '未提供目录'));
  const models = snapshot.settings.models;
  patch('model-list', models.length ? `<div class="library-heading"><h2>模型库 <span>${models.length}</span></h2><span class="muted">生成与向量模型可独立加载</span></div>${models.map(modelRow).join('')}` : '<div class="panel empty"><h2>尚未添加模型</h2><p>添加 Hugging Face GGUF 文件或选择本地模型。</p></div>');
}
function modelRow(model: ModelSpec): string {
  const inv = snapshot?.inventory.find(x => x.id === model.id);
  const download = snapshot?.downloads.find(x => x.id === model.id);
  const loading = transitioning(model.kind);
  const current = isCurrent(model);
  const activeDownload = ['downloading', 'pending', 'queued', 'verifying', 'resolving'].includes(download?.status ?? '');
  const pending = busy.has(`model:${model.id}`);
  const off = disabled() || pending;
  const state = snapshot?.engine[model.kind];
  const available = inv?.exists === true;
  const defaultModel = ['qwen3-generation', 'qwen3-embedding'].includes(model.id);
  const attrs = `data-id="${h(model.id)}"`;
  const action = (label: string, op: string, extra: { disabled?: boolean; icon?: string; class?: string } = {}) => button(label, op, { ...extra, disabled: off || extra.disabled, data: `${attrs} data-focus-key="${h(model.id)}-${op}"` });
  const progress = typeof download?.total === 'number' && download.total > 0 ? Math.min(100, Math.max(0, (download.downloaded ?? 0) / download.total * 100)) : null;
  return `<article class="panel model-row"><div class="model-leading">${icon(model.kind === 'embedding' ? 'models' : 'inference')}</div><div class="model-main"><div class="section-heading"><h2>${h(model.name)}</h2>${badge(model.kind === 'embedding' ? '向量模型' : '生成模型', 'neutral')}${current ? stateBadge(state) : badge(available ? '本地可用' : activeDownload ? statusLabel(download?.status) : '尚未就绪', available ? 'good' : activeDownload ? 'working' : 'neutral')}</div><p class="model-id">${h(model.id)}${defaultModel ? ' · 默认配置' : ' · 自定义模型'}</p><p class="compatibility">${h(model.compatibility || '仅支持与 llama.cpp 兼容的 GGUF 模型。')}</p><dl class="model-details"><div><dt>来源</dt><dd>${h(model.source_repo ? `${model.source_repo} / ${model.filename}` : '本地导入')}</dd></div><div><dt>文件</dt><dd class="path">${h(inv?.path || model.path || '下载完成后生成托管路径')}</dd></div><div><dt>大小</dt><dd>${inv?.exists ? bytes(inv.size) : '—'}${inv ? inv.managed ? ' · 应用托管' : ' · 外部文件' : ''}</dd></div></dl>${activeDownload ? `<div class="download-progress"><div><span>${h(statusLabel(download?.status))}</span><span>${bytes(download?.downloaded ?? 0)} / ${download?.total ? bytes(download.total) : '正在获取大小'}${progress !== null ? ` · ${progress.toFixed(1)}%` : ''}</span></div><progress max="100" ${progress !== null ? `value="${progress}"` : ''} aria-label="${h(model.name)} 下载进度"></progress></div>` : ''}${download?.error ? `<p class="inline-error">${icon('alert')}${h(errorMessage(download.error))}</p>` : ''}${current && state?.error ? `<p class="inline-error">${h(errorMessage(state.error))}</p>` : ''}<div class="model-actions actions">${activeDownload ? action('取消下载', 'cancel-download') : !available && model.source_repo ? action(['failed', 'error', 'cancelled', 'canceled'].includes(download?.status ?? '') ? '重试下载' : '下载模型', 'download', { icon: 'download', class: 'primary' }) : ''}${current && isLoaded(model.kind) ? action('卸载模型', 'unload', { disabled: loading || (state?.active_requests ?? 0) > 0 }) : action(loading ? '模型切换中…' : '加载模型', 'load', { icon: 'play', class: available ? 'primary' : '', disabled: !available || loading || activeDownload })}${available && inv?.managed ? action('删除文件', 'delete-model', { class: 'danger-quiet', disabled: current || loading || activeDownload }) : ''}${!defaultModel ? action('移除注册', 'remove-model', { disabled: current || loading || activeDownload }) : ''}${pending ? '<span class="muted" role="status">正在处理…</span>' : ''}</div></div></article>`;
}

function renderInference(): void {
  signatures.clear();
  const gen = inferenceKind === 'generation';
  $('#page')!.innerHTML = `<div class="tabs" role="tablist" aria-label="推理类型"><button type="button" role="tab" id="tab-generation" aria-controls="inference-panel" aria-selected="${gen}" tabindex="${gen ? 0 : -1}" data-action="inference-tab" data-kind="generation">${icon('inference')}生成与翻译</button><button type="button" role="tab" id="tab-embedding" aria-controls="inference-panel" aria-selected="${!gen}" tabindex="${!gen ? 0 : -1}" data-action="inference-tab" data-kind="embedding">${icon('models')}文本向量化</button></div><div id="inference-panel" role="tabpanel" aria-labelledby="tab-${inferenceKind}"><div id="inference-model" class="model-context"></div><div class="workbench"><form id="inference-form" class="panel input-panel"><div class="section-heading"><h2>${gen ? '输入与参数' : '输入文本'}</h2></div>${gen ? `${select('任务', 'mode', inferenceForm.mode, [['chat', '对话生成'], ['translation', '翻译']])}<div id="translation-target" ${inferenceForm.mode === 'translation' ? '' : 'hidden'}>${field('目标语言', 'target', inferenceForm.target, { required: true })}</div><label class="field" for="system"><span>系统提示词</span><textarea name="system" id="system" rows="2">${h(inferenceForm.system)}</textarea></label><label class="field" for="prompt"><span>消息内容</span><textarea name="prompt" id="prompt" rows="7" required placeholder="输入问题，或粘贴需要翻译的文字…">${h(inferenceForm.prompt)}</textarea></label><div class="parameter-grid">${field('Temperature', 'temperature', inferenceForm.temperature, { type: 'number', min: 0, max: 2, step: '0.05', required: true })}${field('Top P', 'top_p', inferenceForm.top_p, { type: 'number', min: 0.01, max: 1, step: '0.01', required: true })}${field('最大输出 Token', 'max_tokens', inferenceForm.max_tokens, { type: 'number', min: 1, max: 32768, required: true })}</div>${toggle('流式输出', 'stream', Boolean(inferenceForm.stream), '逐步显示模型生成的内容。')}` : `<label class="field" for="texts"><span>每行一条文本</span><textarea id="texts" name="texts" rows="14" required placeholder="输入需要向量化的句子\n每一行作为独立输入">${h(inferenceForm.texts)}</textarea><small>空行会被忽略。向量结果展示维度、L2 范数与前 12 个值。</small></label>`}<div class="inference-submit"><button class="button primary" id="run-inference" type="submit">${icon('play')}<span>开始${gen ? '生成' : '向量化'}</span></button>${button('取消请求', 'cancel-inference', { icon: 'stop', disabled: !currentInference })}</div><small class="hint">Ctrl + Enter 发送请求</small></form><section class="panel output-panel" aria-label="推理结果"><div class="section-heading"><h2>${gen ? '模型输出' : '向量结果'}</h2><div class="actions">${button('复制', 'copy-output', { icon: 'copy', disabled: !output && !vectors.length })}${button('清空', 'clear-output', { disabled: !!currentInference })}</div></div><div id="inference-metrics" class="metrics-strip"></div><div id="inference-error"></div><div id="output-content" class="output-content"></div><div id="output-status" class="output-status" role="status" aria-live="polite"></div></section></div></div>`;
  updateInference();
}
function updateInference(): void {
  const state = snapshot?.engine[inferenceKind];
  patch('inference-model', `<div>${icon('models')}<span>${h(modelName(state) || '未加载模型')}</span>${stateBadge(state)}${state?.mode ? `<span class="muted">${h(state.mode)}</span>` : ''}</div><a class="text-link" href="#models">管理模型${icon('arrow')}</a>`);
  const run = $('#run-inference') as HTMLButtonElement | null;
  if (run) { run.disabled = disabled() || !isLoaded(inferenceKind) || !!currentInference || (inferenceKind === 'generation' && Boolean(inferenceForm.stream) && !eventReady); run.title = !isLoaded(inferenceKind) ? '请先在模型管理页加载模型' : !eventReady && inferenceForm.stream ? '正在连接流式事件通道' : ''; }
  $('[data-action="cancel-inference"]')?.toggleAttribute('disabled', !currentInference || currentInference.cancelled);
  $('[data-action="copy-output"]')?.toggleAttribute('disabled', !output && !vectors.length);
  $('[data-action="clear-output"]')?.toggleAttribute('disabled', !!currentInference);
  document.querySelectorAll<HTMLButtonElement>('[data-action="inference-tab"]').forEach(el => { el.disabled = !!currentInference; });
  if (currentInference) elapsed = performance.now() - currentInference.start;
  const total = typeof usage.total_tokens === 'number' ? usage.total_tokens : undefined;
  const completion = typeof usage.completion_tokens === 'number' ? usage.completion_tokens : undefined;
  patch('inference-metrics', `<div><span>耗时</span><strong>${elapsed ? `${(elapsed / 1000).toFixed(2)} s` : '—'}</strong></div><div><span>输入 Token</span><strong>${typeof usage.prompt_tokens === 'number' ? usage.prompt_tokens : '—'}</strong></div><div><span>${inferenceKind === 'generation' ? '输出 Token' : '总 Token'}</span><strong>${inferenceKind === 'generation' ? completion ?? '—' : total ?? '—'}</strong></div><div><span>${inferenceKind === 'generation' ? '平均 Token / s' : '向量数'}</span><strong>${inferenceKind === 'generation' ? completion !== undefined && elapsed > 0 ? (completion / (elapsed / 1000)).toFixed(1) : '—' : vectors.length || '—'}</strong></div>`);
  patch('inference-error', inferenceError ? `<div class="inline-error" role="alert">${icon('alert')}${h(inferenceError)}</div>` : '');
  const content = $('#output-content');
  const stick = content ? content.scrollHeight - content.scrollTop - content.clientHeight < 70 : true;
  patch('output-content', vectors.length ? vectors.map(v => { const norm = Math.sqrt(v.values.reduce((sum, val) => sum + val * val, 0)); return `<article class="vector-result"><div class="section-heading"><h3>文本 ${v.index + 1}</h3>${badge(`${v.values.length} 维`, 'good')}</div><p>L2 范数 <strong>${norm.toFixed(6)}</strong></p><pre>[${v.values.slice(0, 12).map(val => val.toFixed(6)).join(', ')}${v.values.length > 12 ? ', …' : ''}]</pre></article>`; }).join('') : output || reasoning ? `${reasoning ? `<details class="reasoning"><summary>思考内容</summary><pre>${h(reasoning)}</pre></details>` : ''}<pre class="generated-text">${h(output)}</pre>` : `<div class="empty output-empty">${icon(inferenceKind === 'generation' ? 'inference' : 'models')}<h3>${currentInference ? '正在等待模型响应' : '结果将在这里显示'}</h3><p>${!isLoaded(inferenceKind) ? '先加载模型，再发送测试请求。' : currentInference ? '首次生成可能需要较长时间，可以随时取消。' : '发送真实请求，查看输出与实际使用量。'}</p></div>`);
  if (content && stick) content.scrollTop = content.scrollHeight;
  patch('output-status', `${currentInference ? '<span class="activity-dot"></span>' : icon(inferenceError ? 'alert' : output || vectors.length ? 'check' : 'clock')}<span>${h(inferenceStatus)}${finishReason ? ` · 结束原因：${h(finishReason)}` : ''}</span>${total !== undefined ? `<span class="muted">总计 ${total} Token</span>` : ''}`);
}
function captureInference(): void {
  const form = $('#inference-form') as HTMLFormElement | null;
  if (!form) return;
  for (const el of form.elements) {
    if (el instanceof HTMLInputElement) inferenceForm[el.name] = el.type === 'checkbox' ? el.checked : el.value;
    if (el instanceof HTMLSelectElement || el instanceof HTMLTextAreaElement) inferenceForm[el.name] = el.value;
  }
}
function consumeResponse(raw: unknown): void {
  const response = record(raw);
  if (response.error) { inferenceError = errorMessage(response.error); return; }
  if (response.usage) usage = { ...usage, ...record(response.usage) };
  if (Array.isArray(response.choices)) {
    for (const choice of response.choices) {
      const c = record(choice), message = record(c.delta ?? c.message);
      if (typeof message.content === 'string') output += message.content;
      if (typeof message.reasoning_content === 'string') reasoning += message.reasoning_content;
      if (typeof c.text === 'string') output += c.text;
      if (typeof c.finish_reason === 'string') finishReason = c.finish_reason;
    }
  }
  if (Array.isArray(response.data)) {
    vectors = response.data.map((entry: unknown, index: number) => { const item = record(entry); return { index: typeof item.index === 'number' ? item.index : index, values: Array.isArray(item.embedding) ? item.embedding : [] }; });
    if (vectors.some(v => !v.values.length || v.values.some(x => typeof x !== 'number' || !Number.isFinite(x)))) { vectors = []; inferenceError = '服务返回了无效向量，无法显示结果。'; }
  }
}
async function runInference(): Promise<void> {
  if (currentInference || !online() || !isLoaded(inferenceKind)) return;
  captureInference();
  const gen = inferenceKind === 'generation';
  const text = String(gen ? inferenceForm.prompt : inferenceForm.texts).trim();
  if (!text) { notice('请输入要推理的文本。', true); return; }
  output = ''; reasoning = ''; vectors = []; usage = {}; elapsed = 0; finishReason = ''; inferenceError = ''; inferenceStatus = '正在请求';
  const request_id = crypto.randomUUID();
  const stream = gen && Boolean(inferenceForm.stream);
  const parser = new SSEParser((data, event) => {
    if (data.trim() === '[DONE]') return;
    try { const value: unknown = JSON.parse(data); if (event === 'error') inferenceError = errorMessage(record(value).error ?? value); else consumeResponse(value); }
    catch { inferenceError = '收到无法解析的流式响应，请查看日志。'; }
    if (route === 'inference') updateInference();
  });
  currentInference = { request_id, parser, start: performance.now(), cancelled: false };
  const selected = snapshot!.settings.models.find(m => isCurrent(m) && m.kind === inferenceKind);
  const model = selected?.name ?? modelName(snapshot!.engine[inferenceKind]);
  const system = inferenceForm.mode === 'translation' ? `${inferenceForm.system}\n将用户输入的文本翻译成${inferenceForm.target}，只输出翻译结果。` : String(inferenceForm.system);
  const body = gen ? { model, messages: [...(system.trim() ? [{ role: 'system', content: system }] : []), { role: 'user', content: text }], temperature: Number(inferenceForm.temperature), top_p: Number(inferenceForm.top_p), max_tokens: Number(inferenceForm.max_tokens), stream, ...(stream ? { stream_options: { include_usage: true } } : {}) } : { model, input: text.split(/\r?\n/).map(x => x.trim()).filter(Boolean), encoding_format: 'float' };
  updateInference();
  try {
    const response = await dispatch('run_inference', { kind: inferenceKind, request_id, body });
    if (stream) parser.finish(); else consumeResponse(response);
    if (inferenceError) throw new Error(inferenceError);
    inferenceStatus = currentInference?.cancelled ? '请求已取消，已保留部分结果' : '推理完成';
    if (!currentInference?.cancelled && !output && !reasoning && !vectors.length) { inferenceStatus = '请求完成，未返回内容'; }
  } catch (error) {
    if (currentInference?.cancelled) inferenceStatus = '请求已取消，已保留部分结果';
    else { inferenceError = errorMessage(error); inferenceStatus = '推理失败'; notice(inferenceError, true); }
  } finally {
    if (currentInference) elapsed = performance.now() - currentInference.start;
    currentInference = null;
    if (route === 'inference') updateInference();
    void poll();
  }
}

function openDialog(html: string): void {
  lastFocus = document.activeElement as HTMLElement;
  $('#dialog-content')!.innerHTML = html;
  ($('#dialog') as HTMLDialogElement).showModal();
}
function closeDialog(): void { ($('#dialog') as HTMLDialogElement).close(); lastFocus?.focus(); }
function confirmAction(title: string, description: string, action: () => Promise<void>): void {
  openDialog(`<div class="dialog-heading"><h2 id="dialog-title">${h(title)}</h2>${button('关闭', 'close-dialog', { icon: 'close', class: 'icon-only' })}</div><p>${h(description)}</p><div class="dialog-actions">${button('取消', 'close-dialog')}<button type="button" class="button danger" id="confirm-action">${h(title)}</button></div>`);
  $('#confirm-action')!.addEventListener('click', () => { closeDialog(); void action(); }, { once: true });
}
function addModelDialog(): void {
  openDialog(`<div class="dialog-heading"><div><h2 id="dialog-title">添加模型</h2><p>导入本地 GGUF，或注册 Hugging Face 文件。</p></div>${button('关闭', 'close-dialog', { icon: 'close', class: 'icon-only' })}</div><form id="add-model-form"><div class="form-grid">${field('唯一 ID', 'id', '', { required: true, placeholder: 'my-qwen-model', hint: '1–64 位字母、数字、下划线或连字符。' })}${field('模型名称 / API 别名', 'name', '', { required: true, placeholder: 'my-local-model' })}${select('模型类型', 'kind', 'generation', [['generation', '生成模型'], ['embedding', '向量模型']])}${select('模型来源', 'source', 'local', [['local', '本地 GGUF 文件'], ['hf', 'Hugging Face']])}</div><div id="source-local"><label class="field" for="model-path"><span>本地文件</span><div class="input-action"><input id="model-path" name="path" placeholder="选择 .gguf 文件" required>${button('选择文件', 'choose-file', { icon: 'folder' })}</div></label></div><div id="source-hf" hidden><div class="form-grid">${field('Hugging Face 仓库', 'source_repo', '', { placeholder: 'owner/repository' })}${field('仓库内文件名', 'filename', '', { placeholder: 'model-Q4_K_M.gguf' })}</div><p class="hint">支持仓库内相对路径。注册后，在模型列表中下载文件。</p></div><p class="hint">只接受 llama.cpp 支持的 GGUF；AWQ、safetensors 权重需使用兼容的 GGUF 版本。</p><div id="model-form-error" role="alert" tabindex="-1"></div><div class="dialog-actions">${button('取消', 'close-dialog')}<button type="submit" class="button primary">添加模型</button></div></form>`);
}
async function addModel(form: HTMLFormElement): Promise<void> {
  const data = new FormData(form); const get = (key: string) => String(data.get(key) ?? '').trim();
  const local = get('source') === 'local';
  const model: ModelSpec = { id: get('id'), name: get('name'), kind: get('kind') as ModelKind, source_repo: local ? '' : get('source_repo'), filename: local ? '' : get('filename'), path: local ? get('path') : '', compatibility: '自定义 GGUF；兼容性由本地推理引擎在加载时验证。' };
  let error = '';
  if (!/^[A-Za-z0-9_-]{1,64}$/.test(model.id)) error = 'ID 只能包含 1–64 位字母、数字、下划线和连字符。';
  else if (snapshot?.settings.models.some(m => m.id === model.id)) error = '此 ID 已被使用，请设置一个不同的 ID。';
  else if (!model.name) error = '请输入模型名称。';
  else if (local && !model.path.toLowerCase().endsWith('.gguf')) error = '请选择 .gguf 格式的本地文件。';
  else if (!local && (!/^[\w.-]+\/[\w.-]+$/.test(model.source_repo) || model.source_repo.split('/').some(x => x === '.' || x === '..'))) error = '仓库格式应为 owner/repository。';
  else if (!local && (!model.filename.toLowerCase().endsWith('.gguf') || model.filename.includes('\\') || model.filename.includes(':') || model.filename.split('/').some(x => !x || x === '.' || x === '..'))) error = '请输入有效的仓库内 GGUF 相对路径。';
  const errorEl = $('#model-form-error')!;
  if (error) { errorEl.textContent = error; errorEl.className = 'inline-error'; errorEl.focus(); return; }
  form.querySelectorAll<HTMLButtonElement | HTMLInputElement | HTMLSelectElement>('button,input,select').forEach(el => { el.disabled = true; });
  try { await dispatch('add_model', { model }); closeDialog(); notice('模型已注册，可以在模型库中下载或加载。'); await poll(); }
  catch (err) { errorEl.textContent = errorMessage(err); errorEl.className = 'inline-error'; errorEl.focus(); }
  finally { form.querySelectorAll<HTMLButtonElement | HTMLInputElement | HTMLSelectElement>('button,input,select').forEach(el => { el.disabled = false; }); }
}
function captureSettings(el: HTMLInputElement | HTMLSelectElement): void {
  if (!draft || !el.name) return;
  const keys = el.name.split('.'); let obj: any = draft;
  for (const key of keys.slice(0, -1)) obj = obj?.[key];
  if (!obj) return;
  obj[keys.at(-1)!] = el instanceof HTMLInputElement && el.type === 'checkbox' ? el.checked : el instanceof HTMLInputElement && el.type === 'number' ? Number(el.value) : el.value;
  dirty = true;
  patch('settings-dirty', '有未保存的修改');
  updateNetworkWarning();
}
function updateNetworkWarning(): void {
  const host = draft?.service.host.trim() ?? '';
  patch('network-warning', host && !isLoopback(host) ? `<div class="inline-error">${icon('alert')}<span>当前地址不是回环地址。此 API 无访问认证，其他设备可能通过网络连接并使用模型；请确认网络环境可信。</span></div>` : '');
}
function mergeSettingsDraft(edited: Settings, baseline: Settings | null, latest: Settings): Settings {
  if (!baseline) return structuredClone(edited);
  const merged = structuredClone(latest);
  for (const section of ['app', 'service', 'inference'] as const) {
    for (const [key, value] of Object.entries(edited[section])) {
      if (value !== (baseline[section] as any)[key]) (merged[section] as any)[key] = value;
    }
  }
  for (const model of merged.models) {
    const original = baseline.models.find(m => m.id === model.id);
    const changed = edited.models.find(m => m.id === model.id);
    if (original && changed) for (const key of ['name', 'path'] as const) { if (changed[key] !== original[key]) model[key] = changed[key]; }
  }
  return merged;
}
async function saveSettings(): Promise<void> {
  if (!draft || busy.has('settings')) return;
  const validation = validateSettings(draft);
  if (validation) { notice(validation, true); return; }
  const changes = mergeSettingsDraft(draft, draftBaseline, snapshot?.settings ?? draft);
  const previous = snapshot?.settings;
  await perform('settings', async () => {
    const saved = await dispatch<Settings>('save_settings', changes);
    draft = structuredClone(saved); draftBaseline = structuredClone(saved); dirty = false;
    pendingRestart = pendingRestart || !!(snapshot?.service.running && JSON.stringify(previous?.service) !== JSON.stringify(saved.service));
    pendingReload = pendingReload || JSON.stringify(previous?.inference) !== JSON.stringify(saved.inference) || JSON.stringify(previous?.models) !== JSON.stringify(saved.models);
    if (snapshot) snapshot.settings = saved;
    notice(`设置已保存。${pendingRestart ? '请重启 API 服务使服务设置生效。' : ''}${pendingReload ? '请重新加载模型使推理配置生效。' : ''}${route === 'app' ? '启动、窗口和日志设置已更新。' : ''}`);
  });
}
async function perform(key: string, action: () => Promise<void>, success?: string): Promise<void> {
  if (busy.has(key)) return;
  busy.add(key); updateLive();
  try { await action(); if (success) notice(success); }
  catch (err) { notice(errorMessage(err), true); }
  finally { busy.delete(key); await poll(); updateLive(); }
}
async function poll(): Promise<void> {
  if (!native || pollBusy) return;
  pollBusy = true;
  try {
    const raw = await dispatch<Snapshot>('snapshot');
    if (!raw || !Array.isArray(raw.settings?.models) || !raw.settings.inference || !raw.settings.service || !raw.settings.app) throw new Error('运行时返回的配置结构无效。');
    const first = !snapshot;
    snapshot = { ...raw, engine: record(raw.engine), inventory: Array.isArray(raw.inventory) ? raw.inventory.filter(item => typeof record(item).id === 'string') : [], downloads: Array.isArray(raw.downloads) ? raw.downloads.filter(item => typeof record(item).id === 'string') : [], service: { ...raw.service, running: raw.service?.running === true }, hf: record(raw.hf), paths: record(raw.paths) };
    connectionError = ''; connecting = false;
    if (!draft || (!dirty && !$('#settings-form'))) { draft = structuredClone(snapshot.settings); draftBaseline = structuredClone(snapshot.settings); }
    if (first && (route === 'service' || route === 'app')) renderPage();
  } catch (err) { connectionError = errorMessage(err); connecting = false; }
  finally { pollBusy = false; updateLive(); }
}

app.addEventListener('click', event => {
  const target = (event.target as HTMLElement).closest<HTMLButtonElement>('[data-action]');
  if (!target || target.disabled) return;
  const action = target.dataset.action;
  const id = target.dataset.id;
  const model = snapshot?.settings.models.find(m => m.id === id);
  if (action === 'dismiss-notice') clearNotice();
  if (action === 'refresh') void poll();
  if (action === 'window') void dispatch('window', { action: target.dataset.window }).catch(err => notice(errorMessage(err), true));
  if (action === 'service') void perform('service', async () => { await dispatch('service', { action: target.dataset.service }); if (target.dataset.service === 'restart' || target.dataset.service === 'start') pendingRestart = false; }, target.dataset.service === 'stop' ? 'API 服务已停止。' : 'API 服务已启动。');
  if (action === 'add-model') addModelDialog();
  if (action === 'close-dialog') closeDialog();
  if (action === 'choose-file') void perform('choose-file', async () => { const path = await dispatch<string | null>('choose_model_file'); if (path && $('#model-path')) ($('#model-path') as HTMLInputElement).value = path; });
  if (action === 'open-logs') void perform('logs', async () => { await dispatch('open_logs'); }, '已打开日志目录。');
  if (action === 'reset-settings' && snapshot) {
    const reset = async () => { draft = structuredClone(snapshot!.settings); draftBaseline = structuredClone(snapshot!.settings); dirty = false; renderPage(); };
    if (dirty) confirmAction('放弃未保存的修改', '当前设置草稿将恢复为最近保存的配置。', reset); else void reset();
  }
  if (action === 'inference-tab') {
    captureInference(); inferenceKind = target.dataset.kind as ModelKind;
    output = ''; reasoning = ''; vectors = []; usage = {}; elapsed = 0; inferenceError = ''; finishReason = ''; inferenceStatus = '等待输入';
    renderInference(); $(`#tab-${inferenceKind}`)?.focus();
  }
  if (action === 'cancel-inference' && currentInference) {
    const request = currentInference;
    request.cancelled = true; inferenceStatus = '正在取消…'; updateInference();
    void dispatch('cancel_inference', { request_id: request.request_id }).catch(err => { if (currentInference === request) request.cancelled = false; notice(errorMessage(err), true); updateInference(); });
  }
  if (action === 'copy-output') void navigator.clipboard.writeText(vectors.length ? JSON.stringify({ data: vectors.map(v => ({ object: 'embedding', index: v.index, embedding: v.values })), usage }, null, 2) : output).then(() => notice('结果已复制。')).catch(err => notice(`无法复制：${errorMessage(err)}`, true));
  if (action === 'clear-output') { output = ''; reasoning = ''; vectors = []; usage = {}; elapsed = 0; inferenceError = ''; finishReason = ''; inferenceStatus = '等待输入'; updateInference(); }
  if (model) {
    const op = ({ download: 'download', 'cancel-download': 'cancel_download', load: 'load', unload: 'unload', 'delete-model': 'delete', 'remove-model': 'remove_model' } as Record<string, string>)[action ?? ''];
    if (!op) return;
    const run = async () => perform(`model:${model.id}`, async () => { await dispatch(op, op === 'unload' ? { kind: model.kind } : { id: model.id }); }, ({ download: '下载任务已提交。', cancel_download: '已请求取消下载，可稍后重试。', load: '模型已加载。', unload: '模型已卸载。', delete: '托管模型文件已删除。', remove_model: '模型注册已移除，本地导入文件不会删除。' } as Record<string, string>)[op]);
    if (op === 'delete') confirmAction('删除模型文件', `将删除「${model.name}」的托管文件。再次使用时需要重新下载。`, run);
    else if (op === 'remove_model') confirmAction('移除模型注册', `将从模型库移除「${model.name}」。外部导入文件不会被删除。`, run);
    else if (op === 'load' && isLoaded(model.kind) && !isCurrent(model)) confirmAction('切换已加载模型', `当前${model.kind === 'generation' ? '生成' : '向量'}模型将被替换为「${model.name}」。`, run);
    else void run();
  }
});
app.addEventListener('submit', event => {
  event.preventDefault();
  const form = event.target as HTMLFormElement;
  if (form.id === 'inference-form') void runInference();
  if (form.id === 'settings-form') void saveSettings();
  if (form.id === 'add-model-form') void addModel(form);
});
app.addEventListener('input', event => {
  const element = event.target as HTMLInputElement;
  if (element.closest('#settings-form')) captureSettings(element);
  if (element.closest('#inference-form')) { captureInference(); updateInference(); }
});
app.addEventListener('change', event => {
  const element = event.target as HTMLInputElement;
  if (element.name === 'mode') $('#translation-target')!.hidden = element.value !== 'translation';
  if (element.name === 'source') {
    const local = element.value === 'local'; $('#source-local')!.hidden = !local; $('#source-hf')!.hidden = local;
    ($('#model-path') as HTMLInputElement).required = local;
    for (const name of ['source_repo', 'filename']) ($(`[name="${name}"]`) as HTMLInputElement).required = !local;
  }
});
app.addEventListener('keydown', event => {
  if ((event.ctrlKey || event.metaKey) && event.key === 'Enter' && (event.target as HTMLElement).closest('#inference-form')) { event.preventDefault(); if (!($('#run-inference') as HTMLButtonElement).disabled) ($('#inference-form') as HTMLFormElement).requestSubmit(); }
  if (['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key) && (event.target as HTMLElement).matches('[role="tab"]') && !currentInference) { event.preventDefault(); const kind = event.key === 'Home' ? 'generation' : event.key === 'End' ? 'embedding' : inferenceKind === 'generation' ? 'embedding' : 'generation'; $(`#tab-${kind}`)?.click(); }
});
document.addEventListener('contextmenu', event => event.preventDefault());
window.addEventListener('hashchange', () => { captureInference(); route = routes.some(r => r.id === location.hash.slice(1)) ? location.hash.slice(1) as Route : 'dashboard'; renderPage(); $('#main')!.focus({ preventScroll: true }); });
window.addEventListener('beforeunload', () => { if (currentInference) void dispatch('cancel_inference', { request_id: currentInference.request_id }); });
renderPage();
if (native) {
  void listen<{ request_id: string; chunk: string }>('inference_chunk', event => { if (event.payload?.request_id === currentInference?.request_id && typeof event.payload.chunk === 'string') currentInference?.parser.feed(event.payload.chunk); }).then(() => { eventReady = true; if (route === 'inference') updateInference(); }).catch(err => notice(`流式事件连接失败：${errorMessage(err)}。可关闭流式输出后重试。`, true));
  void listen('app_error', event => notice(errorMessage(event.payload), true)).catch(err => notice(`应用事件连接失败：${errorMessage(err)}`, true));
  void poll();
  window.setInterval(() => { void poll(); }, 1200);
}
window.setInterval(() => { if (currentInference && route === 'inference') updateInference(); }, 200);
