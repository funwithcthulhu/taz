use crate::taz::Article;
use anyhow::{Context, Result};
use log::{debug, info};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    collections::HashSet,
    path::Path,
    sync::Mutex,
};

#[derive(Debug, Clone)]
pub struct StoredArticle {
    pub id: i64,
    pub url: String,
    pub title: String,
    pub subtitle: String,
    pub author: String,
    pub date: String,
    pub section: String,
    pub body_text: String,
    pub clean_text: String,
    pub word_count: i64,
    pub difficulty: i64,
    pub fetched_at: String,
    pub uploaded_to_lingq: bool,
    pub lingq_lesson_id: Option<i64>,
    pub lingq_lesson_url: String,
}

/// Lightweight article metadata for list display — excludes body_text and clean_text
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
    conn: Mutex<Connection>,
}

impl Database {
    pub fn open_default() -> Result<Self> {
        let db_path = crate::app_data_dir()?.join("taz_lingq_tool.db");
        Self::open(&db_path)
    }

    pub fn open(path: &Path) -> Result<Self> {
        info!("Opening database at {}", path.display());
        let conn = Connection::open(path)
            .with_context(|| format!("failed to open database {}", path.display()))?;
        // WAL mode allows concurrent readers + one writer without blocking.
        conn.pragma_update(None, "journal_mode", "WAL")
            .context("failed to enable WAL mode")?;
        let database = Self { conn: Mutex::new(conn) };
        database.migrate()?;
        Ok(database)
    }

    fn conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| anyhow::anyhow!("database mutex poisoned — a background thread likely panicked"))
    }

    pub fn save_article(&self, article: &Article) -> Result<i64> {
        debug!("Saving article: {} ({})", article.title, article.url);
        let conn = self.conn()?;
        // Use RETURNING id to get the row id in a single statement,
        // whether the row was inserted or updated via ON CONFLICT.
        let id: i64 = conn.query_row(
            r#"
            INSERT INTO articles (
                url, title, subtitle, author, date, section, body_text, clean_text, word_count, difficulty, fetched_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            ON CONFLICT(url) DO UPDATE SET
                title = excluded.title,
                subtitle = excluded.subtitle,
                author = excluded.author,
                date = excluded.date,
                section = excluded.section,
                body_text = excluded.body_text,
                clean_text = excluded.clean_text,
                word_count = excluded.word_count,
                difficulty = excluded.difficulty,
                fetched_at = excluded.fetched_at
            RETURNING id
            "#,
            params![
                article.url,
                article.title,
                article.subtitle,
                article.author,
                article.date,
                article.section,
                article.body_text,
                article.clean_text,
                article.word_count as i64,
                article.difficulty,
                article.fetched_at,
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
        let mut saved = 0;
        for article in articles {
            debug!("Batch saving: {} ({})", article.title, article.url);
            match conn.execute(
                r#"
                INSERT INTO articles (
                    url, title, subtitle, author, date, section, body_text, clean_text, word_count, difficulty, fetched_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                ON CONFLICT(url) DO UPDATE SET
                    title = excluded.title,
                    subtitle = excluded.subtitle,
                    author = excluded.author,
                    date = excluded.date,
                    section = excluded.section,
                    body_text = excluded.body_text,
                    clean_text = excluded.clean_text,
                    word_count = excluded.word_count,
                    difficulty = excluded.difficulty,
                    fetched_at = excluded.fetched_at
                "#,
                params![
                    article.url,
                    article.title,
                    article.subtitle,
                    article.author,
                    article.date,
                    article.section,
                    article.body_text,
                    article.clean_text,
                    article.word_count as i64,
                    article.difficulty,
                    article.fetched_at,
                ],
            ) {
                Ok(_) => saved += 1,
                Err(err) => log::warn!("Batch save failed for {}: {err:#}", article.url),
            }
        }
        conn.execute_batch("COMMIT")?;
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
                r#"
                SELECT
                    a.id, a.url, a.title, a.subtitle, a.author, a.date, a.section,
                    a.body_text, a.clean_text, a.word_count, a.difficulty, a.fetched_at,
                    a.uploaded_to_lingq, a.lingq_lesson_id, a.lingq_lesson_url
                FROM articles a
                INNER JOIN articles_fts ON articles_fts.rowid = a.id
                WHERE articles_fts MATCH ?1
                  AND (?2 IS NULL OR a.section = ?2)
                  AND (?3 = 0 OR a.uploaded_to_lingq = 0)
                  AND (?4 IS NULL OR a.word_count >= ?4)
                  AND (?5 IS NULL OR a.word_count <= ?5)
                ORDER BY {order_clause}
                LIMIT ?6
                "#
            )
        } else {
            format!(
                r#"
                SELECT
                    id, url, title, subtitle, author, date, section, body_text, clean_text,
                    word_count, difficulty, fetched_at, uploaded_to_lingq, lingq_lesson_id, lingq_lesson_url
                FROM articles
                WHERE (?1 IS NULL OR title LIKE '%' || ?1 || '%' OR body_text LIKE '%' || ?1 || '%')
                  AND (?2 IS NULL OR section = ?2)
                  AND (?3 = 0 OR uploaded_to_lingq = 0)
                  AND (?4 IS NULL OR word_count >= ?4)
                  AND (?5 IS NULL OR word_count <= ?5)
                ORDER BY {order_clause}
                LIMIT ?6
                "#
            )
        };

        let search_param: Option<String> = if use_fts {
            fts_term
        } else {
            query.search.clone()
        };

        let conn = self.conn()?;
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

    /// List articles returning only metadata (no body_text/clean_text) for list display.
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
                r#"
                SELECT
                    a.id, a.url, a.title, a.subtitle, a.author, a.date, a.section,
                    a.word_count, a.difficulty, a.fetched_at,
                    a.uploaded_to_lingq, a.lingq_lesson_id, a.lingq_lesson_url
                FROM articles a
                INNER JOIN articles_fts ON articles_fts.rowid = a.id
                WHERE articles_fts MATCH ?1
                  AND (?2 IS NULL OR a.section = ?2)
                  AND (?3 = 0 OR a.uploaded_to_lingq = 0)
                  AND (?4 IS NULL OR a.word_count >= ?4)
                  AND (?5 IS NULL OR a.word_count <= ?5)
                ORDER BY {order_clause}
                LIMIT ?6
                "#
            )
        } else {
            format!(
                r#"
                SELECT
                    id, url, title, subtitle, author, date, section,
                    word_count, difficulty, fetched_at,
                    uploaded_to_lingq, lingq_lesson_id, lingq_lesson_url
                FROM articles
                WHERE (?1 IS NULL OR title LIKE '%' || ?1 || '%' OR body_text LIKE '%' || ?1 || '%')
                  AND (?2 IS NULL OR section = ?2)
                  AND (?3 = 0 OR uploaded_to_lingq = 0)
                  AND (?4 IS NULL OR word_count >= ?4)
                  AND (?5 IS NULL OR word_count <= ?5)
                ORDER BY {order_clause}
                LIMIT ?6
                "#
            )
        };

        let search_param: Option<String> = if use_fts {
            fts_term
        } else {
            query.search.clone()
        };

        let conn = self.conn()?;
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
        self.conn()?
            .query_row(
                r#"
                SELECT
                    id, url, title, subtitle, author, date, section, body_text, clean_text,
                    word_count, difficulty, fetched_at, uploaded_to_lingq, lingq_lesson_id, lingq_lesson_url
                FROM articles
                WHERE id = ?1
                "#,
                params![id],
                map_article_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn get_all_article_urls(&self) -> Result<HashSet<String>> {
        let conn = self.conn()?;
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
        self.conn()?
            .execute("DELETE FROM articles WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn get_stats(&self) -> Result<LibraryStats> {
        let conn = self.conn()?;
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

        // Future migrations go here:
        // if current_version < 5 { ... }

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
        body_text: row.get(7)?,
        clean_text: row.get(8)?,
        word_count: row.get(9)?,
        difficulty: row.get(10)?,
        fetched_at: row.get(11)?,
        uploaded_to_lingq: row.get::<_, i64>(12)? != 0,
        lingq_lesson_id: row.get(13)?,
        lingq_lesson_url: row.get(14)?,
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
        };
        let id = db.save_article(&article).unwrap();
        assert!(id > 0);

        let stored = db.get_article(id).unwrap().unwrap();
        assert_eq!(stored.title, "Test Article");
        assert_eq!(stored.url, "https://taz.de/test/!1234/");
        assert!(!stored.uploaded_to_lingq);
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
        };
        db.save_article(&article).unwrap();

        let stats = db.get_stats().unwrap();
        assert_eq!(stats.total_articles, 1);
        assert_eq!(stats.uploaded_articles, 0);
        assert_eq!(stats.average_word_count, 5);
        assert_eq!(stats.sections.len(), 1);
        assert_eq!(stats.sections[0].section, "Sport");
    }
}
