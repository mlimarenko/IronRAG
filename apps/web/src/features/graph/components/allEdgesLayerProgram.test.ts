import { describe, expect, test } from 'vitest'
import {
  createAllEdgesLayerState,
  createCoordinateAllEdgesLayerState,
} from './allEdgesLayerProgram'

describe('allEdgesLayerProgram', () => {
  // Under JSDOM there is no real WebGL context; the factories detect this and
  // return null so every caller no-ops instead of throwing. This guards the
  // extraction: if the JSDOM short-circuit regressed, the whole graph test
  // suite would start crashing on context creation.
  test('factories return null under JSDOM so callers safely skip the GL layer', () => {
    const canvas = document.createElement('canvas')
    expect(createAllEdgesLayerState(canvas)).toBeNull()
    expect(createCoordinateAllEdgesLayerState(canvas)).toBeNull()
  })
})
