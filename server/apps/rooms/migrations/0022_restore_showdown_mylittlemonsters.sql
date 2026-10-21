-- Restore the two recovered 2025 RRO records to browse/search category feeds.
-- Their metadata was imported after the original room seed and missed the authoritative
-- room_tag row; MyLittleMonsters was also imported with its temporary quarantine flag set.
-- Keep the update conditional on the room existing so fresh/local databases can apply the
-- migration before an operator imports the archived metadata.

UPDATE room
SET data = json_set(data, '$.ExcludeFromLists', false, '$.ExcludeFromSearch', false)
WHERE room_id IN (52815862, 52815871);

INSERT OR IGNORE INTO room_tag (room_id, tag, type, is_primary_genre)
SELECT room_id, 'rro', 2, 0
FROM room
WHERE room_id IN (52815862, 52815871);
