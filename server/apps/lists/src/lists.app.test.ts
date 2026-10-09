import { describe, expect, it } from 'vitest'

import { buildStoreFeedIds } from './lists.app'

describe('2025 Store feeds', () => {
  it('returns the full wearable storefront for Clothing, not the random legacy subset', () => {
    const ids = buildStoreFeedIds('clothingitems')
    expect(ids).not.toBeNull()
    expect(ids!.length).toBeGreaterThan(2000)
    expect(new Set(ids).size).toBe(ids!.length)
    // Query drops are the random boxes that previously polluted the Clothing tab.
    for (const boxId of ['2454', '2455', '2456', '2457', '2458']) {
      expect(ids).not.toContain(boxId)
    }
    // The developer-only/unreleased rows from the historical capture are not public Store items.
    expect(ids).not.toContain('1584')
    expect(ids).not.toContain('23208')
  })

  it('recognizes the exact StoreClothing list-service slugs', () => {
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
      ['HairDyeItems', 'hairdyeitems'],
      ['EquipmentSkinsItems', 'skinsitems'],
    ]
    for (const [alias, canonical] of aliases) {
      expect(buildStoreFeedIds(alias)).toEqual(buildStoreFeedIds(canonical))
    }
  })

  it('keeps hairstyle and facial-hair feeds populated using canonical avatar names', () => {
    const clothing = buildStoreFeedIds('clothingitems')!
    const hair = buildStoreFeedIds('HeadHairItems')!
    const facial = buildStoreFeedIds('FacialHairItems')!
    expect(hair.length).toBeGreaterThan(0)
    expect(facial.length).toBeGreaterThan(0)
    expect(new Set(facial).size).toBe(facial.length)
    expect(facial).not.toEqual(hair)
    expect(hair.every((id) => clothing.includes(id))).toBe(true)
    expect(facial.every((id) => clothing.includes(id))).toBe(true)
  })

  it('preserves zero-price/free hairstyle and facial-hair entries', () => {
    const hair = buildStoreFeedIds('hairitems')!
    const facial = buildStoreFeedIds('facialhairitems')!
    expect(hair.length).toBeGreaterThan(10)
    expect(facial.length).toBeGreaterThan(2)
  })
})
