CREATE TABLE state(id INTEGER PRIMARY KEY CHECK(id=1),data TEXT NOT NULL);
CREATE TABLE invitations(
    hash TEXT PRIMARY KEY,
    expires INTEGER NOT NULL,
    allow INTEGER NOT NULL,
    kind TEXT NOT NULL DEFAULT 'link',
    invitation_id TEXT,
    locator TEXT,
    attempts INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'active',
    key_fp TEXT,
    next_attempt_ms INTEGER NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX invitation_identity ON invitations(invitation_id);
CREATE TABLE receipts(device TEXT PRIMARY KEY,data TEXT NOT NULL);
CREATE TABLE freshness(id INTEGER PRIMARY KEY CHECK(id=1),data TEXT NOT NULL);
