import { describe, expect, it } from 'vitest'

import { buildStoreFeedIds } from './lists.app'

describe('2025 Store feeds', () => {
  it('uses the full wearable snapshot for the top-level Clothing page', () => {
    const ids = buildStoreFeedIds('clothingitems')
    expect(ids).not.toBeNull()
    expect(ids!.length).toBeGreaterThan(2000)
    expect(ids).not.toContain('538')
    expect(ids).not.toContain('541')
  })

  it('accepts the exact 2025 storefront slugs as aliases', () => {
    const aliases: Array<[string, string]> = [
      ['HatsItems', 'headwearitems'],
      ['TorsoItems', 'topsitems'],
      ['BottomsItems', 'bottomsitems'],
      ['ShoesItems', 'footwearitems'],
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
      const fromAlias = buildStoreFeedIds(alias)
      const fromCanonical = buildStoreFeedIds(canonical)
      expect(fromAlias).not.toBeNull()
      expect(fromCanonical).not.toBeNull()
      expect(fromAlias).toEqual(fromCanonical)
      expect(fromAlias!.length).toBeGreaterThan(0)
    }
  })

  it('puts hairstyles and facial hair in their dedicated feeds', () => {
    const hair = buildStoreFeedIds('HeadHairItems')
    const facial = buildStoreFeedIds('FacialHairItems')
    expect(hair).not.toBeNull()
    expect(facial).not.toBeNull()
    expect(hair!.length).toBeGreaterThan(0)
    expect(facial!.length).toBeGreaterThan(0)
    expect(hair!.length).toBeGreaterThanOrEqual(facial!.length)
  })

  it('does not expose unreleased rarity -1 items through the wearable Store feeds', () => {
    const ids = buildStoreFeedIds('clothingitems')!
    expect(ids).not.toContain('1584')
    expect(ids).not.toContain('23208')
  })
})
