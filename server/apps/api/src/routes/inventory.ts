import { Hono } from 'hono'
import { describeRoute } from 'hono-openapi'

import { AUTHED, json, JsonArray, UNAUTHORIZED_RESPONSE } from '../openapi'
import { proxyTo } from '../service-proxy'

import type { App } from '../context'

// ---- Inventory -------------------------------------------------------------
// Some client builds probe the API host first; forward these paths to Econ, which owns the
// account-scoped inventory rows, instead of acknowledging the request with an empty list.
export const inventoryRoutes = new Hono<App>({ strict: false })
	.get(
		'/api/consumables/v1/all',
		describeRoute({
			tags: ['Inventory'],
			summary: 'All consumable definitions',
			description:
				'Forwards the public consumable definitions to Econ, including the Unity model configuration used by the 2025 client.',
			responses: { 200: json(JsonArray, 'Consumable item definitions') },
		}),
		proxyTo((env) => env.ECON)
	)
	.post(
		'/api/consumables/v1/query/bulk',
		describeRoute({
			tags: ['Inventory'],
			summary: 'Resolve consumable definitions in bulk',
			description: 'Forwards a consumable descriptor lookup to Econ.',
			responses: { 200: json(JsonArray, 'Matching consumable item definitions') },
		}),
		proxyTo((env) => env.ECON)
	)
	.get(
		'/api/equipment/v2/getUnlocked',
		describeRoute({
			tags: ['Inventory'],
			summary: 'Unlocked equipment',
				description:
					'Forwards to the `econ` worker, which owns this account’s unlocked equipment inventory.',
				security: AUTHED,
				responses: {
					200: json(JsonArray, 'The caller’s unlocked equipment'),
					401: UNAUTHORIZED_RESPONSE,
				},
			}),
			proxyTo((env) => env.ECON)
	)
	.get(
		'/api/consumables/v2/getUnlocked',
		describeRoute({
			tags: ['Inventory'],
			summary: 'Unlocked consumables',
				description:
					'Forwards to the `econ` worker, which owns this account’s unlocked consumables.',
			security: AUTHED,
			responses: {
				200: json(JsonArray, 'An empty list'),
				401: UNAUTHORIZED_RESPONSE,
			},
		}),
			proxyTo((env) => env.ECON)
		)
