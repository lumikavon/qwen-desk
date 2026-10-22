const paths: Record<string, string> = {
  logo: '<path d="m12 3 8 5v8l-8 5-8-5V8l8-5Z"/><path d="m4 8 8 5 8-5M12 13v8M8 5.5l8 5v5"/>',
  dashboard: '<rect x="3" y="3" width="7" height="7" rx="1.5"/><rect x="14" y="3" width="7" height="7" rx="1.5"/><rect x="3" y="14" width="7" height="7" rx="1.5"/><rect x="14" y="14" width="7" height="7" rx="1.5"/>',
  models: '<path d="m12 3 9 5-9 5-9-5 9-5Zm-9 9 9 5 9-5M3 16l9 5 9-5"/>',
  inference: '<path d="m8 5-5 7 5 7m8-14 5 7-5 7m-3-15-2 16"/>',
  service: '<rect x="3" y="3" width="18" height="7" rx="2"/><rect x="3" y="14" width="18" height="7" rx="2"/><path d="M7 6.5h.01M7 17.5h.01M12 6.5h5M12 17.5h5"/>',
  settings: '<path d="M4 7h16M4 17h16"/><circle cx="9" cy="7" r="3" fill="currentColor"/><circle cx="15" cy="17" r="3" fill="currentColor"/>',
  arrow: '<path d="M5 12h14m-5-5 5 5-5 5"/>',
  play: '<path d="m8 4 12 8-12 8V4Z"/>', stop: '<rect x="5" y="5" width="14" height="14" rx="2"/>',
  download: '<path d="M12 3v12m-5-5 5 5 5-5M4 16v5h16v-5"/>',
  plus: '<path d="M12 5v14M5 12h14"/>', refresh: '<path d="M20 7v5h-5M4 17v-5h5M5 8a7 7 0 0 1 12-3l3 4M4 15l3 4a7 7 0 0 0 12-3"/>',
  check: '<path d="m5 12 4 4L19 6"/>', alert: '<path d="m12 3 10 18H2L12 3Zm0 6v5m0 3v.01"/>',
  folder: '<path d="M3 7V4h7l3 3h8v13H3V7Z"/>', close: '<path d="m6 6 12 12M6 18 18 6"/>',
  minus: '<path d="M5 12h14"/>', maximize: '<rect x="5" y="5" width="14" height="14"/>',
  terminal: '<path d="m4 6 6 6-6 6m9 0h7"/>', clock: '<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/>',
  dot: '<circle cx="12" cy="12" r="4" fill="currentColor"/>', copy: '<rect x="8" y="8" width="12" height="13" rx="2"/><path d="M16 8V3H3v13h5"/>',
};
export function icon(name: string): string { return `<svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.65" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${paths[name] ?? paths.dot}</svg>`; }
export function escape(value: unknown): string { return String(value ?? '').replace(/[&<>"']/g, char => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[char]!); }
export function button(label: string, action: string, options: { icon?: string; class?: string; disabled?: boolean; data?: string; title?: string } = {}): string {
  return `<button type="button" class="button ${options.class ?? ''}" data-action="${action}" ${options.data ?? ''} ${options.disabled ? 'disabled' : ''} ${options.title ? `title="${escape(options.title)}"` : ''}>${options.icon ? icon(options.icon) : ''}<span>${escape(label)}</span></button>`;
}
export function badge(label: string, tone = 'neutral'): string { return `<span class="badge ${tone}">${icon(tone === 'bad' ? 'alert' : tone === 'good' ? 'check' : 'dot')}${escape(label)}</span>`; }
export function bytes(value: unknown): string {
  if (typeof value !== 'number' || !Number.isFinite(value) || value < 0) return '未知大小';
  if (value < 1024) return `${value} B`;
  const unit = Math.min(Math.floor(Math.log(value) / Math.log(1024)), 4);
  return `${(value / 1024 ** unit).toFixed(1)} ${['B', 'KB', 'MB', 'GB', 'TB'][unit]}`;
}
export function field(label: string, name: string, value: unknown, options: { type?: string; min?: number; max?: number; step?: string; hint?: string; required?: boolean; placeholder?: string } = {}): string {
  const id = name.replaceAll('.', '-');
  return `<label class="field" for="${id}"><span>${escape(label)}</span><input id="${id}" name="${name}" type="${options.type ?? 'text'}" value="${escape(value)}" ${options.min !== undefined ? `min="${options.min}"` : ''} ${options.max !== undefined ? `max="${options.max}"` : ''} ${options.step ? `step="${options.step}"` : ''} ${options.required ? 'required' : ''} ${options.placeholder ? `placeholder="${escape(options.placeholder)}"` : ''} ${options.hint ? `aria-describedby="${id}-hint"` : ''}>${options.hint ? `<small id="${id}-hint">${escape(options.hint)}</small>` : ''}</label>`;
}
export function select(label: string, name: string, value: unknown, options: [string, string][]): string {
  const id = name.replaceAll('.', '-');
  return `<label class="field" for="${id}"><span>${escape(label)}</span><select id="${id}" name="${name}">${options.map(([val, text]) => `<option value="${escape(val)}" ${val === value ? 'selected' : ''}>${escape(text)}</option>`).join('')}</select></label>`;
}
export function toggle(label: string, name: string, value: boolean, hint: string): string {
  const id = name.replaceAll('.', '-');
  return `<label class="toggle-row" for="${id}"><span><strong id="${id}-label">${escape(label)}</strong><small id="${id}-hint">${escape(hint)}</small></span><input id="${id}" type="checkbox" name="${name}" aria-labelledby="${id}-label" aria-describedby="${id}-hint" ${value ? 'checked' : ''}><span class="switch" aria-hidden="true"></span></label>`;
}
