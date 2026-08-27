import { listen, type UnlistenFn } from '@tauri-apps/api/event';

const OPEN_ABOUT_EVENT = 'application-menu-open-about';

export class ApplicationMenuService {
  static onOpenAbout(handler: () => void): Promise<UnlistenFn> {
    return listen(OPEN_ABOUT_EVENT, handler);
  }
}
