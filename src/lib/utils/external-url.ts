const ALLOWED_EXTERNAL_PROTOCOLS = new Set(['http:', 'https:', 'mailto:']);

/**
 * Returns a canonical URL only when it can be handed to an external
 * application safely. Keeping this policy outside individual views prevents
 * remotely supplied release notes from bypassing the same boundary used by
 * project-owned links.
 */
export function normalizeExternalUrl(value: string): string | null {
  const candidate = value.trim();
  if (!candidate) return null;

  try {
    const url = new URL(candidate);
    return ALLOWED_EXTERNAL_PROTOCOLS.has(url.protocol.toLowerCase()) ? url.href : null;
  } catch {
    return null;
  }
}
