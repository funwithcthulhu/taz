#[derive(Debug, Clone, Copy)]
pub struct Section {
    pub id: &'static str,
    pub label: &'static str,
    pub url: &'static str,
}

#[derive(Debug, Clone)]
pub struct ArticleSummary {
    pub article_key: String,
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
    pub(super) fn record_source_visit(&mut self, source_kind: DiscoverySourceKind) {
        self.source_pages_visited += 1;
        match source_kind {
            DiscoverySourceKind::Section => self.section_pages_visited += 1,
            DiscoverySourceKind::Subsection => self.subsection_pages_visited += 1,
            DiscoverySourceKind::Topic => self.topic_pages_visited += 1,
            DiscoverySourceKind::Search => {}
        }
    }

    pub(super) fn record_article(&mut self, source_kind: DiscoverySourceKind) {
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
    pub article_key: String,
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
    pub article_key: String,
    pub url: String,
    pub title: String,
    pub date: String,
    pub section: String,
}
