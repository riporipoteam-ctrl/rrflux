import { cloudflareTest } from '@cloudflare/vitest-pool-workers'
import { defineConfig } from 'vitest/config'

export default defineConfig({
	plugins: [
		cloudflareTest({
			wrangler: { configPath: `${__dirname}/wrangler.jsonc` },
			remoteBindings: false,
			miniflare: {
				bindings: {
					ENVIRONMENT: 'VITEST',
					HF_TOKEN: 'test-hf-token',
				},
			},
		}),
	],
})
