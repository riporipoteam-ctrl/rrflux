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
