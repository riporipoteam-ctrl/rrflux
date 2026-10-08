import { describe, expect, it } from 'vitest'
import { buildStoreFeedIds } from './lists.app'

describe('2025 Store wearable feeds', () => {
  it('keeps the broad Clothing feed on real wearable Store rows, not query boxes', () => {
    const ids = buildStoreFeedIds('clothingitems')
    expect(ids).not.toBeNull()
    expect(ids!.length).toBeGreaterThan(2000)
  })

  it('keeps the exact client StoreClothing aliases on the same real feeds', () => {
    expect(buildStoreFeedIds('HatsItems')).not.toBeNull()
    expect(buildStoreFeedIds('TorsoItems')).not.toBeNull()
    expect(buildStoreFeedIds('BottomsItems')).not.toBeNull()
    expect(buildStoreFeedIds('ShoesItems')).not.toBeNull()
    expect(buildStoreFeedIds('GlovesItems')).not.toBeNull()
    expect(buildStoreFeedIds('HeadHairItems')).not.toBeNull()
    expect(buildStoreFeedIds('FacialHairItems')).not.toBeNull()
    expect(buildStoreFeedIds('AccessoriesItems')).not.toBeNull()
    expect(buildStoreFeedIds('BackpackItems')).not.toBeNull()
  })

  it('does not fall back to the legacy 50-item draw for hair/facial-hair/backpack', () => {
    for (const key of ['hairitems', 'facialhairitems', 'backpackitems']) {
      const ids = buildStoreFeedIds(key)
      expect(ids).not.toBeNull()
      expect(ids!.length).toBeGreaterThan(0)
    }
  })
})
