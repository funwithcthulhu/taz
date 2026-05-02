use super::*;

async fn wait_for_cancel(cancel: Arc<AtomicBool>) {
    while !cancel.load(Ordering::Relaxed) {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

async fn cancelable_sleep(cancel: Arc<AtomicBool>, duration: tokio::time::Duration) -> bool {
    tokio::select! {
        _ = wait_for_cancel(cancel) => true,
        _ = tokio::time::sleep(duration) => false,
    }
}

const ARTICLE_FETCH_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(20);
const SECTION_DISCOVERY_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(35);

fn search_date_label(date_from: Option<NaiveDate>, date_to: Option<NaiveDate>) -> String {
    match (date_from, date_to) {
        (Some(from), Some(to)) => format!(" from {from} to {to}"),
        (Some(from), None) => format!(" from {from}"),
        (None, Some(to)) => format!(" through {to}"),
        (None, None) => String::new(),
    }
}

fn compact_url_label(url: &str) -> String {
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    if without_scheme.len() > 54 {
        format!("{}...", &without_scheme[..54])
    } else {
        without_scheme.to_owned()
    }
}

impl AppState {
    pub(super) fn load_browse(&mut self) {
        self.save_settings();
        self.set_status(format!("Loading {} from taz...", self.current_section().label));
        let section_id = self.current_section().id.to_owned();
        let limit = self.browse.limit;
        let scraper = self.scraper.clone();
        self.spawn_background(async move {
            let result = async {
                let section = scraper
                    .section_by_id(&section_id)
                    .ok_or_else(|| format!("unknown section '{section_id}'"))?;
                scraper
                    .browse_section_detailed(section, limit)
                    .await
                    .map_err(|err| format!("{err:#}"))
            }
            .await;
            AppEvent::BrowseLoaded(result)
        });
    }

    pub(super) fn run_taz_search(&mut self) {
        let query = self.browse.search_query.trim().to_owned();
        if query.is_empty() {
            self.set_status("Enter a search term first.");
            return;
        }
        let date_from = match parse_date_input(&self.browse.date_from) {
            Ok(date) => date,
            Err(err) => {
                self.set_status(err);
                return;
            }
        };
        let date_to = match parse_date_input(&self.browse.date_to) {
            Ok(date) => date,
            Err(err) => {
                self.set_status(err);
                return;
            }
        };
        if let (Some(from), Some(to)) = (date_from, date_to) {
            if from > to {
                self.set_status("From date must be on or before To date.");
                return;
            }
        }

        let date_label = search_date_label(date_from, date_to);
        self.set_status(format!("Searching taz.de for \"{query}\"{date_label}..."));
        let scraper = self.scraper.clone();
        self.spawn_background(async move {
            let result = async {
                let articles = scraper
                    .search_articles(&query, 5)
                    .await
                    .map_err(|err| format!("{err:#}"))?;

                if date_from.is_none() && date_to.is_none() {
                    return Ok(articles);
                }

                let mut filtered = Vec::new();
                for article in articles {
                    let metadata = tokio::time::timeout(
                        ARTICLE_FETCH_TIMEOUT,
                        scraper.fetch_article_metadata(&article.url),
                    )
                    .await
                    .map_err(|_| {
                        format!(
                            "{}: metadata lookup timed out after {}s",
                            article.title,
                            ARTICLE_FETCH_TIMEOUT.as_secs()
                        )
                    });

                    let Ok(Ok(metadata)) = metadata else {
                        continue;
                    };
                    let Some(article_date) = parse_article_date(&metadata.date) else {
                        continue;
                    };
                    if date_from.is_some_and(|from| article_date < from) {
                        continue;
                    }
                    if date_to.is_some_and(|to| article_date > to) {
                        continue;
                    }

                    let mut article = article;
                    article.section = metadata.section;
                    filtered.push(article);
                    tokio::time::sleep(REQUEST_THROTTLE).await;
                }

                Ok(filtered)
            }
            .await;
            AppEvent::SearchLoaded(result)
        });
    }

    pub(super) fn refresh_saved_urls(&mut self) {
        let db = self.db.clone();
        self.spawn_background(async move {
            let result = tokio::task::spawn_blocking(move || {
                db.get_all_article_urls()
            }).await.unwrap_or_else(|err| Err(anyhow::anyhow!("{err}")))
            .map_err(|err| format!("{err:#}"));
            AppEvent::SavedUrlsLoaded(result)
        });
    }

    pub(super) fn load_library(&mut self) {
        let db = self.db.clone();
        let query = self.build_library_query();
        self.spawn_background(async move {
            let result = tokio::task::spawn_blocking(move || {
                db.list_articles_meta(&query)
            }).await.unwrap_or_else(|err| Err(anyhow::anyhow!("{err}")))
            .map_err(|err| format!("{err:#}"));
            AppEvent::LibraryLoaded(result)
        });
    }

    fn build_library_query(&self) -> ArticleQuery {
        let search = {
            let s = self.library.search.trim();
            if s.is_empty() { None } else { Some(s.to_owned()) }
        };
        let section = {
            let s = self.library.section.trim();
            if s.is_empty() || s == "All sections" { None } else { Some(s.to_owned()) }
        };
        let sort = Some(match self.library.sort_mode {
            LibrarySortMode::Newest => "newest",
            LibrarySortMode::Oldest => "oldest",
            LibrarySortMode::Longest => "longest",
            LibrarySortMode::Shortest => "shortest",
            LibrarySortMode::Title => "title",
        }.to_owned());

        ArticleQuery {
            search,
            section,
            only_not_uploaded: self.library.only_not_uploaded,
            min_words: parse_optional_i64(&self.library.min_words).unwrap_or(None),
            max_words: parse_optional_i64(&self.library.max_words).unwrap_or(None),
            sort,
            limit: 4000,
        }
    }

    pub(super) fn refresh_stats(&mut self) {
        let db = self.db.clone();
        self.spawn_background(async move {
            let result = tokio::task::spawn_blocking(move || {
                db.get_stats()
            }).await.unwrap_or_else(|err| Err(anyhow::anyhow!("{err}")))
            .map_err(|err| format!("{err:#}"));
            AppEvent::StatsLoaded(result)
        });
    }

    pub(super) fn load_collections_if_possible(&mut self) {
        if !self.lq.api_key.trim().is_empty() {
            self.load_collections();
        }
    }

    pub(super) fn load_collections(&mut self) {
        if self.lq.api_key.trim().is_empty() {
            self.set_status("Save a LingQ token first.");
            return;
        }
        if !is_plausible_api_key(&self.lq.api_key) {
            self.set_status("LingQ token looks invalid — expected 8+ alphanumeric characters.");
            return;
        }
        let api_key = self.lq.api_key.clone();
        let language = self.lq.language.clone();
        let lingq = self.lingq.clone();
        self.spawn_background(async move {
            let result = lingq.get_collections(&api_key, &language).await.map_err(|err| format!("{err:#}"));
            AppEvent::CollectionsLoaded(result)
        });
    }

    pub(super) fn login_to_lingq(&mut self) {
        if self.lq.username.trim().is_empty() || self.lq.password.is_empty() {
            self.set_status("Enter your LingQ username/email and password.");
            return;
        }
        // Rate-limit login attempts to prevent API lockout
        if let Some(until) = self.lq.login_cooldown_until {
            if std::time::Instant::now() < until {
                let secs = until.duration_since(std::time::Instant::now()).as_secs();
                self.set_status(&format!(
                    "Too many failed login attempts. Try again in {secs}s."
                ));
                return;
            }
            self.lq.login_cooldown_until = None;
        }
        let username = self.lq.username.clone();
        let password = self.lq.password.clone();
        let lingq = self.lingq.clone();
        self.spawn_background(async move {
            let result = lingq
                .login(&username, &password)
                .await
                .map(|login| login.token)
                .map_err(|err| format!("{err:#}"));
            AppEvent::LingqLoggedIn(result)
        });
    }

    pub(super) fn save_browse_selection(&mut self) {
        if self.browse.selected.is_empty() {
            self.set_status("Select at least one article first.");
            return;
        }
        let urls = self.browse.selected.iter().cloned().collect::<Vec<_>>();
        self.enqueue_job(QueuedJob::SaveUrls {
            urls,
            label: "Saving selected articles".to_owned(),
        });
    }

    pub(super) fn save_single_browse(&mut self, url: String) {
        self.enqueue_job(QueuedJob::SaveUrls {
            urls: vec![url],
            label: "Saving article".to_owned(),
        });
    }

    fn enqueue_job(&mut self, job: QueuedJob) {
        let label = job.label();
        self.job_queue.push_back(job);
        let queue_len = self.job_queue.len();
        self.add_activity("queued", &label, format!("{queue_len} job(s) waiting."));
        self.dirty.progress = true;
        if self.background_job_active() {
            self.set_status(format!("Queued: {label}"));
            return;
        }
        self.start_next_job_if_idle();
    }

    fn start_next_job_if_idle(&mut self) {
        if self.background_job_active() {
            return;
        }
        let Some(job) = self.job_queue.pop_front() else {
            self.dirty.progress = true;
            self.sync_to_window();
            return;
        };
        let label = job.label();
        self.add_activity("started", &label, "Background job started.");
        match job {
            QueuedJob::SaveUrls { urls, label } => self.start_save_job(urls, &label),
            QueuedJob::BulkFetch {
                section_ids,
                imported_urls,
                max_articles,
                per_section_cap,
                stop_after_old,
                date_from,
                date_to,
            } => self.start_bulk_fetch_job(
                section_ids,
                imported_urls,
                max_articles,
                per_section_cap,
                stop_after_old,
                date_from,
                date_to,
            ),
            QueuedJob::UploadArticles {
                ids,
                api_key,
                language,
                collection_id,
            } => self.start_upload_job(ids, api_key, language, collection_id),
        }
    }

    fn start_save_job(&mut self, urls: Vec<String>, label: &str) {
        let total = urls.len();
        self.cancel_flag.store(false, Ordering::Relaxed);
        self.save_progress = Some(FetchProgress {
            label: label.to_owned(),
            completed: 0,
            total,
        });
        self.sync_to_window();
        let tx = self.tx.clone();
        let label = label.to_owned();
        let scraper = self.scraper.clone();
        let db = self.db.clone();
        let cancel = self.cancel_flag.clone();
        let handle = self.runtime.spawn(async move {
            let result: Result<(usize, Vec<JobFailure>), String> = async {
                let mut saved = 0usize;
                let mut failed = Vec::new();
                // Overall time cap: 10 minutes for the entire batch
                let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(600);
                for (index, url) in urls.into_iter().enumerate() {
                    if cancel.load(Ordering::Relaxed) || tokio::time::Instant::now() > deadline {
                        if tokio::time::Instant::now() > deadline {
                            failed.push(JobFailure {
                                label: "Save batch".to_owned(),
                                detail: "Operation timed out (10 min limit).".to_owned(),
                                retry: None,
                            });
                            cancel.store(true, Ordering::Relaxed);
                        }
                        break;
                    }
                    if index > 0 {
                        if cancelable_sleep(cancel.clone(), REQUEST_THROTTLE).await {
                            break;
                        }
                    }
                    let _ = tx.send(AppEvent::SaveProgress(FetchProgress {
                        label: format!("Saving {}", compact_url_label(&url)),
                        completed: index,
                        total,
                    }));
                    let mut fetch_result = Err(anyhow::anyhow!("not attempted"));
                    for attempt in 1..=2 {
                        let attempt_label = if attempt == 1 {
                            format!("Saving {}", compact_url_label(&url))
                        } else {
                            format!("Retry {attempt}/2: {}", compact_url_label(&url))
                        };
                        let _ = tx.send(AppEvent::SaveProgress(FetchProgress {
                            label: attempt_label,
                            completed: index,
                            total,
                        }));
                        fetch_result = tokio::select! {
                            _ = wait_for_cancel(cancel.clone()) => break,
                            result = tokio::time::timeout(ARTICLE_FETCH_TIMEOUT, scraper.fetch_article(&url)) => {
                                match result {
                                    Ok(result) => result,
                                    Err(_) => Err(anyhow::anyhow!("fetch timed out after {}s", ARTICLE_FETCH_TIMEOUT.as_secs())),
                                }
                            },
                        };
                        if fetch_result.is_ok() || attempt == 2 {
                            break;
                        }
                        if cancelable_sleep(cancel.clone(), tokio::time::Duration::from_millis(700)).await {
                            break;
                        }
                    }
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    match fetch_result {
                        Ok(article) => match db.save_article(&article) {
                            Ok(_) => saved += 1,
                            Err(err) => failed.push(JobFailure {
                                label: article.title,
                                detail: err.to_string(),
                                retry: Some(RetryAction::SaveUrl(url.clone())),
                            }),
                        },
                        Err(err) => failed.push(JobFailure {
                            label: compact_url_label(&url),
                            detail: err.to_string(),
                            retry: Some(RetryAction::SaveUrl(url.clone())),
                        }),
                    }
                    let _ = tx.send(AppEvent::SaveProgress(FetchProgress {
                        label: label.clone(),
                        completed: index + 1,
                        total,
                    }));
                }
                Ok((saved, failed))
            }
            .await;

            let cancelled = cancel.load(Ordering::Relaxed);
            let event = match result {
                Ok((saved, failed)) => AppEvent::SaveFinished {
                    message: if cancelled {
                        format!("Cancelled. Saved {saved} article(s) before stopping.")
                    } else {
                        format!("Saved {saved} article(s) to the library.")
                    },
                    failed,
                },
                Err(err) => AppEvent::SaveFinished {
                    message: err,
                    failed: Vec::new(),
                },
            };
            let _ = tx.send(event);
        });
        self.set_current_job(handle);
    }

    pub(super) fn run_bulk_fetch(&mut self) {
        if self.bulk.selected_sections.is_empty() {
            self.set_status("Select at least one section for bulk fetch.");
            return;
        }

        let max_articles = match parse_positive_usize_input(&self.bulk.max_articles, "Max run") {
            Ok(value) => value,
            Err(err) => {
                self.set_status(err);
                return;
            }
        };
        let per_section_cap =
            match parse_positive_usize_input(&self.bulk.per_section_cap, "Per section") {
                Ok(value) => value,
                Err(err) => {
                    self.set_status(err);
                    return;
                }
            };
        let stop_after_old =
            match parse_positive_usize_input(&self.bulk.stop_after_old, "Stop after old") {
                Ok(value) => value,
                Err(err) => {
                    self.set_status(err);
                    return;
                }
            };
        let date_from = match parse_date_input(&self.browse.date_from) {
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
        let date_to = match parse_date_input(&self.browse.date_to) {
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

        let section_ids = self.bulk.selected_sections.iter().cloned().collect::<Vec<_>>();
        let imported_urls = self.browse.saved_urls.clone();
        self.enqueue_job(QueuedJob::BulkFetch {
            section_ids,
            imported_urls,
            max_articles,
            per_section_cap,
            stop_after_old,
            date_from,
            date_to,
        });
    }

    fn start_bulk_fetch_job(
        &mut self,
        section_ids: Vec<String>,
        imported_urls: HashSet<String>,
        max_articles: usize,
        per_section_cap: usize,
        stop_after_old: usize,
        date_from: NaiveDate,
        date_to: NaiveDate,
    ) {
        self.cancel_flag.store(false, Ordering::Relaxed);
        self.progress = Some(FetchProgress {
            label: "Fetching selected sections".to_owned(),
            completed: 0,
            total: max_articles,
        });
        self.sync_to_window();
        let tx = self.tx.clone();
        let scraper = self.scraper.clone();
        let db = self.db.clone();
        let cancel = self.cancel_flag.clone();
        let handle = self.runtime.spawn(async move {
            let result: Result<(usize, usize, usize, Vec<JobFailure>, bool), String> = async {
                let discovery_limit = per_section_cap.saturating_mul(4).max(160);
                let mut seen = HashSet::new();
                let mut saved = 0usize;
                let mut skipped_existing = 0usize;
                let mut skipped_out_of_range = 0usize;
                let mut failed = Vec::new();

                for (section_idx, section_id) in section_ids.iter().enumerate() {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }

                    let section = scraper
                        .section_by_id(section_id)
                        .ok_or_else(|| format!("unknown section '{section_id}'"))?;
                    let mut accepted_for_section = 0usize;
                    let mut consecutive_old = 0usize;

                    // Show which section is being scanned so the user knows it's not stuck
                    let _ = tx.send(AppEvent::FetchProgress(FetchProgress {
                        label: format!(
                            "Scanning {} ({}/{})",
                            section.label,
                            section_idx + 1,
                            section_ids.len()
                        ),
                        completed: saved.min(max_articles),
                        total: max_articles,
                    }));

                    let summaries = tokio::select! {
                        _ = wait_for_cancel(cancel.clone()) => break,
                        result = tokio::time::timeout(
                            SECTION_DISCOVERY_TIMEOUT,
                            scraper.browse_section(section, discovery_limit),
                        ) => {
                            match result {
                                Ok(result) => result.map_err(|err| format!("{err:#}"))?,
                                Err(_) => {
                                    failed.push(JobFailure {
                                        label: section.label.to_owned(),
                                        detail: format!(
                                            "source discovery timed out after {}s",
                                            SECTION_DISCOVERY_TIMEOUT.as_secs()
                                        ),
                                        retry: None,
                                    });
                                    continue;
                                }
                            }
                        }
                    };

                    for summary in summaries {
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

                        if cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        if cancelable_sleep(cancel.clone(), REQUEST_THROTTLE).await {
                            break;
                        }

                        let fetch_result = tokio::select! {
                            _ = wait_for_cancel(cancel.clone()) => break,
                            result = tokio::time::timeout(ARTICLE_FETCH_TIMEOUT, scraper.fetch_article(&summary.url)) => {
                                match result {
                                    Ok(result) => result,
                                    Err(_) => Err(anyhow::anyhow!("fetch timed out after {}s", ARTICLE_FETCH_TIMEOUT.as_secs())),
                                }
                            },
                        };
                        match fetch_result {
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
                                            label: format!("Fetching: {}", section.label),
                                            completed: saved.min(max_articles),
                                            total: max_articles,
                                        }));
                                    }
                                    Err(err) => {
                                        failed.push(JobFailure {
                                            label: article.title,
                                            detail: err.to_string(),
                                            retry: Some(RetryAction::SaveUrl(summary.url.clone())),
                                        });
                                    }
                                }
                            }
                            Err(err) => failed.push(JobFailure {
                                label: summary.title,
                                detail: err.to_string(),
                                retry: Some(RetryAction::SaveUrl(summary.url.clone())),
                            }),
                        }
                    }

                    if saved >= max_articles || cancel.load(Ordering::Relaxed) {
                        break;
                    }
                }

                let cancelled = cancel.load(Ordering::Relaxed);
                Ok((saved, skipped_existing, skipped_out_of_range, failed, cancelled))
            }
            .await;

            let event = match result {
                Ok((saved, skipped_existing, skipped_out_of_range, failed, cancelled)) => {
                    let prefix = if cancelled { "Cancelled. " } else { "" };
                    AppEvent::FetchFinished {
                        message: format!(
                            "{prefix}Saved {saved} article(s). Skipped {skipped_existing} existing and {skipped_out_of_range} out-of-range article(s)."
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
        self.set_current_job(handle);
    }

    /// Auto-fetch recent articles on startup.  Uses all available sections,
    /// fetches articles from the last 2 days, capped at 5 per section / 30 total.
    pub(super) fn run_auto_fetch(&mut self) {
        let today = chrono::Local::now().date_naive();
        let two_days_ago = today - chrono::Duration::days(2);

        let section_ids: Vec<String> = self.sections.iter().map(|s| s.id.to_owned()).collect();
        let imported_urls = self.browse.saved_urls.clone();
        let max_articles: usize = 30;
        let per_section_cap: usize = 5;
        let stop_after_old: usize = 8;

        self.cancel_flag.store(false, Ordering::Relaxed);
        self.progress = Some(FetchProgress {
            label: "Auto-fetching recent articles".to_owned(),
            completed: 0,
            total: max_articles,
        });
        self.sync_to_window();

        let tx = self.tx.clone();
        let scraper = self.scraper.clone();
        let db = self.db.clone();
        let cancel = self.cancel_flag.clone();

        let handle = self.runtime.spawn(async move {
            let result: Result<(usize, usize, Vec<JobFailure>, bool), String> = async {
                let discovery_limit = per_section_cap.saturating_mul(4).max(40);
                let mut seen = std::collections::HashSet::new();
                let mut saved = 0usize;
                let mut skipped_existing = 0usize;
                let mut failed = Vec::new();

                for (section_idx, section_id) in section_ids.iter().enumerate() {
                    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    let Some(section) = scraper.section_by_id(section_id) else {
                        continue;
                    };
                    let mut accepted_for_section = 0usize;
                    let mut consecutive_old = 0usize;

                    let _ = tx.send(AppEvent::FetchProgress(FetchProgress {
                        label: format!(
                            "Auto-fetch: scanning {} ({}/{})",
                            section.label,
                            section_idx + 1,
                            section_ids.len()
                        ),
                        completed: saved.min(max_articles),
                        total: max_articles,
                    }));

                    let summaries = match tokio::select! {
                        _ = wait_for_cancel(cancel.clone()) => break,
                        result = tokio::time::timeout(
                            SECTION_DISCOVERY_TIMEOUT,
                            scraper.browse_section(section, discovery_limit),
                        ) => result,
                    } {
                        Ok(Ok(s)) => s,
                        Ok(Err(_)) => continue,
                        Err(_) => {
                            failed.push(JobFailure {
                                label: section.label.to_owned(),
                                detail: format!(
                                    "source discovery timed out after {}s",
                                    SECTION_DISCOVERY_TIMEOUT.as_secs()
                                ),
                                retry: None,
                            });
                            continue;
                        }
                    };

                    for summary in summaries {
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
                        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                            break;
                        }
                        if cancelable_sleep(cancel.clone(), REQUEST_THROTTLE).await {
                            break;
                        }

                        let fetch_result = tokio::select! {
                            _ = wait_for_cancel(cancel.clone()) => break,
                            result = tokio::time::timeout(ARTICLE_FETCH_TIMEOUT, scraper.fetch_article(&summary.url)) => {
                                match result {
                                    Ok(result) => result,
                                    Err(_) => Err(anyhow::anyhow!("fetch timed out after {}s", ARTICLE_FETCH_TIMEOUT.as_secs())),
                                }
                            },
                        };
                        match fetch_result {
                            Ok(article) => {
                                let Some(article_date) = parse_article_date(&article.date) else {
                                    consecutive_old += 1;
                                    if consecutive_old >= stop_after_old {
                                        break;
                                    }
                                    continue;
                                };
                                if article_date < two_days_ago || article_date > today {
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
                                            label: format!("Auto-fetch: {}", section.label),
                                            completed: saved.min(max_articles),
                                            total: max_articles,
                                        }));
                                    }
                                    Err(err) => {
                                        failed.push(JobFailure {
                                            label: article.title,
                                            detail: err.to_string(),
                                            retry: Some(RetryAction::SaveUrl(summary.url.clone())),
                                        });
                                    }
                                }
                            }
                            Err(err) => failed.push(JobFailure {
                                label: summary.title,
                                detail: err.to_string(),
                                retry: Some(RetryAction::SaveUrl(summary.url.clone())),
                            }),
                        }
                    }

                    if saved >= max_articles
                        || cancel.load(std::sync::atomic::Ordering::Relaxed)
                    {
                        break;
                    }
                }

                let cancelled = cancel.load(std::sync::atomic::Ordering::Relaxed);
                Ok((saved, skipped_existing, failed, cancelled))
            }
            .await;

            let event = match result {
                Ok((saved, skipped_existing, failed, cancelled)) => {
                    let prefix = if cancelled { "Cancelled. " } else { "" };
                    AppEvent::FetchFinished {
                        message: format!(
                            "{prefix}Auto-fetch: saved {saved} new article(s), skipped {skipped_existing} existing."
                        ),
                        failed,
                    }
                }
                Err(err) => AppEvent::FetchFinished {
                    message: format!("Auto-fetch error: {err}"),
                    failed: Vec::new(),
                },
            };
            let _ = tx.send(event);
        });
        self.set_current_job(handle);
    }

    pub(super) fn delete_selected(&mut self) {
        if self.lq.selected_articles.is_empty() {
            self.set_status("Select at least one article to delete.");
            return;
        }
        let visible_selected_ids: Vec<i64> = self
            .filtered_library_articles()
            .into_iter()
            .filter(|article| self.lq.selected_articles.contains(&article.id))
            .map(|article| article.id)
            .collect();
        if visible_selected_ids.is_empty() {
            self.set_status("No currently visible selected articles to delete.");
            return;
        }
        let hidden_selected_count = self
            .lq
            .selected_articles
            .len()
            .saturating_sub(visible_selected_ids.len());
        let count = visible_selected_ids.len();
        match self.db.delete_articles_batch(&visible_selected_ids) {
            Ok(deleted) => {
                for id in &visible_selected_ids {
                    self.lq.selected_articles.remove(id);
                }
                self.library.selected_article_id = None;
                let hidden_suffix = if hidden_selected_count > 0 {
                    format!(" Left {hidden_selected_count} hidden selection(s) untouched.")
                } else {
                    String::new()
                };
                self.set_status(format!(
                    "Deleted {deleted} of {count} visible selected article(s).{hidden_suffix}"
                ));
                self.refresh_saved_urls();
                self.refresh_stats();
                self.load_library();
            }
            Err(err) => {
                self.set_status(format!("Delete failed: {err:#}"));
            }
        }
    }

    pub(super) fn upload_selected(&mut self) {
        if self.lq.api_key.trim().is_empty() {
            self.set_status("Open LingQ settings and save a token first.");
            return;
        }
        if self.lq.selected_articles.is_empty() {
            self.set_status("Select at least one saved article to upload.");
            return;
        }

        let ids = self
            .filtered_library_articles()
            .into_iter()
            .filter(|article| {
                self.lq.selected_articles.contains(&article.id)
                    && self.matches_lingq_upload_filters(article)
            })
            .map(|article| article.id)
            .collect::<Vec<_>>();
        if ids.is_empty() {
            self.set_status("No selected articles match the current LingQ upload filters.");
            return;
        }

        let api_key = self.lq.api_key.clone();
        let language = self.lq.language.clone();
        let collection_id = self.lq.selected_collection;
        self.enqueue_job(QueuedJob::UploadArticles {
            ids,
            api_key,
            language,
            collection_id,
        });
    }

    pub(super) fn apply_filter_preset(&mut self, index: usize) {
        self.library.filter_preset_index = index;
        match index {
            1 => {
                self.library.min_words = "600".to_owned();
                self.library.max_words = "999".to_owned();
                self.library.only_not_uploaded = true;
                self.library.duplicate_only = false;
                self.lq.min_words = "600".to_owned();
                self.lq.max_words = "999".to_owned();
                self.lq.only_not_uploaded = true;
            }
            2 => {
                self.library.min_words = "1000".to_owned();
                self.library.max_words = "1800".to_owned();
                self.library.only_not_uploaded = true;
                self.library.duplicate_only = false;
                self.lq.min_words = "1000".to_owned();
                self.lq.max_words = "1800".to_owned();
                self.lq.only_not_uploaded = true;
            }
            3 => {
                self.library.min_words = "1800".to_owned();
                self.library.max_words.clear();
                self.library.only_not_uploaded = true;
                self.library.duplicate_only = false;
                self.lq.min_words = "1800".to_owned();
                self.lq.max_words.clear();
                self.lq.only_not_uploaded = true;
            }
            4 => {
                self.library.only_not_uploaded = true;
                self.library.duplicate_only = false;
                self.lq.only_not_uploaded = true;
            }
            5 => {
                self.library.duplicate_only = true;
                self.library.only_not_uploaded = false;
                self.lq.only_not_uploaded = false;
            }
            _ => {
                self.library.duplicate_only = false;
            }
        }
        self.save_settings();
        self.add_activity(
            "filter",
            "Applied library preset",
            filter_preset_labels()
                .get(index)
                .copied()
                .unwrap_or("No preset"),
        );
        self.load_library();
        self.sync_to_window();
    }

    pub(super) fn open_library_folder(&mut self) {
        match crate::app_data_dir() {
            Ok(path) => {
                #[cfg(windows)]
                let opened = std::process::Command::new("explorer")
                    .arg(&path)
                    .spawn()
                    .map(|_| ());

                #[cfg(not(windows))]
                let opened = webbrowser::open(path.to_string_lossy().as_ref()).map(|_| ());

                match opened {
                    Ok(()) => {
                        self.add_activity(
                            "opened",
                            "Opened library folder",
                            path.display().to_string(),
                        );
                        self.set_status("Opened the local library folder.");
                    }
                    Err(err) => self.set_status(format!("Could not open library folder: {err}")),
                }
            }
            Err(err) => self.set_status(format!("Could not locate library folder: {err:#}")),
        }
    }

    pub(super) fn retry_failed_items(&mut self) {
        if self.failed_items.is_empty() {
            self.set_status("There are no failed items to retry.");
            return;
        }

        let failures = std::mem::take(&mut self.failed_items);
        let mut save_urls = Vec::new();
        let mut upload_ids = Vec::new();
        let mut not_retryable = Vec::new();

        for failure in failures {
            match failure.retry.clone() {
                Some(RetryAction::SaveUrl(url)) => save_urls.push(url),
                Some(RetryAction::UploadArticle(id)) => upload_ids.push(id),
                None => not_retryable.push(failure),
            }
        }

        self.failed_items = not_retryable;
        self.dirty.activity = true;

        let mut queued = 0usize;
        if !save_urls.is_empty() {
            save_urls.sort();
            save_urls.dedup();
            queued += save_urls.len();
            self.enqueue_job(QueuedJob::SaveUrls {
                urls: save_urls,
                label: "Retry failed saves".to_owned(),
            });
        }

        if !upload_ids.is_empty() {
            if self.lq.api_key.trim().is_empty() {
                self.set_status("Open LingQ settings and save a token before retrying uploads.");
                self.failed_items.extend(upload_ids.into_iter().map(|id| JobFailure {
                    label: format!("article #{id}"),
                    detail: "LingQ token missing during retry.".to_owned(),
                    retry: Some(RetryAction::UploadArticle(id)),
                }));
                self.dirty.activity = true;
                self.sync_to_window();
                return;
            }
            upload_ids.sort_unstable();
            upload_ids.dedup();
            queued += upload_ids.len();
            self.enqueue_job(QueuedJob::UploadArticles {
                ids: upload_ids,
                api_key: self.lq.api_key.clone(),
                language: self.lq.language.clone(),
                collection_id: self.lq.selected_collection,
            });
        }

        self.add_activity("queued", "Retry requested", format!("{queued} item(s) queued."));
        self.set_status(format!("Queued {queued} failed item(s) for retry."));
    }

    pub(super) fn clear_failed_items(&mut self) {
        let cleared = self.failed_items.len();
        self.failed_items.clear();
        self.add_activity("cleared", "Cleared retry list", format!("{cleared} item(s) removed."));
        self.set_status("Cleared the retry list.");
    }

    fn start_upload_job(
        &mut self,
        ids: Vec<i64>,
        api_key: String,
        language: String,
        collection_id: Option<i64>,
    ) {
        self.cancel_flag.store(false, Ordering::Relaxed);
        self.progress = Some(FetchProgress {
            label: "Uploading to LingQ".to_owned(),
            completed: 0,
            total: ids.len(),
        });
        self.sync_to_window();

        let tx = self.tx.clone();
        let lingq = self.lingq.clone();
        let db = self.db.clone();
        let cancel = self.cancel_flag.clone();
        let handle = self.runtime.spawn(async move {
            let result: Result<(usize, usize, Vec<JobFailure>, bool), String> = async {
                let mut uploaded = 0usize;
                let mut skipped_already = 0usize;
                let mut failed = Vec::new();
                let total = ids.len();
                // Overall time cap: 15 minutes for upload batch
                let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(900);

                for (index, id) in ids.into_iter().enumerate() {
                    if cancel.load(Ordering::Relaxed) || tokio::time::Instant::now() > deadline {
                        if tokio::time::Instant::now() > deadline {
                            failed.push(JobFailure {
                                label: "Upload batch".to_owned(),
                                detail: "Upload timed out (15 min limit).".to_owned(),
                                retry: None,
                            });
                            cancel.store(true, Ordering::Relaxed);
                        }
                        break;
                    }
                    if index > 0 {
                        if cancelable_sleep(cancel.clone(), REQUEST_THROTTLE).await {
                            break;
                        }
                    }
                    let Some(article) = db.get_article(id).map_err(|err| format!("{err:#}"))? else {
                        failed.push(JobFailure {
                            label: format!("article #{id}"),
                            detail: "not found".to_owned(),
                            retry: None,
                        });
                        continue;
                    };
                    let _ = tx.send(AppEvent::FetchProgress(FetchProgress {
                        label: format!("Uploading {}", article.title),
                        completed: index,
                        total,
                    }));
                    let request = UploadRequest {
                        api_key: api_key.clone(),
                        language_code: language.clone(),
                        collection_id,
                        title: article.title.clone(),
                        text: article.clean_text.clone(),
                        original_url: Some(article.url.clone()),
                    };

                    // If already on LingQ, update the existing lesson; otherwise create new
                    let upload_result = tokio::select! {
                        _ = wait_for_cancel(cancel.clone()) => break,
                        result = async {
                            if let Some(existing_id) = article.lingq_lesson_id {
                                lingq.update_lesson(&request, existing_id).await
                            } else {
                                lingq.upload_lesson(&request).await
                            }
                        } => result
                    };

                    match upload_result {
                        Ok(response) => {
                            if article.uploaded_to_lingq {
                                // Re-upload: lesson was updated, not newly created
                                skipped_already += 1;
                            }
                            if let Err(err) =
                                db.mark_uploaded(article.id, response.lesson_id, &response.lesson_url)
                            {
                                failed.push(JobFailure {
                                    label: article.title,
                                    detail: format!("uploaded but DB update failed: {err}"),
                                    retry: Some(RetryAction::UploadArticle(article.id)),
                                });
                            } else {
                                uploaded += 1;
                            }
                        }
                        Err(err) => failed.push(JobFailure {
                            label: article.title,
                            detail: err.to_string(),
                            retry: Some(RetryAction::UploadArticle(article.id)),
                        }),
                    }

                    let _ = tx.send(AppEvent::FetchProgress(FetchProgress {
                        label: "Uploading to LingQ".to_owned(),
                        completed: index + 1,
                        total,
                    }));
                }

                let cancelled = cancel.load(Ordering::Relaxed);
                Ok((uploaded, skipped_already, failed, cancelled))
            }
            .await;

            let event = match result {
                Ok((uploaded, skipped_already, failed, cancelled)) => AppEvent::UploadFinished {
                    uploaded,
                    skipped_already,
                    failed,
                    cancelled,
                },
                Err(err) => AppEvent::UploadFinished {
                    uploaded: 0,
                    skipped_already: 0,
                    failed: vec![JobFailure {
                        label: "Upload".to_owned(),
                        detail: err,
                        retry: None,
                    }],
                    cancelled: false,
                },
            };
            let _ = tx.send(event);
        });
        self.set_current_job(handle);
    }

    pub(super) fn poll_events(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                AppEvent::BrowseLoaded(result) => match result {
                    Ok(result) => {
                        self.browse.articles = result.articles;
                        self.browse.report = Some(format!(
                            "Sources: {} section / {} subsection / {} topic, {} deduped.",
                            result.report.section_articles,
                            result.report.subsection_articles,
                            result.report.topic_articles,
                            result.report.deduped_articles
                        ));
                        self.dirty.browse = true;
                        self.set_status(format!(
                            "Loaded {} article candidates from taz.",
                            self.browse.articles.len()
                        ));
                    }
                    Err(err) => {
                        error!("Browse load failed: {err}");
                        self.set_status(err);
                    }
                },
                AppEvent::SearchLoaded(result) => match result {
                    Ok(articles) => {
                        let count = articles.len();
                        let date_label = search_date_label(
                            parse_date_input(&self.browse.date_from).unwrap_or(None),
                            parse_date_input(&self.browse.date_to).unwrap_or(None),
                        );
                        self.browse.articles = articles;
                        self.browse.report = Some(format!(
                            "Search results for \"{}\"{}.",
                            self.browse.search_query,
                            date_label
                        ));
                        self.dirty.browse = true;
                        self.set_status(format!(
                            "Found {count} articles for \"{}\"{}.",
                            self.browse.search_query,
                            date_label
                        ));
                    }
                    Err(err) => {
                        error!("Search failed: {err}");
                        self.set_status(format!("Search failed: {err}"));
                    }
                },
                AppEvent::SavedUrlsLoaded(result) => match result {
                    Ok(urls) => {
                        self.browse.saved_urls = urls;
                        self.dirty.browse = true;
                        self.sync_to_window();
                    }
                    Err(err) => self.set_status(err),
                },
                AppEvent::LibraryLoaded(result) => match result {
                    Ok(articles) => {
                        self.library.articles = articles;
                        if self.library.selected_article_id.is_none() {
                            let first_id = self.library.articles.first().map(|a| a.id);
                            self.select_article(first_id);
                        } else {
                            self.dirty.library = true;
                            self.dirty.preview = true;
                        }
                        self.sync_to_window();
                    }
                    Err(err) => self.set_status(err),
                },
                AppEvent::StatsLoaded(result) => match result {
                    Ok(stats) => {
                        self.stats = Some(stats);
                        self.dirty.stats = true;
                        self.sync_to_window();
                    }
                    Err(err) => self.set_status(err),
                },
                AppEvent::FetchProgress(progress) => {
                    self.progress = Some(progress);
                    self.dirty.progress = true;
                    self.sync_to_window();
                }
                AppEvent::FetchFinished { message, failed } => {
                    self.clear_current_job();
                    self.progress = None;
                    self.cancel_flag.store(false, Ordering::Relaxed);
                    self.record_failures(&failed);
                    self.add_activity("finished", "Fetch job finished", &message);
                    self.set_status(format!("{message}{}", format_failure_suffix(&failed)));
                    self.refresh_saved_urls();
                    self.refresh_stats();
                    self.load_library();
                    self.load_browse();
                    self.start_next_job_if_idle();
                }
                AppEvent::SaveProgress(progress) => {
                    self.save_progress = Some(progress);
                    self.dirty.progress = true;
                    self.sync_to_window();
                }
                AppEvent::SaveFinished { message, failed } => {
                    self.clear_current_job();
                    self.save_progress = None;
                    self.cancel_flag.store(false, Ordering::Relaxed);
                    self.record_failures(&failed);
                    self.add_activity("finished", "Save job finished", &message);
                    self.set_status(format!("{message}{}", format_failure_suffix(&failed)));
                    self.browse.selected.clear();
                    self.dirty.browse = true;
                    self.refresh_saved_urls();
                    self.refresh_stats();
                    self.load_library();
                    self.start_next_job_if_idle();
                }
                AppEvent::CollectionsLoaded(result) => match result {
                    Ok(collections) => {
                        self.lq.collections = collections;
                        if self.lq.selected_collection.is_none() {
                            self.lq.selected_collection =
                                self.lq.collections.first().map(|course| course.id);
                        }
                        self.dirty.collections = true;
                        self.set_status("LingQ courses refreshed.");
                    }
                    Err(err) => self.set_status(err),
                },
                AppEvent::LingqLoggedIn(result) => match result {
                    Ok(token) => {
                        self.lq.api_key = token;
                        self.lq.login_failures = 0;
                        self.lq.login_cooldown_until = None;
                        // Clear password from memory after successful login
                        self.lq.password.clear();
                        self.save_settings();
                        self.load_collections();
                        self.set_status("LingQ login succeeded.");
                    }
                    Err(err) => {
                        self.lq.password.clear();
                        self.lq.login_failures += 1;
                        if self.lq.login_failures >= 3 {
                            let cooldown = std::time::Duration::from_secs(
                                30 * (self.lq.login_failures as u64 - 2),
                            );
                            self.lq.login_cooldown_until =
                                Some(std::time::Instant::now() + cooldown);
                            self.set_status(&format!(
                                "{err} (Cooldown: {cooldown:.0?} before next attempt)"
                            ));
                        } else {
                            self.set_status(&err);
                        }
                    }
                },
                AppEvent::UploadFinished { uploaded, skipped_already, failed, cancelled } => {
                    self.clear_current_job();
                    self.progress = None;
                    self.cancel_flag.store(false, Ordering::Relaxed);
                    self.record_failures(&failed);
                    let prefix = if cancelled { "Cancelled. " } else { "" };
                    let skip_suffix = if skipped_already > 0 {
                        format!(" Skipped {skipped_already} already uploaded.")
                    } else {
                        String::new()
                    };
                    self.set_status(format!(
                        "{prefix}Uploaded {uploaded} article(s) to LingQ.{skip_suffix}{}",
                        format_failure_suffix(&failed)
                    ));
                    self.add_activity(
                        "finished",
                        "LingQ upload finished",
                        format!("{uploaded} uploaded, {} failed.", failed.len()),
                    );
                    self.lq.selected_articles.clear();
                    self.dirty.library = true;
                    self.refresh_stats();
                    self.load_library();
                    self.start_next_job_if_idle();
                }
            }
        }
    }
}

/// Quick plausibility check for an API token: must be 8+ alphanumeric characters.
fn is_plausible_api_key(key: &str) -> bool {
    let trimmed = key.trim();
    trimmed.len() >= 8 && trimmed.chars().all(|ch| ch.is_ascii_alphanumeric())
}
