export class AppUpdateProgressUtils {
  static percent(downloadedBytes: number, totalBytes: number | null): number | null {
    if (!totalBytes || totalBytes <= 0) return null;
    return Math.min(100, Math.max(0, (downloadedBytes / totalBytes) * 100));
  }
}
