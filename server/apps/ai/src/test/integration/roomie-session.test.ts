import { describe, expect, it } from 'vitest'
import { createRoomieClientSecret, verifyRoomieClientSecret } from '../../roomie-session'

describe('Roomie session credentials', () => {
	it('binds the credential to one account/session and expires it', async () => {
		const token = await createRoomieClientSecret('test-secret', 42, 'session-a', {
			model: 'ignored-client-model',
			instructions: 'Be cheerful',
			voice: 'sage',
		}, 1_000)

		expect(await verifyRoomieClientSecret('test-secret', token, 1_001)).toMatchObject({
			aud: 'roomie-realtime',
			sub: 42,
			sid: 'session-a',
			model: 'ignored-client-model',
			instructions: 'Be cheerful',
			voice: 'sage',
		})
		expect(await verifyRoomieClientSecret('wrong-secret', token, 1_001)).toBeNull()
		expect(await verifyRoomieClientSecret('test-secret', token, 1_900)).toBeNull()
	})

	it('rejects malformed and tampered credentials', async () => {
		const token = await createRoomieClientSecret('test-secret', 42, 'session-a', {}, 1_000)
		expect(await verifyRoomieClientSecret('test-secret', 'bad')).toBeNull()
		expect(await verifyRoomieClientSecret('test-secret', token + 'x', 1_001)).toBeNull()
	})
})
