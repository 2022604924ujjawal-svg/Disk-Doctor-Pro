export const UNINSTALL_CANCELLATION_TOAST_ID = 'application-uninstall-cancellation';

export function shouldNotifyUninstallCancellation(wasOpen: boolean, nextOpen: boolean): boolean {
  return wasOpen && !nextOpen;
}
