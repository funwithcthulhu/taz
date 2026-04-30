use super::*;

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
        self.set_status(format!("Searching taz.de for \"{query}\"..."));
        let scraper = self.scraper.clone();
        self.spawn_background(async move {
            let result = scraper
                .search_articles(&query, 5)
                .await
                .map_err(|err| format!("{err:#}"));
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
        self.run_fetch_job(urls, "Saving selected articles");
    }

    pub(super) fn save_single_browse(&mut self, url: String) {
        self.run_fetch_job(vec![url], "Saving article");
    }

    fn run_fetch_job(&mut self, urls: Vec<String>, label: &str) {
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
        self.runtime.spawn(async move {
            let result: Result<(usize, Vec<String>), String> = async {
                let mut saved = 0usize;
                let mut failed = Vec::new();
                // Overall time cap: 10 minutes for the entire batch
                let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(600);
                for (index, url) in urls.into_iter().enumerate() {
                    if cancel.load(Ordering::Relaxed) || tokio::time::Instant::now() > deadline {
                        if tokio::time::Instant::now() > deadline {
                            failed.push("Operation timed out (10 min limit).".to_owned());
                            cancel.store(true, Ordering::Relaxed);
                        }
                        break;
                    }
                    if index > 0 {
                        tokio::time::sleep(REQUEST_THROTTLE).await;
                    }
                    match scraper.fetch_article(&url).await {
                        Ok(article) => match db.save_article(&article) {
                            Ok(_) => saved += 1,
                            Err(err) => failed.push(format!("{}: {}", article.title, err)),
                        },
                        Err(err) => failed.push(format!("{url}: {err}")),
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
        self.runtime.spawn(async move {
            let result: Result<(usize, usize, usize, Vec<String>, bool), String> = async {
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

                    let summaries = scraper
                        .browse_section(section, discovery_limit)
                        .await
                        .map_err(|err| format!("{err:#}"))?;

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
                        tokio::time::sleep(REQUEST_THROTTLE).await;

                        match scraper.fetch_article(&summary.url).await {
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
                                        failed.push(format!("{}: {}", article.title, err));
                                    }
                                }
                            }
                            Err(err) => failed.push(format!("{}: {}", summary.title, err)),
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

        self.runtime.spawn(async move {
            let result: Result<(usize, usize, Vec<String>, bool), String> = async {
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

                    let summaries = match scraper.browse_section(section, discovery_limit).await {
                        Ok(s) => s,
                        Err(_) => continue,
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
                        tokio::time::sleep(REQUEST_THROTTLE).await;

                        match scraper.fetch_article(&summary.url).await {
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
                                        failed.push(format!("{}: {}", article.title, err));
                                    }
                                }
                            }
                            Err(err) => failed.push(format!("{}: {}", summary.title, err)),
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
    }

    pub(super) fn delete_selected(&mut self) {
        if self.lq.selected_articles.is_empty() {
            self.set_status("Select at least one article to delete.");
            return;
        }
        let ids: Vec<i64> = self.lq.selected_articles.iter().copied().collect();
        let count = ids.len();
        match self.db.delete_articles_batch(&ids) {
            Ok(deleted) => {
                self.lq.selected_articles.clear();
                self.library.selected_article_id = None;
                self.set_status(format!("Deleted {deleted} of {count} selected article(s)."));
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
        self.runtime.spawn(async move {
            let result: Result<(usize, usize, Vec<String>, bool), String> = async {
                let mut uploaded = 0usize;
                let mut skipped_already = 0usize;
                let mut failed = Vec::new();
                let total = ids.len();
                // Overall time cap: 15 minutes for upload batch
                let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(900);

                for (index, id) in ids.into_iter().enumerate() {
                    if cancel.load(Ordering::Relaxed) || tokio::time::Instant::now() > deadline {
                        if tokio::time::Instant::now() > deadline {
                            failed.push("Upload timed out (15 min limit).".to_owned());
                            cancel.store(true, Ordering::Relaxed);
                        }
                        break;
                    }
                    if index > 0 {
                        tokio::time::sleep(REQUEST_THROTTLE).await;
                    }
                    let Some(article) = db.get_article(id).map_err(|err| format!("{err:#}"))? else {
                        failed.push(format!("article #{id} not found"));
                        continue;
                    };
                    let request = UploadRequest {
                        api_key: api_key.clone(),
                        language_code: language.clone(),
                        collection_id,
                        title: article.title.clone(),
                        text: article.clean_text.clone(),
                        original_url: Some(article.url.clone()),
                    };

                    // If already on LingQ, update the existing lesson; otherwise create new
                    let upload_result = if let Some(existing_id) = article.lingq_lesson_id {
                        lingq.update_lesson(&request, existing_id).await
                    } else {
                        lingq.upload_lesson(&request).await
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
                    failed: vec![err],
                    cancelled: false,
                },
            };
            let _ = tx.send(event);
        });
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
                        self.browse.articles = articles;
                        self.browse.report = Some(format!(
                            "Search results for \"{}\".",
                            self.browse.search_query
                        ));
                        self.dirty.browse = true;
                        self.set_status(format!(
                            "Found {count} articles for \"{}\".",
                            self.browse.search_query
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
                    self.progress = None;
                    self.cancel_flag.store(false, Ordering::Relaxed);
                    self.set_status(format!("{message}{}", format_failure_suffix(&failed)));
                    self.refresh_saved_urls();
                    self.refresh_stats();
                    self.load_library();
                    self.load_browse();
                }
                AppEvent::SaveProgress(progress) => {
                    self.save_progress = Some(progress);
                    self.dirty.progress = true;
                    self.sync_to_window();
                }
                AppEvent::SaveFinished { message, failed } => {
                    self.save_progress = None;
                    self.cancel_flag.store(false, Ordering::Relaxed);
                    self.set_status(format!("{message}{}", format_failure_suffix(&failed)));
                    self.refresh_saved_urls();
                    self.refresh_stats();
                    self.load_library();
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
                    self.progress = None;
                    self.cancel_flag.store(false, Ordering::Relaxed);
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
                    self.refresh_stats();
                    self.load_library();
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
