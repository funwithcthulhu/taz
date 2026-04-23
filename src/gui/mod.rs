mod actions;
mod callbacks;
mod sync;

use crate::{
    database::{ArticleQuery, Database, LibraryStats, StoredArticle, StoredArticleMeta},
    lingq::{Collection, LingqClient, UploadRequest},
    settings::{self, SettingsStore},
    taz::{ArticleSummary, BrowseSectionResult, Section, TazClient},
};
use chrono::{Datelike, NaiveDate};
use log::{error, info};
use slint::{ModelRc, SharedString, Timer, TimerMode, VecModel, Weak};
use std::{
    cell::RefCell,
    collections::HashSet,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
};
use tokio::runtime::Runtime;

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

#[derive(Clone)]
struct FetchProgress {
    label: String,
    completed: usize,
    total: usize,
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
        failed: Vec<String>,
    },
    SaveProgress(FetchProgress),
    SaveFinished {
        message: String,
        failed: Vec<String>,
    },
    CollectionsLoaded(Result<Vec<Collection>, String>),
    LingqLoggedIn(Result<String, String>),
    UploadFinished {
        uploaded: usize,
        skipped_already: usize,
        failed: Vec<String>,
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
    show_filters: bool,
    show_upload_tools: bool,
    delete_confirm_id: Option<i64>,
    preview_wide: bool,
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
    cancel_flag: Arc<AtomicBool>,
    /// Signals all background tasks to stop when the app is shutting down.
    shutdown_flag: Arc<AtomicBool>,
    settings_dirty: bool,

    // ── Page state ──
    browse: BrowseState,
    bulk: BulkFetchState,
    library: LibraryState,
    lq: LingqState,
}

// ── Core AppState helpers ──

impl AppState {
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
    window.set_heading_labels(ModelRc::from(Rc::new(VecModel::from(Vec::<SharedString>::new()))));
    window.set_section_labels(ModelRc::from(Rc::new(VecModel::from(Vec::<SharedString>::new()))));
    window.set_sort_labels(ModelRc::from(Rc::new(VecModel::from(Vec::<SharedString>::new()))));
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
        cancel_flag: Arc::new(AtomicBool::new(false)),
        shutdown_flag: Arc::new(AtomicBool::new(false)),
        settings_dirty: false,
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
            show_filters: sd.show_library_filters,
            show_upload_tools: sd.show_upload_tools,
            delete_confirm_id: None,
            preview_wide: sd.preview_wide,
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

fn format_failure_suffix(failed: &[String]) -> String {
    if failed.is_empty() {
        return String::new();
    }
    let details = failed.iter().take(3).cloned().collect::<Vec<_>>().join("; ");
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
        let result = format_failure_suffix(&["error1".to_owned()]);
        assert!(result.contains("1 failed"));
        assert!(result.contains("error1"));
    }

    #[test]
    fn format_failure_suffix_truncates_at_three() {
        let failures: Vec<String> = (1..=5).map(|i| format!("err{i}")).collect();
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
}
