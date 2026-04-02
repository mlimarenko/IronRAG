export const DOCUMENT_UPLOAD_FORMAT_TOKENS = ['pdf', 'docx', 'pptx', 'txt', 'md', 'images'] as const

export type DocumentUploadFormatToken = (typeof DOCUMENT_UPLOAD_FORMAT_TOKENS)[number]

const IMAGE_EXTENSIONS = [
  'png',
  'jpg',
  'jpeg',
  'gif',
  'webp',
  'bmp',
  'svg',
  'tif',
  'tiff',
  'heic',
  'heif',
]
const TEXT_EXTENSIONS = [
  'md',
  'markdown',
  'txt',
  'text',
  'log',
  'csv',
  'json',
  'yaml',
  'yml',
  'xml',
]

export function normalizeDocumentUploadFormats(formats: string[]): DocumentUploadFormatToken[] {
  const normalized = formats
    .map((format) => format.trim().toLowerCase())
    .filter((format): format is DocumentUploadFormatToken =>
      DOCUMENT_UPLOAD_FORMAT_TOKENS.includes(format as DocumentUploadFormatToken),
    )

  return Array.from(new Set(normalized))
}

export function buildDocumentUploadAcceptString(formats: string[]): string {
  return normalizeDocumentUploadFormats(formats)
    .flatMap((format) => {
      switch (format) {
        case 'pdf':
          return ['.pdf', 'application/pdf']
        case 'docx':
          return [
            '.docx',
            'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
          ]
        case 'pptx':
          return [
            '.pptx',
            'application/vnd.openxmlformats-officedocument.presentationml.presentation',
          ]
        case 'txt':
          return ['.txt', 'text/plain']
        case 'md':
          return ['.md', 'text/markdown']
        case 'images':
          return ['image/*']
      }
    })
    .join(',')
}

export function formatAcceptedDocumentFormats(
  formats: string[],
  resolveLabel: (format: string) => string,
): string {
  const normalized = normalizeDocumentUploadFormats(formats)
  return normalized.length > 0 ? normalized.map((format) => resolveLabel(format)).join(', ') : '—'
}

export function isAcceptedDocumentUpload(file: File, formats: string[]): boolean {
  const normalized = normalizeDocumentUploadFormats(formats)
  if (normalized.length === 0) {
    return true
  }

  const extension = file.name.split('.').pop()?.toLowerCase() ?? ''
  return normalized.some((format) => {
    switch (format) {
      case 'pdf':
        return extension === 'pdf' || file.type === 'application/pdf'
      case 'docx':
        return extension === 'docx' || file.type.includes('wordprocessingml')
      case 'pptx':
        return extension === 'pptx' || file.type.includes('presentationml')
      case 'txt':
        return extension === 'txt' || file.type === 'text/plain'
      case 'md':
        return extension === 'md' || file.type === 'text/markdown'
      case 'images':
        return file.type.startsWith('image/') || IMAGE_EXTENSIONS.includes(extension)
    }
  })
}

export function inferDocumentFileType(fileName: string, mimeType: string | null): string {
  const extension = fileName.split('.').pop()?.toLowerCase() ?? ''
  if (mimeType?.startsWith('image/') || IMAGE_EXTENSIONS.includes(extension)) {
    return 'Image'
  }
  if (mimeType === 'application/pdf' || extension === 'pdf') {
    return 'PDF'
  }
  if (extension === 'docx' || mimeType?.includes('wordprocessingml')) {
    return 'DOCX'
  }
  if (extension === 'pptx' || mimeType?.includes('presentationml')) {
    return 'PPTX'
  }
  if (TEXT_EXTENSIONS.includes(extension) || mimeType?.startsWith('text/')) {
    return 'Text'
  }
  return 'File'
}

export function inferDocumentFormatTokenFromMime(mimeType: string | null): string | null {
  if (!mimeType) {
    return null
  }
  const normalized = mimeType.trim().toLowerCase()
  if (normalized === 'application/pdf') {
    return 'pdf'
  }
  if (normalized.includes('wordprocessingml')) {
    return 'docx'
  }
  if (normalized.includes('presentationml')) {
    return 'pptx'
  }
  if (normalized.startsWith('image/')) {
    return 'images'
  }
  if (normalized === 'text/plain') {
    return 'txt'
  }
  if (normalized === 'text/markdown') {
    return 'md'
  }

  const raw = normalized.split('/').pop() ?? normalized
  return raw.replace('.', '')
}
