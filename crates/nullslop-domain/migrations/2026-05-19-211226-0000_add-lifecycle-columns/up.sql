ALTER TABLE sessions ADD COLUMN lifecycle_name TEXT;
ALTER TABLE sessions ADD COLUMN lifecycle_args TEXT NOT NULL DEFAULT '[]';
