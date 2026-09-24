import { cloudflareTest } from '@cloudflare/vitest-pool-workers'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { defineConfig } from 'vitest/config'

export default defineConfig({
	plugins: [
		cloudflareTest({
			wrangler: { configPath: `${__dirname}/wrangler.jsonc` },
			miniflare: {
				bindings: {
					ENVIRONMENT: 'VITEST',
					// Signing is off in `wrangler.jsonc`; turn it on here so the `?sig=p1`
					// path stays covered. The flag-off behaviour is tested by calling the
					// app directly with an overridden env.
					IMG_SIGNING_ENABLED: true,
				},
				serviceBindings: {
					FIRESTORE_TEST_BACKEND: 'fake-firestore',
				},
				workers: [
					// In-memory fake of the Firestore REST surface `@repo/blob-store` uses
					// (see packages/blob-store/test/fake-firestore.worker.js). The blob store
					// routes every Firestore call through it and skips the OAuth2 flow when
					// the `FIRESTORE_TEST_BACKEND` service binding is present, so tests
					// exercise the real REST layer (batchWrite / document GET / deletes,
					// chunk layout, base64 payloads) with no network access.
					{
						name: 'fake-firestore',
						modules: true,
						script: readFileSync(
						resolve(__dirname, '../../packages/blob-store/test/fake-firestore.worker.js'),
						'utf8',
					),
						compatibilityDate: '2026-06-16',
					},
				],
			},
		}),
	],
})
