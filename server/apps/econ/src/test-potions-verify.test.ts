import { describe, test, expect } from 'vitest'
import { buildRecCenterStorefront } from './storefront-rotation'

describe('potions and films in rec center storefront', () => {
	test('all 5 potions and 6 films always visible at token prices', () => {
		const store = buildRecCenterStorefront() as {
			StoreItems: Array<{ PurchasableItemId: number; Prices: Array<{ CurrencyType: number; Price: number }>; GiftDrop: { FriendlyName: string; ConsumableItemDesc: string } }>
		}
		const byId = new Map(store.StoreItems.map(i => [i.PurchasableItemId, i]))
		const expected: Array<[number, string, number]> = [
			[2176, 'High Five Potion (Golden)', 30],
			[2179, 'High Five Potion (Magic)', 30],
			[577, 'High Five Potion (Laser)', 30],
			[576, 'High Five Potion (Explosive)', 30],
			[1004, 'High Five Potion (Whip Crack)', 30],
			[2168, 'Film (Black & White)', 20],
			[2169, 'Film (Dawn)', 20],
			[2174, 'Film (Sepia)', 20],
			[585, 'Film (Jumbotron)', 20],
			[587, 'Film (Ghost Beard)', 20],
			[1092, 'Film (Dracula)', 20],
		]
		for (const [id, name, price] of expected) {
			const item = byId.get(id)
			expect(item, `${name} (${id}) missing from rec center storefront`).toBeDefined()
			expect(item!.GiftDrop.FriendlyName).toBe(name)
			expect(item!.GiftDrop.ConsumableItemDesc).not.toBe('')
			const tokenPrice = item!.Prices.find(p => p.CurrencyType === 2)
			expect(tokenPrice, `${name} has no token price`).toBeDefined()
			expect(tokenPrice!.Price).toBe(price)
		}
		// every listed potion/film must be buyable with tokens
		for (const i of store.StoreItems) {
			if (i.GiftDrop.ConsumableItemDesc !== '' && /potion|film/i.test(i.GiftDrop.FriendlyName)) {
				expect(i.Prices.some(p => p.CurrencyType === 2), `${i.GiftDrop.FriendlyName} not buyable with tokens`).toBe(true)
			}
		}
		console.log('total storefront items:', store.StoreItems.length)
	})
})
