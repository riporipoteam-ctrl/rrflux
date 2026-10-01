-- Player subscriptions (the "Subscribe" button on profiles).
-- A player subscribing to another player follows their content (rooms, inventions).
-- Honest empty: no invented subscription data, just the player's own actions.
CREATE TABLE IF NOT EXISTS player_subscription (
	subscriber_id INTEGER NOT NULL,
	subscribed_to_id INTEGER NOT NULL,
	created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
	PRIMARY KEY (subscriber_id, subscribed_to_id)
);
CREATE INDEX IF NOT EXISTS idx_player_subscription_to ON player_subscription (subscribed_to_id);
