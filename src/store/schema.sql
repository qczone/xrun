CREATE TABLE meta(key TEXT PRIMARY KEY NOT NULL,value TEXT NOT NULL);
INSERT INTO meta VALUES('log_bytes','0');
INSERT INTO meta VALUES('db_id','db_' || lower(hex(randomblob(16))));

CREATE TABLE jobs(
    job_id TEXT PRIMARY KEY NOT NULL,
    source_device_id TEXT NOT NULL,
    target_device_id TEXT NOT NULL,
    request_id TEXT NOT NULL,
    request_hash TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('exec','stream_exec','push','pull','screenshot','forward')),
    params_json TEXT NOT NULL CHECK(json_valid(params_json)),
    result_json TEXT CHECK(result_json IS NULL OR json_valid(result_json)),
    state TEXT NOT NULL CHECK(state IN ('accepted','running','succeeded','failed','canceled','timed_out','lost')),
    error_code TEXT,
    error_message TEXT,
    created_at_ms INTEGER NOT NULL,
    started_at_ms INTEGER,
    finished_at_ms INTEGER,
    updated_at_ms INTEGER NOT NULL,
    process_json TEXT CHECK(process_json IS NULL OR json_valid(process_json)),
    leftover_possible INTEGER NOT NULL DEFAULT 0 CHECK(leftover_possible IN (0,1)),
    last_log_seq INTEGER NOT NULL DEFAULT 0 CHECK(last_log_seq>=0),
    log_bytes INTEGER NOT NULL DEFAULT 0 CHECK(log_bytes>=0),
    output_complete INTEGER CHECK(output_complete IN (0,1)),
    output_loss_reason TEXT,
    UNIQUE(source_device_id,request_id)
);
CREATE INDEX jobs_timeline ON jobs(created_at_ms DESC,job_id DESC);
CREATE INDEX jobs_source_timeline ON jobs(source_device_id,created_at_ms DESC,job_id DESC);
CREATE INDEX jobs_active ON jobs(kind,source_device_id) WHERE state IN ('accepted','running');
CREATE INDEX jobs_failed ON jobs(created_at_ms DESC,job_id DESC)
    WHERE state IN ('failed','canceled','timed_out','lost');
CREATE INDEX jobs_finished_logs ON jobs(finished_at_ms) WHERE log_bytes>0 AND state NOT IN ('accepted','running');

CREATE TABLE job_logs(
    job_id TEXT NOT NULL REFERENCES jobs(job_id) ON DELETE CASCADE,
    seq INTEGER NOT NULL CHECK(seq>0),
    stream TEXT NOT NULL CHECK(stream IN ('stdout','stderr')),
    bytes BLOB NOT NULL,
    PRIMARY KEY(job_id,seq)
);
CREATE TABLE job_attachments(
    attachment_id TEXT PRIMARY KEY NOT NULL,
    job_id TEXT NOT NULL REFERENCES jobs(job_id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    size_bytes INTEGER NOT NULL CHECK(size_bytes>=0),
    sha256 TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('available','expired','missing')),
    deleted_at_ms INTEGER
);
CREATE INDEX job_attachments_job ON job_attachments(job_id,created_at_ms,attachment_id);
CREATE INDEX job_attachments_cleanup ON job_attachments(status,created_at_ms);
