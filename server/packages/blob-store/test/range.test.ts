import { describe, expect, test } from 'vitest'

import { parseRangeHeader } from '../src/store'

describe('parseRangeHeader', () => {
	const size = 1000

	test('no header or non-bytes unit → null (ignored, plain 200)', () => {
		expect(parseRangeHeader(null, size)).toBeNull()
		expect(parseRangeHeader(undefined, size)).toBeNull()
		expect(parseRangeHeader('', size)).toBeNull()
		expect(parseRangeHeader('items=0-1', size)).toBeNull()
	})

	test('offset-length ranges', () => {
		expect(parseRangeHeader('bytes=0-99', size)).toEqual({ offset: 0, length: 100 })
		expect(parseRangeHeader('bytes=500-', size)).toEqual({ offset: 500, length: 500 })
		expect(parseRangeHeader('bytes=0-', size)).toEqual({ offset: 0, length: 1000 })
		expect(parseRangeHeader('bytes=999-', size)).toEqual({ offset: 999, length: 1 })
		// End clamped to the blob.
		expect(parseRangeHeader('bytes=200-9999', size)).toEqual({ offset: 200, length: 800 })
	})

	test('suffix ranges', () => {
		expect(parseRangeHeader('bytes=-100', size)).toEqual({ offset: 900, length: 100 })
		// Longer than the blob → the whole blob.
		expect(parseRangeHeader('bytes=-9999', size)).toEqual({ offset: 0, length: 1000 })
	})

	test('malformed or unsatisfiable `bytes=` values resolve to the whole object (206)', () => {
		const whole = { offset: 0, length: 1000 }
		expect(parseRangeHeader('bytes=abc', size)).toEqual(whole)
		expect(parseRangeHeader('bytes=500-200', size)).toEqual(whole)
		expect(parseRangeHeader('bytes=1000-', size)).toEqual(whole) // starts past the end
		expect(parseRangeHeader('bytes=-0', size)).toEqual(whole)
		expect(parseRangeHeader('bytes=0-1,2-3', size)).toEqual(whole) // multi-range
		expect(parseRangeHeader('bytes=', size)).toEqual(whole)
	})
})
