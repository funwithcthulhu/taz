use crate::taz::Article;
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
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

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn open_default() -> Result<Self> {
        let mut base_dir = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from(r"C:\Users\Admin\AppData\Local"));
        base_dir.push("taz_lingq_tool");
        std::fs::create_dir_all(&base_dir)
            .with_context(|| format!("failed to create {}", base_dir.display()))?;

        let db_path = base_dir.join("taz_lingq_tool.db");
        Self::open(&db_path)
    }

    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("failed to open database {}", path.display()))?;
        let database = Self { conn };
        database.migrate()?;
        Ok(database)
    }

    pub fn save_article(&self, article: &Article) -> Result<i64> {
        self.conn.execute(
            r#"
            INSERT INTO articles (
                url, title, subtitle, author, date, section, body_text, clean_text, word_count, fetched_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT(url) DO UPDATE SET
                title = excluded.title,
                subtitle = excluded.subtitle,
                author = excluded.author,
                date = excluded.date,
                section = excluded.section,
                body_text = excluded.body_text,
                clean_text = excluded.clean_text,
                word_count = excluded.word_count,
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
                article.fetched_at,
            ],
        )?;

        let id: i64 = self.conn.query_row(
            "SELECT id FROM articles WHERE url = ?1",
            params![article.url],
            |row| row.get(0),
        )?;

        Ok(id)
    }

    pub fn list_articles(
        &self,
        search: Option<&str>,
        section: Option<&str>,
        only_not_uploaded: bool,
        limit: usize,
    ) -> Result<Vec<StoredArticle>> {
        let sql = r#"
            SELECT
                id, url, title, subtitle, author, date, section, body_text, clean_text,
                word_count, fetched_at, uploaded_to_lingq, lingq_lesson_id, lingq_lesson_url
            FROM articles
            WHERE (?1 IS NULL OR title LIKE '%' || ?1 || '%' OR body_text LIKE '%' || ?1 || '%')
              AND (?2 IS NULL OR section = ?2)
              AND (?3 = 0 OR uploaded_to_lingq = 0)
            ORDER BY fetched_at DESC
            LIMIT ?4
        "#;

        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(
            params![
                search,
                section,
                if only_not_uploaded { 1 } else { 0 },
                limit as i64
            ],
            map_article_row,
        )?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn get_article(&self, id: i64) -> Result<Option<StoredArticle>> {
        self.conn
            .query_row(
                r#"
                SELECT
                    id, url, title, subtitle, author, date, section, body_text, clean_text,
                    word_count, fetched_at, uploaded_to_lingq, lingq_lesson_id, lingq_lesson_url
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
        let mut stmt = self.conn.prepare("SELECT url FROM articles")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let urls = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(urls.into_iter().collect())
    }

    pub fn mark_uploaded(&self, id: i64, lesson_id: i64, lesson_url: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE articles SET uploaded_to_lingq = 1, lingq_lesson_id = ?1, lingq_lesson_url = ?2 WHERE id = ?3",
            params![lesson_id, lesson_url, id],
        )?;
        Ok(())
    }

    pub fn delete_article(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM articles WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn get_stats(&self) -> Result<LibraryStats> {
        let total_articles: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM articles", [], |row| row.get(0))?;
        let uploaded_articles: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM articles WHERE uploaded_to_lingq = 1",
            [],
            |row| row.get(0),
        )?;
        let average_word_count = self.conn.query_row(
            "SELECT COALESCE(ROUND(AVG(word_count)), 0) FROM articles",
            [],
            |row| row.get(0),
        )?;

        let mut stmt = self.conn.prepare(
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
        self.conn.execute_batch(
            r#"
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
            "#,
        )?;

        Ok(())
    }
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
        fetched_at: row.get(10)?,
        uploaded_to_lingq: row.get::<_, i64>(11)? != 0,
        lingq_lesson_id: row.get(12)?,
        lingq_lesson_url: row.get(13)?,
    })
}
