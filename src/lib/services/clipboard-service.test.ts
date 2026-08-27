import { describe, expect, it, vi } from 'vitest';

import { ClipboardService } from './clipboard-service';

describe('ClipboardService', () => {
  it('writes the complete value without changing its contents', async () => {
    const writeText = vi.fn(async () => undefined);
    const value = '/bin/launchctl setenv OLLAMA_ORIGINS http://tauri.localhost,https://tauri.localhost';

    await ClipboardService.writeText(value, { writeText });

    expect(writeText).toHaveBeenCalledWith(value);
  });

  it('fails explicitly when clipboard access is unavailable', async () => {
    await expect(ClipboardService.writeText('value', undefined)).rejects.toThrow('Clipboard access is unavailable');
  });
});
