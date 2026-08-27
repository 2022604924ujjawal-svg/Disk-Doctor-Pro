import { describe, expect, it } from 'vitest';

import { FILE_CATEGORY_IDS } from '@/lib/models/file-category';

import { FileTypeUtils } from './file-type';

describe('file type classification', () => {
  it('classifies only high-confidence AI model formats as candidates', () => {
    for (const fileName of [
      'weights.safetensors',
      'qwen.GGUF',
      'legacy.ggml',
      'encoder.onnx',
      'runtime.ort',
      'mobile.tflite',
      'vision.mlmodel',
      'export.keras',
      'torch.pt',
      'weights.PTH',
      'training.ckpt',
    ]) {
      expect(FileTypeUtils.category(fileName)).toBe(FILE_CATEGORY_IDS.aiModel);
      expect(FileTypeUtils.descriptor(fileName)).toMatchObject({ iconSource: 'semantic', kind: 'ai-model' });
    }
  });

  it('keeps ambiguous model-like formats in Other', () => {
    for (const fileName of ['weights.bin', 'state.pkl', 'weights.h5', 'weights.hdf5']) {
      expect(FileTypeUtils.category(fileName)).toBe(FILE_CATEGORY_IDS.other);
    }
  });

  it('counts AI model candidates without changing the total', () => {
    const counts = FileTypeUtils.categoryCounts(['first.gguf', 'second.safetensors', 'notes.txt']);

    expect(counts[FILE_CATEGORY_IDS.all]).toBe(3);
    expect(counts[FILE_CATEGORY_IDS.aiModel]).toBe(2);
    expect(counts[FILE_CATEGORY_IDS.document]).toBe(1);
  });

  it('keeps executable formats in Other without losing their visual identity', () => {
    expect(FileTypeUtils.category('compiler.exe')).toBe(FILE_CATEGORY_IDS.other);
    expect(FileTypeUtils.category('package.pkg')).toBe(FILE_CATEGORY_IDS.other);
    expect(FileTypeUtils.descriptor('compiler.exe')).toMatchObject({ iconSource: 'native', kind: 'installer' });
  });
});
