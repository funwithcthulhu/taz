mod actions;
mod callbacks;
mod sync;

use crate::{
    database::{ArticleQuery, Database, LibraryStats, StoredArticle, StoredArticleMeta},
    lingq::{Collection, LingqClient, RemoteLesson, UploadRequest},
    settings::{self, SettingsStore},
    taz::{ArticleSummary, BrowseSectionResult, Section, TazClient},
};
use chrono::{Datelike, NaiveDate};
use log::{error, info};
use slint::{ModelRc, SharedString, Timer, TimerMode, VecModel, Weak};
use std::{
    cell::RefCell,
    collections::{HashSet, VecDeque},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
};
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;

slint::include_modules!();

// ── Types ──

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Browse,
    Library,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LibrarySortMode {
    Newest,
    Oldest,
    Longest,
    Shortest,
    Title,
}

impl LibrarySortMode {
    fn all() -> [Self; 5] {
        [
            Self::Newest,
            Self::Oldest,
            Self::Longest,
            Self::Shortest,
            Self::Title,
        ]
    }

    fn label(self) -> &'static str {
        match self {
            Self::Newest => "Newest",
            Self::Oldest => "Oldest",
            Self::Longest => "Longest",
            Self::Shortest => "Shortest",
            Self::Title => "Title",
        }
    }

    fn from_index(index: i32) -> Self {
        Self::all()
            .get(index.max(0) as usize)
            .copied()
            .unwrap_or(Self::Newest)
    }

    fn index(self) -> i32 {
        match self {
            Self::Newest => 0,
            Self::Oldest => 1,
            Self::Longest => 2,
            Self::Shortest => 3,
            Self::Title => 4,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArticleDensity {
    Compact,
    Comfortable,
}

impl ArticleDensity {
    fn all() -> [Self; 2] {
        [Self::Compact, Self::Comfortable]
    }

    fn label(self) -> &'static str {
        match self {
            Self::Compact => "Compact",
            Self::Comfortable => "Comfortable",
        }
    }

    fn from_index(index: i32) -> Self {
        Self::all()
            .get(index.max(0) as usize)
            .copied()
            .unwrap_or(Self::Compact)
    }

    fn from_setting(value: &str) -> Self {
        match value {
            "comfortable" => Self::Comfortable,
            _ => Self::Compact,
        }
    }

    fn index(self) -> i32 {
        match self {
            Self::Compact => 0,
            Self::Comfortable => 1,
        }
    }

    fn setting(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Comfortable => "comfortable",
        }
    }
}

#[derive(Clone)]
struct FetchProgress {
    label: String,
    completed: usize,
    total: usize,
}

#[derive(Clone)]
enum RetryAction {
    SaveUrl(String),
    UploadArticle(i64),
}

#[derive(Clone)]
struct JobFailure {
    label: String,
    detail: String,
    retry: Option<RetryAction>,
}

#[derive(Clone)]
struct ActivityEntry {
    kind: String,
    message: String,
    detail: String,
}

enum QueuedJob {
    SaveUrls {
        urls: Vec<String>,
        label: String,
    },
    BulkFetch {
        section_ids: Vec<String>,
        imported_urls: HashSet<String>,
        max_articles: usize,
        per_section_cap: usize,
        stop_after_old: usize,
        date_from: NaiveDate,
        date_to: NaiveDate,
    },
    UploadArticles {
        ids: Vec<i64>,
        api_key: String,
        language: String,
        collection_id: Option<i64>,
    },
    SyncLingqCourse {
        api_key: String,
        language: String,
        collection_id: i64,
    },
}

impl QueuedJob {
    fn label(&self) -> String {
        match self {
            Self::SaveUrls { urls, label } => format!("{label} ({})", urls.len()),
            Self::BulkFetch {
                section_ids,
                max_articles,
                ..
            } => format!(
                "Bulk fetch from {} section(s), max {max_articles}",
                section_ids.len()
            ),
            Self::UploadArticles { ids, .. } => format!("Upload {} article(s)", ids.len()),
            Self::SyncLingqCourse { collection_id, .. } => {
                format!("Sync LingQ status for course #{collection_id}")
            }
        }
    }
}

enum AppEvent {
    BrowseLoaded(Result<BrowseSectionResult, String>),
    SearchLoaded(Result<Vec<ArticleSummary>, String>),
    SavedUrlsLoaded(Result<HashSet<String>, String>),
    LibraryLoaded(Result<Vec<StoredArticleMeta>, String>),
    StatsLoaded(Result<LibraryStats, String>),
    FetchProgress(FetchProgress),
    FetchFinished {
        message: String,
        failed: Vec<JobFailure>,
    },
    SaveProgress(FetchProgress),
    SaveFinished {
        message: String,
        failed: Vec<JobFailure>,
    },
    CollectionsLoaded(Result<Vec<Collection>, String>),
    LingqLoggedIn(Result<String, String>),
    UploadFinished {
        uploaded: usize,
        skipped_already: usize,
        failed: Vec<JobFailure>,
        cancelled: bool,
    },
    LingqCourseSynced {
        scanned: usize,
        matched: usize,
        ambiguous: usize,
        failed: Vec<JobFailure>,
        cancelled: bool,
    },
}

/// Tracks which parts of the UI need rebuilding on the next `sync_to_window`.
#[derive(Default)]
struct DirtyFlags {
    browse: bool,
    library: bool,
    stats: bool,
    collections: bool,
    preview: bool,
    progress: bool,
    activity: bool,
}

impl DirtyFlags {
    fn all() -> Self {
        Self {
            browse: true,
            library: true,
            stats: true,
            collections: true,
            preview: true,
            progress: true,
            activity: true,
        }
    }

    fn clear(&mut self) {
        *self = Self::default();
    }
}

/// State for the Browse page: section navigation, article candidates, and selection.
#[derive(Default)]
struct BrowseState {
    section_index: usize,
    limit: usize,
    articles: Vec<ArticleSummary>,
    report: Option<String>,
    selected: HashSet<String>,
    saved_urls: HashSet<String>,
    only_new: bool,
    date_from: String,
    date_to: String,
    search_query: String,
}

/// State for bulk-fetch settings.
#[derive(Default)]
struct BulkFetchState {
    selected_sections: HashSet<String>,
    max_articles: String,
    per_section_cap: String,
    stop_after_old: String,
    auto_fetch_on_startup: bool,
}

/// State for the Library page: stored articles, filters, and preview.
struct LibraryState {
    articles: Vec<StoredArticleMeta>,
    selected_article_id: Option<i64>,
    /// Cached full article for the preview pane (loaded on demand from DB).
    preview_article: Option<StoredArticle>,
    search: String,
    heading: String,
    section: String,
    sort_mode: LibrarySortMode,
    only_not_uploaded: bool,
    min_words: String,
    max_words: String,
    duplicate_only: bool,
    filter_preset_index: usize,
    show_filters: bool,
    show_upload_tools: bool,
    delete_confirm_id: Option<i64>,
    preview_wide: bool,
    /// Cached heading labels derived from articles — rebuilt when dirty.library is set.
    cached_heading_labels: Vec<String>,
    /// Cached section labels derived from articles — rebuilt when dirty.library is set.
    cached_section_labels: Vec<String>,
}

/// State for LingQ integration: authentication, collection selection, and upload.
struct LingqState {
    language: String,
    collections: Vec<Collection>,
    selected_collection: Option<i64>,
    selected_articles: HashSet<i64>,
    only_not_uploaded: bool,
    min_words: String,
    max_words: String,
    show_settings: bool,
    api_key: String,
    /// Snapshot of the API key at last save, to avoid redundant disk writes.
    api_key_last_saved: String,
    username: String,
    password: String,
    /// Tracks consecutive failed login attempts for rate-limiting.
    login_failures: u32,
    /// Earliest time the next login attempt is allowed.
    login_cooldown_until: Option<std::time::Instant>,
}

struct AppState {
    // ── Infrastructure ──
    window: Weak<AppWindow>,
    tx: Sender<AppEvent>,
    rx: Receiver<AppEvent>,
    runtime: Arc<Runtime>,
    db: Arc<Database>,
    settings: SettingsStore,
    sections: &'static [Section],
    scraper: Arc<TazClient>,
    lingq: Arc<LingqClient>,
    dirty: DirtyFlags,
    current_view: View,
    status_message: String,
    stats: Option<LibraryStats>,
    progress: Option<FetchProgress>,
    save_progress: Option<FetchProgress>,
    current_job: Option<JoinHandle<()>>,
    current_job_started_at: Option<std::time::Instant>,
    job_queue: VecDeque<QueuedJob>,
    failed_items: Vec<JobFailure>,
    activity: VecDeque<ActivityEntry>,
    cancel_flag: Arc<AtomicBool>,
    /// Signals all background tasks to stop when the app is shutting down.
    shutdown_flag: Arc<AtomicBool>,
    settings_dirty: bool,
    article_density: ArticleDensity,

    // ── Page state ──
    browse: BrowseState,
    bulk: BulkFetchState,
    library: LibraryState,
    lq: LingqState,
}

// ── Core AppState helpers ──

impl AppState {
    fn background_job_active(&self) -> bool {
        self.current_job.is_some() || self.progress.is_some() || self.save_progress.is_some()
    }

    fn set_current_job(&mut self, handle: JoinHandle<()>) {
        self.current_job_started_at = Some(std::time::Instant::now());
        self.current_job = Some(handle);
    }

    fn clear_current_job(&mut self) {
        self.current_job = None;
        self.current_job_started_at = None;
    }

    fn cancel_active_job(&mut self) -> bool {
        self.cancel_flag.store(true, Ordering::Relaxed);
        let aborted = if let Some(handle) = self.current_job.take() {
            handle.abort();
            true
        } else {
            false
        };
        self.progress = None;
        self.save_progress = None;
        self.current_job_started_at = None;
        let cleared = self.job_queue.len();
        self.job_queue.clear();
        if cleared > 0 {
            self.add_activity(
                "cancelled",
                "Cleared queued work",
                format!("{cleared} queued job(s) removed."),
            );
        }
        self.dirty.progress = true;
        aborted
    }

    /// Mark library + preview dirty and update selected article.
    /// Loads the full article text from DB for the preview pane.
    fn select_article(&mut self, id: Option<i64>) {
        self.library.selected_article_id = id;
        self.library.delete_confirm_id = None;
        // Load full article text for preview (single-row lookup by PK — fast)
        self.library.preview_article = id.and_then(|article_id| {
            match self.db.get_article(article_id) {
                Ok(article) => article,
                Err(err) => {
                    log::warn!("Failed to load article #{article_id} for preview: {err:#}");
                    None
                }
            }
        });
        self.dirty.library = true;
        self.dirty.preview = true;
    }

    /// Toggle an article's upload selection. Returns whether it is now selected.
    fn toggle_upload_selection(&mut self, id: i64) -> bool {
        let selected = if !self.lq.selected_articles.insert(id) {
            self.lq.selected_articles.remove(&id);
            false
        } else {
            true
        };
        self.dirty.library = true;
        selected
    }

    /// Spawn an async background task that produces an `AppEvent`.
    /// Clones the sender and runs the future on the tokio runtime.
    fn spawn_background<F>(&self, task: F)
    where
        F: std::future::Future<Output = AppEvent> + Send + 'static,
    {
        let tx = self.tx.clone();
        self.runtime.spawn(async move {
            let event = task.await;
            let _ = tx.send(event);
        });
    }

    fn current_section(&self) -> &'static Section {
        self.sections
            .get(self.browse.section_index)
            .unwrap_or(&self.sections[0])
    }

    fn set_status(&mut self, message: impl Into<String>) {
        self.status_message = message.into();
        self.sync_to_window();
    }

    fn add_activity(
        &mut self,
        kind: impl Into<String>,
        message: impl Into<String>,
        detail: impl Into<String>,
    ) {
        self.activity.push_front(ActivityEntry {
            kind: kind.into(),
            message: message.into(),
            detail: detail.into(),
        });
        while self.activity.len() > 30 {
            self.activity.pop_back();
        }
        self.dirty.activity = true;
    }

    fn record_failures(&mut self, failures: &[JobFailure]) {
        if failures.is_empty() {
            return;
        }
        for failure in failures {
            self.failed_items.push(failure.clone());
        }
        if self.failed_items.len() > 100 {
            let remove_count = self.failed_items.len() - 100;
            self.failed_items.drain(0..remove_count);
        }
        self.add_activity(
            "failed",
            format!("{} failed item(s)", failures.len()),
            failures
                .first()
                .map(|failure| failure.label.clone())
                .unwrap_or_default(),
        );
        self.dirty.activity = true;
    }
}

// ── Entry point ──

pub fn run() -> Result<(), slint::PlatformError> {
    info!("Starting GUI");
    let window = AppWindow::new()?;
    window.set_browse_section_labels(ModelRc::from(Rc::new(VecModel::from(
        Vec::<SharedString>::new(),
    ))));
    window.set_bulk_section_rows(ModelRc::from(Rc::new(VecModel::from(
        Vec::<BulkSectionRow>::new(),
    ))));
    window.set_library_rows(ModelRc::from(Rc::new(VecModel::from(Vec::<LibraryRow>::new()))));
    window.set_browse_rows(ModelRc::from(Rc::new(VecModel::from(Vec::<BrowseRow>::new()))));
    window.set_stat_cards(ModelRc::from(Rc::new(VecModel::from(Vec::<StatCard>::new()))));
    window.set_activity_rows(ModelRc::from(Rc::new(VecModel::from(Vec::<ActivityRow>::new()))));
    window.set_failed_rows(ModelRc::from(Rc::new(VecModel::from(Vec::<FailedRow>::new()))));
    window.set_heading_labels(ModelRc::from(Rc::new(VecModel::from(Vec::<SharedString>::new()))));
    window.set_section_labels(ModelRc::from(Rc::new(VecModel::from(Vec::<SharedString>::new()))));
    window.set_sort_labels(ModelRc::from(Rc::new(VecModel::from(Vec::<SharedString>::new()))));
    window.set_filter_preset_labels(ModelRc::from(Rc::new(VecModel::from(
        filter_preset_labels()
            .into_iter()
            .map(SharedString::from)
            .collect::<Vec<_>>(),
    ))));
    window.set_density_labels(ModelRc::from(Rc::new(VecModel::from(
        ArticleDensity::all()
            .into_iter()
            .map(|density| SharedString::from(density.label()))
            .collect::<Vec<_>>(),
    ))));
    window.set_collection_labels(ModelRc::from(Rc::new(VecModel::from(vec![
        SharedString::from("No course selected"),
    ]))));

    let settings = SettingsStore::load_default().unwrap_or_else(|err| {
        log::warn!("Failed to load settings: {err:#}, using defaults");
        SettingsStore::in_memory_default()
    });
    let db = Arc::new(Database::open_default().expect("failed to open database"));
    let scraper = Arc::new(TazClient::new().expect("failed to initialize taz client"));
    let lingq_client = Arc::new(LingqClient::new().expect("failed to initialize LingQ client"));
    let sections = scraper.sections();
    let browse_section_index = sections
        .iter()
        .position(|section| section.id == settings.data().browse_section)
        .unwrap_or(0);
    let current_view = if settings.data().last_view == "library" {
        View::Library
    } else {
        View::Browse
    };
    let mut sd = settings.data().clone();
    let api_key = settings::load_api_key(&mut sd);
    let library_sort_mode = match sd.library_sort.as_str() {
        "oldest" => LibrarySortMode::Oldest,
        "longest" => LibrarySortMode::Longest,
        "shortest" => LibrarySortMode::Shortest,
        "title" => LibrarySortMode::Title,
        _ => LibrarySortMode::Newest,
    };
    let article_density = ArticleDensity::from_setting(&sd.article_density);
    let (tx, rx) = mpsc::channel();
    let runtime = Arc::new(
        Runtime::new().expect("failed to create tokio runtime"),
    );

    let state = Rc::new(RefCell::new(AppState {
        window: window.as_weak(),
        tx,
        rx,
        runtime,
        db,
        settings,
        sections,
        scraper,
        lingq: lingq_client,
        dirty: DirtyFlags::all(),
        current_view,
        status_message: "Loading taz sections, library, and LingQ status.".to_owned(),
        stats: None,
        progress: None,
        save_progress: None,
        current_job: None,
        current_job_started_at: None,
        job_queue: VecDeque::new(),
        failed_items: Vec::new(),
        activity: VecDeque::new(),
        cancel_flag: Arc::new(AtomicBool::new(false)),
        shutdown_flag: Arc::new(AtomicBool::new(false)),
        settings_dirty: false,
        article_density,
        browse: BrowseState {
            section_index: browse_section_index,
            limit: 80,
            articles: Vec::new(),
            report: None,
            selected: HashSet::new(),
            saved_urls: HashSet::new(),
            only_new: sd.browse_only_new,
            date_from: sd.browse_date_from,
            date_to: sd.browse_date_to,
            search_query: String::new(),
        },
        bulk: BulkFetchState {
            selected_sections: HashSet::new(),
            max_articles: sd.bulk_max_articles,
            per_section_cap: sd.bulk_per_section_cap,
            stop_after_old: sd.bulk_stop_after_old,
            auto_fetch_on_startup: sd.auto_fetch_on_startup,
        },
        library: LibraryState {
            articles: Vec::new(),
            selected_article_id: None,
            preview_article: None,
            search: String::new(),
            heading: String::new(),
            section: String::new(),
            sort_mode: library_sort_mode,
            only_not_uploaded: sd.library_only_not_uploaded,
            min_words: sd.library_min_words,
            max_words: sd.library_max_words,
            duplicate_only: sd.library_duplicate_only,
            filter_preset_index: 0,
            show_filters: sd.show_library_filters,
            show_upload_tools: sd.show_upload_tools,
            delete_confirm_id: None,
            preview_wide: sd.preview_wide,
            cached_heading_labels: Vec::new(),
            cached_section_labels: Vec::new(),
        },
        lq: LingqState {
            language: sd.lingq_language,
            collections: Vec::new(),
            selected_collection: sd.lingq_collection_id,
            selected_articles: HashSet::new(),
            only_not_uploaded: sd.lingq_only_not_uploaded,
            min_words: sd.lingq_min_words,
            max_words: sd.lingq_max_words,
            show_settings: false,
            api_key_last_saved: api_key.clone(),
            api_key,
            username: String::new(),
            password: String::new(),
            login_failures: 0,
            login_cooldown_until: None,
        },
    }));

    callbacks::wire_callbacks(&window, &state);

    {
        let mut app = state.borrow_mut();
        app.refresh_saved_urls();
        app.refresh_stats();
        app.load_library();
        app.load_browse();
        app.load_collections_if_possible();
        if sd.auto_fetch_on_startup {
            app.run_auto_fetch();
        }
        // Fire-and-forget: discover any new nav sections on taz.de
        let discover_scraper = app.scraper.clone();
        app.runtime.spawn(async move {
            if let Err(err) = discover_scraper.discover_new_sections().await {
                log::debug!("Section discovery failed: {err:#}");
            }
        });
        app.sync_to_window();
    }

    let poll_state = state.clone();
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, std::time::Duration::from_millis(100), move || {
        if let Ok(mut app) = poll_state.try_borrow_mut() {
            app.poll_events();
            app.flush_settings();
        }
    });

    let result = window.run();

    // Signal all background tasks to stop
    if let Ok(app) = state.try_borrow() {
        app.shutdown_flag.store(true, Ordering::Relaxed);
        // Also trip the cancel flag so in-flight loops exit promptly
        app.cancel_flag.store(true, Ordering::Relaxed);
    }

    // Give background tasks a moment to finish cleanly
    std::thread::sleep(std::time::Duration::from_millis(200));

    result
}

// ── Free helper functions ──

fn format_failure_suffix(failed: &[JobFailure]) -> String {
    if failed.is_empty() {
        return String::new();
    }
    let details = failed
        .iter()
        .take(3)
        .map(|failure| {
            if failure.detail.is_empty() {
                failure.label.clone()
            } else {
                format!("{}: {}", failure.label, failure.detail)
            }
        })
        .collect::<Vec<_>>()
        .join("; ");
    let more = if failed.len() > 3 {
        format!(" (+{} more)", failed.len() - 3)
    } else {
        String::new()
    };
    format!(" {} failed: {details}{more}", failed.len())
}

/// Delay between consecutive network requests to avoid overwhelming taz.de.
const REQUEST_THROTTLE: tokio::time::Duration = tokio::time::Duration::from_millis(250);

fn parse_positive_usize_input(input: &str, label: &str) -> Result<usize, String> {
    let value = input.trim();
    if value.is_empty() {
        return Err(format!("{label} cannot be empty."));
    }
    value
        .parse::<usize>()
        .map_err(|_| format!("{label} must be a positive whole number."))
        .and_then(|parsed| {
            if parsed == 0 {
                Err(format!("{label} must be greater than zero."))
            } else {
                Ok(parsed)
            }
        })
}

fn parse_optional_i64(input: &str) -> Result<Option<i64>, String> {
    let value = input.trim();
    if value.is_empty() {
        return Ok(None);
    }
    value
        .parse::<i64>()
        .map(Some)
        .map_err(|_| "Word-count filters must be whole numbers.".to_owned())
}

fn parse_date_input(input: &str) -> Result<Option<NaiveDate>, String> {
    let value = input.trim();
    if value.is_empty() {
        return Ok(None);
    }
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map(Some)
        .map_err(|_| "Dates must use YYYY-MM-DD.".to_owned())
}

fn parse_article_date(input: &str) -> Option<NaiveDate> {
    let trimmed = input.trim();
    NaiveDate::parse_from_str(trimmed, "%Y-%m-%d")
        .ok()
        .or_else(|| {
            trimmed
                .get(0..10)
                .and_then(|prefix| NaiveDate::parse_from_str(prefix, "%Y-%m-%d").ok())
        })
        .or_else(|| NaiveDate::parse_from_str(trimmed, "%d.%m.%Y").ok())
}

fn section_heading(section: &str) -> String {
    section
        .split(" - ")
        .next()
        .unwrap_or(section)
        .trim()
        .to_owned()
}

fn index_of_label(labels: &[String], current: &str) -> usize {
    labels
        .iter()
        .position(|label| label == current)
        .unwrap_or(0)
}

fn indexed_label(labels: &[String], index: i32) -> String {
    labels
        .get(index.max(0) as usize)
        .cloned()
        .unwrap_or_else(|| labels.first().cloned().unwrap_or_default())
}

fn filter_preset_labels() -> Vec<&'static str> {
    vec![
        "No preset",
        "Short LingQ (600-999)",
        "Standard LingQ (1000-1800)",
        "Long reads (1800+)",
        "Not uploaded",
        "Duplicates",
    ]
}

fn normalized_duplicate_title(title: &str) -> String {
    title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn normalized_lingq_url(url: &str) -> String {
    let trimmed = url.trim();
    let without_fragment = trimmed.split('#').next().unwrap_or(trimmed);
    let without_query = without_fragment.split('?').next().unwrap_or(without_fragment);
    without_query
        .trim_end_matches('/')
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("www.")
        .to_lowercase()
}

fn match_lingq_lessons(
    lessons: &[RemoteLesson],
    articles: &[StoredArticleMeta],
) -> (Vec<(i64, i64, String)>, usize) {
    let mut by_url = std::collections::HashMap::<String, i64>::new();
    let mut by_title = std::collections::HashMap::<String, Vec<i64>>::new();
    for article in articles {
        by_url.insert(normalized_lingq_url(&article.url), article.id);
        by_title
            .entry(normalized_duplicate_title(&article.title))
            .or_default()
            .push(article.id);
    }

    let mut matched_article_ids = HashSet::new();
    let mut matches = Vec::new();
    let mut ambiguous = 0usize;
    for lesson in lessons {
        let mut article_id = lesson
            .original_url
            .as_deref()
            .map(normalized_lingq_url)
            .and_then(|url| by_url.get(&url).copied());

        if article_id.is_none() {
            let title_key = normalized_duplicate_title(&lesson.title);
            if let Some(ids) = by_title.get(&title_key) {
                if ids.len() == 1 {
                    article_id = ids.first().copied();
                } else if ids.len() > 1 {
                    ambiguous += 1;
                }
            }
        }

        if let Some(id) = article_id {
            if matched_article_ids.insert(id) {
                matches.push((id, lesson.id, lesson.lesson_url.clone()));
            }
        }
    }

    (matches, ambiguous)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_positive_usize_input ──

    #[test]
    fn parse_positive_usize_valid() {
        assert_eq!(parse_positive_usize_input("42", "limit").unwrap(), 42);
    }

    #[test]
    fn parse_positive_usize_zero_rejected() {
        assert!(parse_positive_usize_input("0", "limit").is_err());
    }

    #[test]
    fn parse_positive_usize_empty_rejected() {
        assert!(parse_positive_usize_input("", "limit").is_err());
    }

    #[test]
    fn parse_positive_usize_negative_rejected() {
        assert!(parse_positive_usize_input("-1", "limit").is_err());
    }

    #[test]
    fn parse_positive_usize_whitespace_trimmed() {
        assert_eq!(parse_positive_usize_input("  10  ", "limit").unwrap(), 10);
    }

    // ── parse_optional_i64 ──

    #[test]
    fn parse_optional_i64_valid() {
        assert_eq!(parse_optional_i64("100").unwrap(), Some(100));
    }

    #[test]
    fn parse_optional_i64_empty_is_none() {
        assert_eq!(parse_optional_i64("").unwrap(), None);
    }

    #[test]
    fn parse_optional_i64_invalid() {
        assert!(parse_optional_i64("abc").is_err());
    }

    // ── parse_date_input ──

    #[test]
    fn parse_date_input_valid() {
        let result = parse_date_input("2025-03-24").unwrap().unwrap();
        assert_eq!(result.to_string(), "2025-03-24");
    }

    #[test]
    fn parse_date_input_empty_is_none() {
        assert!(parse_date_input("").unwrap().is_none());
    }

    #[test]
    fn parse_date_input_bad_format() {
        assert!(parse_date_input("24/03/2025").is_err());
    }

    // ── parse_article_date ──

    #[test]
    fn parse_article_date_iso() {
        let date = parse_article_date("2025-03-24").unwrap();
        assert_eq!(date.to_string(), "2025-03-24");
    }

    #[test]
    fn parse_article_date_iso_timestamp() {
        let date = parse_article_date("2025-03-24T10:00:00").unwrap();
        assert_eq!(date.to_string(), "2025-03-24");
    }

    #[test]
    fn parse_article_date_german() {
        let date = parse_article_date("24.03.2025").unwrap();
        assert_eq!(date.to_string(), "2025-03-24");
    }

    #[test]
    fn parse_article_date_invalid() {
        assert!(parse_article_date("not a date").is_none());
    }

    // ── section_heading ──

    #[test]
    fn section_heading_strips_suffix() {
        assert_eq!(section_heading("Politik - Inland"), "Politik");
    }

    #[test]
    fn section_heading_no_separator() {
        assert_eq!(section_heading("Kultur"), "Kultur");
    }

    // ── format_failure_suffix ──

    #[test]
    fn format_failure_suffix_empty() {
        assert_eq!(format_failure_suffix(&[]), "");
    }

    #[test]
    fn format_failure_suffix_one() {
        let result = format_failure_suffix(&[JobFailure {
            label: "error1".to_owned(),
            detail: String::new(),
            retry: None,
        }]);
        assert!(result.contains("1 failed"));
        assert!(result.contains("error1"));
    }

    #[test]
    fn format_failure_suffix_truncates_at_three() {
        let failures: Vec<JobFailure> = (1..=5)
            .map(|i| JobFailure {
                label: format!("err{i}"),
                detail: String::new(),
                retry: None,
            })
            .collect();
        let result = format_failure_suffix(&failures);
        assert!(result.contains("5 failed"));
        assert!(result.contains("+2 more"));
    }

    // ── index_of_label / indexed_label ──

    #[test]
    fn index_of_label_found() {
        let labels = vec!["A".to_owned(), "B".to_owned(), "C".to_owned()];
        assert_eq!(index_of_label(&labels, "B"), 1);
    }

    #[test]
    fn index_of_label_not_found() {
        let labels = vec!["A".to_owned()];
        assert_eq!(index_of_label(&labels, "Z"), 0);
    }

    #[test]
    fn indexed_label_valid() {
        let labels = vec!["A".to_owned(), "B".to_owned()];
        assert_eq!(indexed_label(&labels, 1), "B");
    }

    #[test]
    fn indexed_label_negative_clamps() {
        let labels = vec!["A".to_owned(), "B".to_owned()];
        assert_eq!(indexed_label(&labels, -5), "A");
    }

    #[test]
    fn indexed_label_out_of_bounds() {
        let labels = vec!["A".to_owned()];
        assert_eq!(indexed_label(&labels, 99), "A");
    }

    // ── LibrarySortMode ──

    #[test]
    fn sort_mode_roundtrip() {
        for mode in LibrarySortMode::all() {
            assert_eq!(LibrarySortMode::from_index(mode.index()), mode);
        }
    }

    #[test]
    fn sort_mode_invalid_index_defaults() {
        assert_eq!(LibrarySortMode::from_index(-1), LibrarySortMode::Newest);
        assert_eq!(LibrarySortMode::from_index(99), LibrarySortMode::Newest);
    }

    #[test]
    fn article_density_roundtrip() {
        for density in ArticleDensity::all() {
            assert_eq!(ArticleDensity::from_index(density.index()), density);
            assert_eq!(ArticleDensity::from_setting(density.setting()), density);
        }
    }

    #[test]
    fn lingq_matching_prefers_original_url() {
        let articles = vec![StoredArticleMeta {
            id: 42,
            url: "https://taz.de/Foo/!123/?utm=abc".to_owned(),
            title: "Different title".to_owned(),
            subtitle: String::new(),
            author: String::new(),
            date: String::new(),
            section: String::new(),
            word_count: 1000,
            difficulty: 3,
            fetched_at: String::new(),
            uploaded_to_lingq: false,
            lingq_lesson_id: None,
            lingq_lesson_url: String::new(),
            paywalled: false,
        }];
        let lessons = vec![RemoteLesson {
            id: 99,
            title: "Remote title".to_owned(),
            original_url: Some("https://www.taz.de/Foo/!123/".to_owned()),
            lesson_url: "https://www.lingq.com/de/learn/lesson/99/".to_owned(),
        }];

        let (matches, ambiguous) = match_lingq_lessons(&lessons, &articles);
        assert_eq!(ambiguous, 0);
        assert_eq!(matches, vec![(42, 99, lessons[0].lesson_url.clone())]);
    }

    #[test]
    fn lingq_matching_skips_ambiguous_titles() {
        let make_article = |id| StoredArticleMeta {
            id,
            url: format!("https://taz.de/a{id}/"),
            title: "Same title".to_owned(),
            subtitle: String::new(),
            author: String::new(),
            date: String::new(),
            section: String::new(),
            word_count: 1000,
            difficulty: 3,
            fetched_at: String::new(),
            uploaded_to_lingq: false,
            lingq_lesson_id: None,
            lingq_lesson_url: String::new(),
            paywalled: false,
        };
        let articles = vec![make_article(1), make_article(2)];
        let lessons = vec![RemoteLesson {
            id: 99,
            title: "Same title".to_owned(),
            original_url: None,
            lesson_url: "https://www.lingq.com/de/learn/lesson/99/".to_owned(),
        }];

        let (matches, ambiguous) = match_lingq_lessons(&lessons, &articles);
        assert_eq!(matches.len(), 0);
        assert_eq!(ambiguous, 1);
    }
}
