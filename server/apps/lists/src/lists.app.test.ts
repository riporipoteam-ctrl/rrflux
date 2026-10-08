import { describe, expect, it } from 'vitest'

import { buildStoreFeedIds } from './lists.app'

describe('2025 Store feeds', () => {
  it('returns thousands of public wearable items for the Clothing page', () => {
    const ids = buildStoreFeedIds('clothingitems')
    expect(ids).not.toBeNull()
    expect(ids!.length).toBeGreaterThan(2000)
    expect(new Set(ids).size).toBe(ids!.length)
    expect(ids).not.toContain('538')
    expect(ids).not.toContain('539')
    expect(ids).not.toContain('541')
  })

  it('accepts the exact 2025 Store endpoint slugs', () => {
    const aliases: Array<[string, string]> = [
      ['HatsItems', 'headwearitems'],
      ['TorsoItems', 'topsitems'],
      ['BottomsItems', 'bottomsitems'],
      ['ShoesItems', 'footwearitems'],
      ['WaistItems', 'waistitems'],
      ['GlovesItems', 'handsitems'],
      ['ShoulderItems', 'shoulderitems'],
      ['HeadHairItems', 'hairitems'],
      ['FacialHairItems', 'facialhairitems'],
      ['AccessoriesItems', 'accessoriesitems'],
      ['BackpackItems', 'backpackitems'],
      ['EyewearItems', 'eyewearitems'],
      ['EarwearItems', 'earwearitems'],
      ['NeckwearItems', 'neckwearitems'],
    ]

    for (const [alias, canonical] of aliases) {
      expect(buildStoreFeedIds(alias)).toEqual(buildStoreFeedIds(canonical))
    }
  })

  it('keeps hairstyles and facial hair non-empty and distinct', () => {
    const hair = buildStoreFeedIds('HeadHairItems')
    const facial = buildStoreFeedIds('FacialHairItems')
    expect(hair).not.toBeNull()
    expect(facial).not.toBeNull()
    expect(hair!.length).toBeGreaterThan(0)
    expect(facial!.length).toBeGreaterThan(0)
    expect(facial).not.toEqual(hair)
  })

  it('never exposes the developer-only rarity tier', () => {
    const ids = buildStoreFeedIds('clothingitems')!
    expect(ids).not.toContain('1584')
    expect(ids).not.toContain('23208')
  })
})
