use super::{
    Article, ArticleMetadata, ArticleSummary, BrowseSectionResult, DiscoveryReport,
    DiscoverySourceKind, Section,
    extract::{
        absolute_url, author_fallback_selectors, author_selectors, collect_articles_from_document,
        detect_paywall, extract_body, extract_browse_title, extract_date,
        extract_section_from_html, extract_teaser, extract_topic_urls, first_attr, first_text,
        infer_section_from_url, looks_like_article_title, section_selectors, strip_search_suffix,
        subtitle_selectors, title_selectors,
    },
    normalize::{build_clean_text, estimate_difficulty, iso_timestamp_now, normalize_date},
    sections::SECTIONS,
};
use crate::identity::article_key_from_url;
use anyhow::{Context, Result, bail};
use log::{debug, info, warn};
use regex::Regex;
use reqwest::{Client, StatusCode};
use scraper::{Html, Selector};
use std::{
    collections::{HashSet, VecDeque},
    time::Duration,
};

const BASE_URL: &str = "https://taz.de";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/135.0.0.0 Safari/537.36";

#[derive(Clone)]
pub struct TazClient {
    client: Client,
    article_url_re: Regex,
    topic_url_re: Regex,
}

impl TazClient {
    pub fn new() -> Result<Self> {
        let client = Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .context("failed to build HTTP client")?;
        let article_url_re =
            Regex::new(r"^https://taz\.de/(?:[^/]+/)*(?:%21|!)\d+/??$").context("bad regex")?;
        let topic_url_re =
            Regex::new(r"^https://taz\.de/.*/(?:%21|!)t\d+/??$").context("bad topic regex")?;

        Ok(Self {
            client,
            article_url_re,
            topic_url_re,
        })
    }

    pub fn sections(&self) -> &'static [Section] {
        SECTIONS
    }

    pub async fn discover_new_sections(&self) -> Result<Vec<(String, String)>> {
        let html = self.client.get(BASE_URL).send().await?.text().await?;
        let document = Html::parse_document(&html);
        let nav_sel =
            Selector::parse("nav a[href]").unwrap_or_else(|_| Selector::parse("a").unwrap());
        let known_urls: std::collections::HashSet<&str> = SECTIONS.iter().map(|s| s.url).collect();
        let mut discovered = Vec::new();
        for element in document.select(&nav_sel) {
            let Some(href) = element.value().attr("href") else {
                continue;
            };
            let url = if href.starts_with('/') {
                format!("{BASE_URL}{href}")
            } else {
                href.to_owned()
            };
            if !url.starts_with("https://taz.de/") || !url.contains("!p") {
                continue;
            }
            if known_urls.contains(url.as_str()) {
                continue;
            }
            let label = element.text().collect::<String>().trim().to_owned();
            if !label.is_empty() && !discovered.iter().any(|(u, _): &(String, String)| *u == url) {
                info!("Discovered new taz section: {label} → {url}");
                discovered.push((url, label));
            }
        }
        if discovered.is_empty() {
            debug!("No new sections discovered on taz.de homepage");
        } else {
            info!(
                "Found {} section(s) not in hardcoded list",
                discovered.len()
            );
        }
        Ok(discovered)
    }

    pub fn section_by_id(&self, id: &str) -> Option<&'static Section> {
        SECTIONS.iter().find(|section| section.id == id)
    }

    pub async fn browse_section(
        &self,
        section: &Section,
        limit: usize,
    ) -> Result<Vec<ArticleSummary>> {
        Ok(self.browse_section_detailed(section, limit).await?.articles)
    }

    pub async fn browse_section_detailed(
        &self,
        section: &Section,
        limit: usize,
    ) -> Result<BrowseSectionResult> {
        let mut articles = Vec::new();
        let mut seen_articles = HashSet::new();
        let mut queued_sources = VecDeque::new();
        let mut seen_sources = HashSet::new();
        let subsection_count = self.related_sections(section).len();
        let max_sources = (limit.max(80).div_ceil(20) + subsection_count).clamp(12, 40);
        let mut report = empty_report();

        queued_sources.push_back((
            section.url.to_owned(),
            section.label.to_owned(),
            DiscoverySourceKind::Section,
        ));
        for related in self.related_sections(section) {
            queued_sources.push_back((
                related.url.to_owned(),
                related.label.to_owned(),
                DiscoverySourceKind::Subsection,
            ));
        }

        let mut first_source = true;
        while let Some((source_url, fallback_section, source_kind)) = queued_sources.pop_front() {
            if !seen_sources.insert(source_url.clone()) {
                continue;
            }
            if seen_sources.len() > max_sources || articles.len() >= limit {
                break;
            }
            if first_source {
                first_source = false;
            } else {
                tokio::time::sleep(Duration::from_millis(150)).await;
            }

            let html = self.fetch_html(&source_url).await?;
            let document = Html::parse_document(&html);
            report.record_source_visit(source_kind);

            collect_articles_from_document(
                &self.article_url_re,
                &document,
                Some(&fallback_section),
                &source_url,
                source_kind,
                limit,
                &mut seen_articles,
                &mut articles,
                &mut report,
            );

            if articles.len() >= limit {
                break;
            }

            for topic_url in extract_topic_urls(&self.article_url_re, &self.topic_url_re, &document)
            {
                if !seen_sources.contains(&topic_url) {
                    queued_sources.push_back((
                        topic_url,
                        fallback_section.clone(),
                        DiscoverySourceKind::Topic,
                    ));
                }
            }
        }

        Ok(BrowseSectionResult { articles, report })
    }

    pub async fn browse_url(
        &self,
        url: &str,
        fallback_section: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ArticleSummary>> {
        let html = self.fetch_html(url).await?;
        let document = Html::parse_document(&html);
        let mut articles = Vec::new();
        let mut seen = HashSet::new();
        collect_articles_from_document(
            &self.article_url_re,
            &document,
            fallback_section,
            url,
            DiscoverySourceKind::Section,
            limit,
            &mut seen,
            &mut articles,
            &mut empty_report(),
        );

        Ok(articles)
    }

    pub async fn fetch_article(&self, url: &str) -> Result<Article> {
        info!("Fetching article: {url}");
        let html = self.fetch_html(url).await?;
        let document = Html::parse_document(&html);

        let title = normalized_title(
            first_text(&document, title_selectors()).unwrap_or_else(|| "Untitled".to_owned()),
        );
        let subtitle = first_attr(&document, subtitle_selectors(), "content").unwrap_or_default();
        let author = first_attr(&document, author_selectors(), "content")
            .or_else(|| first_text(&document, author_fallback_selectors()))
            .unwrap_or_default();
        let date = extract_date(&document, &html, url)
            .map(|d| normalize_date(&d))
            .unwrap_or_default();
        let section = first_attr(&document, section_selectors(), "content")
            .or_else(|| extract_section_from_html(&html))
            .unwrap_or_else(|| infer_section_from_url(url));
        let paywalled = detect_paywall(&document, &html);
        let body_text = extract_body(&document)?;
        let word_count = body_text.split_whitespace().count();
        if word_count < 80 {
            bail!("article extraction produced too little text for {url}");
        }

        let clean_text = build_clean_text(&title, &subtitle, &author, &date, &body_text);
        let difficulty = estimate_difficulty(&body_text);

        Ok(Article {
            article_key: article_key_from_url(url),
            url: url.to_owned(),
            title,
            subtitle,
            author,
            date,
            section,
            body_text,
            clean_text,
            word_count,
            difficulty,
            paywalled,
            fetched_at: iso_timestamp_now(),
        })
    }

    pub async fn fetch_article_metadata(&self, url: &str) -> Result<ArticleMetadata> {
        let html = self.fetch_html(url).await?;
        let document = Html::parse_document(&html);

        let title = normalized_title(
            first_text(&document, title_selectors()).unwrap_or_else(|| "Untitled".to_owned()),
        );
        let date = extract_date(&document, &html, url)
            .map(|d| normalize_date(&d))
            .unwrap_or_default();
        let section = first_attr(&document, section_selectors(), "content")
            .or_else(|| extract_section_from_html(&html))
            .unwrap_or_else(|| infer_section_from_url(url));

        Ok(ArticleMetadata {
            article_key: article_key_from_url(url),
            url: url.to_owned(),
            title,
            date,
            section,
        })
    }

    pub async fn search_articles(
        &self,
        query: &str,
        max_pages: usize,
    ) -> Result<Vec<ArticleSummary>> {
        if query.trim().is_empty() {
            bail!("search query is empty");
        }

        let encoded_query = urlencoding::encode(query.trim());
        let mut articles = Vec::new();
        let mut seen = HashSet::new();
        let selector = Selector::parse("a[href]").unwrap();

        for page in 0..max_pages {
            let url = if page == 0 {
                format!("{BASE_URL}/!s={encoded_query}/")
            } else {
                format!("{BASE_URL}/!s={encoded_query}/?search_page={page}")
            };

            let html = self.fetch_html(&url).await?;
            let document = Html::parse_document(&html);
            let mut page_count = 0;

            for link in document.select(&selector) {
                let Some(raw_href) = link.value().attr("href") else {
                    continue;
                };

                let raw_url = absolute_url(raw_href);
                let article_url = strip_search_suffix(&raw_url);

                if !self.article_url_re.is_match(&article_url) {
                    continue;
                }
                if !seen.insert(article_url.clone()) {
                    continue;
                }

                let title = extract_browse_title(link);
                if !looks_like_article_title(&title) {
                    continue;
                }

                let teaser = extract_teaser(link);
                let section = infer_section_from_url(&article_url);

                articles.push(ArticleSummary {
                    article_key: article_key_from_url(&article_url),
                    url: article_url,
                    title,
                    teaser,
                    section,
                    source_kind: DiscoverySourceKind::Search,
                    source_label: format!("search: {query}"),
                });
                page_count += 1;
            }

            if page_count == 0 {
                break;
            }
        }

        Ok(articles)
    }

    async fn fetch_html(&self, url: &str) -> Result<String> {
        let mut last_error = None;

        for attempt in 1..=3 {
            debug!("HTTP GET {url} (attempt {attempt})");
            match self.client.get(url).send().await {
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() {
                        debug!("HTTP {status} for {url}");
                        return response
                            .text()
                            .await
                            .with_context(|| format!("network: failed to read body for {url}"));
                    }

                    let retryable = is_retryable_status(status);
                    warn!("HTTP {status} for {url} (retryable={retryable}, attempt={attempt})");
                    last_error = Some(anyhow::anyhow!(
                        "network: non-success response {} for {}",
                        status,
                        url
                    ));
                    if !retryable || attempt == 3 {
                        break;
                    }
                }
                Err(err) => {
                    warn!("HTTP request failed for {url}: {err} (attempt {attempt})");
                    last_error = Some(anyhow::anyhow!("network: request failed for {url}: {err}"));
                    if attempt == 3 {
                        break;
                    }
                }
            }

            tokio::time::sleep(Duration::from_millis(450 * attempt as u64)).await;
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("network: failed to fetch {url}")))
    }

    fn related_sections(&self, section: &Section) -> Vec<&'static Section> {
        if section.id.contains('-') {
            return Vec::new();
        }

        let prefix = format!("{}-", section.id);
        SECTIONS
            .iter()
            .filter(|candidate| candidate.id.starts_with(&prefix))
            .collect()
    }
}

fn empty_report() -> DiscoveryReport {
    DiscoveryReport {
        source_pages_visited: 0,
        section_pages_visited: 0,
        subsection_pages_visited: 0,
        topic_pages_visited: 0,
        section_articles: 0,
        subsection_articles: 0,
        topic_articles: 0,
        deduped_articles: 0,
    }
}

fn normalized_title(title: String) -> String {
    title
        .replace(" | taz.de", "")
        .replace(" | taz", "")
        .trim()
        .to_owned()
}

fn is_retryable_status(status: StatusCode) -> bool {
    status.is_server_error()
        || matches!(
            status,
            StatusCode::TOO_MANY_REQUESTS | StatusCode::REQUEST_TIMEOUT
        )
}
