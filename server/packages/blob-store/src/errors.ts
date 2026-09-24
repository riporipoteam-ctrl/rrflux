/**
 * Thrown when a blob operation is attempted without the store being configured.
 *
 * Configuration means a service-account JSON secret (`FIRESTORE_SA_JSON`) — or, in
 * tests, the `FIRESTORE_TEST_BACKEND` service binding. Call sites map this to a 503
 * on writes (uploads) and to the existing static-asset fallback on reads, so a worker
 * deployed without the secret fails loudly instead of silently dropping bytes.
 */
export class BlobStoreNotConfiguredError extends Error {
	constructor() {
		super(
			'Blob storage is not configured: set the FIRESTORE_SA_JSON secret ' +
				'(see `wrangler secret put FIRESTORE_SA_JSON`)'
		)
		this.name = 'BlobStoreNotConfiguredError'
	}
}
