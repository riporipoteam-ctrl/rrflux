/**
 * Vote-to-kick ballots on the shared `recflare` D1 database.
 *
 * One row per vote cast: who was voted on, in which game session, by whom, and which way.
 * Append-only like the `report` and `warning` tables (see reports-db.ts), so a vote is a
 * record of what happened rather than a tally that gets rewritten — the tally is computed
 * from the rows.
 *
 * The rows outlive the vote. Nothing deletes them when a session ends, and nothing needs to:
 * a ballot is keyed to its `game_session_id`, and instance ids are not reused, so an old
 * session's votes can never be counted into a new one's.
 *
 * The `api` worker owns this schema/migration (migrations/0023_room_vote.sql, applied under
 * its own `migrations_table` so it doesn't clash with the other workers' migrations that
 * share the database).
 */

/** Schema DDL (mirror of migrations/0023_room_vote.sql). */
export const SCHEMA_DDL: string[] = [
	`CREATE TABLE IF NOT EXISTS room_vote (
		id INTEGER PRIMARY KEY AUTOINCREMENT,
		game_session_id INTEGER NOT NULL,
		player_id INTEGER NOT NULL,
		response INTEGER NOT NULL,
		voter_id INTEGER NOT NULL,
		voted_at TEXT NOT NULL
	)`,
	// The tally's own query: every ballot cast against one player in one session.
	`CREATE INDEX IF NOT EXISTS idx_room_vote_subject ON room_vote (game_session_id, player_id)`,
]

/** One cast ballot. `response` is 1 for yes (kick them) and 0 for no. */
export interface RoomVoteRow {
	id: number
	game_session_id: number
	player_id: number
	response: number
	voter_id: number
	voted_at: string
}

/** A ballot as cast — the timestamp is the table's. */
export interface NewRoomVote {
	gameSessionId: number
	/** The player being voted on. */
	playerId: number
	voterId: number
	/** True to kick them. */
	response: boolean
}

/** Record one cast ballot, returning the stored row. */
export async function recordRoomVote(db: D1Database, input: NewRoomVote): Promise<RoomVoteRow> {
	const row = await db
		.prepare(
			`INSERT INTO room_vote (game_session_id, player_id, response, voter_id, voted_at)
			 VALUES (?1, ?2, ?3, ?4, ?5)
			 RETURNING *`
		)
		.bind(
			input.gameSessionId,
			input.playerId,
			input.response ? 1 : 0,
			input.voterId,
			new Date().toISOString()
		)
		.first<RoomVoteRow>()
	// RETURNING always yields the inserted row.
	return row!
}

/**
 * How many DISTINCT players have voted to kick this player out of this session.
 *
 * Distinct voters, not rows, and only each voter's LATEST ballot: the table is append-only,
 * so a player who votes twice leaves two rows, and counting rows would let one person carry
 * a vote on their own by posting it repeatedly. Counting their latest also lets someone
 * change their mind — a yes followed by a no is a no.
 *
 * `MAX(id)` picks the latest rather than `MAX(voted_at)`: two ballots cast in the same
 * millisecond carry the same timestamp, and the id is the order they were actually recorded
 * in.
 */
export async function countKickVotes(
	db: D1Database,
	gameSessionId: number,
	playerId: number
): Promise<number> {
	const row = await db
		.prepare(
			`SELECT COUNT(*) AS yes FROM (
				 SELECT response FROM room_vote
				 WHERE game_session_id = ?1 AND player_id = ?2
					 AND id IN (
						 SELECT MAX(id) FROM room_vote
						 WHERE game_session_id = ?1 AND player_id = ?2
						 GROUP BY voter_id
					 )
			 ) WHERE response = 1`
		)
		.bind(gameSessionId, playerId)
		.first<{ yes: number }>()
	return row?.yes ?? 0
}

/**
 * Whether `yesVotes` carries a vote in a session holding `playerCount` players: STRICTLY more
 * than half. A tie is not a majority, and a session whose presence count has somehow reached
 * zero can never carry one — `0 > 0` is false — rather than every vote passing unopposed.
 */
export const isKickMajority = (yesVotes: number, playerCount: number): boolean =>
	playerCount > 0 && yesVotes > playerCount / 2
