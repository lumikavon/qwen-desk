import test from 'node:test';
import assert from 'node:assert/strict';
import { SSEParser } from './sse.ts';
test('SSE survives arbitrary IPC boundaries, CRLF, comments and multiline data', () => {
  const events: string[][] = [];
  const parser = new SSEParser((data, event) => events.push([data, event]));
  const wire = ': heartbeat\r\nevent: error\r\ndata: {"error":\r\ndata: "失败"}\r\n\r\ndata: [DONE]\n\n';
  for (const char of wire) parser.feed(char);
  assert.deepEqual(events, [['{"error":\n"失败"}', 'error'], ['[DONE]', 'message']]);
});
test('SSE handles multiple chunks, usage-only packets and terminal partial frame', () => {
  const events: string[] = [];
  const parser = new SSEParser(data => events.push(data));
  parser.feed('data: {"choices":[{"delta":{"content":"你');
  parser.feed('好"}}]}\n\ndata: {"usage":{"total_tokens":9}}\n\ndata: [DONE]');
  parser.finish();
  assert.equal(JSON.parse(events[0]).choices[0].delta.content, '你好');
  assert.equal(JSON.parse(events[1]).usage.total_tokens, 9);
  assert.equal(events[2], '[DONE]');
});
