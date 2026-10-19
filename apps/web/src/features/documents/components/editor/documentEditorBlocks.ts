import { codeLanguageForSourceFormat, isCodeLikeSourceFormat } from './editorSurfaceMode'

export function serializeSourceTextForEditor(sourceText: string, sourceFormat?: string): string {
  const normalized = sourceText.replace(/^\uFEFF/, '').replace(/\r\n?/g, '\n')
  if (!isCodeLikeSourceFormat(sourceFormat)) {
    return normalized
  }

  const language = codeLanguageForSourceFormat(sourceFormat)
  const fence = '```'
  return [`${fence}${language ?? ''}`, normalized, fence].join('\n')
}
