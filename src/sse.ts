// An IPC event may contain part of a line or several complete SSE events.
export class SSEParser {
  private buffer = '';
  private lines: string[] = [];
  private skipLF = false;
  private readonly onData: (data: string, event: string) => void;
  constructor(onData: (data: string, event: string) => void) { this.onData = onData; }
  feed(chunk: string): void {
    for (const char of chunk) {
      if (this.skipLF) { this.skipLF = false; if (char === '\n') continue; }
      if (char === '\r' || char === '\n') {
        this.line(this.buffer); this.buffer = ''; this.skipLF = char === '\r';
      } else this.buffer += char;
    }
  }
  finish(): void { if (this.buffer) this.line(this.buffer); this.buffer = ''; this.line(''); }
  private line(line: string): void {
    if (line !== '') { this.lines.push(line); return; }
    const data: string[] = []; let event = 'message';
    for (const entry of this.lines) {
      if (entry.startsWith(':')) continue;
      const colon = entry.indexOf(':');
      const field = colon < 0 ? entry : entry.slice(0, colon);
      const value = colon < 0 ? '' : entry.slice(colon + 1).replace(/^ /, '');
      if (field === 'data') data.push(value);
      if (field === 'event') event = value;
    }
    this.lines = [];
    if (data.length) this.onData(data.join('\n'), event);
  }
}
