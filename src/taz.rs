use anyhow::{Context, Result, bail};
use log::{debug, info, warn};
use regex::Regex;
use reqwest::{Client, StatusCode};
use scraper::{ElementRef, Html, Selector};
use std::{
    collections::{HashSet, VecDeque},
    sync::LazyLock,
    time::Duration,
};

const BASE_URL: &str = "https://taz.de";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/135.0.0.0 Safari/537.36";

/// ── Parsed CSS selectors (cached via LazyLock) ─────────────────────────────
/// These are parsed once on first use and reused for every scrape call.
mod parsed {
    use super::*;
    pub static LINKS: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse(super::selectors::LINKS).unwrap());
    pub static ARTICLE: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse(super::selectors::ARTICLE).unwrap());
    pub static BODY_ELEMENTS: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse(super::selectors::BODY_ELEMENTS).unwrap());
    pub static BODY_FALLBACK: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse(super::selectors::BODY_FALLBACK).unwrap());
    pub static HEADLINE: LazyLock<Vec<Selector>> = LazyLock::new(|| {
        super::selectors::HEADLINE
            .iter()
            .filter_map(|s| Selector::parse(s).ok())
            .collect()
    });
}

/// ── CSS selectors for taz.de article extraction ────────────────────────────
/// Centralised here so they're easy to audit and update when the site redesigns.
mod selectors {
    // Article metadata
    pub const TITLE: &[&str] = &["h1", "meta[property=\"og:title\"]", "title"];
    pub const SUBTITLE: &[&str] = &["meta[name=\"description\"]"];
    pub const AUTHOR: &[&str] = &["meta[property=\"article:author\"]"];
    pub const AUTHOR_FALLBACK: &[&str] = &["[rel=\"author\"]", ".author", "[class*=\"author\"]"];
    pub const SECTION: &[&str] = &["meta[property=\"article:section\"]"];
    pub const DATE_TIME: &[&str] = &["time[datetime]"];
    pub const DATE_META: &[&str] = &[
        "meta[property=\"article:published_time\"]",
        "meta[name=\"date\"]",
    ];

    // Article body extraction
    pub const ARTICLE: &str = "article";
    pub const BODY_ELEMENTS: &str = "p, h2, h3, li";
    pub const BODY_FALLBACK: &str = "main p";

    // Link discovery
    pub const LINKS: &str = "a[href]";

    // Browse page headline extraction
    pub const HEADLINE: &[&str] = &[".headline", "h1", "h2", "h3"];

    // Donate / boilerplate markers to strip from body text
    pub const DONATE_MARKERS: &[&str] = &[
        "Gemeinsam für freie Presse",
        "Jetzt unterstützen",
        "Diesen Artikel teilen",
        "Feedback",
        "Leser*innenkommentar",
        "Fehlerhinweis",
        "Mehr zum Thema",
    ];

    // CSS selectors that indicate a paywall
    pub const PAYWALL: &[&str] = &[
        ".paywall",
        ".paywall-overlay",
        "[data-paywall]",
        ".tzi-paywall",
        ".article-paywall",
        ".hide-paywall",
    ];

    // Text patterns in the HTML that indicate paywall-truncated content
    pub const PAYWALL_TEXT_MARKERS: &[&str] = &[
        "taz zahl ich",
        "Lesen Sie diesen Artikel mit taz-zahl-ich",
        "Dieser Artikel ist für Abonnent",
        "Für diesen Artikel müssen Sie",
        "Jetzt taz zahl ich Mitglied werden",
        "Ab jetzt frei lesen",
    ];
}

#[derive(Debug, Clone, Copy)]
pub struct Section {
    pub id: &'static str,
    pub label: &'static str,
    pub url: &'static str,
}

#[derive(Debug, Clone)]
pub struct ArticleSummary {
    pub url: String,
    pub title: String,
    pub teaser: String,
    pub section: String,
    pub source_kind: DiscoverySourceKind,
    pub source_label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoverySourceKind {
    Section,
    Subsection,
    Topic,
    Search,
}

impl DiscoverySourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Section => "section",
            Self::Subsection => "subsection",
            Self::Topic => "topic",
            Self::Search => "search",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DiscoveryReport {
    pub source_pages_visited: usize,
    pub section_pages_visited: usize,
    pub subsection_pages_visited: usize,
    pub topic_pages_visited: usize,
    pub section_articles: usize,
    pub subsection_articles: usize,
    pub topic_articles: usize,
    pub deduped_articles: usize,
}

impl DiscoveryReport {
    fn record_source_visit(&mut self, source_kind: DiscoverySourceKind) {
        self.source_pages_visited += 1;
        match source_kind {
            DiscoverySourceKind::Section => self.section_pages_visited += 1,
            DiscoverySourceKind::Subsection => self.subsection_pages_visited += 1,
            DiscoverySourceKind::Topic => self.topic_pages_visited += 1,
            DiscoverySourceKind::Search => {}
        }
    }

    fn record_article(&mut self, source_kind: DiscoverySourceKind) {
        match source_kind {
            DiscoverySourceKind::Section => self.section_articles += 1,
            DiscoverySourceKind::Subsection => self.subsection_articles += 1,
            DiscoverySourceKind::Topic => self.topic_articles += 1,
            DiscoverySourceKind::Search => {}
        }
    }
}

#[derive(Debug, Clone)]
pub struct BrowseSectionResult {
    pub articles: Vec<ArticleSummary>,
    pub report: DiscoveryReport,
}

#[derive(Debug, Clone)]
pub struct Article {
    pub url: String,
    pub title: String,
    pub subtitle: String,
    pub author: String,
    pub date: String,
    pub section: String,
    pub body_text: String,
    pub clean_text: String,
    pub word_count: usize,
    pub difficulty: i64,
    pub fetched_at: String,
    /// True if paywall markers were detected — content may be truncated.
    pub paywalled: bool,
}

#[derive(Debug, Clone)]
pub struct ArticleMetadata {
    pub url: String,
    pub title: String,
    pub date: String,
    pub section: String,
}

pub const SECTIONS: &[Section] = &[
    Section {
        id: "start",
        label: "Startseite",
        url: "https://taz.de",
    },
    Section {
        id: "oeko",
        label: "Oeko",
        url: "https://taz.de/Oeko/!p4610/",
    },
    Section {
        id: "oeko-oekologie",
        label: "Oeko - Oekologie",
        url: "https://taz.de/Oeko/Oekologie/!p4624/",
    },
    Section {
        id: "oeko-oekonomie",
        label: "Oeko - Oekonomie",
        url: "https://taz.de/Oeko/Oekonomie/!p4623/",
    },
    Section {
        id: "oeko-konsum",
        label: "Oeko - Konsum",
        url: "https://taz.de/Oeko/Konsum/!p4625/",
    },
    Section {
        id: "oeko-netzoekonomie",
        label: "Oeko - Netzoekonomie",
        url: "https://taz.de/Oeko/Netzoekonomie/!p4627/",
    },
    Section {
        id: "oeko-verkehr",
        label: "Oeko - Verkehr",
        url: "https://taz.de/Oeko/Verkehr/!p4628/",
    },
    Section {
        id: "oeko-arbeit",
        label: "Oeko - Arbeit",
        url: "https://taz.de/Oeko/Arbeit/!p4629/",
    },
    Section {
        id: "oeko-wissenschaft",
        label: "Oeko - Wissenschaft",
        url: "https://taz.de/Oeko/Wissenschaft/!p4636/",
    },
    Section {
        id: "politik",
        label: "Politik",
        url: "https://taz.de/Politik/!p4615/",
    },
    Section {
        id: "politik-deutschland",
        label: "Politik - Deutschland",
        url: "https://taz.de/Politik/Deutschland/!p4616/",
    },
    Section {
        id: "politik-europa",
        label: "Politik - Europa",
        url: "https://taz.de/Politik/Europa/!p4617/",
    },
    Section {
        id: "politik-amerika",
        label: "Politik - Amerika",
        url: "https://taz.de/Politik/Amerika/!p4618/",
    },
    Section {
        id: "politik-asien",
        label: "Politik - Asien",
        url: "https://taz.de/Politik/Asien/!p4619/",
    },
    Section {
        id: "politik-nahost",
        label: "Politik - Nahost",
        url: "https://taz.de/Politik/Nahost/!p4620/",
    },
    Section {
        id: "politik-afrika",
        label: "Politik - Afrika",
        url: "https://taz.de/Politik/Afrika/!p4621/",
    },
    Section {
        id: "politik-netzpolitik",
        label: "Politik - Netzpolitik",
        url: "https://taz.de/Politik/Netzpolitik/!p4622/",
    },
    Section {
        id: "gesellschaft",
        label: "Gesellschaft",
        url: "https://taz.de/Gesellschaft/!p4611/",
    },
    Section {
        id: "gesellschaft-medien",
        label: "Gesellschaft - Medien",
        url: "https://taz.de/Gesellschaft/Medien/!p4630/",
    },
    Section {
        id: "gesellschaft-alltag",
        label: "Gesellschaft - Alltag",
        url: "https://taz.de/Gesellschaft/Alltag/!p4632/",
    },
    Section {
        id: "gesellschaft-debatte",
        label: "Gesellschaft - Debatte",
        url: "https://taz.de/Gesellschaft/Debatte/!p4633/",
    },
    Section {
        id: "gesellschaft-kolumnen",
        label: "Gesellschaft - Kolumnen",
        url: "https://taz.de/Gesellschaft/Kolumnen/!p4634/",
    },
    Section {
        id: "gesellschaft-bildung",
        label: "Gesellschaft - Bildung",
        url: "https://taz.de/Gesellschaft/Bildung/!p4635/",
    },
    Section {
        id: "gesellschaft-gesundheit",
        label: "Gesellschaft - Gesundheit",
        url: "https://taz.de/Gesellschaft/Gesundheit/!p4637/",
    },
    Section {
        id: "gesellschaft-reise",
        label: "Gesellschaft - Reise",
        url: "https://taz.de/Gesellschaft/Reise/!p4638/",
    },
    Section {
        id: "gesellschaft-reportage",
        label: "Gesellschaft - Reportage und Recherche",
        url: "https://taz.de/Gesellschaft/Reportage-und-Recherche/!p5265/",
    },
    Section {
        id: "wirtschaft",
        label: "Wirtschaft",
        url: "https://taz.de/Wirtschaft/!t5008636/",
    },
    Section {
        id: "kultur",
        label: "Kultur",
        url: "https://taz.de/Kultur/!p4639/",
    },
    Section {
        id: "kultur-musik",
        label: "Kultur - Musik",
        url: "https://taz.de/Kultur/Musik/!p4640/",
    },
    Section {
        id: "kultur-film",
        label: "Kultur - Film",
        url: "https://taz.de/Kultur/Film/!p4641/",
    },
    Section {
        id: "kultur-kuenste",
        label: "Kultur - Kuenste",
        url: "https://taz.de/Kultur/Kuenste/!p4642/",
    },
    Section {
        id: "kultur-buch",
        label: "Kultur - Buch",
        url: "https://taz.de/Kultur/Buch/!p4643/",
    },
    Section {
        id: "kultur-netzkultur",
        label: "Kultur - Netzkultur",
        url: "https://taz.de/Kultur/Netzkultur/!p4631/",
    },
    Section {
        id: "wahrheit",
        label: "Wahrheit",
        url: "https://taz.de/Wahrheit/!p4644/",
    },
    Section {
        id: "sport",
        label: "Sport",
        url: "https://taz.de/Sport/!p4646/",
    },
    Section {
        id: "sport-kolumnen",
        label: "Sport - Kolumnen",
        url: "https://taz.de/Sport/Kolumnen/!p4648/",
    },
    Section {
        id: "berlin",
        label: "Berlin",
        url: "https://taz.de/Berlin/!p4649/",
    },
    Section {
        id: "nord",
        label: "Nord",
        url: "https://taz.de/Nord/!p4650/",
    },
    Section {
        id: "nord-hamburg",
        label: "Nord - Hamburg",
        url: "https://taz.de/Nord/Hamburg/!p4651/",
    },
    Section {
        id: "nord-bremen",
        label: "Nord - Bremen",
        url: "https://taz.de/Nord/Bremen/!p4652/",
    },
    Section {
        id: "nord-kultur",
        label: "Nord - Kultur",
        url: "https://taz.de/Nord/Kultur/!p4653/",
    },
    Section {
        id: "archiv",
        label: "Archiv",
        url: "https://taz.de/Archiv/!p4311/",
    },
];

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
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(10))
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

    /// Scrape the taz.de homepage navigation and log any sections not in the
    /// hardcoded list. Returns the URLs of any newly discovered nav links.
    pub async fn discover_new_sections(&self) -> Result<Vec<(String, String)>> {
        let html = self
            .client
            .get("https://taz.de")
            .send()
            .await?
            .text()
            .await?;
        let document = Html::parse_document(&html);
        let nav_sel =
            Selector::parse("nav a[href]").unwrap_or_else(|_| Selector::parse("a").unwrap());
        let known_urls: std::collections::HashSet<&str> =
            SECTIONS.iter().map(|s| s.url).collect();
        let mut discovered = Vec::new();
        for element in document.select(&nav_sel) {
            let Some(href) = element.value().attr("href") else {
                continue;
            };
            let url = if href.starts_with('/') {
                format!("https://taz.de{href}")
            } else {
                href.to_owned()
            };
            if !url.starts_with("https://taz.de/") {
                continue;
            }
            // Skip article URLs, only want section/category pages (contain !p)
            if !url.contains("!p") {
                continue;
            }
            if known_urls.contains(url.as_str()) {
                continue;
            }
            let label = element.text().collect::<String>().trim().to_owned();
            if !label.is_empty() && !discovered.iter().any(|(u, _): &(String, String)| *u == url) {
                log::info!("Discovered new taz section: {label} → {url}");
                discovered.push((url, label));
            }
        }
        if discovered.is_empty() {
            log::debug!("No new sections discovered on taz.de homepage");
        } else {
            log::info!(
                "Found {} section(s) not in hardcoded list",
                discovered.len()
            );
        }
        Ok(discovered)
    }

    pub fn section_by_id(&self, id: &str) -> Option<&'static Section> {
        SECTIONS.iter().find(|section| section.id == id)
    }

    pub async fn browse_section(&self, section: &Section, limit: usize) -> Result<Vec<ArticleSummary>> {
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
        let mut report = DiscoveryReport {
            source_pages_visited: 0,
            section_pages_visited: 0,
            subsection_pages_visited: 0,
            topic_pages_visited: 0,
            section_articles: 0,
            subsection_articles: 0,
            topic_articles: 0,
            deduped_articles: 0,
        };

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
            // Throttle between source page fetches to avoid hammering taz.de
            if first_source {
                first_source = false;
            } else {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            }

            let html = self.fetch_html(&source_url).await?;
            let document = Html::parse_document(&html);
            report.record_source_visit(source_kind);

            self.collect_articles_from_document(
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

            for topic_url in self.extract_topic_urls(&document) {
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
        self.collect_articles_from_document(
            &document,
            fallback_section,
            url,
            DiscoverySourceKind::Section,
            limit,
            &mut seen,
            &mut articles,
            &mut DiscoveryReport {
                source_pages_visited: 0,
                section_pages_visited: 0,
                subsection_pages_visited: 0,
                topic_pages_visited: 0,
                section_articles: 0,
                subsection_articles: 0,
                topic_articles: 0,
                deduped_articles: 0,
            },
        );

        Ok(articles)
    }

    pub async fn fetch_article(&self, url: &str) -> Result<Article> {
        info!("Fetching article: {url}");
        let html = self.fetch_html(url).await?;
        let document = Html::parse_document(&html);

        let title = first_text(&document, selectors::TITLE)
            .map(|value| {
                value
                    .replace(" | taz.de", "")
                    .replace(" | taz", "")
                    .trim()
                    .to_owned()
            })
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "Untitled".to_owned());

        let subtitle =
            first_attr(&document, selectors::SUBTITLE, "content").unwrap_or_default();

        let author = first_attr(&document, selectors::AUTHOR, "content")
            .or_else(|| {
                first_text(&document, selectors::AUTHOR_FALLBACK)
            })
            .unwrap_or_default();

        let date = extract_date(&document, &html, url)
            .map(|d| normalize_date(&d))
            .unwrap_or_default();

        let section = first_attr(&document, selectors::SECTION, "content")
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

        let title = first_text(&document, selectors::TITLE)
            .map(|value| {
                value
                    .replace(" | taz.de", "")
                    .replace(" | taz", "")
                    .trim()
                    .to_owned()
            })
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "Untitled".to_owned());

        let date = extract_date(&document, &html, url)
            .map(|d| normalize_date(&d))
            .unwrap_or_default();
        let section = first_attr(&document, selectors::SECTION, "content")
        .or_else(|| extract_section_from_html(&html))
        .unwrap_or_else(|| infer_section_from_url(url));

        Ok(ArticleMetadata {
            url: url.to_owned(),
            title,
            date,
            section,
        })
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

    fn collect_articles_from_document(
        &self,
        document: &Html,
        fallback_section: Option<&str>,
        source_url: &str,
        source_kind: DiscoverySourceKind,
        limit: usize,
        seen: &mut HashSet<String>,
        articles: &mut Vec<ArticleSummary>,
        report: &mut DiscoveryReport,
    ) {
        let selector = parsed::LINKS.clone();

        for link in document.select(&selector) {
            let Some(raw_href) = link.value().attr("href") else {
                continue;
            };

            let article_url = absolute_url(raw_href);
            if !self.article_url_re.is_match(&article_url) {
                continue;
            }
            if !seen.insert(article_url.clone()) {
                report.deduped_articles += 1;
                continue;
            }

            let title = self.extract_browse_title(link);
            if !looks_like_article_title(&title) {
                continue;
            }

            let teaser = self.extract_teaser(link);
            let section = fallback_section
                .map(str::to_owned)
                .unwrap_or_else(|| infer_section_from_url(&article_url));

            articles.push(ArticleSummary {
                url: article_url,
                title,
                teaser,
                section,
                source_kind,
                source_label: source_label(source_url),
            });
            report.record_article(source_kind);

            if articles.len() >= limit {
                break;
            }
        }
    }

    fn extract_topic_urls(&self, document: &Html) -> Vec<String> {
        let selector = parsed::LINKS.clone();
        let mut urls = Vec::new();
        let mut seen = HashSet::new();

        for link in document.select(&selector) {
            let Some(raw_href) = link.value().attr("href") else {
                continue;
            };

            let url = absolute_url(raw_href);
            if !self.topic_url_re.is_match(&url) {
                continue;
            }
            if self.article_url_re.is_match(&url) || !seen.insert(url.clone()) {
                continue;
            }

            urls.push(url);
        }

        urls
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

    fn extract_teaser(&self, link: ElementRef<'_>) -> String {
        let title = self.extract_browse_title(link);
        let mut parent = link.parent();
        for _ in 0..3 {
            let Some(node) = parent else {
                break;
            };

            if let Some(value) = ElementRef::wrap(node) {
                let text = strip_markup(&clean_whitespace(&collect_text(value)));
                if text.len() > title.len() && text.len() > 20 {
                    return trim_chars(&text, 220);
                }
            }

            parent = node.parent();
        }

        String::new()
    }

    fn extract_browse_title(&self, link: ElementRef<'_>) -> String {
        let mut parent = link.parent();

        for _ in 0..2 {
            let Some(node) = parent else {
                break;
            };

            if let Some(element) = ElementRef::wrap(node) {
                for selector in parsed::HEADLINE.iter() {
                    if let Some(candidate) = element
                        .select(selector)
                        .map(collect_text)
                        .map(|text| clean_whitespace(&text))
                        .find(|text| looks_like_article_title(text))
                    {
                        return candidate;
                    }
                }
            }

            parent = node.parent();
        }

        clean_whitespace(&collect_text(link))
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
        let selector = parsed::LINKS.clone();

        for page in 0..max_pages {
            let url = if page == 0 {
                format!("{BASE_URL}/!s={encoded_query}/")
            } else {
                format!(
                    "{BASE_URL}/!s={encoded_query}/?search_page={page}"
                )
            };

            let html = self.fetch_html(&url).await?;
            let document = Html::parse_document(&html);
            let mut page_count = 0;

            for link in document.select(&selector) {
                let Some(raw_href) = link.value().attr("href") else {
                    continue;
                };

                let raw_url = absolute_url(raw_href);
                // Search result URLs have &s=Query suffix — strip it
                let article_url = strip_search_suffix(&raw_url);

                if !self.article_url_re.is_match(&article_url) {
                    continue;
                }
                if !seen.insert(article_url.clone()) {
                    continue;
                }

                let title = self.extract_browse_title(link);
                if !looks_like_article_title(&title) {
                    continue;
                }

                let teaser = self.extract_teaser(link);
                let section = infer_section_from_url(&article_url);

                articles.push(ArticleSummary {
                    url: article_url,
                    title,
                    teaser,
                    section,
                    source_kind: DiscoverySourceKind::Search,
                    source_label: format!("search: {query}"),
                });
                page_count += 1;
            }

            // If a page yielded no articles, we've exhausted results
            if page_count == 0 {
                break;
            }
        }

        Ok(articles)
    }
}

fn strip_search_suffix(url: &str) -> String {
    // Search result URLs look like: https://taz.de/Title/!123456&s=Query/
    // Strip the &s=... portion to get the clean article URL
    if let Some(pos) = url.find("&s=") {
        let before = &url[..pos];
        // Preserve trailing slash if present after the &s=... part
        if url.ends_with('/') {
            format!("{before}/")
        } else {
            before.to_owned()
        }
    } else {
        url.to_owned()
    }
}

fn is_retryable_status(status: StatusCode) -> bool {
    status.is_server_error()
        || matches!(
            status,
            StatusCode::TOO_MANY_REQUESTS | StatusCode::REQUEST_TIMEOUT
        )
}

/// Check whether the page contains paywall indicators (CSS elements or text markers).
fn detect_paywall(document: &Html, html: &str) -> bool {
    // Check for paywall CSS selectors
    for selector_str in selectors::PAYWALL {
        let Ok(selector) = Selector::parse(selector_str) else { continue };
        if document.select(&selector).next().is_some() {
            return true;
        }
    }
    // Check for paywall text markers in raw HTML (faster than parsing)
    let html_lower = html.to_lowercase();
    for marker in selectors::PAYWALL_TEXT_MARKERS {
        if html_lower.contains(&marker.to_lowercase()) {
            return true;
        }
    }
    false
}

/// Tags whose `<p>` / `<li>` descendants should be excluded from the article
/// body.  These typically contain image captions, author bios, or sidebar
/// info-boxes that are *inside* the `<article>` element but are not part of the
/// running text.
const EXCLUDE_ANCESTOR_TAGS: &[&str] = &["figure", "figcaption", "aside"];

/// CSS class substrings on ancestor `<div>` / `<section>` elements that mark a
/// region as non-body content (e.g. author bio boxes, info-boxes).
const EXCLUDE_ANCESTOR_CLASSES: &[&str] = &["webelement_bio", "webelement_info", "author-container"];

/// Minimum character length for an interview-style question/answer label
/// (e.g. "taz: Wie war das?" or "Reynolds: Ja.").  These are intentionally
/// short and would be dropped by the normal 45-char paragraph filter.
const INTERVIEW_MIN_LEN: usize = 6;

/// Prefixes that mark a paragraph as an interview question or speaker label.
/// Matched case-insensitively against the start of the cleaned text.
const INTERVIEW_PREFIXES: &[&str] = &["taz:", "taz :", "taz :"];

/// Check whether `node` has any ancestor whose tag name is in
/// `EXCLUDE_ANCESTOR_TAGS` or whose CSS class contains one of the
/// `EXCLUDE_ANCESTOR_CLASSES` substrings.
fn has_excluded_ancestor(node: &ElementRef<'_>) -> bool {
    // Walk up via the underlying ego-tree node references.
    let mut current = node.parent();
    while let Some(parent_ref) = current {
        if let Some(element) = parent_ref.value().as_element() {
            let tag = element.name();
            if EXCLUDE_ANCESTOR_TAGS.contains(&tag) {
                return true;
            }
            if let Some(classes) = element.attr("class") {
                if EXCLUDE_ANCESTOR_CLASSES.iter().any(|c| classes.contains(c)) {
                    return true;
                }
            }
        }
        current = parent_ref.parent();
    }
    false
}

/// Return `true` when the paragraph looks like an interview speaker label —
/// either an exact known prefix ("taz:") or any `Name:` / `Name Name:` pattern
/// at the start of the text (common for the interviewee's answers).
fn is_interview_line(text: &str) -> bool {
    let lower = text.to_lowercase();
    if INTERVIEW_PREFIXES.iter().any(|p| lower.starts_with(p)) {
        return true;
    }
    // Detect "SomeName:" or "First Last:" at the start (answer lines).
    // Pattern: 1-3 capitalised words followed by a colon within the first 40 chars.
    if let Some(colon_pos) = text[..text.len().min(40)].find(':') {
        let prefix = &text[..colon_pos];
        let words: Vec<&str> = prefix.split_whitespace().collect();
        if (1..=3).contains(&words.len())
            && words.iter().all(|w| {
                w.chars().next().map_or(false, |c| c.is_uppercase())
                    && w.chars().all(|c| c.is_alphabetic())
            })
        {
            return true;
        }
    }
    false
}

fn extract_body(document: &Html) -> Result<String> {
    let article_selector = parsed::ARTICLE.clone();
    let paragraph_selector = parsed::BODY_ELEMENTS.clone();
    let donate_markers = selectors::DONATE_MARKERS;

    let mut best_blocks = Vec::new();

    for article in document.select(&article_selector) {
        let mut blocks = Vec::new();

        for node in article.select(&paragraph_selector) {
            // Skip elements nested inside figure/figcaption/aside
            if has_excluded_ancestor(&node) {
                continue;
            }

            let name = node.value().name();
            let text = clean_whitespace(&collect_text(node));
            if text.is_empty() || donate_markers.iter().any(|marker| text.contains(marker)) {
                continue;
            }

            match name {
                "h2" | "h3" if text.len() >= 4 => blocks.push(format!("## {text}")),
                "li" if text.len() >= 20 => blocks.push(format!("- {text}")),
                "p" => {
                    // Keep short paragraphs that are interview Q&A labels
                    if text.len() >= 45
                        || (text.len() >= INTERVIEW_MIN_LEN && is_interview_line(&text))
                    {
                        blocks.push(text);
                    }
                }
                _ => {}
            }
        }

        if blocks.len() > best_blocks.len() {
            best_blocks = blocks;
        }
    }

    if best_blocks.is_empty() {
        let fallback_selector = parsed::BODY_FALLBACK.clone();
        for node in document.select(&fallback_selector) {
            let text = clean_whitespace(&collect_text(node));
            if text.len() >= 45 {
                best_blocks.push(text);
            }
        }
    }

    dedupe_lines(&mut best_blocks);

    if best_blocks.is_empty() {
        bail!("could not extract article body");
    }

    Ok(best_blocks.join("\n\n"))
}

fn dedupe_lines(lines: &mut Vec<String>) {
    let mut seen = HashSet::new();
    lines.retain(|line| {
        let key = trim_chars(line, 120).to_lowercase();
        seen.insert(key)
    });
}

static RE_SECTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"cp:\s*"Redaktion/([^/",]+)"#).expect("section regex"));

fn extract_section_from_html(html: &str) -> Option<String> {
    RE_SECTION
        .captures(html)
        .and_then(|captures| captures.get(1))
        .map(|value| clean_whitespace(value.as_str()))
}

fn extract_date(document: &Html, html: &str, url: &str) -> Option<String> {
    first_attr(document, selectors::DATE_TIME, "datetime")
        .or_else(|| first_attr(document, selectors::DATE_META, "content"))
        .or_else(|| extract_date_from_json_ld(html))
        .or_else(|| extract_date_from_text(html))
        .or_else(|| extract_date_from_url(url))
}

static RE_DATE_JSON_LD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""datePublished"\s*:\s*"([^"]+)""#).expect("json-ld date regex"));
static RE_DATE_TEXT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(\d{1,2}\.\d{1,2}\.\d{4})\b").expect("text date regex"));
static RE_DATE_URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/(\d{4})/(\d{2})/(\d{2})/").expect("url date regex"));

fn extract_date_from_json_ld(html: &str) -> Option<String> {
    RE_DATE_JSON_LD
        .captures(html)
        .and_then(|captures| captures.get(1))
        .map(|value| clean_whitespace(value.as_str()))
}

fn extract_date_from_text(html: &str) -> Option<String> {
    RE_DATE_TEXT
        .captures(html)
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str().to_owned())
}

fn extract_date_from_url(url: &str) -> Option<String> {
    let captures = RE_DATE_URL.captures(url)?;
    Some(format!(
        "{}-{}-{}",
        captures.get(1)?.as_str(),
        captures.get(2)?.as_str(),
        captures.get(3)?.as_str()
    ))
}

fn source_label(source_url: &str) -> String {
    if source_url == BASE_URL || source_url.trim_end_matches('/') == BASE_URL {
        return "Startseite".to_owned();
    }

    let path = source_url.trim_start_matches(BASE_URL).trim_matches('/');

    if path.is_empty() {
        "Startseite".to_owned()
    } else {
        path.to_owned()
    }
}

fn build_clean_text(title: &str, subtitle: &str, author: &str, date: &str, body: &str) -> String {
    let normalized_subtitle = clean_whitespace(subtitle);
    let normalized_body = normalize_body_for_lingq(body, title, &normalized_subtitle);

    let mut pieces = vec![title.to_owned()];
    if !normalized_subtitle.is_empty() && !same_enough(&normalized_subtitle, title) {
        pieces.push(String::new());
        pieces.push(normalized_subtitle);
    }
    if !author.is_empty() {
        pieces.push(format!("Von {author}"));
    }
    if !date.is_empty() {
        pieces.push(date.to_owned());
    }
    pieces.push(String::new());
    pieces.push(normalized_body);
    pieces.join("\n")
}

fn normalize_body_for_lingq(body: &str, title: &str, subtitle: &str) -> String {
    let mut cleaned_blocks = Vec::new();
    // Pre-compute canonical forms to avoid recalculating per block
    let canon_title = canonical_text(title);
    let canon_subtitle = canonical_text(subtitle);

    for raw_block in body.split("\n\n") {
        let block = clean_whitespace(raw_block);
        if block.is_empty() {
            continue;
        }

        let normalized_block = if let Some(heading) = block.strip_prefix("## ") {
            heading.trim().to_owned()
        } else {
            block
        };

        let canon_block = canonical_text(&normalized_block);
        if matches_title_or_subtitle(&canon_block, &canon_title, &canon_subtitle) {
            continue;
        }

        cleaned_blocks.push(normalized_block);
    }

    dedupe_similar_blocks(&mut cleaned_blocks);
    drop_intro_like_duplicates(&mut cleaned_blocks, title, subtitle);
    cleaned_blocks.join("\n\n")
}

fn dedupe_similar_blocks(blocks: &mut Vec<String>) {
    let mut seen: HashSet<String> = HashSet::new();
    blocks.retain(|block| {
        let canonical = canonical_text(block);
        if seen.contains(&canonical) {
            return false;
        }

        let duplicate = seen
            .iter()
            .any(|existing| near_duplicate_text(existing, &canonical));
        if duplicate {
            return false;
        }

        seen.insert(canonical)
    });
}

fn canonical_text(value: &str) -> String {
    let collapsed: String = value
        .chars()
        .filter(|ch| ch.is_alphanumeric() || ch.is_whitespace())
        .collect();
    let mut result = String::with_capacity(collapsed.len());
    for word in collapsed.split_whitespace() {
        if !result.is_empty() {
            result.push(' ');
        }
        result.push_str(word);
    }
    result.to_lowercase()
}

/// Check if a block's canonical form matches title or subtitle (already canonicalized).
fn matches_title_or_subtitle(canon_block: &str, canon_title: &str, canon_subtitle: &str) -> bool {
    if !canon_block.is_empty() {
        // Same as title?
        if canon_block == canon_title {
            return true;
        }
        // Overlaps title?
        if overlaps_canonical(canon_block, canon_title) {
            return true;
        }
        // Same as or overlaps subtitle?
        if !canon_subtitle.is_empty()
            && (canon_block == canon_subtitle || overlaps_canonical(canon_block, canon_subtitle))
        {
            return true;
        }
    }
    false
}

fn same_enough(left: &str, right: &str) -> bool {
    let left = canonical_text(left);
    let right = canonical_text(right);
    !left.is_empty() && left == right
}

fn overlaps_enough(left: &str, right: &str) -> bool {
    overlaps_canonical(&canonical_text(left), &canonical_text(right))
}

/// Check overlap between two already-canonicalized strings.
fn overlaps_canonical(left: &str, right: &str) -> bool {
    if left.is_empty() || right.is_empty() {
        return false;
    }
    let shorter = left.len().min(right.len());
    if shorter < 40 {
        return false;
    }
    left.contains(right) || right.contains(left)
}

fn near_duplicate_text(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }

    let shorter = left.len().min(right.len());
    if shorter < 80 {
        return false;
    }

    let prefix = shorter.min(180);
    trim_chars(left, prefix) == trim_chars(right, prefix)
}

fn drop_intro_like_duplicates(blocks: &mut Vec<String>, title: &str, subtitle: &str) {
    if blocks.is_empty() {
        return;
    }

    let first = blocks[0].clone();
    if overlaps_enough(&first, title) || (!subtitle.is_empty() && overlaps_enough(&first, subtitle))
    {
        blocks.remove(0);
        return;
    }

    if blocks.len() >= 2 {
        let second = blocks[1].clone();
        if near_duplicate_text(&canonical_text(&first), &canonical_text(&second)) {
            blocks.remove(1);
        }
    }
}

fn first_text(document: &Html, selectors: &[&str]) -> Option<String> {
    for selector in selectors {
        let Ok(selector) = Selector::parse(selector) else {
            continue;
        };
        let value = document.select(&selector).find_map(|node| {
            let attr_content = node.value().attr("content").map(clean_whitespace);
            let text_content =
                Some(clean_whitespace(&collect_text(node))).filter(|value| !value.is_empty());
            attr_content.or(text_content)
        });
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            return Some(value);
        }
    }
    None
}

fn first_attr(document: &Html, selectors: &[&str], attr: &str) -> Option<String> {
    for selector in selectors {
        let Ok(selector) = Selector::parse(selector) else {
            continue;
        };
        if let Some(value) = document
            .select(&selector)
            .find_map(|node| node.value().attr(attr))
            .map(clean_whitespace)
            .filter(|value| !value.is_empty())
        {
            return Some(value);
        }
    }
    None
}

fn looks_like_article_title(title: &str) -> bool {
    title.len() >= 15
        && title.len() <= 220
        && !title.starts_with("Jetzt unterstützen")
        && !title.starts_with("Fehlerhinweis")
        && !title.starts_with("Diesen Artikel teilen")
}

fn infer_section_from_url(url: &str) -> String {
    let path = url
        .trim_start_matches(BASE_URL)
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or("taz");

    if path.starts_with('!') || path.is_empty() {
        "Startseite".to_owned()
    } else {
        path.replace('-', " ")
    }
}

fn absolute_url(raw_href: &str) -> String {
    if raw_href.starts_with("http://") || raw_href.starts_with("https://") {
        return raw_href.to_owned();
    }

    if raw_href.starts_with('/') {
        return format!("{BASE_URL}{raw_href}");
    }

    format!("{BASE_URL}/{raw_href}")
}

fn collect_text(node: ElementRef<'_>) -> String {
    node.text().collect::<String>()
}

static RE_PUNCTUATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s+([,;:.!?)])").expect("punctuation regex"));
static RE_OPENING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([(\[])\s+").expect("opening punctuation regex"));
static RE_QUOTE_SPACING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"([–—-])(["„‚»])"#).expect("dash quote spacing regex"));
static RE_QUOTE_OPENING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(["„‚«])\s+"#).expect("quote opening spacing regex"));
static RE_SPLIT_WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b([A-Za-zÄÖÜäöüß]) ([a-zäöüß]{2,})\b").expect("split word regex"));
static RE_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<[^>]+>").expect("tag regex"));

fn clean_whitespace(input: &str) -> String {
    let cleaned = input
        .replace(
            [
                '\u{00ad}', '\u{200b}', '\u{200c}', '\u{200d}', '\u{2060}', '\u{feff}',
            ],
            "",
        )
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    let cleaned = RE_PUNCTUATION.replace_all(&cleaned, "$1").into_owned();
    let cleaned = RE_OPENING.replace_all(&cleaned, "$1").into_owned();
    let cleaned = RE_QUOTE_SPACING.replace_all(&cleaned, "$1 $2").into_owned();
    let cleaned = RE_QUOTE_OPENING.replace_all(&cleaned, "$1").into_owned();
    RE_SPLIT_WORD.replace_all(&cleaned, "$1$2").into_owned()
}

fn strip_markup(input: &str) -> String {
    clean_whitespace(&RE_TAG.replace_all(input, " "))
}

fn trim_chars(input: &str, max: usize) -> String {
    input.chars().take(max).collect()
}

fn iso_timestamp_now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Normalize a date string from any taz.de format into YYYY-MM-DD.
/// Handles ISO timestamps (2025-03-24T10:00:00+01:00), German dates (24.3.2025),
/// and plain YYYY-MM-DD. Returns the original string if parsing fails.
fn normalize_date(input: &str) -> String {
    let trimmed = input.trim();
    // Try YYYY-MM-DD (already normalized)
    if chrono::NaiveDate::parse_from_str(trimmed, "%Y-%m-%d").is_ok() && trimmed.len() == 10 {
        return trimmed.to_owned();
    }
    // Try ISO timestamp prefix (first 10 chars = YYYY-MM-DD)
    if trimmed.len() >= 10 {
        if let Ok(date) = chrono::NaiveDate::parse_from_str(&trimmed[..10], "%Y-%m-%d") {
            return date.format("%Y-%m-%d").to_string();
        }
    }
    // Try German date format (d.m.YYYY or dd.mm.YYYY)
    if let Ok(date) = chrono::NaiveDate::parse_from_str(trimmed, "%d.%m.%Y") {
        return date.format("%Y-%m-%d").to_string();
    }
    // Fallback: return as-is
    trimmed.to_owned()
}

/// Estimate article reading difficulty on a 1-5 scale for German text.
///
/// Heuristics used:
/// - Average sentence length (words per sentence)
/// - Average word length (characters per word)
/// - Proportion of long words (>= 10 chars, common in German compounds)
///
/// Returns a score from 1 (easiest) to 5 (hardest).
pub fn estimate_difficulty(body_text: &str) -> i64 {
    let words: Vec<&str> = body_text.split_whitespace().collect();
    if words.len() < 20 {
        return 3; // default for very short texts
    }

    // Count sentences (split on . ! ? followed by whitespace or end)
    let sentence_count = body_text
        .chars()
        .zip(body_text.chars().skip(1).chain(std::iter::once(' ')))
        .filter(|(ch, next)| matches!(ch, '.' | '!' | '?') && (next.is_whitespace() || *next == '"'))
        .count()
        .max(1);

    let avg_sentence_len = words.len() as f64 / sentence_count as f64;
    let avg_word_len = words.iter().map(|w| w.chars().count()).sum::<usize>() as f64 / words.len() as f64;
    let long_word_ratio = words.iter().filter(|w| w.chars().count() >= 10).count() as f64 / words.len() as f64;

    // Weighted score: each metric contributes a 0.0–1.0 normalized component.
    // Thresholds tuned for German newspaper text (taz articles).
    let sentence_score = ((avg_sentence_len - 8.0) / 20.0).clamp(0.0, 1.0);
    let word_len_score = ((avg_word_len - 4.0) / 4.0).clamp(0.0, 1.0);
    let long_word_score = (long_word_ratio / 0.25).clamp(0.0, 1.0);

    let combined = sentence_score * 0.4 + word_len_score * 0.3 + long_word_score * 0.3;
    let level = (combined * 4.0 + 1.0).round() as i64;
    level.clamp(1, 5)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── strip_search_suffix ──

    #[test]
    fn strip_search_suffix_removes_query_param() {
        assert_eq!(
            strip_search_suffix("https://taz.de/Title/!123456&s=Query/"),
            "https://taz.de/Title/!123456/"
        );
    }

    #[test]
    fn strip_search_suffix_no_trailing_slash() {
        assert_eq!(
            strip_search_suffix("https://taz.de/Title/!123456&s=Query"),
            "https://taz.de/Title/!123456"
        );
    }

    #[test]
    fn strip_search_suffix_no_match() {
        let url = "https://taz.de/Title/!123456/";
        assert_eq!(strip_search_suffix(url), url);
    }

    // ── normalize_date ──

    #[test]
    fn normalize_date_already_normalized() {
        assert_eq!(normalize_date("2025-03-24"), "2025-03-24");
    }

    #[test]
    fn normalize_date_iso_timestamp() {
        assert_eq!(normalize_date("2025-03-24T10:00:00+01:00"), "2025-03-24");
    }

    #[test]
    fn normalize_date_german_format() {
        assert_eq!(normalize_date("24.03.2025"), "2025-03-24");
    }

    #[test]
    fn normalize_date_unparseable_returns_as_is() {
        assert_eq!(normalize_date("not a date"), "not a date");
    }

    #[test]
    fn normalize_date_whitespace_trimmed() {
        assert_eq!(normalize_date("  2025-03-24  "), "2025-03-24");
    }

    // ── clean_whitespace ──

    #[test]
    fn clean_whitespace_collapses_spaces() {
        assert_eq!(clean_whitespace("hello   world"), "hello world");
    }

    #[test]
    fn clean_whitespace_strips_zero_width_chars() {
        assert_eq!(clean_whitespace("hel\u{200b}lo"), "hello");
    }

    #[test]
    fn clean_whitespace_fixes_punctuation_spacing() {
        // Space before period should be removed
        assert_eq!(clean_whitespace("hello ."), "hello.");
    }

    // ── estimate_difficulty ──

    #[test]
    fn estimate_difficulty_short_text_defaults_to_3() {
        assert_eq!(estimate_difficulty("Ein paar Wörter."), 3);
    }

    #[test]
    fn estimate_difficulty_returns_1_to_5() {
        // Simple sentences with short words
        let easy = "Das ist gut. Er hat Spaß. Sie mag das. Wir sind da. ".repeat(10);
        let score = estimate_difficulty(&easy);
        assert!((1..=5).contains(&score), "score {score} out of range");
    }

    #[test]
    fn estimate_difficulty_complex_text_scores_higher() {
        let easy = "Das ist gut. Er ist da. Sie mag es. Wir auch. ".repeat(10);
        let hard = "Die Bundesverfassungsgerichtsentscheidung über die Grundgesetzänderung zur Arbeitnehmerüberlassungsgesetzgebung wird weitreichende Konsequenzen haben. Die Verwaltungsgerichtsbarkeit prüft die Verhältnismäßigkeit der Maßnahmen zur Bekämpfung der Umweltverschmutzung. ".repeat(5);
        assert!(
            estimate_difficulty(&hard) > estimate_difficulty(&easy),
            "complex German text should score higher"
        );
    }

    // ── strip_markup ──

    #[test]
    fn strip_markup_removes_tags() {
        assert_eq!(strip_markup("<b>bold</b> text"), "bold text");
    }

    #[test]
    fn strip_markup_handles_nested_tags() {
        assert_eq!(strip_markup("<div><p>hello</p></div>"), "hello");
    }

    // ── HTML fixture helpers ──

    /// Minimal taz-like article HTML for extraction tests.
    fn fixture_full_article() -> &'static str {
        r#"<!DOCTYPE html>
<html>
<head>
    <title>Klimawandel bedroht Küsten | taz.de</title>
    <meta property="og:title" content="Klimawandel bedroht Küsten">
    <meta name="description" content="Steigende Meeresspiegel gefährden Millionen Menschen an den Küsten weltweit.">
    <meta property="article:author" content="Maria Schmidt">
    <meta property="article:section" content="Umwelt">
    <meta property="article:published_time" content="2025-03-15T10:30:00+01:00">
</head>
<body>
<article>
    <h1>Klimawandel bedroht Küsten</h1>
    <p>Steigende Meeresspiegel gefährden Millionen Menschen an den Küsten weltweit. Wissenschaftler warnen vor den Folgen des Klimawandels.</p>
    <p>Die Temperaturen steigen seit Jahrzehnten kontinuierlich an, und die Auswirkungen werden immer deutlicher sichtbar in allen Regionen der Welt.</p>
    <h2>Forschungsergebnisse</h2>
    <p>Neue Studien zeigen, dass der Meeresspiegel bis zum Ende des Jahrhunderts um bis zu einem Meter steigen könnte, was dramatische Folgen hätte.</p>
    <p>Besonders betroffen sind Inselstaaten im Pazifik, die bereits heute mit den Auswirkungen kämpfen und internationale Hilfe benötigen dringend.</p>
    <p>Die internationale Gemeinschaft muss schnell handeln, um die schlimmsten Auswirkungen abzuwenden und betroffenen Regionen zu helfen bei der Anpassung.</p>
    <p>Experten fordern eine drastische Reduktion der Treibhausgasemissionen und massive Investitionen in erneuerbare Energien als einzigen Ausweg aus der Krise.</p>
</article>
</body>
</html>"#
    }

    fn fixture_minimal_article() -> &'static str {
        r#"<!DOCTYPE html>
<html>
<head><title>Kurzmeldung | taz.de</title></head>
<body>
<article>
    <h1>Kurzmeldung</h1>
    <p>Ein kurzer Absatz.</p>
</article>
</body>
</html>"#
    }

    fn fixture_json_ld_date() -> &'static str {
        r#"<!DOCTYPE html>
<html>
<head><title>Test</title>
<script type="application/ld+json">{"@type":"NewsArticle","datePublished":"2025-06-20T08:00:00Z"}</script>
</head>
<body>
<article>
    <p>Placeholder text that is long enough to pass the minimum length check for paragraph extraction in the body parser.</p>
</article>
</body>
</html>"#
    }

    fn fixture_date_in_url() -> &'static str {
        "https://taz.de/Artikel/!2025/04/10/some-slug/"
    }

    fn fixture_section_in_cp() -> &'static str {
        r#"<html><head></head><body><script>cp: "Redaktion/Politik", page: "artikel"</script></body></html>"#
    }

    // ── extract_body tests ──

    #[test]
    fn extract_body_finds_paragraphs_in_article_tag() {
        let doc = Html::parse_document(fixture_full_article());
        let body = extract_body(&doc).unwrap();
        assert!(body.contains("Temperaturen steigen"), "should contain body paragraph");
        assert!(body.contains("## Forschungsergebnisse"), "should preserve h2 as markdown heading");
        assert!(!body.contains("<p>"), "should not contain raw HTML tags");
    }

    #[test]
    fn extract_body_rejects_too_short_article() {
        let doc = Html::parse_document(fixture_minimal_article());
        // The minimal fixture has only one short paragraph — extract_body should still
        // return what it finds (or empty). The 80-word check is in fetch_article, not extract_body.
        let result = extract_body(&doc);
        // With only "Ein kurzer Absatz." (< 45 chars), extract_body should fail
        assert!(result.is_err(), "should fail for article with no substantial paragraphs");
    }

    #[test]
    fn extract_body_deduplicates_identical_paragraphs() {
        let html = r#"<html><body><article>
            <p>Dies ist ein langer Absatz der mindestens fünfundvierzig Zeichen haben muss um gezählt zu werden.</p>
            <p>Dies ist ein langer Absatz der mindestens fünfundvierzig Zeichen haben muss um gezählt zu werden.</p>
            <p>Ein zweiter unterschiedlicher Absatz der ebenfalls lang genug sein muss für die Extraktion.</p>
        </article></body></html>"#;
        let doc = Html::parse_document(html);
        let body = extract_body(&doc).unwrap();
        let count = body.matches("mindestens fünfundvierzig").count();
        assert_eq!(count, 1, "duplicate paragraph should be removed");
    }

    // ── first_text / first_attr tests ──

    #[test]
    fn first_text_extracts_h1() {
        let doc = Html::parse_document(fixture_full_article());
        let title = first_text(&doc, selectors::TITLE);
        assert_eq!(title.as_deref(), Some("Klimawandel bedroht Küsten"));
    }

    #[test]
    fn first_text_falls_back_to_og_title() {
        let html = r#"<html><head><meta property="og:title" content="OG Titel"></head><body></body></html>"#;
        let doc = Html::parse_document(html);
        let title = first_text(&doc, selectors::TITLE);
        assert_eq!(title.as_deref(), Some("OG Titel"));
    }

    #[test]
    fn first_text_returns_none_when_no_match() {
        let doc = Html::parse_document("<html><body></body></html>");
        assert!(first_text(&doc, selectors::TITLE).is_none());
    }

    #[test]
    fn first_attr_extracts_meta_content() {
        let doc = Html::parse_document(fixture_full_article());
        let author = first_attr(&doc, selectors::AUTHOR, "content");
        assert_eq!(author.as_deref(), Some("Maria Schmidt"));
    }

    #[test]
    fn first_attr_extracts_section() {
        let doc = Html::parse_document(fixture_full_article());
        let section = first_attr(&doc, selectors::SECTION, "content");
        assert_eq!(section.as_deref(), Some("Umwelt"));
    }

    // ── date extraction tests ──

    #[test]
    fn extract_date_from_meta_tag() {
        let doc = Html::parse_document(fixture_full_article());
        let date = extract_date(&doc, fixture_full_article(), "https://taz.de/test/");
        assert_eq!(date.as_deref(), Some("2025-03-15T10:30:00+01:00"));
    }

    #[test]
    fn extract_date_from_json_ld_fixture() {
        let html = fixture_json_ld_date();
        let result = extract_date_from_json_ld(html);
        assert_eq!(result.as_deref(), Some("2025-06-20T08:00:00Z"));
    }

    #[test]
    fn extract_date_from_url_fixture() {
        let result = extract_date_from_url(fixture_date_in_url());
        assert_eq!(result.as_deref(), Some("2025-04-10"));
    }

    #[test]
    fn extract_date_from_german_text() {
        let html = "<html><body>Veröffentlicht am 15.03.2025 um 10 Uhr</body></html>";
        let result = extract_date_from_text(html);
        assert_eq!(result.as_deref(), Some("15.03.2025"));
    }

    // ── section extraction tests ──

    #[test]
    fn extract_section_from_cp_variable() {
        let result = extract_section_from_html(fixture_section_in_cp());
        assert_eq!(result.as_deref(), Some("Politik"));
    }

    #[test]
    fn extract_section_returns_none_without_cp() {
        assert!(extract_section_from_html("<html><body>no cp here</body></html>").is_none());
    }

    // ── build_clean_text tests ──

    #[test]
    fn build_clean_text_includes_all_metadata() {
        let result = build_clean_text("Titel", "Untertitel", "Autor", "2025-01-01", "Body text hier.");
        assert!(result.contains("Titel"), "should contain title");
        assert!(result.contains("Untertitel"), "should contain subtitle");
        assert!(result.contains("Von Autor"), "should contain author with Von prefix");
        assert!(result.contains("2025-01-01"), "should contain date");
        assert!(result.contains("Body text hier."), "should contain body");
    }

    #[test]
    fn build_clean_text_omits_empty_subtitle() {
        let result = build_clean_text("Titel", "", "Autor", "2025-01-01", "Body.");
        assert!(!result.contains("\n\n\n"), "should not have double blank lines from empty subtitle");
    }

    #[test]
    fn build_clean_text_removes_duplicate_title_from_body() {
        let result = build_clean_text("Titel", "Sub", "", "", "Titel\n\nActual body content here.");
        // The title appearing in the body should be deduplicated
        let title_count = result.matches("Titel").count();
        assert!(title_count <= 2, "title should not appear more than twice (header + possible sub overlap)");
    }

    // ── dedupe_lines tests ──

    #[test]
    fn dedupe_lines_removes_exact_duplicates() {
        let mut lines = vec!["aaa".to_owned(), "bbb".to_owned(), "aaa".to_owned()];
        dedupe_lines(&mut lines);
        assert_eq!(lines, vec!["aaa", "bbb"]);
    }

    // ── looks_like_article_title tests ──

    #[test]
    fn looks_like_article_title_accepts_normal_title() {
        assert!(looks_like_article_title("Klimawandel bedroht Küsten"));
    }

    #[test]
    fn looks_like_article_title_rejects_short_text() {
        assert!(!looks_like_article_title("Mehr"));
    }

    // ── source_label tests ──

    #[test]
    fn source_label_startseite_for_base_url() {
        assert_eq!(source_label("https://taz.de"), "Startseite");
        assert_eq!(source_label("https://taz.de/"), "Startseite");
    }

    #[test]
    fn source_label_returns_path_for_section() {
        assert_eq!(source_label("https://taz.de/Politik/"), "Politik");
    }

    // ── infer_section_from_url ──

    #[test]
    fn infer_section_from_url_extracts_first_segment() {
        let result = infer_section_from_url("https://taz.de/Politik/!1234567/");
        assert_eq!(result, "Politik");
    }

    // ── is_interview_line tests ──

    #[test]
    fn is_interview_line_detects_taz_prefix() {
        assert!(is_interview_line("taz: Wie waren Sie als Kind?"));
    }

    #[test]
    fn is_interview_line_detects_speaker_name() {
        assert!(is_interview_line("Reynolds: Ja, das stimmt."));
        assert!(is_interview_line("Jason Reynolds: Ich sehe mich selbst."));
    }

    #[test]
    fn is_interview_line_rejects_normal_text() {
        assert!(!is_interview_line("Dies ist ein normaler Absatz ohne Sprecherkennzeichnung."));
    }

    #[test]
    fn is_interview_line_rejects_non_name_prefix() {
        // Lowercase words before colon should not match
        assert!(!is_interview_line("das problem: es gibt keine lösung für dieses thema."));
        // Numbers before colon should not match
        assert!(!is_interview_line("2025: Ein Jahr der Veränderungen in der Politik."));
    }

    // ── extract_body interview & figure tests ──

    #[test]
    fn extract_body_keeps_short_interview_questions() {
        let html = r#"<html><body><article>
            <p><strong>taz: Wie waren Sie als Kind?</strong></p>
            <p>Reynolds: Ich war empfindsam, und zwar auf eine beständige und geerdete Art und Weise, die mich überall hinführte.</p>
            <p><strong>taz: Schon vom ersten Buch an?</strong></p>
            <p>Reynolds: Ja, weil es so lange gedauert hatte, überhaupt dahin zu kommen und das war wirklich bemerkenswert!</p>
        </article></body></html>"#;
        let doc = Html::parse_document(html);
        let body = extract_body(&doc).unwrap();
        assert!(body.contains("taz: Wie waren Sie als Kind?"), "should keep short taz: question");
        assert!(body.contains("taz: Schon vom ersten Buch an?"), "should keep another short question");
    }

    #[test]
    fn extract_body_excludes_figure_captions() {
        let html = r#"<html><body><article>
            <figure>
                <img src="photo.jpg" alt="Portrait von Jason Reynolds">
                <figcaption>
                    <p>In seiner Kindheit gab es keine Bücher, sagt Jason Reynolds.</p>
                    <span>Foto: Anna Tiessen</span>
                </figcaption>
            </figure>
            <p>Dies ist der eigentliche Artikeltext, der lang genug sein muss, um die Mindestlänge zu überschreiten.</p>
        </article></body></html>"#;
        let doc = Html::parse_document(html);
        let body = extract_body(&doc).unwrap();
        assert!(!body.contains("Foto: Anna Tiessen"), "should not contain photo credit");
        assert!(!body.contains("keine Bücher, sagt"), "should not contain figcaption text");
        assert!(body.contains("eigentliche Artikeltext"), "should contain actual body text");
    }

    #[test]
    fn extract_body_excludes_bio_sidebar() {
        let html = r#"<html><body><article>
            <p>Haupttext des Artikels der lang genug sein muss, um die Mindestlänge von fünfundvierzig Zeichen zu erreichen.</p>
            <div class="webelement_bio webelement-pos-7">
                <p><strong>Der Autor</strong></p>
                <p>Jason Reynolds, 42, wuchs in einem Vorort von Washington D.C. auf und studierte Literaturwissenschaften.</p>
            </div>
            <p>Noch ein Absatz im Haupttext der ebenfalls lang genug ist, um mitgenommen zu werden und nicht gefiltert.</p>
        </article></body></html>"#;
        let doc = Html::parse_document(html);
        let body = extract_body(&doc).unwrap();
        assert!(!body.contains("Der Autor"), "should not contain bio heading");
        assert!(!body.contains("Literaturwissenschaften"), "should not contain bio text");
        assert!(body.contains("Haupttext des Artikels"), "should contain body text");
    }
}
