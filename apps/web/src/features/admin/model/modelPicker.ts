import type { AiBindingPurpose } from '@/shared/api/generated'
import type { AdminModelOption } from './aiAdapter'

/** Money a model costs per million tokens, in the currency it was quoted in. */
interface ModelPickerPrice {
  currencyCode: string
  input: string
  output: string
}

/** One entry the binding dialog can offer for a purpose. */
export interface ModelPickerOption {
  /** Stable identity for the row. Discovered models have no catalog row yet. */
  key: string
  modelName: string
  /** Present once a catalog row represents this model. */
  modelCatalogId?: string
  selectable: boolean
  /** Everything the picker shows in columns. Absent means nobody stated it. */
  contextWindow?: number
  maxOutputTokens?: number
  price?: ModelPickerPrice
  /** What the provider says the model accepts. Empty means it did not say. */
  inputModalities: string[]
  /**
   * Why the entry reads the way it does, translated by the dialog:
   * - `cannot_serve` — a seeded model whose curated purposes exclude this one.
   * - `adds_purpose` — an operator's row; choosing it records the new purpose.
   * - `not_in_catalog` — offered by the provider right now; choosing it adds it.
   * - `needs_vision` — the provider says the model takes no images, and
   *   document extraction reads page images.
   * - `inactive` — a catalog row that is deprecated or disabled: the seed
   *   retired it, so it must not be offered again even though the provider
   *   may still answer for its id.
   */
  note?: 'cannot_serve' | 'adds_purpose' | 'not_in_catalog' | 'needs_vision' | 'inactive'
}

/** The subset of the provider's live listing this decision needs. */
export interface DiscoveredModelInput {
  modelName: string
  inputModalities: string[]
  contextWindow?: number | null | undefined
  maxOutputTokens?: number | null | undefined
  pricing?:
    { currencyCode: string; inputPricePer1m: string; outputPricePer1m: string } | null | undefined
}

/**
 * Document extraction reads page images. A provider that publishes its input
 * modalities is stating the model cannot, and that is worth blocking before an
 * ingest fails on the first page. Silence is not a claim: a listing that says
 * nothing about modalities blocks nothing.
 */
/**
 * `exactOptionalPropertyTypes` refuses an explicit `undefined`, and the API
 * sends `null` for a field the provider did not publish. Both mean the same
 * thing on screen — a dash — so the key is simply left out.
 */
function numberOrNothing<K extends string>(
  key: K,
  value: number | null | undefined,
): Partial<Record<K, number>> {
  return typeof value === 'number' ? ({ [key]: value } as Partial<Record<K, number>>) : {}
}

function readsPageImages(modalities: string[]): boolean {
  return modalities.length === 0 || modalities.includes('image')
}

/**
 * Every model the operator could bind for one purpose: the catalog rows for
 * the provider, plus whatever the provider is offering right now that the
 * catalog has never seen.
 *
 * Nothing is hidden. An entry that cannot serve the purpose stays visible and
 * says why — hiding them is what produced an empty dropdown on a router, whose
 * chat catalogue is never seeded, and on a text-only vendor asked for document
 * understanding.
 */
export function buildModelPickerOptions({
  purpose,
  catalog,
  discovered,
}: Readonly<{
  purpose: AiBindingPurpose | null
  catalog: AdminModelOption[]
  discovered: DiscoveredModelInput[]
}>): ModelPickerOption[] {
  // A catalog row's own columns, with the provider's live listing filling in
  // what the row does not carry: the catalog stores no modalities for a model
  // an operator added, and the listing knows them.
  const listedByName = new Map(discovered.map((model) => [model.modelName, model]))
  const fromCatalog = catalog.map((model): ModelPickerOption => {
    const listed = listedByName.get(model.modelName)
    const columns = {
      ...(model.contextWindow === undefined
        ? numberOrNothing('contextWindow', listed?.contextWindow)
        : { contextWindow: model.contextWindow }),
      ...(model.maxOutputTokens === undefined
        ? numberOrNothing('maxOutputTokens', listed?.maxOutputTokens)
        : { maxOutputTokens: model.maxOutputTokens }),
      price: {
        currencyCode: model.pricing.currencyCode,
        input: model.pricing.inputPricePer1m,
        output: model.pricing.outputPricePer1m,
      },
      inputModalities: listed?.inputModalities ?? [],
    }
    const inactive = model.lifecycleState === 'deprecated' || model.lifecycleState === 'disabled'
    const serves = Boolean(purpose && model.allowedBindingPurposes.includes(purpose))
    if (inactive) {
      // The seed retired this row. The provider may still answer for the id,
      // but offering it again would resurrect what the catalog just removed.
      return {
        key: model.id,
        modelName: model.modelName,
        modelCatalogId: model.id,
        ...columns,
        selectable: false,
        note: 'inactive',
      }
    }
    if (serves) {
      return {
        key: model.id,
        modelName: model.modelName,
        modelCatalogId: model.id,
        selectable: true,
        ...columns,
      }
    }
    return {
      key: model.id,
      modelName: model.modelName,
      modelCatalogId: model.id,
      ...columns,
      // A seeded row states what the model can do. An operator's row only
      // records what has been bound so far, so extending it is an edit, not a
      // contradiction.
      selectable: !model.seeded,
      note: model.seeded ? 'cannot_serve' : 'adds_purpose',
    }
  })
  const known = new Set(catalog.map((model) => model.modelName))
  const fromProvider = discovered
    .filter((model) => !known.has(model.modelName))
    .map((model): ModelPickerOption => {
      const blocked = purpose === 'extract_text' && !readsPageImages(model.inputModalities)
      return {
        key: `provider:${model.modelName}`,
        modelName: model.modelName,
        selectable: !blocked,
        note: blocked ? 'needs_vision' : 'not_in_catalog',
        ...numberOrNothing('contextWindow', model.contextWindow),
        ...numberOrNothing('maxOutputTokens', model.maxOutputTokens),
        ...(model.pricing
          ? {
              price: {
                currencyCode: model.pricing.currencyCode,
                input: model.pricing.inputPricePer1m,
                output: model.pricing.outputPricePer1m,
              },
            }
          : {}),
        inputModalities: model.inputModalities,
      }
    })
  return [...fromCatalog, ...fromProvider].sort((left, right) =>
    left.modelName.localeCompare(right.modelName),
  )
}
