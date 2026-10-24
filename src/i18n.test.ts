import { test } from 'node:test';
import assert from 'node:assert/strict';
import { getLanguage, html, setLanguage, t } from './i18n.ts';

test('language switching translates authored labels and preserves interpolated user data', () => {
  const storage = new Map<string, string>();
  Object.assign(globalThis, { document: { documentElement: { lang: '' } }, localStorage: { setItem: (key: string, value: string) => storage.set(key, value) } });
  const userText = '模型管理 <script>用户输入</script>';
  setLanguage('en');
  assert.equal(getLanguage(), 'en');
  assert.equal(t('应用设置'), 'Application Settings');
  assert.equal(html`<h2>模型管理</h2><pre>${userText}</pre>`, `<h2>Models</h2><pre>${userText}</pre>`);
  assert.equal(html`<span>最大输出 Token</span>`, '<span>Max output tokens</span>');
  assert.equal(document.documentElement.lang, 'en');
  assert.equal(storage.get('qwen-desk-language'), 'en');
  setLanguage('zh-CN');
  assert.equal(t('应用设置'), '应用设置');
  assert.equal(html`<h2>模型管理</h2>${userText}`, `<h2>模型管理</h2>${userText}`);
});
