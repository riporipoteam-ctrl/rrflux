/**
 * The rec center storefront 24-hour rotation.
 *
 * Every 24 hours (UTC midnight boundary), the storefront serves a fresh set of 5 items:
 * 2 avatar items, 2 consumables (potions, food, film), and 1 gift box — a mix the
 * player can actually buy and use. 3 of the 5 are exclusive to Rec Room+ subscribers
 * (SubscribersOnly: true).
 *
 * The selection is a pure function of the day index — the same 5 items for every player,
 * recomputed identically by every isolate and every request. A seeded PRNG picks from the
 * item pools; nothing calls Math.random().
 *
 * The rotation covers the broken timer Armin reported ("New items in: time has passed!"):
 * NextUpdate is always the next UTC midnight, never a stale past date.
 */

import sf1 from '../static/storefronts/sf1.json'
import sf2 from '../static/storefronts/sf2.json'
import consumables from '../static/db/consumables.json'
import sf100 from '../static/storefronts/sf100.json'
import sf101 from '../static/storefronts/sf101.json'
import sf102 from '../static/storefronts/sf102.json'
import sf103 from '../static/storefronts/sf103.json'
import sf300 from '../static/storefronts/sf300.json'
import sf400 from '../static/storefronts/sf400.json'
import sf401 from '../static/storefronts/sf401.json'
import sf402 from '../static/storefronts/sf402.json'
import sf403 from '../static/storefronts/sf403.json'
import sf404 from '../static/storefronts/sf404.json'
import sf405 from '../static/storefronts/sf405.json'
import sf406 from '../static/storefronts/sf406.json'
import sf500 from '../static/storefronts/sf500.json'

/** Milliseconds in a day. */
const DAY_MS = 24 * 60 * 60 * 1000

/** How many items the rec center storefront shows. */
const STOREFRONT_SIZE = 5

/** How many of the 5 are Plus-exclusive. */
const PLUS_EXCLUSIVE_COUNT = 3

/** How many of the 5 are consumables (potions, food, film). */
const CONSUMABLE_COUNT = 2

/** How many of the 5 are gift boxes. */
const BOX_COUNT = 1

/**
 * Simple seeded PRNG (mulberry32). Deterministic per day index.
 */
function mulberry32(seed: number): () => number {
	let a = seed >>> 0
	return () => {
		a |= 0
		a = (a + 0x6d2b79f5) | 0
		let t = Math.imul(a ^ (a >>> 15), 1 | a)
		t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t
		return ((t ^ (t >>> 14)) >>> 0) / 4294967296
	}
}

interface StoreItem {
	PurchasableItemId: number
	GiftDrop: Record<string, unknown>
	Prices: unknown
	Type: number
}

/**
 * A storefront item is a consumable when its GiftDrop names a ConsumableItemDesc —
 * the same check the purchase path (grantGiftDrop) uses to grant it into the
 * consumable table instead of the avatar inventory.
 */
function isConsumable(item: StoreItem): boolean {
	const desc = (item.GiftDrop as Record<string, unknown>)?.ConsumableItemDesc
	return typeof desc === 'string' && desc !== ''
}

/**
 * A storefront item is a gift box when "box" appears as a standalone word in its
 * name. The word boundary matters: "Boxing Gloves" and "Boombox" are avatar items,
 * not boxes — the old /box/i test swallowed them.
 */
function isGiftBox(item: StoreItem): boolean {
	const name = String((item.GiftDrop as Record<string, unknown>)?.FriendlyName ?? '')
	return /\bbox\b/i.test(name)
}

interface ItemPools {
	avatar: StoreItem[]
	consumable: StoreItem[]
	boxes: StoreItem[]
}

/**
 * Build the three item pools from the captured storefront catalogs, deduplicated by
 * PurchasableItemId. Avatar wearables come from sf1/sf2; consumables (potions, food,
 * film) from sf100–sf103, sf300, sf400–sf406, sf500; gift boxes from sf2.
 */
function buildPools(): ItemPools {
	const avatar: StoreItem[] = []
	const consumable: StoreItem[] = []
	const boxes: StoreItem[] = []
	const seen = new Set<number>()

	const catalogs = [
		// The consumables database (food, potions, film, KO icons, gift boxes) FIRST:
		// it is the authoritative source for consumable PurchasableItemIds and wins ID
		// collisions against the storefront captures. (sf2.json is a stale capture that
		// reuses 2174/2196/2197 for avatar items — Wolf Amulet, Gladiator Cape/Helmet —
		// while sf3, sf3-2025 and the consumables DB all agree those IDs are Film
		// (Sepia), Red Velvet Cake and Root Beer. First-wins dedup below would otherwise
		// drop the real consumables from the pool entirely.)
		{ StoreItems: consumables as unknown as StoreItem[] },
		sf1,
		sf2,
		sf100,
		sf101,
		sf102,
		sf103,
		sf300,
		sf400,
		sf401,
		sf402,
		sf403,
		sf404,
		sf405,
		sf406,
		sf500,
	] as Array<{ StoreItems: StoreItem[] }>

	for (const sf of catalogs) {
		for (const si of sf.StoreItems) {
			if (seen.has(si.PurchasableItemId)) continue
			seen.add(si.PurchasableItemId)
			if (isGiftBox(si)) boxes.push(si)
			else if (isConsumable(si)) consumable.push(si)
			else avatar.push(si)
		}
	}
	return { avatar, consumable, boxes }
}

const POOLS = buildPools()

/**
 * PurchasableItemIds of food consumables in the consumables database.
 * These are always shown in the Rec Center storefront (not part of the daily
 * rotation) so players can reliably buy food.
 */
const FOOD_ITEM_IDS = new Set([
	2181, // 87 Flavor Cake
	2182, // Assorted Donuts
	2183, // Fancy Bubbly
	2184, // Candy Apples
	2185, // Celebration Cake
	2186, // Cheese Pizza
	2187, // Chocolate Cake
	2188, // Chocolate Donuts
	2189, // Assorted Chocolates
	2190, // Glazed Donuts
	2191, // Hawaiian Pizza
	2192, // Tray of Lattes
	2193, // Pepperoni Pizza
	2194, // Popcorn
	2196, // Red Velvet Cake
	2197, // Root Beer
	2198, // Salted Pretzels
	2199, // Supreme Pizza
	2200, // Sushi
])

/**
 * Get all food items from the consumable pool, sorted by PurchasableItemId.
 */
function getFoodItems(): StoreItem[] {
	return POOLS.consumable
		.filter((item) => FOOD_ITEM_IDS.has(item.PurchasableItemId))
		.sort((a, b) => a.PurchasableItemId - b.PurchasableItemId)
}

/**
 * PurchasableItemIds of potion consumables in the consumables database.
 * These are always shown in the Rec Center storefront (not part of the daily
 * rotation) so players can reliably buy potions.
 */
const POTION_ITEM_IDS = new Set([
	2176, // High Five Potion (Golden)
	2179, // High Five Potion (Magic)
	577, // High Five Potion (Laser) — captured in sf1, priced in LaserTagTickets
	576, // High Five Potion (Explosive) — Isle of Lost Skulls quest store (sf100), priced in LostSkullsGold
	1004, // High Five Potion (Whip Crack) — Rise of Jumbotron quest store (sf101), priced in DraculaSilver
])

/**
 * PurchasableItemIds of camera film consumables in the consumables database.
 * Always shown in the Rec Center storefront so players can reliably buy film
 * for the in-game camera.
 */
const FILM_ITEM_IDS = new Set([
	2168, // Film (Black & White)
	2169, // Film (Dawn)
	2174, // Film (Sepia)
	585, // Film (Jumbotron) — captured in sf1, priced in LaserTagTickets
	587, // Film (Ghost Beard) — Isle of Lost Skulls quest store (sf100), priced in LostSkullsGold
	1092, // Film (Dracula) — Rise of Jumbotron quest store (sf101), priced in DraculaSilver
])

/**
 * PurchasableItemIds of KO icon consumables in the consumables database.
 * Always shown in the Rec Center storefront so players can reliably buy them.
 */
const KO_ICON_ITEM_IDS = new Set([
	2201, // KO Icon - Bear Claw
	2206, // KO Icon - Grenade
	2208, // KO Icon - Sword & Shield
	2209, // KO Icon - Winged Skull
	2210, // KO Icon - Star Power
	2211, // KO Icon - Tire Track
])

/**
 * Get all potion, film, and KO icon items from the consumable pool,
 * sorted by PurchasableItemId.
 *
 * Items captured with a per-activity price (LaserTagTickets, LostSkullsGold,
 * DraculaSilver — currencies no player can currently earn) are re-priced in
 * RecCenterTokens here, matching the consumables.json pricing (potions 30,
 * films 20). Without this they would list in the Rec Center at a price nobody
 * can pay. The quest stores keep their original captures untouched.
 */
function getAlwaysVisibleConsumables(): StoreItem[] {
	return POOLS.consumable
		.filter(
			(item) =>
				POTION_ITEM_IDS.has(item.PurchasableItemId) ||
				FILM_ITEM_IDS.has(item.PurchasableItemId) ||
				KO_ICON_ITEM_IDS.has(item.PurchasableItemId)
		)
		.map(withTokenPrice)
		.sort((a, b) => a.PurchasableItemId - b.PurchasableItemId)
}

/**
 * Recenter a storefront item on RecCenterTokens when its captured price is in a
 * currency the buyer can't hold a balance in practice (see getAlwaysVisibleConsumables).
 * Items already priced in tokens pass through untouched. Returns the item itself when
 * no change was needed, otherwise a copy with only Prices replaced — the purchase
 * path reads the same built storefront, so browse and buy stay consistent.
 */
function withTokenPrice(item: StoreItem): StoreItem {
	const prices = Array.isArray(item.Prices) ? item.Prices : []
	if (
		prices.some(
			(p) => (p as { CurrencyType?: unknown } | null)?.CurrencyType === 2
		)
	) {
		return item
	}
	const tokenPrice = POTION_ITEM_IDS.has(item.PurchasableItemId)
		? 30
		: FILM_ITEM_IDS.has(item.PurchasableItemId)
			? 20
			: null
	if (tokenPrice === null) return item
	return {
		...item,
		Prices: [{ CurrencyType: 2, Price: tokenPrice, StorefrontSaleData: null, Type: 0 }],
	}
}

/**
 * Re-price potion/film consumables captured in dead activity currencies
 * (LaserTagTickets, LostSkullsGold, DraculaSilver — no live faucet) to
 * RecCenterTokens, so they are actually buyable in their native quest stores.
 * Items already priced in tokens pass through untouched, as do non-potion/film
 * items. Apply to any served catalog AND to the purchase path (`loadStorefront`)
 * so browse and buy stay consistent.
 *
 * Generic over a minimal structural item shape because `econ.app.ts` defines
 * its own richer `StoreItem` — this only needs the id and the price list.
 */
export function repriceDeadCurrencyItems<
	T extends { PurchasableItemId: number; Prices: unknown },
>(items: T[]): T[] {
	return items.map((item) => {
		const prices = Array.isArray(item.Prices) ? item.Prices : []
		if (
			prices.some(
				(p) => (p as { CurrencyType?: unknown } | null)?.CurrencyType === 2
			)
		) {
			return item
		}
		const tokenPrice = POTION_ITEM_IDS.has(item.PurchasableItemId)
			? 30
			: FILM_ITEM_IDS.has(item.PurchasableItemId)
				? 20
				: null
		if (tokenPrice === null) return item
		return {
			...item,
			Prices: [
				{ CurrencyType: 2, Price: tokenPrice, StorefrontSaleData: null, Type: 0 },
			],
		}
	})
}

/**
 * Day index since epoch. Day 0 = 2026-01-01 UTC.
 */
function dayIndex(nowMs: number): number {
	const epoch = Date.UTC(2026, 0, 1)
	return Math.floor((nowMs - epoch) / DAY_MS)
}

/**
 * Deterministically pick `count` items from a pool using the seeded RNG.
 */
function pickFromPool(pool: StoreItem[], count: number, rng: () => number): StoreItem[] {
	if (pool.length === 0) return []
	const shuffled = [...pool]
	for (let i = shuffled.length - 1; i > 0; i--) {
		const j = Math.floor(rng() * (i + 1))
		;[shuffled[i], shuffled[j]] = [shuffled[j], shuffled[i]]
	}
	return shuffled.slice(0, Math.min(count, shuffled.length))
}

/**
 * Generate the 5 items for the given day index: 2 avatar items, 2 consumables,
 * 1 gift box. Returns { items, nextUpdateMs }.
 */
export function buildStorefrontRotation(nowMs: number = Date.now()): {
	items: StoreItem[]
	nextUpdateMs: number
} {
	const day = dayIndex(nowMs)
	const rng = mulberry32(day * 2654435761)

	// 2 avatar + 2 consumable + 1 box = 5 items.
	// Consumable picks go through withTokenPrice so a rotation-drawn quest-currency
	// item (e.g. an Explosive potion) lists at a token price players can actually pay.
	const selected: StoreItem[] = [
		...pickFromPool(POOLS.avatar, STOREFRONT_SIZE - CONSUMABLE_COUNT - BOX_COUNT, rng),
		...pickFromPool(POOLS.consumable, CONSUMABLE_COUNT, rng).map(withTokenPrice),
		...pickFromPool(POOLS.boxes, BOX_COUNT, rng),
	]

	const items = selected.map((item, idx) => {
		const isPlusExclusive = idx >= selected.length - PLUS_EXCLUSIVE_COUNT
		if (!isPlusExclusive) return item
		// Mark as Plus-exclusive
		return {
			...item,
			GiftDrop: {
				...item.GiftDrop,
				SubscribersOnly: true,
			},
		}
	})

	// Next update: start of next day UTC
	const epoch = Date.UTC(2026, 0, 1)
	const nextUpdateMs = epoch + (day + 1) * DAY_MS

	return { items, nextUpdateMs }
}

/**
 * Build the full storefront JSON for the rec center (storefront id 2).
 * Matches the sf{id}.json shape the client expects.
 *
 * Includes the 5-item daily rotation plus all 19 food consumables and the
 * potion/film/KO-icon consumables (always shown, not rotated) so players can
 * reliably buy them.
 */
export function buildRecCenterStorefront(nowMs: number = Date.now()): Record<string, unknown> {
	const { items, nextUpdateMs } = buildStorefrontRotation(nowMs)
	// Append food items (always available, not part of rotation)
	const foodItems = getFoodItems()
	// Append potions, film, and KO icons (always available, not part of rotation)
	const alwaysVisible = getAlwaysVisibleConsumables()
	// Dedupe: a rotation pick can also be an always-visible item (e.g. the seeded
	// consumable/box roll lands on Supreme Pizza). The client would render it twice.
	const seen = new Set(items.map((i) => i.PurchasableItemId))
	const takeNew = (i: StoreItem): boolean => {
		if (seen.has(i.PurchasableItemId)) return false
		seen.add(i.PurchasableItemId)
		return true
	}
	const allItems = [...items, ...foodItems.filter(takeNew), ...alwaysVisible.filter(takeNew)]
	return {
		StoreItems: allItems,
		SubscriberDiscountPercent: 0,
		StorefrontType: 2,
		NextUpdate: new Date(nextUpdateMs).toISOString(),
		NewUntil: null,
	}
}
