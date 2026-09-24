import { describe, test, expect } from 'vitest'
import { buildRecCenterStorefront } from './storefront-rotation'

describe('food in rec center storefront', () => {
	test('includes all 19 food items', () => {
		const store = buildRecCenterStorefront() as {
			StoreItems: Array<{ PurchasableItemId: number; GiftDrop: { FriendlyName: string } }>
		}
		const ids = new Set(store.StoreItems.map(i => i.PurchasableItemId))
		const expectedFoodIds = [2181,2182,2183,2184,2185,2186,2187,2188,2189,2190,2191,2192,2193,2194,2196,2197,2198,2199,2200]
		for (const id of expectedFoodIds) {
			expect(ids.has(id), `food item ${id} missing`).toBe(true)
		}
		console.log('Total items in storefront:', store.StoreItems.length)
		const food = store.StoreItems.filter(i => expectedFoodIds.includes(i.PurchasableItemId))
		console.log('Food items found:', food.length)
		for (const f of food) {
			console.log(`  ${f.PurchasableItemId}: ${f.GiftDrop.FriendlyName}`)
		}
	})
})
