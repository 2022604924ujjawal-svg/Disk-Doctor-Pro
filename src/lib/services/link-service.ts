import { openUrl } from '@tauri-apps/plugin-opener';

import { normalizeExternalUrl } from '@/lib/utils/external-url';

export class LinkService {
  static async open(url: string): Promise<void> {
    const externalUrl = normalizeExternalUrl(url);
    if (!externalUrl) throw new Error('The external link uses an unsupported or invalid URL.');
    await openUrl(externalUrl);
  }
}
