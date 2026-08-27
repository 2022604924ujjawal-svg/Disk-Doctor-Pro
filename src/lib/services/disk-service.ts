import { invoke } from '@tauri-apps/api/core';

import type { DiskInfo } from '@/lib/models/disk';

/** Exposes disk enumeration without aggregating unrelated native commands. */
export class DiskService {
  static getSystemDisk(): Promise<DiskInfo> {
    return invoke<DiskInfo>('get_system_disk');
  }

  static listDisks(): Promise<DiskInfo[]> {
    return invoke<DiskInfo[]>('list_disks');
  }
}
