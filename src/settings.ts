import { field, select, toggle, escape, button } from './ui';
import type { Settings } from './types';
import { html, t } from './i18n';
export function settingsForm(settings: Settings, page: 'service' | 'app'): string {
  const s = settings.service, i = settings.inference, a = settings.app;
  const startup = toggle(t('启动后自动加载模型并启动 API 服务'), 'service.auto_start', s.auto_start, t('保存后于下次启动生效。自动加载本机已有的生成、向量模型，不会自动下载；也适用于开机启动和静默启动。'));
  const groups = page === 'service' ? html`
    <section class="panel settings-section"><div class="section-intro"><h2>本地 API 服务</h2><p>提供 OpenAI 兼容接口，默认只允许本机连接。</p></div><div class="form-grid">
      ${field(t('监听地址'), 'service.host', s.host, { required: true, hint: t('建议 127.0.0.1。监听 0.0.0.0 会向局域网开放无认证接口。') })}
      ${field(t('端口'), 'service.port', s.port, { type: 'number', min: 1, max: 65535, required: true })}
      ${field(t('最大并发请求数'), 'service.concurrency', s.concurrency, { type: 'number', min: 1, max: 16, required: true })}
      ${field(t('请求超时（秒）'), 'service.timeout_secs', s.timeout_secs, { type: 'number', min: 5, max: 1800, required: true })}
    </div><div id="network-warning"></div>${startup}</section>
    <section class="panel settings-section"><div class="section-intro"><h2>推理与资源</h2><p>参数在下次加载模型时生效。Vulkan 加载失败时自动尝试 CPU。</p></div><div class="form-grid">
      ${field(t('上下文长度'), 'inference.context_size', i.context_size, { type: 'number', min: 512, max: 131072, required: true })}
      ${field(t('批处理大小'), 'inference.batch_size', i.batch_size, { type: 'number', min: 32, max: 4096, required: true })}
      ${field(t('CPU 线程数'), 'inference.threads', i.threads, { type: 'number', min: 1, max: 256, required: true, hint: t('根据 CPU 核心数分配，建议先使用默认值。') })}
      ${field(t('GPU 卸载层数'), 'inference.gpu_layers', i.gpu_layers, { type: 'number', min: -1, max: 1000, required: true, hint: t('0 使用 CPU；-1 尽可能卸载所有层。') })}
      ${field(t('显存适配余量（MB）'), 'inference.gpu_memory_mb', i.gpu_memory_mb, { type: 'number', min: 0, max: 65536, required: true, hint: t('为系统预留的显存，交由引擎进行适配。') })}
      ${select(t('KV 缓存类型'), 'inference.cache_type', i.cache_type, [['f16', t('F16 · 半精度')], ['q8_0', t('Q8_0 · 8 位量化')], ['q4_0', t('Q4_0 · 4 位量化')]])}
    </div></section>
    <section class="panel settings-section"><div class="section-intro"><h2>模型名称与路径</h2><p>编辑已注册模型的名称与本地 GGUF 路径。已加载模型需卸载后重新加载。</p></div>
    ${settings.models.length ? settings.models.map((m, index) => `<div class="model-config"><h3>${escape(m.name)} <span class="muted">${escape(m.kind === 'embedding' ? t('向量模型') : t('生成模型'))}</span></h3><div class="form-grid">${field(t('模型名称'), `models.${index}.name`, m.name, { required: true })}${field(t('本地 GGUF 路径'), `models.${index}.path`, m.path, { placeholder: t('留空使用托管下载路径'), hint: t('可在模型管理页通过文件选择器导入。') })}</div></div>`).join('') : html`<p class="muted">尚未注册模型，请前往模型管理添加。</p>`}</section>` : html`
    <section class="panel settings-section"><div class="section-intro"><h2>界面语言</h2><p>选择中文或 English，立即应用并在保存后记住。</p></div>${select(t('语言'), 'app.language', a.language, [['zh-CN', t('简体中文')], ['en', 'English']])}</section>
    <section class="panel settings-section"><div class="section-intro"><h2>启动与窗口</h2><p>让本地推理服务按你的工作习惯运行。</p></div>
      ${toggle(t('开机启动'), 'app.autostart', a.autostart, t('登录 Windows 后启动 Qwen Desk。'))}
      ${startup}
      ${toggle(t('静默启动'), 'app.silent_start', a.silent_start, t('启动时隐藏主窗口；需要启用系统托盘。'))}
      ${toggle(t('系统托盘常驻'), 'app.tray_enabled', a.tray_enabled, t('通过托盘恢复窗口或退出应用。'))}
      ${toggle(t('关闭窗口时隐藏到托盘'), 'app.close_to_tray', a.close_to_tray, t('关闭后继续运行推理服务；禁用后关闭窗口将退出应用。'))}
    </section>
    <section class="panel settings-section"><div class="section-intro"><h2>Hugging Face 认证</h2><p>输入新的只读访问令牌，单独保存后即可用于模型下载。已保存的令牌不会回显。</p></div><div id="hf-status"></div><label class="field credential-field" for="hf-token"><span>访问令牌</span><input id="hf-token" type="${'pass' + 'word'}" autocomplete="new-${'pass' + 'word'}" spellcheck="false" autocapitalize="none" placeholder="hf_…" maxlength="512" aria-describedby="hf-token-hint" data-private></label><p id="hf-token-hint" class="hint">令牌保存在当前用户的应用数据目录，优先于其他 huggingface.yml；无需重启。留空不会更改已保存的令牌。</p><div class="credential-actions">${button(t('保存令牌'), 'save-hf-token')}</div></section>
    <section class="panel settings-section"><div class="section-intro"><h2>日志与诊断</h2><p>保留运行信息，便于排查模型加载和请求错误。</p></div><div class="form-grid">${select(t('日志级别'), 'app.log_level', a.log_level, [['error', t('Error · 仅错误')], ['warn', t('Warn · 警告')], ['info', t('Info · 常规')], ['debug', t('Debug · 调试')]])}<div class="field"><span>日志目录</span><code id="log-path">正在读取…</code>${button(t('打开日志目录'), 'open-logs', { icon: 'folder' })}</div></div></section>`;
  return html`<form id="settings-form">${groups}<div class="save-bar"><span id="settings-dirty" class="muted">配置已同步</span><div class="actions">${button(t('放弃修改'), 'reset-settings')}<button type="submit" class="button primary" id="save-settings">保存设置</button></div></div><div id="restart-feedback"></div></form>`;
}
export function isLoopback(host: string): boolean { return host === '::1' || host === '0:0:0:0:0:0:0:1' || /^127\./.test(host); }
function isIPAddress(host: string): boolean {
  if (/^\d+\.\d+\.\d+\.\d+$/.test(host)) return host.split('.').every(part => /^(0|[1-9]\d{0,2})$/.test(part) && Number(part) <= 255);
  if (!host.includes(':') || !/^[\da-fA-F:.]+$/.test(host)) return false;
  try { return new URL(`http://[${host}]/`).hostname.length > 2; } catch { return false; }
}
export function validateSettings(settings: Settings): string | null {
  if ((settings.app.silent_start || settings.app.close_to_tray) && !settings.app.tray_enabled) return t('静默启动和关闭到托盘需要启用「系统托盘常驻」。');
  if (settings.inference.batch_size > settings.inference.context_size) return t('批处理大小不能大于上下文长度。');
  if (!isIPAddress(settings.service.host.trim())) return t('监听地址必须是有效的 IPv4 或 IPv6 地址，不要包含协议、端口或路径。');
  for (const model of settings.models) {
    if (!model.name.trim()) return t('模型名称不能为空。');
    if (model.path && !model.path.toLowerCase().endsWith('.gguf')) return t('本地模型路径必须指向 .gguf 文件。');
  }
  return null;
}
