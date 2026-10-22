import { field, select, toggle, escape, button } from './ui';
import type { Settings } from './types';
export function settingsForm(settings: Settings, page: 'service' | 'app'): string {
  const s = settings.service, i = settings.inference, a = settings.app;
  const startup = toggle('启动后自动加载模型并启动 API 服务', 'service.auto_start', s.auto_start, '保存后于下次启动生效。自动加载本机已有的生成、向量模型，不会自动下载；也适用于开机启动和静默启动。');
  const groups = page === 'service' ? `
    <section class="panel settings-section"><div class="section-intro"><h2>本地 API 服务</h2><p>提供 OpenAI 兼容接口，默认只允许本机连接。</p></div><div class="form-grid">
      ${field('监听地址', 'service.host', s.host, { required: true, hint: '建议 127.0.0.1。监听 0.0.0.0 会向局域网开放无认证接口。' })}
      ${field('端口', 'service.port', s.port, { type: 'number', min: 1, max: 65535, required: true })}
      ${field('最大并发请求数', 'service.concurrency', s.concurrency, { type: 'number', min: 1, max: 16, required: true })}
      ${field('请求超时（秒）', 'service.timeout_secs', s.timeout_secs, { type: 'number', min: 5, max: 1800, required: true })}
    </div><div id="network-warning"></div>${startup}</section>
    <section class="panel settings-section"><div class="section-intro"><h2>推理与资源</h2><p>参数在下次加载模型时生效。Vulkan 加载失败时自动尝试 CPU。</p></div><div class="form-grid">
      ${field('上下文长度', 'inference.context_size', i.context_size, { type: 'number', min: 512, max: 131072, required: true })}
      ${field('批处理大小', 'inference.batch_size', i.batch_size, { type: 'number', min: 32, max: 4096, required: true })}
      ${field('CPU 线程数', 'inference.threads', i.threads, { type: 'number', min: 1, max: 256, required: true, hint: '根据 CPU 核心数分配，建议先使用默认值。' })}
      ${field('GPU 卸载层数', 'inference.gpu_layers', i.gpu_layers, { type: 'number', min: -1, max: 1000, required: true, hint: '0 使用 CPU；-1 尽可能卸载所有层。' })}
      ${field('显存适配余量（MB）', 'inference.gpu_memory_mb', i.gpu_memory_mb, { type: 'number', min: 0, max: 65536, required: true, hint: '为系统预留的显存，交由引擎进行适配。' })}
      ${select('KV 缓存类型', 'inference.cache_type', i.cache_type, [['f16', 'F16 · 半精度'], ['q8_0', 'Q8_0 · 8 位量化'], ['q4_0', 'Q4_0 · 4 位量化']])}
    </div></section>
    <section class="panel settings-section"><div class="section-intro"><h2>模型名称与路径</h2><p>编辑已注册模型的名称与本地 GGUF 路径。已加载模型需卸载后重新加载。</p></div>
    ${settings.models.length ? settings.models.map((m, index) => `<div class="model-config"><h3>${escape(m.name)} <span class="muted">${escape(m.kind === 'embedding' ? '向量模型' : '生成模型')}</span></h3><div class="form-grid">${field('模型名称', `models.${index}.name`, m.name, { required: true })}${field('本地 GGUF 路径', `models.${index}.path`, m.path, { placeholder: '留空使用托管下载路径', hint: '可在模型管理页通过文件选择器导入。' })}</div></div>`).join('') : '<p class="muted">尚未注册模型，请前往模型管理添加。</p>'}</section>` : `
    <section class="panel settings-section"><div class="section-intro"><h2>启动与窗口</h2><p>让本地推理服务按你的工作习惯运行。</p></div>
      ${toggle('开机启动', 'app.autostart', a.autostart, '登录 Windows 后启动 Qwen Desk。')}
      ${startup}
      ${toggle('静默启动', 'app.silent_start', a.silent_start, '启动时隐藏主窗口；需要启用系统托盘。')}
      ${toggle('系统托盘常驻', 'app.tray_enabled', a.tray_enabled, '通过托盘恢复窗口或退出应用。')}
      ${toggle('关闭窗口时隐藏到托盘', 'app.close_to_tray', a.close_to_tray, '关闭后继续运行推理服务；禁用后关闭窗口将退出应用。')}
    </section>
    <section class="panel settings-section"><div class="section-intro"><h2>Hugging Face 认证</h2><p>从 huggingface.yml 读取私有凭据。认证令牌不会传入此界面。</p></div><div id="hf-status"></div><p class="hint">支持 token、hf_token 或 huggingface.token。配置文件可置于工作目录、应用程序目录或应用数据目录。</p></section>
    <section class="panel settings-section"><div class="section-intro"><h2>日志与诊断</h2><p>保留运行信息，便于排查模型加载和请求错误。</p></div><div class="form-grid">${select('日志级别', 'app.log_level', a.log_level, [['error', 'Error · 仅错误'], ['warn', 'Warn · 警告'], ['info', 'Info · 常规'], ['debug', 'Debug · 调试']])}<div class="field"><span>日志目录</span><code id="log-path">正在读取…</code>${button('打开日志目录', 'open-logs', { icon: 'folder' })}</div></div></section>`;
  return `<form id="settings-form">${groups}<div class="save-bar"><span id="settings-dirty" class="muted">配置已同步</span><div class="actions">${button('放弃修改', 'reset-settings')}<button type="submit" class="button primary" id="save-settings">保存设置</button></div></div><div id="restart-feedback"></div></form>`;
}
export function isLoopback(host: string): boolean { return host === '::1' || host === '0:0:0:0:0:0:0:1' || /^127\./.test(host); }
function isIPAddress(host: string): boolean {
  if (/^\d+\.\d+\.\d+\.\d+$/.test(host)) return host.split('.').every(part => /^(0|[1-9]\d{0,2})$/.test(part) && Number(part) <= 255);
  if (!host.includes(':') || !/^[\da-fA-F:.]+$/.test(host)) return false;
  try { return new URL(`http://[${host}]/`).hostname.length > 2; } catch { return false; }
}
export function validateSettings(settings: Settings): string | null {
  if ((settings.app.silent_start || settings.app.close_to_tray) && !settings.app.tray_enabled) return '静默启动和关闭到托盘需要启用「系统托盘常驻」。';
  if (settings.inference.batch_size > settings.inference.context_size) return '批处理大小不能大于上下文长度。';
  if (!isIPAddress(settings.service.host.trim())) return '监听地址必须是有效的 IPv4 或 IPv6 地址，不要包含协议、端口或路径。';
  for (const model of settings.models) {
    if (!model.name.trim()) return '模型名称不能为空。';
    if (model.path && !model.path.toLowerCase().endsWith('.gguf')) return '本地模型路径必须指向 .gguf 文件。';
  }
  return null;
}
