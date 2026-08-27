import { FILE_CATEGORY_IDS } from '@/lib/models/file-category';
import type { FileCategoryId } from '@/lib/models/file-category';

export type FileVisualKind =
  | 'pdf'
  | 'document'
  | 'spreadsheet'
  | 'presentation'
  | 'text'
  | 'code'
  | 'data'
  | 'audio'
  | 'video'
  | 'image'
  | 'archive'
  | 'installer'
  | 'disk-image'
  | 'binary'
  | 'ai-model'
  | 'other';

export type FileVisualSource = 'native' | 'semantic';

export interface FileTypeDescriptor {
  extension: string;
  iconSource: FileVisualSource;
  kind: FileVisualKind;
}

const CATEGORY_EXTENSIONS: Readonly<Record<Exclude<FileCategoryId, 'all' | 'other'>, readonly string[]>> = {
  video: ['mp4', 'mov', 'mkv', 'avi', 'wmv', 'webm', 'm4v', 'mpg', 'mpeg', 'flv', '3gp'],
  audio: ['mp3', 'wav', 'flac', 'aac', 'm4a', 'ogg', 'wma', 'aiff', 'opus'],
  document: [
    'pdf',
    'doc',
    'docx',
    'odt',
    'rtf',
    'txt',
    'md',
    'epub',
    'xls',
    'xlsx',
    'ods',
    'csv',
    'ppt',
    'pptx',
    'odp',
  ],
  archive: ['zip', '7z', 'rar', 'tar', 'gz', 'bz2', 'xz', 'tgz'],
  // This category is only an informational result filter, so it can include
  // established model/checkpoint conventions without affecting cleanup safety.
  // Generic serialization formats such as .bin, .pkl, .h5, and .hdf5 remain
  // in Other because filenames alone cannot identify their owners or content.
  aiModel: ['safetensors', 'gguf', 'ggml', 'onnx', 'ort', 'tflite', 'mlmodel', 'keras', 'pt', 'pth', 'ckpt'],
  // `.raw` also identifies disk and VM images, so only unambiguous camera RAW
  // extensions are classified as images.
  image: [
    'jpg',
    'jpeg',
    'png',
    'gif',
    'webp',
    'heic',
    'tiff',
    'bmp',
    'svg',
    'ico',
    'avif',
    'dng',
    'cr2',
    'cr3',
    'nef',
    'arw',
    'orf',
    'rw2',
  ],
};

const CATEGORY_EXTENSION_SETS = Object.fromEntries(
  Object.entries(CATEGORY_EXTENSIONS).map(([category, extensions]) => [category, new Set(extensions)])
) as Record<Exclude<FileCategoryId, 'all' | 'other'>, ReadonlySet<string>>;
const EXTENSION_CATEGORIES = new Map<string, FileCategoryId>(
  Object.entries(CATEGORY_EXTENSIONS).flatMap(([category, extensions]) =>
    extensions.map(extension => [extension, category as FileCategoryId] as const)
  )
);

const SPREADSHEET_EXTENSIONS = new Set(['xls', 'xlsx', 'ods', 'csv']);
const PRESENTATION_EXTENSIONS = new Set(['ppt', 'pptx', 'odp']);
const DOCUMENT_EXTENSIONS = new Set(['doc', 'docx', 'odt', 'rtf', 'epub']);
const TEXT_EXTENSIONS = new Set(['txt', 'md', 'log']);
const CODE_EXTENSIONS = new Set([
  'js',
  'jsx',
  'ts',
  'tsx',
  'vue',
  'html',
  'htm',
  'css',
  'scss',
  'less',
  'json',
  'xml',
  'yaml',
  'yml',
  'toml',
  'ini',
  'sh',
  'zsh',
  'fish',
  'py',
  'rb',
  'rs',
  'go',
  'java',
  'kt',
  'swift',
  'c',
  'h',
  'cpp',
  'hpp',
]);
const DATA_EXTENSIONS = new Set(['db', 'sqlite', 'sqlite3', 'sql']);
const DISK_IMAGE_EXTENSIONS = new Set(['dmg', 'iso']);
const BINARY_EXTENSIONS = new Set(['bin', 'dat', 'dll', 'dylib', 'so']);
const INSTALLER_VISUAL_EXTENSIONS = new Set(['exe', 'msi', 'msix', 'appx', 'pkg', 'apk', 'deb', 'rpm']);
const SEMANTIC_ICON_KINDS: ReadonlySet<FileVisualKind> = new Set(['ai-model']);

/** Classifies filenames consistently across all storage result views. */
export class FileTypeUtils {
  static extension(name: string): string {
    const fileName = name.split(/[\\/]/).pop() ?? '';
    const dotIndex = fileName.lastIndexOf('.');
    // Dotfiles with one leading dot have no extension.
    if (dotIndex <= 0 || dotIndex === fileName.length - 1) return '';
    return fileName.slice(dotIndex + 1).toLocaleLowerCase('en-US');
  }

  static category(name: string): FileCategoryId {
    const extension = FileTypeUtils.extension(name);
    // The prebuilt map keeps high-volume filtering at one lookup per row.
    return EXTENSION_CATEGORIES.get(extension) ?? FILE_CATEGORY_IDS.other;
  }

  static categoryCounts(names: readonly string[]): Record<FileCategoryId, number> {
    const counts = Object.fromEntries(Object.values(FILE_CATEGORY_IDS).map(category => [category, 0])) as Record<
      FileCategoryId,
      number
    >;
    counts[FILE_CATEGORY_IDS.all] = names.length;
    names.forEach(name => {
      counts[FileTypeUtils.category(name)] += 1;
    });
    return counts;
  }

  static descriptor(name: string): FileTypeDescriptor {
    const extension = FileTypeUtils.extension(name);
    const kind = FileTypeUtils.visualKind(extension);
    return {
      extension,
      iconSource: SEMANTIC_ICON_KINDS.has(kind) ? 'semantic' : 'native',
      kind,
    };
  }

  private static visualKind(extension: string): FileVisualKind {
    if (extension === 'pdf') return 'pdf';
    if (SPREADSHEET_EXTENSIONS.has(extension)) return 'spreadsheet';
    if (PRESENTATION_EXTENSIONS.has(extension)) return 'presentation';
    if (DOCUMENT_EXTENSIONS.has(extension)) return 'document';
    if (TEXT_EXTENSIONS.has(extension)) return 'text';
    if (CODE_EXTENSIONS.has(extension)) return 'code';
    if (DATA_EXTENSIONS.has(extension)) return 'data';
    if (DISK_IMAGE_EXTENSIONS.has(extension)) return 'disk-image';
    if (CATEGORY_EXTENSION_SETS.aiModel.has(extension)) return 'ai-model';
    if (BINARY_EXTENSIONS.has(extension)) return 'binary';
    if (CATEGORY_EXTENSION_SETS.audio.has(extension)) return 'audio';
    if (CATEGORY_EXTENSION_SETS.video.has(extension)) return 'video';
    if (CATEGORY_EXTENSION_SETS.image.has(extension)) return 'image';
    if (CATEGORY_EXTENSION_SETS.archive.has(extension)) return 'archive';
    if (INSTALLER_VISUAL_EXTENSIONS.has(extension)) return 'installer';
    return 'other';
  }
}
