/**
 * The environment surface the blob store needs. Apps intersect this into their
 * own `Env` (`SharedHonoEnv & BlobStoreEnv & { … }`).
 */
export interface BlobStoreEnv {
	/**
	 * Full service-account JSON, as a Worker SECRET (set with
	 * `wrangler secret put FIRESTORE_SA_JSON` at deploy time — never a wrangler
	 * var, never committed). Absent → every blob operation throws
	 * {@link BlobStoreNotConfiguredError}.
	 */
	FIRESTORE_SA_JSON?: string
	/** Firestore project id; defaults to `flux-544a6` when unset. */
	FIRESTORE_PROJECT_ID?: string
	/**
	 * TEST-ONLY service binding implementing the Firestore REST surface in memory
	 * (`packages/blob-store/test/fake-firestore.worker.js`). When bound, all REST
	 * traffic is routed through it and the OAuth2 flow is skipped. Never bound in
	 * production — it exists so integration tests exercise the real REST layer
	 * without network access.
	 */
	FIRESTORE_TEST_BACKEND?: Fetcher
}
