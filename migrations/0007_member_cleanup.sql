-- A deleted character leaves its campaigns (memberships had no foreign key to characters, so the
-- rows lingered, counted against the member quota and made the feed fall back to other names).
DELETE FROM campaign_members WHERE character_id NOT IN (SELECT id FROM characters);

CREATE TRIGGER IF NOT EXISTS characters_leave_campaigns
AFTER DELETE ON characters
BEGIN
    DELETE FROM campaign_members WHERE character_id = OLD.id;
END;
