use crate::{
    database::{Database, LibraryStats, StoredArticle},
    lingq::{Collection, LingqClient, UploadRequest},
    settings::SettingsStore,
    taz::{ArticleSummary, DiscoverySourceKind, Section, TazClient},
};
use chrono::NaiveDate;
use eframe::egui::{
    self, Align, CentralPanel, Color32, ComboBox, Context, Frame, Layout, Margin, RichText,
    ScrollArea, SidePanel, Stroke, TextEdit, TopBottomPanel, ViewportBuilder,
};
use std::{
    collections::HashSet,
    sync::mpsc::{self, Receiver, Sender},
    time::Instant,
};

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
    fn label(self) -> &'static str {
        match self {
            Self::Newest => "Newest",
            Self::Oldest => "Oldest",
            Self::Longest => "Longest",
            Self::Shortest => "Shortest",
            Self::Title => "Title",
        }
    }

    fn all() -> [Self; 5] {
        [Self::Newest, Self::Oldest, Self::Longest, Self::Shortest, Self::Title]
    }
}

#[derive(Clone)]
struct FetchProgress {
    label: String,
    completed: usize,
    total: usize,
}

enum AppEvent {
    BrowseLoaded(Result<Vec<ArticleSummary>, String>),
    SavedUrlsLoaded(Result<HashSet<String>, String>),
    LibraryLoaded(Result<Vec<StoredArticle>, String>),
    StatsLoaded(Result<LibraryStats, String>),
    FetchProgress(FetchProgress),
    FetchFinished { message: String, failed: Vec<String> },
    CollectionsLoaded(Result<Vec<Collection>, String>),
    LingqLoggedIn(Result<String, String>),
    UploadFinished { uploaded: usize, failed: Vec<String> },
}

pub fn run() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_title("taz Reader")
            .with_inner_size([1320.0, 860.0])
            .with_min_inner_size([980.0, 680.0]),
        ..Default::default()
    };

    eframe::run_native(
        "taz Reader",
        options,
        Box::new(|cc| Ok(Box::new(TazReaderApp::new(cc)))),
    )
}

struct TazReaderApp {
    tx: Sender<AppEvent>,
    rx: Receiver<AppEvent>,
    settings: SettingsStore,
    sections: &'static [Section],
    current_view: View,
    status_message: String,
    last_notice_at: Instant,
    browse_section_index: usize,
    browse_limit: usize,
    browse_articles: Vec<ArticleSummary>,
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
    show_lingq_upload: bool,
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
}

impl TazReaderApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        configure_theme(&cc.egui_ctx);

        let settings = SettingsStore::load_default().unwrap_or_else(|_| {
            SettingsStore::load(std::path::PathBuf::from("settings.json"))
                .expect("fallback settings store")
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
        let (tx, rx) = mpsc::channel();

        let mut app = Self {
            tx,
            rx,
            settings,
            sections,
            current_view,
            status_message: "Loading taz sections, library, and LingQ status.".to_owned(),
            last_notice_at: Instant::now(),
            browse_section_index,
            browse_limit: 80,
            browse_articles: Vec::new(),
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
            show_lingq_upload: true,
            lingq_collections: Vec::new(),
            lingq_selected_collection: settings.data().lingq_collection_id,
            lingq_selected_articles: HashSet::new(),
            lingq_only_not_uploaded: true,
            lingq_min_words: String::new(),
            lingq_max_words: String::new(),
            show_lingq_settings: false,
            lingq_api_key: settings.data().lingq_api_key.clone(),
            lingq_username: String::new(),
            lingq_password: String::new(),
            stats: None,
            progress: None,
        };

        app.refresh_saved_urls();
        app.refresh_stats();
        app.load_library();
        app.load_browse();
        app.load_collections_if_possible();
        app
    }

    fn save_settings(&mut self) {
        let browse_section = self.current_section().id.to_owned();
        let last_view = match self.current_view {
            View::Browse => "browse".to_owned(),
            View::Library => "library".to_owned(),
        };
        let api_key = self.lingq_api_key.clone();
        let selected_collection = self.lingq_selected_collection;
        let _ = self.settings.update(|settings| {
            settings.browse_section = browse_section;
            settings.last_view = last_view;
            settings.lingq_api_key = api_key;
            settings.lingq_collection_id = selected_collection;
        });
    }

    fn current_section(&self) -> &'static Section {
        self.sections
            .get(self.browse_section_index)
            .copied()
            .unwrap_or(&self.sections[0])
    }

    fn set_status(&mut self, message: impl Into<String>) {
        self.status_message = message.into();
        self.last_notice_at = Instant::now();
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
                scraper.browse_section(section, limit).map_err(|err| err.to_string())
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
                db.list_articles(None, None, false, 2000)
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
            Ok(Some(value)) => value,
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
            Ok(Some(value)) => value,
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
                    for summary in scraper.browse_section(section, discovery_limit).map_err(|err| err.to_string())? {
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
                                    if consecutive_old >= stop_after_old { break; }
                                    continue;
                                };
                                if article_date < date_from || article_date > date_to {
                                    skipped_out_of_range += 1;
                                    consecutive_old += 1;
                                    if consecutive_old >= stop_after_old { break; }
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
                                    Err(err) => failed.push(format!("{}: {}", article.title, err)),
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
                Ok((saved, skipped_existing, skipped_out_of_range, failed)) => AppEvent::FetchFinished {
                    message: format!("Saved {saved} article(s). Skipped {skipped_existing} existing and {skipped_out_of_range} out-of-range article(s)."),
                    failed,
                },
                Err(err) => AppEvent::FetchFinished { message: err, failed: Vec::new() },
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
        let ids = self.lingq_selected_articles.iter().copied().collect::<Vec<_>>();
        let api_key = self.lingq_api_key.clone();
        let collection_id = self.lingq_selected_collection;
        self.progress = Some(FetchProgress {
            label: "Uploading to LingQ".to_owned(),
            completed: 0,
            total: ids.len(),
        });
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
                                failed.push(format!("{} uploaded but DB update failed: {}", article.title, err));
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
                    Ok(articles) => {
                        self.browse_articles = articles;
                        self.set_status(format!("Loaded {} article candidates from taz.", self.browse_articles.len()));
                    }
                    Err(err) => self.set_status(err),
                },
                AppEvent::SavedUrlsLoaded(result) => match result {
                    Ok(urls) => self.browse_saved_urls = urls,
                    Err(err) => self.set_status(err),
                },
                AppEvent::LibraryLoaded(result) => match result {
                    Ok(articles) => {
                        self.library_articles = articles;
                        if self.selected_article_id.is_none() {
                            self.selected_article_id = self.library_articles.first().map(|a| a.id);
                        }
                    }
                    Err(err) => self.set_status(err),
                },
                AppEvent::StatsLoaded(result) => match result {
                    Ok(stats) => self.stats = Some(stats),
                    Err(err) => self.set_status(err),
                },
                AppEvent::FetchProgress(progress) => self.progress = Some(progress),
                AppEvent::FetchFinished { message, failed } => {
                    self.progress = None;
                    let suffix = if failed.is_empty() { String::new() } else { format!(" {} item(s) failed.", failed.len()) };
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
                            self.lingq_selected_collection = self.lingq_collections.first().map(|c| c.id);
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
                    let suffix = if failed.is_empty() { String::new() } else { format!(" {} upload(s) failed.", failed.len()) };
                    self.set_status(format!("Uploaded {uploaded} article(s) to LingQ.{suffix}"));
                    self.refresh_stats();
                    self.load_library();
                }
            }
        }
    }
}
