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

function readJson(file) {
  return JSON.parse(fs.readFileSync(file, 'utf8'))
}

function flattenKeys(value, prefix = '', keys = new Set()) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    if (prefix) keys.add(prefix)
    return keys
  }

  for (const [key, child] of Object.entries(value)) {
    flattenKeys(child, prefix ? `${prefix}.${key}` : key, keys)
  }
  return keys
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

function recordDynamicPrefix(usage, prefix, relativeFile) {
  if (!prefix) return
  if (!usage.dynamicPrefixes.has(prefix)) usage.dynamicPrefixes.set(prefix, new Set())
  usage.dynamicPrefixes.get(prefix).add(relativeFile)
  for (const key of usage.definedKeys) {
    if (key.startsWith(prefix)) {
      usage.usedDefinitions.add(key)
    }
  }
}

function visitTranslationCalls(node, usage, relativeFile) {
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
      } else if (ts.isTemplateExpression(keyArg)) {
        recordDynamicPrefix(usage, keyArg.head.text, relativeFile)
      }
    }
  }
  ts.forEachChild(node, (child) => visitTranslationCalls(child, usage, relativeFile))
}

function extractUsage(files, definedKeys) {
  const usage = {
    definedKeys,
    usedKeys: new Map(),
    dynamicPrefixes: new Map(),
    usedDefinitions: new Set(),
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
    visitTranslationCalls(sourceFile, usage, relativeFile)
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

const locales = Object.fromEntries(
  Object.entries(localeFiles).map(([locale, file]) => [locale, flattenKeys(readJson(file))]),
)
const allDefinedKeys = new Set([...locales.en, ...locales.ru])
const sourceFiles = listSourceFiles(srcRoot)
const { usedKeys, dynamicPrefixes, usedDefinitions } = extractUsage(sourceFiles, allDefinedKeys)
const usedKeyNames = sorted(usedKeys.keys())

const missingInEn = usedKeyNames.filter((key) => !hasKey(locales.en, key))
const missingInRu = usedKeyNames.filter((key) => !hasKey(locales.ru, key))
const onlyInEn = sorted([...locales.en].filter((key) => !locales.ru.has(key)))
const onlyInRu = sorted([...locales.ru].filter((key) => !locales.en.has(key)))
const unused = sorted([...allDefinedKeys].filter((key) => !usedDefinitions.has(key)))

console.log('i18n audit')
console.log(`  source files: ${sourceFiles.length}`)
console.log(`  static t() keys: ${usedKeys.size}`)
console.log(`  dynamic t() prefixes: ${dynamicPrefixes.size}`)
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

if (
  missingInEn.length > 0 ||
  missingInRu.length > 0 ||
  onlyInEn.length > 0 ||
  onlyInRu.length > 0 ||
  unused.length > 0
) {
  process.exitCode = 1
}
