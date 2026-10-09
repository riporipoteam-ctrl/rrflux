import { describe, expect, it } from 'vitest'

import sf32025 from '../../econ/static/storefronts/sf3-2025.json'
import { UNSELLABLE_RARITIES } from '../../econ/src/catalog-load'
import { buildStoreFeedIds } from './lists.app'

describe('2025 Store feeds', () => {
  it('returns the entire public wearable snapshot for Clothing, excluding boxes and non-wearables', () => {
    const ids = buildStoreFeedIds('clothingitems')
    expect(ids).not.toBeNull()
    expect(ids!.length).toBeGreaterThan(2000)
    expect(new Set(ids).size).toBe(ids!.length)

    const expected = sf32025.StoreItems
      .filter((item) =>
        !item.GiftDrop.IsQuery &&
        item.GiftDrop.AvatarItemType === 0 &&
        !item.GiftDrop.ConsumableItemDesc?.trim() &&
        !item.GiftDrop.EquipmentModificationGuid?.trim() &&
        !UNSELLABLE_RARITIES.includes(item.GiftDrop.Rarity)
      )
      .map((item) => String(item.PurchasableItemId))
    expect(ids).toEqual([...new Set(expected)])

    const clothingIds = new Set(ids)
    // These are the Rec Center's actual 2/3/4-Star Unique Boxes in the 2025 capture.
    for (const boxId of ['538', '539', '541']) expect(clothingIds.has(boxId)).toBe(false)
    // These ids were box IDs in an older catalog, but are real wearables in this 2025 snapshot.
    for (const wearableId of ['2454', '2455', '2456', '2458']) {
      expect(clothingIds.has(wearableId)).toBe(true)
    }
    for (const item of sf32025.StoreItems) {
      if (item.GiftDrop.ConsumableItemDesc?.trim() || item.GiftDrop.IsQuery) {
        expect(clothingIds.has(String(item.PurchasableItemId))).toBe(false)
      }
    }
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

  it('keeps hairstyle and facial-hair feeds populated and distinct', () => {
    const clothing = new Set(buildStoreFeedIds('clothingitems')!)
    const hair = buildStoreFeedIds('HeadHairItems')!
    const facial = buildStoreFeedIds('FacialHairItems')!
    expect(hair.length).toBeGreaterThan(10)
    expect(facial.length).toBeGreaterThan(2)
    expect(new Set(hair).size).toBe(hair.length)
    expect(new Set(facial).size).toBe(facial.length)
    expect(facial).not.toEqual(hair)
    expect(hair.every((id) => clothing.has(id))).toBe(true)
    expect(facial.every((id) => clothing.has(id))).toBe(true)
  })

  it('retains free items instead of requiring a positive price entry', () => {
    const hair = new Set(buildStoreFeedIds('hairitems')!)
    const facial = new Set(buildStoreFeedIds('facialhairitems')!)
    const freeCatalogIds = new Set(
      sf32025.StoreItems
        .filter((item) => item.GiftDrop.AvatarItemDesc?.trim() && item.Prices.length === 0)
        .map((item) => String(item.PurchasableItemId))
    )
    expect([...hair].some((id) => freeCatalogIds.has(id))).toBe(true)
    expect([...facial].some((id) => freeCatalogIds.has(id))).toBe(true)
  })
})
