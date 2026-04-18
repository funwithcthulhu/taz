use crate::{
    database::{Database, LibraryStats, StoredArticle},
    lingq::{Collection, LingqClient, UploadRequest},
    settings::SettingsStore,
    taz::{ArticleSummary, BrowseSectionResult, Section, TazClient},
};
use chrono::{Datelike, NaiveDate};
use slint::{ModelRc, SharedString, Timer, TimerMode, VecModel, Weak};
use std::{
    cell::RefCell,
    collections::HashSet,
    rc::Rc,
    sync::mpsc::{self, Receiver, Sender},
};

slint::include_modules!();

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Browse,
    Library,
}

#[derive(Clone, Copy, PartialEq, Eq)]
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
    SavedUrlsLoaded(Result<HashSet<String>, String>),
    LibraryLoaded(Result<Vec<StoredArticle>, String>),
    StatsLoaded(Result<LibraryStats, String>),
    FetchProgress(FetchProgress),
    FetchFinished {
        message: String,
        failed: Vec<String>,
    },
    CollectionsLoaded(Result<Vec<Collection>, String>),
    LingqLoggedIn(Result<String, String>),
    UploadFinished {
        uploaded: usize,
        failed: Vec<String>,
    },
}

struct AppState {
    window: Weak<AppWindow>,
    tx: Sender<AppEvent>,
    rx: Receiver<AppEvent>,
    settings: SettingsStore,
    sections: &'static [Section],
    current_view: View,
    status_message: String,
    browse_section_index: usize,
    browse_limit: usize,
    browse_articles: Vec<ArticleSummary>,
    browse_report: Option<String>,
    browse_selected: HashSet<String>,
    browse_saved_urls: HashSet<String>,
    browse_only_new: bool,
    browse_date_from: String,
    browse_date_to: String,
    bulk_selected_sections: HashSet<String>,
    bulk_max_articles: String,
    bulk_per_section_cap: String,
    bulk_stop_after_old: String,
    library_articles: Vec<StoredArticle>,
    selected_article_id: Option<i64>,
    library_search: String,
    library_heading: String,
    library_section: String,
    library_sort_mode: LibrarySortMode,
    library_only_not_uploaded: bool,
    library_min_words: String,
    library_max_words: String,
    show_library_filters: bool,
    show_upload_tools: bool,
    lingq_collections: Vec<Collection>,
    lingq_selected_collection: Option<i64>,
    lingq_selected_articles: HashSet<i64>,
    lingq_only_not_uploaded: bool,
    lingq_min_words: String,
    lingq_max_words: String,
    show_lingq_settings: bool,
    lingq_api_key: String,
    lingq_username: String,
    lingq_password: String,
    stats: Option<LibraryStats>,
    progress: Option<FetchProgress>,
    delete_confirm_id: Option<i64>,
    preview_wide: bool,
}

pub fn run() -> Result<(), slint::PlatformError> {
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

    let settings = SettingsStore::load_default().unwrap_or_else(|_| {
        SettingsStore::load(std::path::PathBuf::from("settings.json")).expect("fallback settings")
    });
    let scraper = TazClient::new().expect("failed to initialize taz client");
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
    let initial_collection_id = settings.data().lingq_collection_id;
    let initial_api_key = settings.data().lingq_api_key.clone();
    let (tx, rx) = mpsc::channel();

    let state = Rc::new(RefCell::new(AppState {
        window: window.as_weak(),
        tx,
        rx,
        settings,
        sections,
        current_view,
        status_message: "Loading taz sections, library, and LingQ status.".to_owned(),
        browse_section_index,
        browse_limit: 80,
        browse_articles: Vec::new(),
        browse_report: None,
        browse_selected: HashSet::new(),
        browse_saved_urls: HashSet::new(),
        browse_only_new: true,
        browse_date_from: String::new(),
        browse_date_to: String::new(),
        bulk_selected_sections: HashSet::new(),
        bulk_max_articles: "60".to_owned(),
        bulk_per_section_cap: "30".to_owned(),
        bulk_stop_after_old: "12".to_owned(),
        library_articles: Vec::new(),
        selected_article_id: None,
        library_search: String::new(),
        library_heading: String::new(),
        library_section: String::new(),
        library_sort_mode: LibrarySortMode::Newest,
        library_only_not_uploaded: false,
        library_min_words: String::new(),
        library_max_words: String::new(),
        show_library_filters: true,
        show_upload_tools: true,
        lingq_collections: Vec::new(),
        lingq_selected_collection: initial_collection_id,
        lingq_selected_articles: HashSet::new(),
        lingq_only_not_uploaded: true,
        lingq_min_words: String::new(),
        lingq_max_words: String::new(),
        show_lingq_settings: false,
        lingq_api_key: initial_api_key,
        lingq_username: String::new(),
        lingq_password: String::new(),
        stats: None,
        progress: None,
        delete_confirm_id: None,
        preview_wide: false,
    }));

    wire_callbacks(&window, &state);

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
        }
    });

    window.run()
}

fn wire_callbacks(window: &AppWindow, state: &Rc<RefCell<AppState>>) {
    let state_clone = state.clone();
    window.on_switch_page(move |index| {
        let mut app = state_clone.borrow_mut();
        app.current_view = if index == 0 { View::Browse } else { View::Library };
        app.save_settings();
        app.sync_to_window();
    });

    let state_clone = state.clone();
    window.on_browse_refresh(move || {
        let mut app = state_clone.borrow_mut();
        app.pull_inputs_from_window();
        app.browse_limit = 80;
        app.load_browse();
    });

    let state_clone = state.clone();
    window.on_browse_load_more(move || {
        let mut app = state_clone.borrow_mut();
        app.pull_inputs_from_window();
        app.browse_limit = app.browse_limit.saturating_add(80);
        app.load_browse();
    });

    let state_clone = state.clone();
    window.on_browse_toggle_article(move |index| {
        let mut app = state_clone.borrow_mut();
        if let Some(url) = app.browse_articles.get(index as usize).map(|article| article.url.clone()) {
            if !app.browse_selected.insert(url.clone()) {
                app.browse_selected.remove(&url);
            }
        }
        app.sync_to_window();
    });

    let state_clone = state.clone();
    window.on_browse_select_all_new(move || {
        let mut app = state_clone.borrow_mut();
        app.browse_selected.clear();
        let urls = app
            .filtered_browse_articles()
            .into_iter()
            .map(|article| article.url.clone())
            .collect::<Vec<_>>();
        app.browse_selected.extend(urls);
        app.sync_to_window();
    });

    let state_clone = state.clone();
    window.on_browse_clear_selection(move || {
        let mut app = state_clone.borrow_mut();
        app.browse_selected.clear();
        app.sync_to_window();
    });

    let state_clone = state.clone();
    window.on_browse_save_selected(move || {
        state_clone.borrow_mut().save_browse_selection();
    });

    window.on_browse_open_url(|url| {
        let _ = webbrowser::open(url.as_str());
    });

    let state_clone = state.clone();
    window.on_browse_save_single(move |url| {
        state_clone.borrow_mut().save_single_browse(url.to_string());
    });

    let state_clone = state.clone();
    window.on_browse_toggle_bulk_section(move |index| {
        let mut app = state_clone.borrow_mut();
        if let Some(section) = app.sections.get(index as usize) {
            if !app.bulk_selected_sections.insert(section.id.to_owned()) {
                app.bulk_selected_sections.remove(section.id);
            }
        }
        app.sync_to_window();
    });

    let state_clone = state.clone();
    window.on_browse_bulk_select_all(move || {
        let mut app = state_clone.borrow_mut();
        app.bulk_selected_sections = app.sections.iter().map(|s| s.id.to_owned()).collect();
        app.sync_to_window();
    });

    let state_clone = state.clone();
    window.on_browse_bulk_clear(move || {
        let mut app = state_clone.borrow_mut();
        app.bulk_selected_sections.clear();
        app.sync_to_window();
    });

    let state_clone = state.clone();
    window.on_browse_run_bulk_fetch(move || {
        let mut app = state_clone.borrow_mut();
        app.pull_inputs_from_window();
        app.run_bulk_fetch();
    });

    let state_clone = state.clone();
    window.on_library_apply_filters(move || {
        let mut app = state_clone.borrow_mut();
        app.pull_inputs_from_window();
        app.sync_to_window();
    });

    let state_clone = state.clone();
    window.on_library_toggle_select(move |index| {
        let mut app = state_clone.borrow_mut();
        if let Some(article_id) = app
            .filtered_library_articles()
            .get(index as usize)
            .map(|article| article.id)
        {
            if !app.lingq_selected_articles.insert(article_id) {
                app.lingq_selected_articles.remove(&article_id);
            }
        }
        app.sync_to_window();
    });

    let state_clone = state.clone();
    window.on_library_select_article(move |index| {
        let mut app = state_clone.borrow_mut();
        if let Some(article) = app.filtered_library_articles().get(index as usize) {
            app.selected_article_id = Some(article.id);
        }
        app.delete_confirm_id = None; // Clear any pending delete confirmation
        app.sync_to_window();
    });

    let state_clone = state.clone();
    window.on_library_delete_article(move |index| {
        let mut app = state_clone.borrow_mut();
        if let Some(article) = app.filtered_library_articles().get(index as usize) {
            let article_id = article.id;
            // Two-click confirmation: first click sets confirm, second click deletes
            if app.delete_confirm_id == Some(article_id) {
                // Confirmed — actually delete
                app.delete_confirm_id = None;
                if let Ok(db) = Database::open_default() {
                    let _ = db.delete_article(article_id);
                    app.lingq_selected_articles.remove(&article_id);
                    if app.selected_article_id == Some(article_id) {
                        app.selected_article_id = None;
                    }
                    app.refresh_saved_urls();
                    app.refresh_stats();
                    app.load_library();
                    app.set_status("Deleted article from the local library.");
                }
            } else {
                // First click — ask for confirmation
                app.delete_confirm_id = Some(article_id);
                app.sync_to_window();
            }
        }
    });

    window.on_library_open_url(|url| {
        let _ = webbrowser::open(url.as_str());
    });

    let state_clone = state.clone();
    window.on_library_select_visible_for_upload(move || {
        let mut app = state_clone.borrow_mut();
        app.pull_inputs_from_window();
        let article_ids = app
            .filtered_library_articles()
            .into_iter()
            .filter(|article| app.matches_lingq_upload_filters(article))
            .map(|article| article.id)
            .collect::<Vec<_>>();
        app.lingq_selected_articles.extend(article_ids);
        app.sync_to_window();
    });

    let state_clone = state.clone();
    window.on_library_clear_upload_selection(move || {
        let mut app = state_clone.borrow_mut();
        app.lingq_selected_articles.clear();
        app.sync_to_window();
    });

    let state_clone = state.clone();
    window.on_library_upload_selected(move || {
        let mut app = state_clone.borrow_mut();
        app.pull_inputs_from_window();
        app.upload_selected();
    });

    let state_clone = state.clone();
    window.on_lingq_open_settings(move || {
        let mut app = state_clone.borrow_mut();
        app.show_lingq_settings = true;
        app.sync_to_window();
    });

    let state_clone = state.clone();
    window.on_lingq_close_settings(move || {
        let mut app = state_clone.borrow_mut();
        app.show_lingq_settings = false;
        app.sync_to_window();
    });

    let state_clone = state.clone();
    window.on_lingq_save_token(move || {
        let mut app = state_clone.borrow_mut();
        app.pull_inputs_from_window();
        app.save_settings();
        app.load_collections_if_possible();
        app.set_status("Saved LingQ token.");
        app.sync_to_window();
    });

    let state_clone = state.clone();
    window.on_lingq_login(move || {
        let mut app = state_clone.borrow_mut();
        app.pull_inputs_from_window();
        app.login_to_lingq();
    });

    let state_clone = state.clone();
    window.on_lingq_refresh_collections(move || {
        let mut app = state_clone.borrow_mut();
        app.pull_inputs_from_window();
        app.load_collections();
    });

    let state_clone = state.clone();
    window.on_lingq_disconnect(move || {
        let mut app = state_clone.borrow_mut();
        app.lingq_api_key.clear();
        app.lingq_username.clear();
        app.lingq_password.clear();
        app.lingq_collections.clear();
        app.lingq_selected_collection = None;
        app.save_settings();
        app.set_status("Disconnected LingQ.");
        app.sync_to_window();
    });

    // Enter key in library search applies filters
    let state_clone = state.clone();
    window.on_library_search_accepted(move || {
        let mut app = state_clone.borrow_mut();
        app.pull_inputs_from_window();
        app.sync_to_window();
    });

    // Date range presets: 0=7d, 1=30d, 2=3mo, 3=this year, 4=last year
    let state_clone = state.clone();
    window.on_browse_set_date_preset(move |preset| {
        let mut app = state_clone.borrow_mut();
        let today = chrono::Local::now().date_naive();
        let (from, to) = match preset {
            0 => (today - chrono::Duration::days(7), today),
            1 => (today - chrono::Duration::days(30), today),
            2 => (today - chrono::Duration::days(90), today),
            3 => {
                let year = today.year();
                (
                    NaiveDate::from_ymd_opt(year, 1, 1).unwrap_or(today),
                    today,
                )
            }
            4 => {
                let year = today.year() - 1;
                (
                    NaiveDate::from_ymd_opt(year, 1, 1).unwrap_or(today),
                    NaiveDate::from_ymd_opt(year, 12, 31).unwrap_or(today),
                )
            }
            _ => return,
        };
        app.browse_date_from = from.format("%Y-%m-%d").to_string();
        app.browse_date_to = to.format("%Y-%m-%d").to_string();
        app.sync_to_window();
    });

    // Toggle preview pane width
    let state_clone = state.clone();
    window.on_toggle_preview_width(move || {
        let mut app = state_clone.borrow_mut();
        app.preview_wide = !app.preview_wide;
        app.sync_to_window();
    });
}

impl AppState {
    fn current_section(&self) -> &'static Section {
        self.sections
            .get(self.browse_section_index)
            .unwrap_or(&self.sections[0])
    }

    fn set_status(&mut self, message: impl Into<String>) {
        self.status_message = message.into();
        self.sync_to_window();
    }

    fn pull_inputs_from_window(&mut self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };

        self.browse_section_index = window.get_browse_section_index().max(0) as usize;
        self.browse_only_new = window.get_browse_only_new();
        self.browse_date_from = window.get_browse_date_from().to_string();
        self.browse_date_to = window.get_browse_date_to().to_string();
        self.bulk_max_articles = window.get_bulk_max_articles().to_string();
        self.bulk_per_section_cap = window.get_bulk_per_section_cap().to_string();
        self.bulk_stop_after_old = window.get_bulk_stop_after_old().to_string();
        self.library_search = window.get_library_search().to_string();
        self.library_heading = indexed_label(&self.heading_labels(), window.get_library_heading_index());
        self.library_section = indexed_label(&self.section_labels(), window.get_library_section_index());
        self.library_sort_mode = LibrarySortMode::from_index(window.get_library_sort_index());
        self.library_only_not_uploaded = window.get_library_only_not_uploaded();
        self.library_min_words = window.get_library_min_words().to_string();
        self.library_max_words = window.get_library_max_words().to_string();
        self.show_library_filters = window.get_show_library_filters();
        self.show_upload_tools = window.get_show_upload_tools();
        self.lingq_min_words = window.get_lingq_min_words().to_string();
        self.lingq_max_words = window.get_lingq_max_words().to_string();
        self.lingq_only_not_uploaded = window.get_lingq_only_not_uploaded();
        self.show_lingq_settings = window.get_show_lingq_settings();
        self.lingq_api_key = window.get_lingq_token().to_string();
        self.lingq_username = window.get_lingq_username().to_string();
        self.lingq_password = window.get_lingq_password().to_string();

        let collection_index = window.get_lingq_collection_index().max(0) as usize;
        self.lingq_selected_collection = if collection_index == 0 {
            None
        } else {
            self.lingq_collections
                .get(collection_index.saturating_sub(1))
                .map(|course| course.id)
        };
    }

    fn save_settings(&mut self) {
        let browse_section = self.current_section().id.to_owned();
        let last_view = match self.current_view {
            View::Browse => "browse".to_owned(),
            View::Library => "library".to_owned(),
        };
        let api_key = self.lingq_api_key.clone();
        let collection_id = self.lingq_selected_collection;
        let _ = self.settings.update(|settings| {
            settings.browse_section = browse_section;
            settings.last_view = last_view;
            settings.lingq_api_key = api_key;
            settings.lingq_collection_id = collection_id;
        });
    }

    fn heading_labels(&self) -> Vec<String> {
        let mut labels = vec!["All headings".to_owned()];
        let mut seen = HashSet::new();
        for article in &self.library_articles {
            let heading = section_heading(&article.section);
            if seen.insert(heading.clone()) {
                labels.push(heading);
            }
        }
        labels.sort_by(|left, right| {
            if left == "All headings" {
                std::cmp::Ordering::Less
            } else if right == "All headings" {
                std::cmp::Ordering::Greater
            } else {
                left.cmp(right)
            }
        });
        labels
    }

    fn section_labels(&self) -> Vec<String> {
        let mut labels = vec!["All sections".to_owned()];
        let mut seen = HashSet::new();
        for article in &self.library_articles {
            let section = article.section.trim().to_owned();
            if !section.is_empty() && seen.insert(section.clone()) {
                labels.push(section);
            }
        }
        labels.sort_by(|left, right| {
            if left == "All sections" {
                std::cmp::Ordering::Less
            } else if right == "All sections" {
                std::cmp::Ordering::Greater
            } else {
                left.cmp(right)
            }
        });
        labels
    }

    fn filtered_browse_articles(&self) -> Vec<&ArticleSummary> {
        self.browse_articles
            .iter()
            .filter(|article| !self.browse_only_new || !self.browse_saved_urls.contains(&article.url))
            .collect()
    }

    fn filtered_library_articles(&self) -> Vec<&StoredArticle> {
        let search = self.library_search.trim().to_lowercase();
        let min_words = parse_optional_i64(&self.library_min_words).unwrap_or(None);
        let max_words = parse_optional_i64(&self.library_max_words).unwrap_or(None);
        let heading = self.library_heading.trim();
        let section = self.library_section.trim();

        let mut articles = self
            .library_articles
            .iter()
            .filter(|article| {
                if !search.is_empty() {
                    let haystack =
                        format!("{} {}", article.title, article.body_text).to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }

                if heading != "All headings" && !heading.is_empty() {
                    if section_heading(&article.section) != heading {
                        return false;
                    }
                }

                if section != "All sections" && !section.is_empty() && article.section != section {
                    return false;
                }

                if self.library_only_not_uploaded && article.uploaded_to_lingq {
                    return false;
                }

                if let Some(min_words) = min_words {
                    if article.word_count < min_words {
                        return false;
                    }
                }

                if let Some(max_words) = max_words {
                    if article.word_count > max_words {
                        return false;
                    }
                }

                true
            })
            .collect::<Vec<_>>();

        match self.library_sort_mode {
            LibrarySortMode::Newest => {
                articles.sort_by(|left, right| right.date.cmp(&left.date).then(right.id.cmp(&left.id)))
            }
            LibrarySortMode::Oldest => {
                articles.sort_by(|left, right| left.date.cmp(&right.date).then(left.id.cmp(&right.id)))
            }
            LibrarySortMode::Longest => {
                articles.sort_by(|left, right| right.word_count.cmp(&left.word_count))
            }
            LibrarySortMode::Shortest => {
                articles.sort_by(|left, right| left.word_count.cmp(&right.word_count))
            }
            LibrarySortMode::Title => {
                articles.sort_by(|left, right| left.title.cmp(&right.title))
            }
        }

        articles
    }

    fn matches_lingq_upload_filters(&self, article: &StoredArticle) -> bool {
        if self.lingq_only_not_uploaded && article.uploaded_to_lingq {
            return false;
        }
        if let Ok(Some(min_words)) = parse_optional_i64(&self.lingq_min_words) {
            if article.word_count < min_words {
                return false;
            }
        }
        if let Ok(Some(max_words)) = parse_optional_i64(&self.lingq_max_words) {
            if article.word_count > max_words {
                return false;
            }
        }
        true
    }

    fn sync_to_window(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };

        window.set_page_index(if self.current_view == View::Browse { 0 } else { 1 });
        window.set_browse_section_index(self.browse_section_index as i32);
        window.set_browse_only_new(self.browse_only_new);
        window.set_browse_date_from(self.browse_date_from.clone().into());
        window.set_browse_date_to(self.browse_date_to.clone().into());
        window.set_bulk_max_articles(self.bulk_max_articles.clone().into());
        window.set_bulk_per_section_cap(self.bulk_per_section_cap.clone().into());
        window.set_bulk_stop_after_old(self.bulk_stop_after_old.clone().into());
        window.set_library_search(self.library_search.clone().into());
        window.set_library_sort_index(self.library_sort_mode.index());
        window.set_library_only_not_uploaded(self.library_only_not_uploaded);
        window.set_library_min_words(self.library_min_words.clone().into());
        window.set_library_max_words(self.library_max_words.clone().into());
        window.set_show_library_filters(self.show_library_filters);
        window.set_show_upload_tools(self.show_upload_tools);
        window.set_lingq_min_words(self.lingq_min_words.clone().into());
        window.set_lingq_max_words(self.lingq_max_words.clone().into());
        window.set_lingq_only_not_uploaded(self.lingq_only_not_uploaded);
        window.set_lingq_connected(!self.lingq_api_key.trim().is_empty());
        window.set_show_lingq_settings(self.show_lingq_settings);
        window.set_lingq_token(self.lingq_api_key.clone().into());
        window.set_lingq_username(self.lingq_username.clone().into());
        window.set_lingq_password(self.lingq_password.clone().into());
        window.set_status_message(self.status_message.clone().into());
        window.set_preview_wide(self.preview_wide);

        let browse_section_labels = self
            .sections
            .iter()
            .map(|section| SharedString::from(section.label))
            .collect::<Vec<_>>();
        window.set_browse_section_labels(ModelRc::from(Rc::new(VecModel::from(
            browse_section_labels,
        ))));

        let filtered_browse = self.filtered_browse_articles();
        let browse_rows = filtered_browse
            .iter()
            .map(|article| BrowseRow {
                title: article.title.clone().into(),
                teaser: article.teaser.clone().into(),
                section: article.section.clone().into(),
                source: article.source_kind.as_str().into(),
                source_label: article.source_label.clone().into(),
                url: article.url.clone().into(),
                selected: self.browse_selected.contains(&article.url),
                saved: self.browse_saved_urls.contains(&article.url),
            })
            .collect::<Vec<_>>();
        window.set_browse_rows(ModelRc::from(Rc::new(VecModel::from(browse_rows))));
        let browse_selected = filtered_browse
            .iter()
            .filter(|article| self.browse_selected.contains(&article.url))
            .count();
        let browse_summary = format!(
            "Showing {} article(s) from {}. {} selected. Limit {}.{}",
            filtered_browse.len(),
            self.current_section().label,
            browse_selected,
            self.browse_limit,
            self.browse_report
                .as_ref()
                .map(|report| format!(" {report}"))
                .unwrap_or_default()
        );
        window.set_browse_summary(browse_summary.into());
        window.set_browse_count(if filtered_browse.is_empty() {
            SharedString::new()
        } else {
            SharedString::from(filtered_browse.len().to_string())
        });

        let bulk_rows = self
            .sections
            .iter()
            .map(|section| BulkSectionRow {
                label: section.label.into(),
                selected: self.bulk_selected_sections.contains(section.id),
            })
            .collect::<Vec<_>>();
        window.set_bulk_section_rows(ModelRc::from(Rc::new(VecModel::from(bulk_rows))));
        let bulk_count = self.bulk_selected_sections.len();
        window.set_bulk_sections_label(
            format!("Sections ({})", bulk_count).into(),
        );

        let stat_cards = self
            .stats
            .as_ref()
            .map(|stats| {
                vec![
                    StatCard {
                        label: "Saved".into(),
                        value: stats.total_articles.to_string().into(),
                    },
                    StatCard {
                        label: "Uploaded".into(),
                        value: stats.uploaded_articles.to_string().into(),
                    },
                    StatCard {
                        label: "Average words".into(),
                        value: stats.average_word_count.to_string().into(),
                    },
                    StatCard {
                        label: "Sections".into(),
                        value: stats.sections.len().to_string().into(),
                    },
                ]
            })
            .unwrap_or_else(|| {
                vec![
                    StatCard { label: "Saved".into(), value: "–".into() },
                    StatCard { label: "Uploaded".into(), value: "–".into() },
                    StatCard { label: "Avg words".into(), value: "–".into() },
                    StatCard { label: "Sections".into(), value: "–".into() },
                ]
            });
        window.set_stat_cards(ModelRc::from(Rc::new(VecModel::from(stat_cards))));

        let heading_labels = self.heading_labels();
        let section_labels = self.section_labels();
        let sort_labels = LibrarySortMode::all()
            .into_iter()
            .map(|mode| SharedString::from(mode.label()))
            .collect::<Vec<_>>();
        let heading_index = index_of_label(&heading_labels, &self.library_heading);
        let section_index = index_of_label(&section_labels, &self.library_section);
        window.set_heading_labels(ModelRc::from(Rc::new(VecModel::from(
            heading_labels
                .iter()
                .cloned()
                .map(SharedString::from)
                .collect::<Vec<_>>(),
        ))));
        window.set_section_labels(ModelRc::from(Rc::new(VecModel::from(
            section_labels
                .iter()
                .cloned()
                .map(SharedString::from)
                .collect::<Vec<_>>(),
        ))));
        window.set_sort_labels(ModelRc::from(Rc::new(VecModel::from(sort_labels))));
        window.set_library_heading_index(heading_index as i32);
        window.set_library_section_index(section_index as i32);

        let filtered_library = self.filtered_library_articles();
        let library_rows = filtered_library
            .iter()
            .map(|article| LibraryRow {
                id: article.id as i32,
                title: article.title.clone().into(),
                section: article.section.clone().into(),
                heading: section_heading(&article.section).into(),
                date: article.date.clone().into(),
                words: article.word_count.to_string().into(),
                url: article.url.clone().into(),
                uploaded: article.uploaded_to_lingq,
                selected: self.lingq_selected_articles.contains(&article.id),
                confirming_delete: self.delete_confirm_id == Some(article.id),
                viewing: self.selected_article_id == Some(article.id),
            })
            .collect::<Vec<_>>();
        window.set_library_rows(ModelRc::from(Rc::new(VecModel::from(library_rows))));
        window.set_library_summary(
            format!("Showing {} saved article(s) after filters.", filtered_library.len()).into(),
        );
        window.set_library_count(if self.library_articles.is_empty() {
            SharedString::new()
        } else {
            SharedString::from(self.library_articles.len().to_string())
        });

        let preview = self
            .selected_article_id
            .and_then(|id| self.library_articles.iter().find(|article| article.id == id));
        if let Some(article) = preview {
            window.set_preview_has_article(true);
            window.set_preview_title(article.title.clone().into());
            window.set_preview_meta(
                format!(
                    "{} | {} words | {} | {}",
                    article.section,
                    article.word_count,
                    article.date,
                    if article.uploaded_to_lingq {
                        "On LingQ"
                    } else {
                        "Not on LingQ"
                    }
                )
                .into(),
            );
            window.set_preview_body(article.clean_text.clone().into());
            window.set_preview_url(article.url.clone().into());
        } else {
            window.set_preview_has_article(false);
            window.set_preview_title("Reading pane".into());
            window.set_preview_meta(
                "Select a saved article to preview its cleaned LingQ upload text.".into(),
            );
            window.set_preview_body("".into());
            window.set_preview_url("".into());
        }

        let mut collection_labels = vec![SharedString::from("No course selected")];
        collection_labels.extend(self.lingq_collections.iter().map(|collection| {
            SharedString::from(format!(
                "{} ({})",
                collection.title, collection.lessons_count
            ))
        }));
        let collection_index = self
            .lingq_selected_collection
            .and_then(|selected| {
                self.lingq_collections
                    .iter()
                    .position(|collection| collection.id == selected)
            })
            .map(|index| index + 1)
            .unwrap_or(0);
        window.set_collection_labels(ModelRc::from(Rc::new(VecModel::from(collection_labels))));
        window.set_lingq_collection_index(collection_index as i32);

        let progress_value = self
            .progress
            .as_ref()
            .map(|progress| {
                if progress.total == 0 {
                    0.0
                } else {
                    (progress.completed as f32 / progress.total as f32).clamp(0.0, 1.0)
                }
            })
            .unwrap_or(0.0);
        window.set_progress_visible(self.progress.is_some());
        window.set_progress_label(
            self.progress
                .as_ref()
                .map(|progress| {
                    format!("{} ({} / {})", progress.label, progress.completed, progress.total)
                })
                .unwrap_or_default()
                .into(),
        );
        window.set_progress_value(progress_value);
    }

    fn load_browse(&mut self) {
        self.save_settings();
        self.set_status(format!("Loading {} from taz...", self.current_section().label));
        let section_id = self.current_section().id.to_owned();
        let limit = self.browse_limit;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let scraper = TazClient::new().map_err(|err| err.to_string())?;
                let section = scraper
                    .section_by_id(&section_id)
                    .ok_or_else(|| format!("unknown section '{section_id}'"))?;
                scraper
                    .browse_section_detailed(section, limit)
                    .map_err(|err| err.to_string())
            })();
            let _ = tx.send(AppEvent::BrowseLoaded(result));
        });
    }

    fn refresh_saved_urls(&mut self) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let db = Database::open_default().map_err(|err| err.to_string())?;
                db.get_all_article_urls().map_err(|err| err.to_string())
            })();
            let _ = tx.send(AppEvent::SavedUrlsLoaded(result));
        });
    }

    fn load_library(&mut self) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let db = Database::open_default().map_err(|err| err.to_string())?;
                db.list_articles(None, None, false, 4000)
                    .map_err(|err| err.to_string())
            })();
            let _ = tx.send(AppEvent::LibraryLoaded(result));
        });
    }

    fn refresh_stats(&mut self) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let db = Database::open_default().map_err(|err| err.to_string())?;
                db.get_stats().map_err(|err| err.to_string())
            })();
            let _ = tx.send(AppEvent::StatsLoaded(result));
        });
    }

    fn load_collections_if_possible(&mut self) {
        if !self.lingq_api_key.trim().is_empty() {
            self.load_collections();
        }
    }

    fn load_collections(&mut self) {
        if self.lingq_api_key.trim().is_empty() {
            self.set_status("Save a LingQ token first.");
            return;
        }
        let api_key = self.lingq_api_key.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let lingq = LingqClient::new().map_err(|err| err.to_string())?;
                lingq.get_collections(&api_key, "de").map_err(|err| err.to_string())
            })();
            let _ = tx.send(AppEvent::CollectionsLoaded(result));
        });
    }

    fn login_to_lingq(&mut self) {
        if self.lingq_username.trim().is_empty() || self.lingq_password.is_empty() {
            self.set_status("Enter your LingQ username/email and password.");
            return;
        }
        let username = self.lingq_username.clone();
        let password = self.lingq_password.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let lingq = LingqClient::new().map_err(|err| err.to_string())?;
                lingq
                    .login(&username, &password)
                    .map(|login| login.token)
                    .map_err(|err| err.to_string())
            })();
            let _ = tx.send(AppEvent::LingqLoggedIn(result));
        });
    }

    fn save_browse_selection(&mut self) {
        if self.browse_selected.is_empty() {
            self.set_status("Select at least one article first.");
            return;
        }
        let urls = self.browse_selected.iter().cloned().collect::<Vec<_>>();
        self.run_fetch_job(urls, "Saving selected articles");
    }

    fn save_single_browse(&mut self, url: String) {
        self.run_fetch_job(vec![url], "Saving article");
    }

    fn run_fetch_job(&mut self, urls: Vec<String>, label: &str) {
        let total = urls.len();
        self.progress = Some(FetchProgress {
            label: label.to_owned(),
            completed: 0,
            total,
        });
        self.sync_to_window();
        let tx = self.tx.clone();
        let label = label.to_owned();
        std::thread::spawn(move || {
            let result = (|| {
                let scraper = TazClient::new().map_err(|err| err.to_string())?;
                let db = Database::open_default().map_err(|err| err.to_string())?;
                let mut saved = 0usize;
                let mut failed = Vec::new();
                for (index, url) in urls.into_iter().enumerate() {
                    match scraper.fetch_article(&url) {
                        Ok(article) => match db.save_article(&article) {
                            Ok(_) => saved += 1,
                            Err(err) => failed.push(format!("{}: {}", article.title, err)),
                        },
                        Err(err) => failed.push(format!("{url}: {err}")),
                    }
                    let _ = tx.send(AppEvent::FetchProgress(FetchProgress {
                        label: label.clone(),
                        completed: index + 1,
                        total,
                    }));
                }
                Ok::<_, String>((saved, failed))
            })();

            let event = match result {
                Ok((saved, failed)) => AppEvent::FetchFinished {
                    message: format!("Saved {saved} article(s) to the library."),
                    failed,
                },
                Err(err) => AppEvent::FetchFinished {
                    message: err,
                    failed: Vec::new(),
                },
            };
            let _ = tx.send(event);
        });
    }

    fn run_bulk_fetch(&mut self) {
        if self.bulk_selected_sections.is_empty() {
            self.set_status("Select at least one section for bulk fetch.");
            return;
        }

        let max_articles = match parse_positive_usize_input(&self.bulk_max_articles, "Max run") {
            Ok(value) => value,
            Err(err) => {
                self.set_status(err);
                return;
            }
        };
        let per_section_cap =
            match parse_positive_usize_input(&self.bulk_per_section_cap, "Per section") {
                Ok(value) => value,
                Err(err) => {
                    self.set_status(err);
                    return;
                }
            };
        let stop_after_old =
            match parse_positive_usize_input(&self.bulk_stop_after_old, "Stop after old") {
                Ok(value) => value,
                Err(err) => {
                    self.set_status(err);
                    return;
                }
            };
        let date_from = match parse_date_input(&self.browse_date_from) {
            Ok(Some(date)) => date,
            Ok(None) => {
                self.set_status("Enter a From date for bulk fetch.");
                return;
            }
            Err(err) => {
                self.set_status(err);
                return;
            }
        };
        let date_to = match parse_date_input(&self.browse_date_to) {
            Ok(Some(date)) => date,
            Ok(None) => {
                self.set_status("Enter a To date for bulk fetch.");
                return;
            }
            Err(err) => {
                self.set_status(err);
                return;
            }
        };
        if date_from > date_to {
            self.set_status("From date must be on or before To date.");
            return;
        }

        let section_ids = self.bulk_selected_sections.iter().cloned().collect::<Vec<_>>();
        let imported_urls = self.browse_saved_urls.clone();
        self.progress = Some(FetchProgress {
            label: "Fetching selected sections".to_owned(),
            completed: 0,
            total: max_articles,
        });
        self.sync_to_window();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let scraper = TazClient::new().map_err(|err| err.to_string())?;
                let db = Database::open_default().map_err(|err| err.to_string())?;
                let discovery_limit = per_section_cap.saturating_mul(4).max(160);
                let mut seen = HashSet::new();
                let mut saved = 0usize;
                let mut skipped_existing = 0usize;
                let mut skipped_out_of_range = 0usize;
                let mut failed = Vec::new();

                for section_id in section_ids {
                    let section = scraper
                        .section_by_id(&section_id)
                        .ok_or_else(|| format!("unknown section '{section_id}'"))?;
                    let mut accepted_for_section = 0usize;
                    let mut consecutive_old = 0usize;

                    for summary in scraper
                        .browse_section(section, discovery_limit)
                        .map_err(|err| err.to_string())?
                    {
                        if saved >= max_articles || accepted_for_section >= per_section_cap {
                            break;
                        }
                        if imported_urls.contains(&summary.url) {
                            skipped_existing += 1;
                            continue;
                        }
                        if !seen.insert(summary.url.clone()) {
                            continue;
                        }

                        match scraper.fetch_article(&summary.url) {
                            Ok(article) => {
                                let Some(article_date) = parse_article_date(&article.date) else {
                                    consecutive_old += 1;
                                    if consecutive_old >= stop_after_old {
                                        break;
                                    }
                                    continue;
                                };

                                if article_date < date_from || article_date > date_to {
                                    skipped_out_of_range += 1;
                                    consecutive_old += 1;
                                    if consecutive_old >= stop_after_old {
                                        break;
                                    }
                                    continue;
                                }

                                consecutive_old = 0;
                                match db.save_article(&article) {
                                    Ok(_) => {
                                        saved += 1;
                                        accepted_for_section += 1;
                                        let _ = tx.send(AppEvent::FetchProgress(FetchProgress {
                                            label: "Fetching selected sections".to_owned(),
                                            completed: saved.min(max_articles),
                                            total: max_articles,
                                        }));
                                    }
                                    Err(err) => {
                                        failed.push(format!("{}: {}", article.title, err));
                                    }
                                }
                            }
                            Err(err) => failed.push(format!("{}: {}", summary.title, err)),
                        }
                    }

                    if saved >= max_articles {
                        break;
                    }
                }

                Ok::<_, String>((saved, skipped_existing, skipped_out_of_range, failed))
            })();

            let event = match result {
                Ok((saved, skipped_existing, skipped_out_of_range, failed)) => {
                    AppEvent::FetchFinished {
                        message: format!(
                            "Saved {saved} article(s). Skipped {skipped_existing} existing and {skipped_out_of_range} out-of-range article(s)."
                        ),
                        failed,
                    }
                }
                Err(err) => AppEvent::FetchFinished {
                    message: err,
                    failed: Vec::new(),
                },
            };
            let _ = tx.send(event);
        });
    }

    fn upload_selected(&mut self) {
        if self.lingq_api_key.trim().is_empty() {
            self.set_status("Open LingQ settings and save a token first.");
            return;
        }
        if self.lingq_selected_articles.is_empty() {
            self.set_status("Select at least one saved article to upload.");
            return;
        }

        let ids = self
            .filtered_library_articles()
            .into_iter()
            .filter(|article| {
                self.lingq_selected_articles.contains(&article.id)
                    && self.matches_lingq_upload_filters(article)
            })
            .map(|article| article.id)
            .collect::<Vec<_>>();
        if ids.is_empty() {
            self.set_status("No selected articles match the current LingQ upload filters.");
            return;
        }

        let api_key = self.lingq_api_key.clone();
        let collection_id = self.lingq_selected_collection;
        self.progress = Some(FetchProgress {
            label: "Uploading to LingQ".to_owned(),
            completed: 0,
            total: ids.len(),
        });
        self.sync_to_window();

        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let lingq = LingqClient::new().map_err(|err| err.to_string())?;
                let db = Database::open_default().map_err(|err| err.to_string())?;
                let mut uploaded = 0usize;
                let mut failed = Vec::new();
                let total = ids.len();

                for (index, id) in ids.into_iter().enumerate() {
                    let Some(article) = db.get_article(id).map_err(|err| err.to_string())? else {
                        failed.push(format!("article #{id} not found"));
                        continue;
                    };
                    let request = UploadRequest {
                        api_key: api_key.clone(),
                        language_code: "de".to_owned(),
                        collection_id,
                        title: article.title.clone(),
                        text: article.clean_text.clone(),
                        original_url: Some(article.url.clone()),
                    };

                    match lingq.upload_lesson(&request) {
                        Ok(response) => {
                            if let Err(err) =
                                db.mark_uploaded(article.id, response.lesson_id, &response.lesson_url)
                            {
                                failed.push(format!(
                                    "{} uploaded but DB update failed: {}",
                                    article.title, err
                                ));
                            } else {
                                uploaded += 1;
                            }
                        }
                        Err(err) => failed.push(format!("{}: {}", article.title, err)),
                    }

                    let _ = tx.send(AppEvent::FetchProgress(FetchProgress {
                        label: "Uploading to LingQ".to_owned(),
                        completed: index + 1,
                        total,
                    }));
                }

                Ok::<_, String>((uploaded, failed))
            })();

            let event = match result {
                Ok((uploaded, failed)) => AppEvent::UploadFinished { uploaded, failed },
                Err(err) => AppEvent::UploadFinished {
                    uploaded: 0,
                    failed: vec![err],
                },
            };
            let _ = tx.send(event);
        });
    }

    fn poll_events(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                AppEvent::BrowseLoaded(result) => match result {
                    Ok(result) => {
                        self.browse_articles = result.articles;
                        self.browse_report = Some(format!(
                            "Sources: {} section / {} subsection / {} topic, {} deduped.",
                            result.report.section_articles,
                            result.report.subsection_articles,
                            result.report.topic_articles,
                            result.report.deduped_articles
                        ));
                        self.set_status(format!(
                            "Loaded {} article candidates from taz.",
                            self.browse_articles.len()
                        ));
                    }
                    Err(err) => self.set_status(err),
                },
                AppEvent::SavedUrlsLoaded(result) => match result {
                    Ok(urls) => {
                        self.browse_saved_urls = urls;
                        self.sync_to_window();
                    }
                    Err(err) => self.set_status(err),
                },
                AppEvent::LibraryLoaded(result) => match result {
                    Ok(articles) => {
                        self.library_articles = articles;
                        if self.selected_article_id.is_none() {
                            self.selected_article_id = self.library_articles.first().map(|a| a.id);
                        }
                        self.sync_to_window();
                    }
                    Err(err) => self.set_status(err),
                },
                AppEvent::StatsLoaded(result) => match result {
                    Ok(stats) => {
                        self.stats = Some(stats);
                        self.sync_to_window();
                    }
                    Err(err) => self.set_status(err),
                },
                AppEvent::FetchProgress(progress) => {
                    self.progress = Some(progress);
                    self.sync_to_window();
                }
                AppEvent::FetchFinished { message, failed } => {
                    self.progress = None;
                    let suffix = if failed.is_empty() {
                        String::new()
                    } else {
                        format!(" {} item(s) failed.", failed.len())
                    };
                    self.set_status(format!("{message}{suffix}"));
                    self.refresh_saved_urls();
                    self.refresh_stats();
                    self.load_library();
                    self.load_browse();
                }
                AppEvent::CollectionsLoaded(result) => match result {
                    Ok(collections) => {
                        self.lingq_collections = collections;
                        if self.lingq_selected_collection.is_none() {
                            self.lingq_selected_collection =
                                self.lingq_collections.first().map(|course| course.id);
                        }
                        self.set_status("LingQ courses refreshed.");
                    }
                    Err(err) => self.set_status(err),
                },
                AppEvent::LingqLoggedIn(result) => match result {
                    Ok(token) => {
                        self.lingq_api_key = token;
                        self.save_settings();
                        self.load_collections();
                        self.set_status("LingQ login succeeded.");
                    }
                    Err(err) => self.set_status(err),
                },
                AppEvent::UploadFinished { uploaded, failed } => {
                    self.progress = None;
                    let suffix = if failed.is_empty() {
                        String::new()
                    } else {
                        format!(" {} upload(s) failed.", failed.len())
                    };
                    self.set_status(format!("Uploaded {uploaded} article(s) to LingQ.{suffix}"));
                    self.refresh_stats();
                    self.load_library();
                }
            }
        }
    }
}

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
