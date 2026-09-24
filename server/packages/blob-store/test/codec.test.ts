import { describe, expect, test } from 'vitest'

import {
	base64Decode,
	base64Encode,
	base64UrlDecode,
	base64UrlEncode,
	bindingAndKeyFor,
	chunkCount,
	chunksForRange,
	docIdFor,
	joinChunks,
	splitChunks,
	CHUNK_SIZE,
} from '../src/codec'

function patternBytes(length: number): Uint8Array {
	const out = new Uint8Array(length)
	for (let i = 0; i < length; i++) out[i] = (i * 31 + 7) % 256
	return out
}

describe('base64 helpers', () => {
	test('base64url encode never contains `/`, `+` or `=`', () => {
		for (let i = 0; i < 200; i++) {
			const bytes = patternBytes(i * 13 + 1)
			const s = base64UrlEncode(bytes)
			expect(s).not.toMatch(/[/+=]/)
			expect(base64UrlDecode(s)).toEqual(bytes)
		}
	})

	test('base64 chunk payload of a full chunk stays under the 1 MiB doc limit', () => {
		const encoded = base64Encode(new Uint8Array(CHUNK_SIZE))
		expect(encoded.length).toBeLessThan(1024 * 1024)
	})

	test('base64 round-trips binary data', () => {
		const bytes = patternBytes(5000)
		expect(base64Decode(base64Encode(bytes))).toEqual(bytes)
	})
})

describe('docIdFor / bindingAndKeyFor', () => {
	test('round-trips bindings and keys, including slashes and unicode', () => {
		const cases: Array<[string, string]> = [
			['IMAGES', 'sharecamera/2026-09-22/123e4567-e89b-12d3-a456-426614174000.jpg'],
			['CDN_ASSETS', 'room/2026-02-03/cached'],
			['IMAGES', 'avatar-item/2026-08-26/abc-design.bin'],
			['CDN_ASSETS', 'invention/2026-07-12/lamp.inv'],
			['IMAGES', 'emoji-🙂-key.png'],
			['IMAGES', ''],
		]
		for (const [binding, key] of cases) {
			const id = docIdFor(binding as 'IMAGES', key)
			expect(id).not.toContain('/')
			expect(bindingAndKeyFor(id)).toEqual({ binding, key })
		}
	})

	test('is deterministic', () => {
		expect(docIdFor('IMAGES', 'a/b.jpg')).toBe(docIdFor('IMAGES', 'a/b.jpg'))
		expect(docIdFor('IMAGES', 'a/b.jpg')).not.toBe(docIdFor('CDN_ASSETS', 'a/b.jpg'))
	})

	test('rejects garbage', () => {
		expect(() => bindingAndKeyFor('!!!not-base64!!!')).toThrow()
		expect(() => bindingAndKeyFor(base64UrlEncode(new TextEncoder().encode('no-newline')))).toThrow()
	})
})

describe('chunkCount', () => {
	test('counts chunks', () => {
		expect(chunkCount(0)).toBe(0)
		expect(chunkCount(1)).toBe(1)
		expect(chunkCount(CHUNK_SIZE)).toBe(1)
		expect(chunkCount(CHUNK_SIZE + 1)).toBe(2)
		expect(chunkCount(3 * CHUNK_SIZE)).toBe(3)
		// 64 MiB (the API upload cap) fits in one batchWrite (94 chunks + meta ≤ 500).
		expect(chunkCount(64 * 1024 * 1024)).toBe(94)
	})
})

describe('splitChunks / joinChunks round-trip', () => {
	for (const size of [0, 1, 100, CHUNK_SIZE - 1, CHUNK_SIZE, CHUNK_SIZE + 1, 2 * CHUNK_SIZE + 123]) {
		test(`round-trips ${size} bytes`, () => {
			const data = patternBytes(size)
			const chunks = splitChunks(data)
			expect(chunks).toHaveLength(chunkCount(size))
			for (const c of chunks) expect(c.length).toBeLessThanOrEqual(CHUNK_SIZE)
			const joined = joinChunks(chunks, 0, size)
			expect(joined).toEqual(data)
		})
	}
})

describe('chunksForRange', () => {
	const size = 2 * CHUNK_SIZE + 100 // 3 chunks

	test('whole blob', () => {
		expect(chunksForRange(size, 0, size)).toEqual({
			first: 0,
			last: 2,
			skipInFirst: 0,
			takeTotal: size,
		})
	})

	test('range inside one chunk', () => {
		expect(chunksForRange(size, 10, 100)).toEqual({
			first: 0,
			last: 0,
			skipInFirst: 10,
			takeTotal: 100,
		})
	})

	test('range spanning a chunk boundary', () => {
		const r = chunksForRange(size, CHUNK_SIZE - 50, 200)!
		expect(r.first).toBe(0)
		expect(r.last).toBe(1)
		expect(r.skipInFirst).toBe(CHUNK_SIZE - 50)
		expect(r.takeTotal).toBe(200)
		// The joined window covers exactly the requested bytes.
		const data = patternBytes(size)
		const chunks = splitChunks(data)
		const joined = joinChunks(chunks.slice(r.first, r.last + 1), r.skipInFirst, r.takeTotal)
		expect(joined).toEqual(data.subarray(CHUNK_SIZE - 50, CHUNK_SIZE + 150))
	})

	test('range clamped to the end of the blob', () => {
		const r = chunksForRange(size, size - 10, 10_000)!
		expect(r.takeTotal).toBe(10)
		expect(r.last).toBe(2)
	})

	test('empty ranges yield null', () => {
		expect(chunksForRange(size, 100, 0)).toBeNull()
		expect(chunksForRange(size, size + 10, 100)).toBeNull()
		expect(chunksForRange(0, 0, 100)).toBeNull()
	})
})
