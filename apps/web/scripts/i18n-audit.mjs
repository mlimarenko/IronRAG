import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import ts from 'typescript'

const scriptDir = path.dirname(fileURLToPath(import.meta.url))
const webRoot = path.resolve(scriptDir, '..')
const srcRoot = path.join(webRoot, 'src')
const localeFiles = {
  en: path.join(srcRoot, 'shared/i18n/en.json'),
  ru: path.join(srcRoot, 'shared/i18n/ru.json'),
}
const pluralSuffixes = ['zero', 'one', 'two', 'few', 'many', 'other']
const sourceExtensions = new Set(['.ts', '.tsx', '.js', '.jsx'])
const ignoredPathSegments = [`${path.sep}shared${path.sep}api${path.sep}generated${path.sep}`]
const ignoredSourceFilePatterns = [/\.test\.[jt]sx?$/, /\.spec\.[jt]sx?$/, /\.stories\.[jt]sx?$/]

const pluralSuffixGroup = `(?:_(?:${pluralSuffixes.join('|')}))?`
// i18next substitutes `{{name}}`, `{{- name}}` and `{{name, format}}` from the
// options object passed alongside the key.
// Scanned with indexOf rather than a global regex: `/\{\{[^}]*\}\}/g` re-scans
// to the next brace from every start position, which is quadratic on a long
// string that contains no placeholder at all. Only the name inside an already
// delimited placeholder is matched with a regex, against bounded input.
const interpolationNamePattern = /^(?:\s*-)?\s*([\p{L}\p{N}_$]+)/u

function readJson(file) {
  return JSON.parse(fs.readFileSync(file, 'utf8'))
}

function flattenEntries(value, prefix = '', entries = new Map()) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    if (prefix) entries.set(prefix, value)
    return entries
  }

  for (const [key, child] of Object.entries(value)) {
    flattenEntries(child, prefix ? `${prefix}.${key}` : key, entries)
  }
  return entries
}

function escapeForRegExp(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, String.raw`\$&`)
}

function interpolationVariables(value) {
  const names = new Set()
  if (typeof value !== 'string') return names
  let from = 0
  for (;;) {
    const open = value.indexOf('{{', from)
    if (open === -1) return names
    const close = value.indexOf('}}', open + 2)
    if (close === -1) return names
    const inner = value.slice(open + 2, close)
    // A stray single `}` inside means this is not a placeholder, matching what
    // the old `[^}]*` body accepted.
    const name = inner.includes('}') ? null : interpolationNamePattern.exec(inner)
    if (name) names.add(name[1])
    from = close + 2
  }
}

/** `documents.count_other` -> `documents.count`; a key with no plural suffix is
 * returned unchanged. */
function pluralBaseKey(key) {
  let base = key
  for (const suffix of pluralSuffixes) {
    if (base.endsWith(`_${suffix}`)) base = base.slice(0, -suffix.length - 1)
  }
  return base
}

function requireVariable(required, key, name) {
  if (!required.has(key)) required.set(key, new Set())
  required.get(key).add(name)
}

// A key that a caller must satisfy with an options object: the union over both
// locales and over every plural form, because any locale that interpolates the
// variable renders the raw placeholder without it.
function collectRequiredVariables(localeEntries) {
  const required = new Map()
  for (const entries of localeEntries) {
    for (const [key, value] of entries) {
      const base = pluralBaseKey(key)
      for (const name of interpolationVariables(value)) {
        requireVariable(required, base, name)
        if (base !== key) requireVariable(required, key, name)
      }
    }
  }
  return required
}

function listSourceFiles(dir, files = []) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const fullPath = path.join(dir, entry.name)
    if (entry.isDirectory()) {
      if (ignoredPathSegments.some((segment) => `${fullPath}${path.sep}`.includes(segment))) {
        continue
      }
      listSourceFiles(fullPath, files)
      continue
    }

    if (
      sourceExtensions.has(path.extname(entry.name)) &&
      !ignoredSourceFilePatterns.some((pattern) => pattern.test(entry.name))
    ) {
      files.push(fullPath)
    }
  }
  return files
}

function hasKey(keys, key) {
  return keys.has(key) || pluralSuffixes.some((suffix) => keys.has(`${key}_${suffix}`))
}

function markUsedDefinition(keys, key, usedDefinitions) {
  if (keys.has(key)) {
    usedDefinitions.add(key)
  }
  for (const suffix of pluralSuffixes) {
    const pluralKey = `${key}_${suffix}`
    if (keys.has(pluralKey)) {
      usedDefinitions.add(pluralKey)
    }
  }
}

function isTranslationCallee(callee) {
  if (ts.isIdentifier(callee)) {
    return callee.text === 't' || callee.text === 'i18nValue'
  }
  if (ts.isPropertyAccessExpression(callee)) {
    return callee.name.text === 't'
  }
  return false
}

function scriptKindFor(file) {
  if (file.endsWith('.tsx')) return ts.ScriptKind.TSX
  if (file.endsWith('.jsx')) return ts.ScriptKind.JSX
  if (file.endsWith('.js')) return ts.ScriptKind.JS
  return ts.ScriptKind.TS
}

function recordStaticKey(usage, key, relativeFile) {
  if (!key || key.includes('${')) return
  if (!usage.usedKeys.has(key)) usage.usedKeys.set(key, new Set())
  usage.usedKeys.get(key).add(relativeFile)
  markUsedDefinition(usage.definedKeys, key, usage.usedDefinitions)
}

// A dynamic key is `head${expr}literal${expr}literal…`. Only the interpolated
// spans are unknown, and an interpolated span is a single key segment rather
// than "anything below this prefix" — so it is `[^.]+`. Reconstructing the
// whole template and anchoring it keeps every literal that follows an
// interpolation load-bearing (`documents.editor.${surface}Mode` must not
// vouch for `documents.editor.undo`), and stops a head that is merely a
// namespace (`assistant.${bucket}`) from vouching for a deeper subtree.
function templateKeyPattern(node) {
  let source = escapeForRegExp(node.head.text)
  for (const span of node.templateSpans) {
    source += `[^.]+${escapeForRegExp(span.literal.text)}`
  }
  // Adjacent interpolations (`${head}${tail}`) produce one segment at runtime;
  // left as-is they would demand one character per span.
  return source.replace(/(?:\[\^\.\]\+)+/gu, '[^.]+')
}

function recordDynamicPattern(usage, pattern, relativeFile) {
  if (!pattern) return
  if (!usage.dynamicPatterns.has(pattern)) usage.dynamicPatterns.set(pattern, new Set())
  usage.dynamicPatterns.get(pattern).add(relativeFile)
  const matcher = new RegExp(`^${pattern}${pluralSuffixGroup}$`, 'u')
  for (const key of usage.definedKeys) {
    if (matcher.test(key)) {
      usage.usedDefinitions.add(key)
    }
  }
}

// A `t()` call whose value interpolates `{{name}}` but whose options object
// omits `name` renders the placeholder verbatim — the braces reach the screen,
// and any assistive technology reading the string reads them out.
function recordInterpolationCall(usage, call, keyArg, relativeFile, sourceFile) {
  const required = usage.requiredVariables.get(keyArg.text)
  if (!required || required.size === 0) return

  const [, optionsArg] = call.arguments
  // Options assembled elsewhere cannot be inspected here; only an object
  // literal (or an absent second argument) is decidable.
  if (optionsArg && !ts.isObjectLiteralExpression(optionsArg)) return

  const provided = new Set()
  for (const property of optionsArg?.properties ?? []) {
    if (ts.isSpreadAssignment(property)) return
    const name = property.name
    if (!name) return
    if (ts.isIdentifier(name) || ts.isStringLiteralLike(name)) {
      provided.add(name.text)
      continue
    }
    return
  }

  const missing = [...required].filter((name) => !provided.has(name)).sort()
  if (missing.length === 0) return

  const line = sourceFile.getLineAndCharacterOfPosition(call.getStart()).line + 1
  usage.interpolationViolations.push({
    key: keyArg.text,
    missing,
    location: `${relativeFile}:${line}`,
  })
}

/**
 * `t('some.key', { defaultValue: 'text' })` on a **static** key is a second copy
 * of the text living next to the code: it never reaches `ru.json`/`en.json`, is
 * never translated into the other language, and drifts from the resource that
 * is supposed to own it.
 *
 * On a **dynamic** key it is the opposite — the key set cannot be enumerated at
 * build time, and the fallback is a runtime token the API produced
 * (`humanizeToken(stage.stage)`, `gap.reason`), not prose anyone would
 * translate. That is the case the policy exempts, so only static keys are
 * reported here.
 *
 * The rule used to be checked with `grep -rn "defaultValue:"`, which cannot
 * tell any of this apart: it counted a hook prop of the same name as a
 * violation and would have counted a genuine one the same way.
 */
function recordDefaultValueOnStaticKey(usage, node, keyArg, relativeFile, sourceFile) {
  const [, optionsArg] = node.arguments
  if (!optionsArg || !ts.isObjectLiteralExpression(optionsArg)) return
  const hasDefaultValue = optionsArg.properties.some(
    (property) =>
      (ts.isPropertyAssignment(property) || ts.isShorthandPropertyAssignment(property)) &&
      property.name &&
      ts.isIdentifier(property.name) &&
      property.name.text === 'defaultValue',
  )
  if (!hasDefaultValue) return
  const { line } = sourceFile.getLineAndCharacterOfPosition(node.getStart(sourceFile))
  usage.defaultValueViolations.push({
    key: keyArg.text,
    location: `${relativeFile}:${line + 1}`,
  })
}

function visitTranslationCalls(node, usage, relativeFile, sourceFile) {
  // Translation keys are also carried through typed config maps and helper
  // functions before reaching `t()`. Count an exact locale-key literal as
  // usage; dead config objects are handled independently by Knip/ts-prune.
  if (ts.isStringLiteralLike(node) && usage.definedKeys.has(node.text)) {
    recordStaticKey(usage, node.text, relativeFile)
  }
  if (ts.isCallExpression(node) && isTranslationCallee(node.expression)) {
    const [keyArg] = node.arguments
    if (keyArg) {
      if (ts.isStringLiteralLike(keyArg)) {
        recordStaticKey(usage, keyArg.text, relativeFile)
        recordInterpolationCall(usage, node, keyArg, relativeFile, sourceFile)
        recordDefaultValueOnStaticKey(usage, node, keyArg, relativeFile, sourceFile)
      } else if (ts.isTemplateExpression(keyArg)) {
        recordDynamicPattern(usage, templateKeyPattern(keyArg), relativeFile)
      }
    }
  }
  ts.forEachChild(node, (child) => visitTranslationCalls(child, usage, relativeFile, sourceFile))
}

function extractUsage(files, definedKeys, requiredVariables) {
  const usage = {
    definedKeys,
    requiredVariables,
    usedKeys: new Map(),
    dynamicPatterns: new Map(),
    usedDefinitions: new Set(),
    interpolationViolations: [],
    defaultValueViolations: [],
  }

  for (const file of files) {
    const source = fs.readFileSync(file, 'utf8')
    const relativeFile = path.relative(webRoot, file)
    const sourceFile = ts.createSourceFile(
      file,
      source,
      ts.ScriptTarget.Latest,
      true,
      scriptKindFor(file),
    )
    visitTranslationCalls(sourceFile, usage, relativeFile, sourceFile)
  }

  return usage
}

function sorted(values) {
  return [...values].sort((a, b) => a.localeCompare(b))
}

function printList(title, values, formatter = (value) => value) {
  console.log(`\n${title}: ${values.length}`)
  for (const value of values) {
    console.log(`  - ${formatter(value)}`)
  }
}

const localeEntries = Object.fromEntries(
  Object.entries(localeFiles).map(([locale, file]) => [locale, flattenEntries(readJson(file))]),
)
const locales = Object.fromEntries(
  Object.entries(localeEntries).map(([locale, entries]) => [locale, new Set(entries.keys())]),
)
const allDefinedKeys = new Set([...locales.en, ...locales.ru])
const requiredVariables = collectRequiredVariables(Object.values(localeEntries))
const sourceFiles = listSourceFiles(srcRoot)
const {
  usedKeys,
  dynamicPatterns,
  usedDefinitions,
  interpolationViolations,
  defaultValueViolations,
} = extractUsage(sourceFiles, allDefinedKeys, requiredVariables)
const usedKeyNames = sorted(usedKeys.keys())

const missingInEn = usedKeyNames.filter((key) => !hasKey(locales.en, key))
const missingInRu = usedKeyNames.filter((key) => !hasKey(locales.ru, key))
const onlyInEn = sorted([...locales.en].filter((key) => !locales.ru.has(key)))
const onlyInRu = sorted([...locales.ru].filter((key) => !locales.en.has(key)))
const unused = sorted([...allDefinedKeys].filter((key) => !usedDefinitions.has(key)))

console.log('i18n audit')
console.log(`  source files: ${sourceFiles.length}`)
console.log(`  static t() keys: ${usedKeys.size}`)
console.log(`  dynamic t() key patterns: ${dynamicPatterns.size}`)
console.log(`  en definitions: ${locales.en.size}`)
console.log(`  ru definitions: ${locales.ru.size}`)

printList(
  'keys missing in EN',
  missingInEn,
  (key) => `${key} (${sorted(usedKeys.get(key)).join(', ')})`,
)
printList(
  'keys missing in RU',
  missingInRu,
  (key) => `${key} (${sorted(usedKeys.get(key)).join(', ')})`,
)
printList('defined only in EN', onlyInEn)
printList('defined only in RU', onlyInRu)
printList('defined but unused', unused)
printList(
  'calls missing interpolation variables',
  [...interpolationViolations].sort((left, right) => left.location.localeCompare(right.location)),
  ({ key, missing, location }) => `${location} — ${key} needs {{${missing.join('}}, {{')}}}`,
)

printList(
  'static keys carrying a defaultValue',
  [...defaultValueViolations].sort((left, right) => left.location.localeCompare(right.location)),
  ({ key, location }) => `${location} — ${key}: put the text in ru.json and en.json instead`,
)

if (
  missingInEn.length > 0 ||
  missingInRu.length > 0 ||
  onlyInEn.length > 0 ||
  onlyInRu.length > 0 ||
  unused.length > 0 ||
  interpolationViolations.length > 0 ||
  defaultValueViolations.length > 0
) {
  process.exitCode = 1
}
