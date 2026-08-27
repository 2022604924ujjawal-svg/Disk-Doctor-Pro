export const FILE_CATEGORY_IDS = {
  all: 'all',
  video: 'video',
  audio: 'audio',
  document: 'document',
  archive: 'archive',
  image: 'image',
  aiModel: 'aiModel',
  other: 'other',
} as const;

export type FileCategoryId = (typeof FILE_CATEGORY_IDS)[keyof typeof FILE_CATEGORY_IDS];

// Result filters use one product-defined order instead of relying on object
// insertion order. AI model classification remains a candidate hint based on
// high-confidence formats; it never changes selection or deletion behavior.
// Executables and package formats remain in Other because a filename extension
// alone cannot establish that a file is an installer.
export const FILE_CATEGORY_FILTER_ORDER = [
  FILE_CATEGORY_IDS.all,
  FILE_CATEGORY_IDS.video,
  FILE_CATEGORY_IDS.audio,
  FILE_CATEGORY_IDS.image,
  FILE_CATEGORY_IDS.document,
  FILE_CATEGORY_IDS.archive,
  FILE_CATEGORY_IDS.aiModel,
  FILE_CATEGORY_IDS.other,
] as const satisfies readonly FileCategoryId[];
