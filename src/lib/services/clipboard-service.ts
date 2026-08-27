interface ClipboardWriter {
  writeText(value: string): Promise<void>;
}

export class ClipboardService {
  static async writeText(value: string, clipboard: ClipboardWriter | undefined = globalThis.navigator?.clipboard) {
    if (!clipboard) throw new Error('Clipboard access is unavailable');
    await clipboard.writeText(value);
  }
}
