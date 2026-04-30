use crate::taz::Article;
use anyhow::{Context, Result};
use log::{debug, info};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    collections::HashSet,
    path::Path,
    sync::Mutex,
};

/// ── SQL column list constants ────────────────────────────────────────────────
/// Centralised here so SELECT and INSERT columns stay in sync across queries.

/// Columns inserted when saving an article (excludes auto-generated id, uploaded fields).
const INSERT_COLS: &str =
    "url, title, subtitle, author, date, section, clean_text, word_count, difficulty, fetched_at, paywalled";

/// ON CONFLICT UPDATE clause shared by save_article and save_articles_batch.
const UPSERT_SET: &str = r#"
    title = excluded.title,
    subtitle = excluded.subtitle,
    author = excluded.author,
    date = excluded.date,
    section = excluded.section,
    clean_text = excluded.clean_text,
    word_count = excluded.word_count,
    difficulty = excluded.difficulty,
    fetched_at = excluded.fetched_at,
    paywalled = excluded.paywalled
"#;

/// All columns for a full StoredArticle row (unqualified — for single-table queries).
const SELECT_ALL_COLS: &str =
    "id, url, title, subtitle, author, date, section, clean_text, word_count, difficulty, fetched_at, uploaded_to_lingq, lingq_lesson_id, lingq_lesson_url, paywalled";

/// All columns for a full StoredArticle row (table-qualified — for JOIN queries).
const SELECT_ALL_COLS_A: &str =
    "a.id, a.url, a.title, a.subtitle, a.author, a.date, a.section, a.clean_text, a.word_count, a.difficulty, a.fetched_at, a.uploaded_to_lingq, a.lingq_lesson_id, a.lingq_lesson_url, a.paywalled";

/// Metadata-only columns for StoredArticleMeta (no clean_text).
const SELECT_META_COLS: &str =
    "id, url, title, subtitle, author, date, section, word_count, difficulty, fetched_at, uploaded_to_lingq, lingq_lesson_id, lingq_lesson_url, paywalled";

/// Metadata-only columns (table-qualified — for JOIN queries).
const SELECT_META_COLS_A: &str =
    "a.id, a.url, a.title, a.subtitle, a.author, a.date, a.section, a.word_count, a.difficulty, a.fetched_at, a.uploaded_to_lingq, a.lingq_lesson_id, a.lingq_lesson_url, a.paywalled";

#[derive(Debug, Clone)]
pub struct StoredArticle {
    pub id: i64,
    pub url: String,
    pub title: String,
    pub subtitle: String,
    pub author: String,
    pub date: String,
    pub section: String,
    pub clean_text: String,
    pub word_count: i64,
    pub difficulty: i64,
    pub fetched_at: String,
    pub uploaded_to_lingq: bool,
    pub lingq_lesson_id: Option<i64>,
    pub lingq_lesson_url: String,
    pub paywalled: bool,
}

/// Lightweight article metadata for list display — excludes clean_text
/// to avoid loading megabytes of text when only metadata columns are needed.
#[derive(Debug, Clone)]
pub struct StoredArticleMeta {
    pub id: i64,
    pub url: String,
    pub title: String,
    pub subtitle: String,
    pub author: String,
    pub date: String,
    pub section: String,
    pub word_count: i64,
    pub difficulty: i64,
    pub fetched_at: String,
    pub uploaded_to_lingq: bool,
    pub lingq_lesson_id: Option<i64>,
    pub lingq_lesson_url: String,
    pub paywalled: bool,
}

#[derive(Debug, Clone)]
pub struct SectionCount {
    pub section: String,
    pub count: i64,
}

#[derive(Debug, Clone)]
pub struct LibraryStats {
    pub total_articles: i64,
    pub uploaded_articles: i64,
    pub average_word_count: i64,
    pub sections: Vec<SectionCount>,
}

#[derive(Debug, Clone, Default)]
pub struct ArticleQuery {
    pub search: Option<String>,
    pub section: Option<String>,
    pub only_not_uploaded: bool,
    pub min_words: Option<i64>,
    pub max_words: Option<i64>,
    pub sort: Option<String>,
    pub limit: usize,
}

pub struct Database {
    /// Write connection — used for INSERT, UPDATE, DELETE, and migrations.
    write_conn: Mutex<Connection>,
    /// Read-only connection — used for SELECT queries. WAL mode allows
    /// readers to proceed concurrently with a writer.
    read_conn: Mutex<Connection>,
}

impl Database {
    pub fn open_default() -> Result<Self> {
        let db_path = crate::app_data_dir()?.join("taz_lingq_tool.db");
        Self::open(&db_path)
    }

    pub fn open(path: &Path) -> Result<Self> {
        info!("Opening database at {}", path.display());
        let is_memory = path.to_str() == Some(":memory:");

        // For :memory: databases each open(":memory:") creates an independent DB.
        // Use shared-cache URI so both connections see the same data.
        let write_conn = if is_memory {
            Connection::open("file::memory:?cache=shared")
                .context("failed to open shared in-memory database")?
        } else {
            Connection::open(path)
                .with_context(|| format!("failed to open database {}", path.display()))?
        };

        // WAL mode allows concurrent readers + one writer without blocking.
        if !is_memory {
            write_conn
                .pragma_update(None, "journal_mode", "WAL")
                .context("failed to enable WAL mode")?;
        }

        let read_conn = if is_memory {
            Connection::open("file::memory:?cache=shared")
                .context("failed to open shared in-memory read connection")?
        } else {
            Connection::open_with_flags(
                path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .with_context(|| format!("failed to open read-only database {}", path.display()))?
        };

        let database = Self {
            write_conn: Mutex::new(write_conn),
            read_conn: Mutex::new(read_conn),
        };
        database.migrate()?;
        Ok(database)
    }

    /// Acquire the write connection.
    fn conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.write_conn
            .lock()
            .map_err(|_| anyhow::anyhow!("database write mutex poisoned — a background thread likely panicked"))
    }

    /// Acquire the read-only connection (does not block writers).
    fn read(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.read_conn
            .lock()
            .map_err(|_| anyhow::anyhow!("database read mutex poisoned — a background thread likely panicked"))
    }

    pub fn save_article(&self, article: &Article) -> Result<i64> {
        debug!("Saving article: {} ({})", article.title, article.url);
        let conn = self.conn()?;
        // Use RETURNING id to get the row id in a single statement,
        // whether the row was inserted or updated via ON CONFLICT.
        let sql = format!(
            "INSERT INTO articles ({INSERT_COLS})
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(url) DO UPDATE SET {UPSERT_SET}
             RETURNING id"
        );
        let id: i64 = conn.query_row(
            &sql,
            params![
                article.url,
                article.title,
                article.subtitle,
                article.author,
                article.date,
                article.section,
                article.clean_text,
                article.word_count as i64,
                article.difficulty,
                article.fetched_at,
                article.paywalled as i64,
            ],
            |row| row.get(0),
        )?;

        Ok(id)
    }

    /// Save multiple articles in a single transaction for better performance.
    /// Returns the number of articles successfully saved.
    pub fn save_articles_batch(&self, articles: &[Article]) -> Result<usize> {
        let conn = self.conn()?;
        conn.execute_batch("BEGIN")?;
        let sql = format!(
            "INSERT INTO articles ({INSERT_COLS})
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(url) DO UPDATE SET {UPSERT_SET}"
        );
        let mut saved = 0;
        for article in articles {
            debug!("Batch saving: {} ({})", article.title, article.url);
            match conn.execute(
                &sql,
                params![
                    article.url,
                    article.title,
                    article.subtitle,
                    article.author,
                    article.date,
                    article.section,
                    article.clean_text,
                    article.word_count as i64,
                    article.difficulty,
                    article.fetched_at,
                    article.paywalled as i64,
                ],
            ) {
                Ok(_) => saved += 1,
                Err(err) => log::warn!("Batch save failed for {}: {err:#}", article.url),
            }
        }
        if let Err(err) = conn.execute_batch("COMMIT") {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(err.into());
        }
        Ok(saved)
    }

    pub fn list_articles(
        &self,
        query: &ArticleQuery,
    ) -> Result<Vec<StoredArticle>> {
        let order_clause = match query.sort.as_deref() {
            Some("oldest") => "date ASC, id ASC",
            Some("longest") => "word_count DESC",
            Some("shortest") => "word_count ASC",
            Some("title") => "title ASC",
            _ => "date DESC, id DESC",
        };

        // Use FTS5 MATCH when search term is provided; fall back to LIKE for
        // terms that contain FTS special characters that might trip up the parser.
        let fts_term = query.search.as_deref().map(|s| sanitize_fts_query(s));
        let use_fts = fts_term.as_ref().is_some_and(|t| !t.is_empty());

        let sql = if use_fts {
            format!(
                "SELECT {SELECT_ALL_COLS_A}
                FROM articles a
                INNER JOIN articles_fts ON articles_fts.rowid = a.id
                WHERE articles_fts MATCH ?1
                  AND (?2 IS NULL OR a.section = ?2)
                  AND (?3 = 0 OR a.uploaded_to_lingq = 0)
                  AND (?4 IS NULL OR a.word_count >= ?4)
                  AND (?5 IS NULL OR a.word_count <= ?5)
                ORDER BY {order_clause}
                LIMIT ?6"
            )
        } else {
            format!(
                "SELECT {SELECT_ALL_COLS}
                FROM articles
                WHERE (?1 IS NULL OR title LIKE '%' || ?1 || '%' OR clean_text LIKE '%' || ?1 || '%')
                  AND (?2 IS NULL OR section = ?2)
                  AND (?3 = 0 OR uploaded_to_lingq = 0)
                  AND (?4 IS NULL OR word_count >= ?4)
                  AND (?5 IS NULL OR word_count <= ?5)
                ORDER BY {order_clause}
                LIMIT ?6"
            )
        };

        let search_param: Option<String> = if use_fts {
            fts_term
        } else {
            query.search.clone()
        };

        let conn = self.read()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![
                search_param.as_deref(),
                query.section.as_deref(),
                if query.only_not_uploaded { 1 } else { 0 },
                query.min_words,
                query.max_words,
                query.limit as i64,
            ],
            map_article_row,
        )?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// List articles returning only metadata (no clean_text) for list display.
    pub fn list_articles_meta(
        &self,
        query: &ArticleQuery,
    ) -> Result<Vec<StoredArticleMeta>> {
        let order_clause = match query.sort.as_deref() {
            Some("oldest") => "date ASC, id ASC",
            Some("longest") => "word_count DESC",
            Some("shortest") => "word_count ASC",
            Some("title") => "title ASC",
            _ => "date DESC, id DESC",
        };

        let fts_term = query.search.as_deref().map(|s| sanitize_fts_query(s));
        let use_fts = fts_term.as_ref().is_some_and(|t| !t.is_empty());

        let sql = if use_fts {
            format!(
                "SELECT {SELECT_META_COLS_A}
                FROM articles a
                INNER JOIN articles_fts ON articles_fts.rowid = a.id
                WHERE articles_fts MATCH ?1
                  AND (?2 IS NULL OR a.section = ?2)
                  AND (?3 = 0 OR a.uploaded_to_lingq = 0)
                  AND (?4 IS NULL OR a.word_count >= ?4)
                  AND (?5 IS NULL OR a.word_count <= ?5)
                ORDER BY {order_clause}
                LIMIT ?6"
            )
        } else {
            format!(
                "SELECT {SELECT_META_COLS}
                FROM articles
                WHERE (?1 IS NULL OR title LIKE '%' || ?1 || '%' OR clean_text LIKE '%' || ?1 || '%')
                  AND (?2 IS NULL OR section = ?2)
                  AND (?3 = 0 OR uploaded_to_lingq = 0)
                  AND (?4 IS NULL OR word_count >= ?4)
                  AND (?5 IS NULL OR word_count <= ?5)
                ORDER BY {order_clause}
                LIMIT ?6"
            )
        };

        let search_param: Option<String> = if use_fts {
            fts_term
        } else {
            query.search.clone()
        };

        let conn = self.read()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![
                search_param.as_deref(),
                query.section.as_deref(),
                if query.only_not_uploaded { 1 } else { 0 },
                query.min_words,
                query.max_words,
                query.limit as i64,
            ],
            map_article_meta_row,
        )?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn get_article(&self, id: i64) -> Result<Option<StoredArticle>> {
        self.read()?
            .query_row(
                &format!("SELECT {SELECT_ALL_COLS} FROM articles WHERE id = ?1"),
                params![id],
                map_article_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn get_all_article_urls(&self) -> Result<HashSet<String>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare("SELECT url FROM articles")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let urls = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(urls.into_iter().collect())
    }

    pub fn mark_uploaded(&self, id: i64, lesson_id: i64, lesson_url: &str) -> Result<()> {
        self.conn()?.execute(
            "UPDATE articles SET uploaded_to_lingq = 1, lingq_lesson_id = ?1, lingq_lesson_url = ?2 WHERE id = ?3",
            params![lesson_id, lesson_url, id],
        )?;
        Ok(())
    }

    pub fn delete_article(&self, id: i64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute("DELETE FROM articles WHERE id = ?1", params![id])?;
        if conn.changes() == 0 {
            log::warn!("delete_article: no article found with id {id}");
        }
        Ok(())
    }

    /// Run PRAGMA integrity_check and return the result string.
    pub fn integrity_check(&self) -> Result<String> {
        let result: String = self.read()?.query_row(
            "PRAGMA integrity_check",
            [],
            |row| row.get(0),
        )?;
        Ok(result)
    }

    /// Reclaim unused space in the database file.
    pub fn vacuum(&self) -> Result<()> {
        self.conn()?.execute_batch("VACUUM")?;
        Ok(())
    }

    /// Export all articles as CSV text.
    pub fn export_csv(&self) -> Result<String> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {SELECT_ALL_COLS} FROM articles ORDER BY date DESC, id DESC"
        ))?;
        let articles = stmt
            .query_map([], map_article_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut csv = String::from("id,url,title,subtitle,author,date,section,word_count,difficulty,fetched_at,uploaded_to_lingq,lingq_lesson_id,lingq_lesson_url\n");
        for a in &articles {
            csv.push_str(&format!(
                "{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
                a.id,
                csv_escape(&a.url),
                csv_escape(&a.title),
                csv_escape(&a.subtitle),
                csv_escape(&a.author),
                csv_escape(&a.date),
                csv_escape(&a.section),
                a.word_count,
                a.difficulty,
                csv_escape(&a.fetched_at),
                a.uploaded_to_lingq,
                a.lingq_lesson_id.map(|id| id.to_string()).unwrap_or_default(),
                csv_escape(&a.lingq_lesson_url),
            ));
        }
        Ok(csv)
    }

    /// Export all articles as JSON text.
    pub fn export_json(&self) -> Result<String> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {SELECT_ALL_COLS} FROM articles ORDER BY date DESC, id DESC"
        ))?;
        let articles = stmt
            .query_map([], map_article_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let entries: Vec<String> = articles.iter().map(|a| {
            format!(
                r#"  {{"id":{},"url":{},"title":{},"subtitle":{},"author":{},"date":{},"section":{},"word_count":{},"difficulty":{},"fetched_at":{},"uploaded_to_lingq":{},"lingq_lesson_id":{},"lingq_lesson_url":{}}}"#,
                a.id,
                json_escape(&a.url),
                json_escape(&a.title),
                json_escape(&a.subtitle),
                json_escape(&a.author),
                json_escape(&a.date),
                json_escape(&a.section),
                a.word_count,
                a.difficulty,
                json_escape(&a.fetched_at),
                a.uploaded_to_lingq,
                a.lingq_lesson_id.map(|id| id.to_string()).unwrap_or_else(|| "null".to_owned()),
                json_escape(&a.lingq_lesson_url),
            )
        }).collect();

        Ok(format!("[\n{}\n]", entries.join(",\n")))
    }

    pub fn get_stats(&self) -> Result<LibraryStats> {
        let conn = self.read()?;
        let total_articles: i64 =
            conn.query_row("SELECT COUNT(*) FROM articles", [], |row| row.get(0))?;
        let uploaded_articles: i64 = conn.query_row(
            "SELECT COUNT(*) FROM articles WHERE uploaded_to_lingq = 1",
            [],
            |row| row.get(0),
        )?;
        let average_word_count: i64 = conn.query_row(
            "SELECT CAST(COALESCE(ROUND(AVG(word_count)), 0) AS INTEGER) FROM articles",
            [],
            |row| row.get(0),
        )?;

        let mut stmt = conn.prepare(
            "SELECT section, COUNT(*) FROM articles GROUP BY section ORDER BY COUNT(*) DESC, section ASC",
        )?;
        let section_rows = stmt.query_map([], |row| {
            Ok(SectionCount {
                section: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                count: row.get(1)?,
            })
        })?;
        let sections = section_rows.collect::<rusqlite::Result<Vec<_>>>()?;

        Ok(LibraryStats {
            total_articles,
            uploaded_articles,
            average_word_count,
            sections,
        })
    }

    fn migrate(&self) -> Result<()> {
        let conn = self.conn()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);",
        )?;

        let current_version: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_version",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        if current_version < 1 {
            conn.execute_batch(
                r#"
                BEGIN;

                CREATE TABLE IF NOT EXISTS articles (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    url TEXT NOT NULL UNIQUE,
                    title TEXT NOT NULL,
                    subtitle TEXT NOT NULL DEFAULT '',
                    author TEXT NOT NULL DEFAULT '',
                    date TEXT NOT NULL DEFAULT '',
                    section TEXT NOT NULL DEFAULT '',
                    body_text TEXT NOT NULL,
                    clean_text TEXT NOT NULL,
                    word_count INTEGER NOT NULL DEFAULT 0,
                    fetched_at TEXT NOT NULL,
                    uploaded_to_lingq INTEGER NOT NULL DEFAULT 0,
                    lingq_lesson_id INTEGER,
                    lingq_lesson_url TEXT NOT NULL DEFAULT ''
                );

                CREATE INDEX IF NOT EXISTS idx_articles_section ON articles(section);
                CREATE INDEX IF NOT EXISTS idx_articles_uploaded ON articles(uploaded_to_lingq);
                CREATE INDEX IF NOT EXISTS idx_articles_word_count ON articles(word_count);

                INSERT INTO schema_version (version) VALUES (1);

                COMMIT;
                "#,
            )?;
        }

        if current_version < 2 {
            conn.execute_batch(
                r#"
                BEGIN;

                CREATE VIRTUAL TABLE IF NOT EXISTS articles_fts USING fts5(
                    title,
                    subtitle,
                    body_text,
                    content='articles',
                    content_rowid='id'
                );

                -- Populate FTS index from existing articles
                INSERT INTO articles_fts(rowid, title, subtitle, body_text)
                    SELECT id, title, subtitle, body_text FROM articles;

                -- Keep FTS in sync on INSERT
                CREATE TRIGGER IF NOT EXISTS articles_ai AFTER INSERT ON articles BEGIN
                    INSERT INTO articles_fts(rowid, title, subtitle, body_text)
                        VALUES (new.id, new.title, new.subtitle, new.body_text);
                END;

                -- Keep FTS in sync on DELETE
                CREATE TRIGGER IF NOT EXISTS articles_ad AFTER DELETE ON articles BEGIN
                    INSERT INTO articles_fts(articles_fts, rowid, title, subtitle, body_text)
                        VALUES ('delete', old.id, old.title, old.subtitle, old.body_text);
                END;

                -- Keep FTS in sync on UPDATE
                CREATE TRIGGER IF NOT EXISTS articles_au AFTER UPDATE ON articles BEGIN
                    INSERT INTO articles_fts(articles_fts, rowid, title, subtitle, body_text)
                        VALUES ('delete', old.id, old.title, old.subtitle, old.body_text);
                    INSERT INTO articles_fts(rowid, title, subtitle, body_text)
                        VALUES (new.id, new.title, new.subtitle, new.body_text);
                END;

                INSERT INTO schema_version (version) VALUES (2);

                COMMIT;
                "#,
            )?;
        }

        if current_version < 3 {
            conn.execute_batch(
                r#"
                BEGIN;

                ALTER TABLE articles ADD COLUMN difficulty INTEGER NOT NULL DEFAULT 3;

                -- Backfill difficulty for existing articles using a simple heuristic:
                -- longer articles with longer average words tend to be harder.
                -- This is a rough approximation; re-fetching will compute proper scores.
                UPDATE articles SET difficulty =
                    CASE
                        WHEN word_count < 200 THEN 1
                        WHEN word_count < 400 THEN 2
                        WHEN word_count < 700 THEN 3
                        WHEN word_count < 1200 THEN 4
                        ELSE 5
                    END;

                INSERT INTO schema_version (version) VALUES (3);

                COMMIT;
                "#,
            )?;
        }

        if current_version < 4 {
            conn.execute_batch(
                r#"
                BEGIN;

                -- Composite index for the common library filter: uploaded + word_count range
                CREATE INDEX IF NOT EXISTS idx_articles_upload_words
                    ON articles(uploaded_to_lingq, word_count);

                -- Composite index for date-sorted queries filtered by section
                CREATE INDEX IF NOT EXISTS idx_articles_section_date
                    ON articles(section, date DESC);

                INSERT INTO schema_version (version) VALUES (4);

                COMMIT;
                "#,
            )?;
        }

        if current_version < 5 {
            conn.execute_batch(
                r#"
                BEGIN;

                -- Rebuild FTS5 index to include clean_text for better search coverage
                DROP TRIGGER IF EXISTS articles_ai;
                DROP TRIGGER IF EXISTS articles_ad;
                DROP TRIGGER IF EXISTS articles_au;
                DROP TABLE IF EXISTS articles_fts;

                CREATE VIRTUAL TABLE articles_fts USING fts5(
                    title,
                    subtitle,
                    body_text,
                    clean_text,
                    content='articles',
                    content_rowid='id'
                );

                INSERT INTO articles_fts(rowid, title, subtitle, body_text, clean_text)
                    SELECT id, title, subtitle, body_text, clean_text FROM articles;

                CREATE TRIGGER articles_ai AFTER INSERT ON articles BEGIN
                    INSERT INTO articles_fts(rowid, title, subtitle, body_text, clean_text)
                        VALUES (new.id, new.title, new.subtitle, new.body_text, new.clean_text);
                END;

                CREATE TRIGGER articles_ad AFTER DELETE ON articles BEGIN
                    INSERT INTO articles_fts(articles_fts, rowid, title, subtitle, body_text, clean_text)
                        VALUES ('delete', old.id, old.title, old.subtitle, old.body_text, old.clean_text);
                END;

                CREATE TRIGGER articles_au AFTER UPDATE ON articles BEGIN
                    INSERT INTO articles_fts(articles_fts, rowid, title, subtitle, body_text, clean_text)
                        VALUES ('delete', old.id, old.title, old.subtitle, old.body_text, old.clean_text);
                    INSERT INTO articles_fts(rowid, title, subtitle, body_text, clean_text)
                        VALUES (new.id, new.title, new.subtitle, new.body_text, new.clean_text);
                END;

                INSERT INTO schema_version (version) VALUES (5);

                COMMIT;
                "#,
            )?;
        }

        if current_version < 6 {
            conn.execute_batch(
                r#"
                BEGIN;

                -- Drop triggers and FTS first (they reference body_text),
                -- then drop the column (SQLite 3.35+).
                DROP TRIGGER IF EXISTS articles_ai;
                DROP TRIGGER IF EXISTS articles_ad;
                DROP TRIGGER IF EXISTS articles_au;
                DROP TABLE IF EXISTS articles_fts;

                ALTER TABLE articles DROP COLUMN body_text;

                CREATE VIRTUAL TABLE articles_fts USING fts5(
                    title,
                    subtitle,
                    clean_text,
                    content='articles',
                    content_rowid='id'
                );

                INSERT INTO articles_fts(rowid, title, subtitle, clean_text)
                    SELECT id, title, subtitle, clean_text FROM articles;

                CREATE TRIGGER articles_ai AFTER INSERT ON articles BEGIN
                    INSERT INTO articles_fts(rowid, title, subtitle, clean_text)
                        VALUES (new.id, new.title, new.subtitle, new.clean_text);
                END;

                CREATE TRIGGER articles_ad AFTER DELETE ON articles BEGIN
                    INSERT INTO articles_fts(articles_fts, rowid, title, subtitle, clean_text)
                        VALUES ('delete', old.id, old.title, old.subtitle, old.clean_text);
                END;

                CREATE TRIGGER articles_au AFTER UPDATE ON articles BEGIN
                    INSERT INTO articles_fts(articles_fts, rowid, title, subtitle, clean_text)
                        VALUES ('delete', old.id, old.title, old.subtitle, old.clean_text);
                    INSERT INTO articles_fts(rowid, title, subtitle, clean_text)
                        VALUES (new.id, new.title, new.subtitle, new.clean_text);
                END;

                INSERT INTO schema_version (version) VALUES (6);

                COMMIT;
                "#,
            )?;
        }

        if current_version < 7 {
            conn.execute_batch(
                r#"
                BEGIN;

                ALTER TABLE articles ADD COLUMN paywalled INTEGER NOT NULL DEFAULT 0;

                INSERT INTO schema_version (version) VALUES (7);

                COMMIT;
                "#,
            )?;
        }

        Ok(())
    }
}

fn map_article_meta_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredArticleMeta> {
    Ok(StoredArticleMeta {
        id: row.get(0)?,
        url: row.get(1)?,
        title: row.get(2)?,
        subtitle: row.get(3)?,
        author: row.get(4)?,
        date: row.get(5)?,
        section: row.get(6)?,
        word_count: row.get(7)?,
        difficulty: row.get(8)?,
        fetched_at: row.get(9)?,
        uploaded_to_lingq: row.get::<_, i64>(10)? != 0,
        lingq_lesson_id: row.get(11)?,
        lingq_lesson_url: row.get(12)?,
        paywalled: row.get::<_, i64>(13)? != 0,
    })
}

fn map_article_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredArticle> {
    Ok(StoredArticle {
        id: row.get(0)?,
        url: row.get(1)?,
        title: row.get(2)?,
        subtitle: row.get(3)?,
        author: row.get(4)?,
        date: row.get(5)?,
        section: row.get(6)?,
        clean_text: row.get(7)?,
        word_count: row.get(8)?,
        difficulty: row.get(9)?,
        fetched_at: row.get(10)?,
        uploaded_to_lingq: row.get::<_, i64>(11)? != 0,
        lingq_lesson_id: row.get(12)?,
        lingq_lesson_url: row.get(13)?,
        paywalled: row.get::<_, i64>(14)? != 0,
    })
}

/// Sanitize user input for FTS5 MATCH queries.
/// Strips FTS5 operators and wraps each word with `"..."` to treat them as literals,
/// joined with implicit AND. Returns empty string if nothing usable remains.
fn sanitize_fts_query(input: &str) -> String {
    input
        .split_whitespace()
        .map(|word| {
            // Strip FTS5 special chars: " * ^ ( ) { } : + -
            let clean: String = word
                .chars()
                .filter(|ch| !matches!(ch, '"' | '*' | '^' | '(' | ')' | '{' | '}' | ':' | '+'))
                .collect();
            clean.trim_matches('-').to_owned()
        })
        .filter(|word| !word.is_empty())
        .map(|word| format!("\"{word}\""))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Escape a field for CSV: wrap in quotes if it contains commas, quotes, or newlines.
fn csv_escape(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

/// Escape a string as a JSON string literal (with surrounding quotes).
fn json_escape(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── sanitize_fts_query ──

    #[test]
    fn sanitize_fts_plain_words() {
        assert_eq!(sanitize_fts_query("hello world"), r#""hello" "world""#);
    }

    #[test]
    fn sanitize_fts_strips_operators() {
        assert_eq!(sanitize_fts_query(r#"hello "world" NOT"#), r#""hello" "world" "NOT""#);
    }

    #[test]
    fn sanitize_fts_strips_stars_and_parens() {
        assert_eq!(sanitize_fts_query("test* (group)"), r#""test" "group""#);
    }

    #[test]
    fn sanitize_fts_trims_leading_trailing_dashes() {
        assert_eq!(sanitize_fts_query("-negated- --double--"), r#""negated" "double""#);
    }

    #[test]
    fn sanitize_fts_empty_input() {
        assert_eq!(sanitize_fts_query(""), "");
    }

    #[test]
    fn sanitize_fts_only_special_chars() {
        assert_eq!(sanitize_fts_query(r#""*^(){}:+"#), "");
    }

    #[test]
    fn sanitize_fts_preserves_german_chars() {
        assert_eq!(sanitize_fts_query("Über Straße"), r#""Über" "Straße""#);
    }

    // ── Database integration ──

    #[test]
    fn save_and_retrieve_article() {
        let db = Database::open(Path::new(":memory:")).unwrap();
        let article = Article {
            url: "https://taz.de/test/!1234/".to_owned(),
            title: "Test Article".to_owned(),
            subtitle: "A subtitle".to_owned(),
            author: "Author".to_owned(),
            date: "2025-01-15".to_owned(),
            section: "Politik".to_owned(),
            body_text: "Body text here.".to_owned(),
            clean_text: "Clean text here.".to_owned(),
            word_count: 3,
            difficulty: 2,
            fetched_at: "2025-01-15T10:00:00Z".to_owned(),
            paywalled: false,
        };
        let id = db.save_article(&article).unwrap();
        assert!(id > 0);

        let stored = db.get_article(id).unwrap().unwrap();
        assert_eq!(stored.title, "Test Article");
        assert_eq!(stored.url, "https://taz.de/test/!1234/");
        assert!(!stored.uploaded_to_lingq);
        assert!(!stored.paywalled);
    }

    #[test]
    fn save_article_upsert_returns_same_id() {
        let db = Database::open(Path::new(":memory:")).unwrap();
        let article = Article {
            url: "https://taz.de/test/!1234/".to_owned(),
            title: "Original".to_owned(),
            subtitle: String::new(),
            author: String::new(),
            date: String::new(),
            section: String::new(),
            body_text: "Body.".to_owned(),
            clean_text: "Clean.".to_owned(),
            word_count: 1,
            difficulty: 3,
            fetched_at: "2025-01-15T10:00:00Z".to_owned(),
            paywalled: false,
        };
        let id1 = db.save_article(&article).unwrap();

        let mut updated = article.clone();
        updated.title = "Updated".to_owned();
        let id2 = db.save_article(&updated).unwrap();
        assert_eq!(id1, id2);

        let stored = db.get_article(id1).unwrap().unwrap();
        assert_eq!(stored.title, "Updated");
    }

    #[test]
    fn mark_uploaded_and_query() {
        let db = Database::open(Path::new(":memory:")).unwrap();
        let article = Article {
            url: "https://taz.de/test/!5678/".to_owned(),
            title: "Upload Test".to_owned(),
            subtitle: String::new(),
            author: String::new(),
            date: String::new(),
            section: "Kultur".to_owned(),
            body_text: "Some body.".to_owned(),
            clean_text: "Some clean.".to_owned(),
            word_count: 2,
            difficulty: 3,
            fetched_at: "2025-01-15T10:00:00Z".to_owned(),
            paywalled: false,
        };
        let id = db.save_article(&article).unwrap();
        db.mark_uploaded(id, 999, "https://lingq.com/lesson/999/").unwrap();

        let stored = db.get_article(id).unwrap().unwrap();
        assert!(stored.uploaded_to_lingq);
        assert_eq!(stored.lingq_lesson_id, Some(999));

        // only_not_uploaded should exclude it
        let results = db.list_articles(&ArticleQuery {
            only_not_uploaded: true,
            limit: 100,
            ..Default::default()
        }).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn delete_article_removes_it() {
        let db = Database::open(Path::new(":memory:")).unwrap();
        let article = Article {
            url: "https://taz.de/test/!9999/".to_owned(),
            title: "Delete Me".to_owned(),
            subtitle: String::new(),
            author: String::new(),
            date: String::new(),
            section: String::new(),
            body_text: "Body.".to_owned(),
            clean_text: "Clean.".to_owned(),
            word_count: 1,
            difficulty: 3,
            fetched_at: "2025-01-15T10:00:00Z".to_owned(),
            paywalled: false,
        };
        let id = db.save_article(&article).unwrap();
        db.delete_article(id).unwrap();
        assert!(db.get_article(id).unwrap().is_none());
    }

    #[test]
    fn stats_reflect_articles() {
        let db = Database::open(Path::new(":memory:")).unwrap();
        let stats = db.get_stats().unwrap();
        assert_eq!(stats.total_articles, 0);

        let article = Article {
            url: "https://taz.de/test/!1111/".to_owned(),
            title: "Stats Test".to_owned(),
            subtitle: String::new(),
            author: String::new(),
            date: String::new(),
            section: "Sport".to_owned(),
            body_text: "One two three four five.".to_owned(),
            clean_text: "One two three four five.".to_owned(),
            word_count: 5,
            difficulty: 2,
            fetched_at: "2025-01-15T10:00:00Z".to_owned(),
            paywalled: false,
        };
        db.save_article(&article).unwrap();

        let stats = db.get_stats().unwrap();
        assert_eq!(stats.total_articles, 1);
        assert_eq!(stats.uploaded_articles, 0);
        assert_eq!(stats.average_word_count, 5);
        assert_eq!(stats.sections.len(), 1);
        assert_eq!(stats.sections[0].section, "Sport");
    }

    // ── Batch save ──

    fn make_article(url: &str, title: &str) -> Article {
        Article {
            url: url.to_owned(),
            title: title.to_owned(),
            subtitle: String::new(),
            author: String::new(),
            date: "2025-01-01".to_owned(),
            section: "Test".to_owned(),
            body_text: "Body.".to_owned(),
            clean_text: "Clean.".to_owned(),
            word_count: 1,
            difficulty: 3,
            fetched_at: "2025-01-15T10:00:00Z".to_owned(),
            paywalled: false,
        }
    }

    #[test]
    fn save_articles_batch_saves_multiple() {
        let db = Database::open(Path::new(":memory:")).unwrap();
        let articles = vec![
            make_article("https://taz.de/a/!1/", "First"),
            make_article("https://taz.de/a/!2/", "Second"),
            make_article("https://taz.de/a/!3/", "Third"),
        ];
        let saved = db.save_articles_batch(&articles).unwrap();
        assert_eq!(saved, 3);

        let stats = db.get_stats().unwrap();
        assert_eq!(stats.total_articles, 3);
    }

    #[test]
    fn save_articles_batch_handles_duplicates() {
        let db = Database::open(Path::new(":memory:")).unwrap();
        let articles = vec![
            make_article("https://taz.de/a/!1/", "First"),
            make_article("https://taz.de/a/!1/", "First Updated"),
        ];
        let saved = db.save_articles_batch(&articles).unwrap();
        assert_eq!(saved, 2); // Both succeed (second is an upsert)

        let stats = db.get_stats().unwrap();
        assert_eq!(stats.total_articles, 1); // Only one unique article
    }

    #[test]
    fn save_articles_batch_empty_input() {
        let db = Database::open(Path::new(":memory:")).unwrap();
        let saved = db.save_articles_batch(&[]).unwrap();
        assert_eq!(saved, 0);
    }

    // ── Export ──

    #[test]
    fn export_csv_includes_header_and_data() {
        let db = Database::open(Path::new(":memory:")).unwrap();
        db.save_article(&make_article("https://taz.de/a/!1/", "Export Test")).unwrap();
        let csv = db.export_csv().unwrap();
        assert!(csv.starts_with("id,url,title,"));
        assert!(csv.contains("Export Test"));
    }

    #[test]
    fn export_json_is_valid_array() {
        let db = Database::open(Path::new(":memory:")).unwrap();
        db.save_article(&make_article("https://taz.de/a/!1/", "JSON Test")).unwrap();
        let json = db.export_json().unwrap();
        assert!(json.starts_with('['));
        assert!(json.ends_with(']'));
        assert!(json.contains("\"JSON Test\""));
    }

    // ── csv_escape / json_escape ──

    #[test]
    fn csv_escape_wraps_commas() {
        assert_eq!(csv_escape("hello, world"), "\"hello, world\"");
    }

    #[test]
    fn csv_escape_doubles_quotes() {
        assert_eq!(csv_escape(r#"say "hi""#), r#""say ""hi""""#);
    }

    #[test]
    fn json_escape_handles_special_chars() {
        assert_eq!(json_escape("line1\nline2"), r#""line1\nline2""#);
        assert_eq!(json_escape(r#"say "hi""#), r#""say \"hi\"""#);
    }
}
