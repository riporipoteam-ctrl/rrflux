/**
 * Daily objective progress storage.
 *
 * One row per (account, group, index) in `objective_status` tracks the player's progress
 * on each daily objective. Group completion is tracked in `objective_group_status`.
 *
 * The client reports progress via `/api/objectives/v1/updateobjective`. When all objectives
 * in a group are complete, the group is marked completed and the reward can be claimed
 * via `/api/objectives/v1/completegroup`.
 */

export const OBJECTIVE_STATUS_SCHEMA_DDL = `
CREATE TABLE IF NOT EXISTS objective_status (
  account_id INTEGER NOT NULL,
  objective_group INTEGER NOT NULL,
  objective_index INTEGER NOT NULL,
  progress INTEGER NOT NULL DEFAULT 0,
  is_completed INTEGER NOT NULL DEFAULT 0,
  has_claimed_reward INTEGER NOT NULL DEFAULT 0,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (account_id, objective_group, objective_index)
);
CREATE TABLE IF NOT EXISTS objective_group_status (
  account_id INTEGER NOT NULL,
  objective_group INTEGER NOT NULL,
  is_completed INTEGER NOT NULL DEFAULT 0,
  cleared_at TEXT,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (account_id, objective_group)
);
`

interface D1Database {
	prepare(query: string): D1PreparedStatement
}

interface D1PreparedStatement {
	bind(...values: unknown[]): D1PreparedStatement
	all(): Promise<{ results: Record<string, unknown>[] }>
	run(): Promise<unknown>
	first(): Promise<Record<string, unknown> | null>
}

export interface ObjectiveStatus {
	group: number
	index: number
	progress: number
	isCompleted: boolean
	hasClaimedReward: boolean
}

export interface ObjectiveGroupStatus {
	group: number
	isCompleted: boolean
	clearedAt: string | null
}

/**
 * Record progress on a single objective. Upserts the row.
 */
export async function recordObjectiveProgress(
	db: D1Database,
	accountId: number,
	group: number,
	index: number,
	progress: number,
	isCompleted: boolean,
	hasClaimedReward: boolean
): Promise<void> {
	const now = new Date().toISOString()
	await db
		.prepare(
			`INSERT INTO objective_status (account_id, objective_group, objective_index, progress, is_completed, has_claimed_reward, updated_at)
			 VALUES (?, ?, ?, ?, ?, ?, ?)
			 ON CONFLICT (account_id, objective_group, objective_index)
			 DO UPDATE SET progress = excluded.progress, is_completed = excluded.is_completed, has_claimed_reward = excluded.has_claimed_reward, updated_at = excluded.updated_at`
		)
		.bind(accountId, group, index, progress, isCompleted ? 1 : 0, hasClaimedReward ? 1 : 0, now)
		.run()
}

/**
 * Get all objective statuses for a player.
 */
export async function getObjectiveStatuses(
	db: D1Database,
	accountId: number
): Promise<Map<string, ObjectiveStatus>> {
	const result = await db
		.prepare(`SELECT objective_group, objective_index, progress, is_completed, has_claimed_reward FROM objective_status WHERE account_id = ?`)
		.bind(accountId)
		.all()
	const map = new Map<string, ObjectiveStatus>()
	for (const row of result.results) {
		const key = `${row.objective_group}:${row.objective_index}`
		map.set(key, {
			group: Number(row.objective_group),
			index: Number(row.objective_index),
			progress: Number(row.progress),
			isCompleted: Number(row.is_completed) === 1,
			hasClaimedReward: Number(row.has_claimed_reward) === 1,
		})
	}
	return map
}

/**
 * Get group completion statuses for a player.
 */
export async function getObjectiveGroupStatuses(
	db: D1Database,
	accountId: number
): Promise<Map<number, ObjectiveGroupStatus>> {
	const result = await db
		.prepare(`SELECT objective_group, is_completed, cleared_at FROM objective_group_status WHERE account_id = ?`)
		.bind(accountId)
		.all()
	const map = new Map<number, ObjectiveGroupStatus>()
	for (const row of result.results) {
		map.set(Number(row.objective_group), {
			group: Number(row.objective_group),
			isCompleted: Number(row.is_completed) === 1,
			clearedAt: (row.cleared_at as string) ?? null,
		})
	}
	return map
}

/**
 * Mark a group as completed. Returns true if this was the first completion
 * (i.e., the reward should be granted).
 */
export async function completeObjectiveGroup(
	db: D1Database,
	accountId: number,
	group: number
): Promise<boolean> {
	const now = new Date().toISOString()
	// Check if already completed
	const existing = await db
		.prepare(`SELECT is_completed FROM objective_group_status WHERE account_id = ? AND objective_group = ?`)
		.bind(accountId, group)
		.first()
	if (existing && Number(existing.is_completed) === 1) {
		return false // Already completed, no reward
	}
	await db
		.prepare(
			`INSERT INTO objective_group_status (account_id, objective_group, is_completed, cleared_at, updated_at)
			 VALUES (?, ?, 1, ?, ?)
			 ON CONFLICT (account_id, objective_group)
			 DO UPDATE SET is_completed = 1, cleared_at = excluded.cleared_at, updated_at = excluded.updated_at`
		)
		.bind(accountId, group, now, now)
		.run()
	return true
}

/**
 * Clear a group's progress (for cleargroup endpoint).
 */
export async function clearObjectiveGroup(
	db: D1Database,
	accountId: number,
	group: number
): Promise<void> {
	await db
		.prepare(`DELETE FROM objective_status WHERE account_id = ? AND objective_group = ?`)
		.bind(accountId, group)
		.run()
	await db
		.prepare(`DELETE FROM objective_group_status WHERE account_id = ? AND objective_group = ?`)
		.bind(accountId, group)
		.run()
}
