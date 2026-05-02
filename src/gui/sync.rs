use super::*;

impl AppState {
    pub(super) fn pull_inputs_from_window(&mut self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };

        self.browse.section_index = window.get_browse_section_index().max(0) as usize;
        self.browse.only_new = window.get_browse_only_new();
        self.browse.date_from = window.get_browse_date_from().to_string();
        self.browse.date_to = window.get_browse_date_to().to_string();
        self.bulk.max_articles = window.get_bulk_max_articles().to_string();
        self.bulk.per_section_cap = window.get_bulk_per_section_cap().to_string();
        self.bulk.stop_after_old = window.get_bulk_stop_after_old().to_string();
        self.library.search = window.get_library_search().to_string();
        self.library.heading = indexed_label(&self.library.cached_heading_labels, window.get_library_heading_index());
        self.library.section = indexed_label(&self.library.cached_section_labels, window.get_library_section_index());
        self.library.sort_mode = LibrarySortMode::from_index(window.get_library_sort_index());
        self.library.only_not_uploaded = window.get_library_only_not_uploaded();
        self.library.min_words = window.get_library_min_words().to_string();
        self.library.max_words = window.get_library_max_words().to_string();
        self.library.show_filters = window.get_show_library_filters();
        self.library.show_upload_tools = window.get_show_upload_tools();
        self.lq.min_words = window.get_lingq_min_words().to_string();
        self.lq.max_words = window.get_lingq_max_words().to_string();
        self.lq.only_not_uploaded = window.get_lingq_only_not_uploaded();
        self.lq.show_settings = window.get_show_lingq_settings();
        self.lq.api_key = window.get_lingq_token().to_string();
        self.lq.username = window.get_lingq_username().to_string();
        self.lq.password = window.get_lingq_password().to_string();
        self.bulk.auto_fetch_on_startup = window.get_auto_fetch_on_startup();

        let collection_index = window.get_lingq_collection_index().max(0) as usize;
        self.lq.selected_collection = if collection_index == 0 {
            None
        } else {
            self.lq.collections
                .get(collection_index.saturating_sub(1))
                .map(|course| course.id)
        };
    }

    pub(super) fn save_settings(&mut self) {
        self.settings_dirty = true;
    }

    /// Actually write settings to disk. Called by the poll timer at most every ~100ms.
    pub(super) fn flush_settings(&mut self) {
        if !self.settings_dirty {
            return;
        }
        self.settings_dirty = false;

        // Save API key to its own file only when it actually changed
        if self.lq.api_key != self.lq.api_key_last_saved {
            let _ = settings::save_api_key(&self.lq.api_key);
            self.lq.api_key_last_saved.clone_from(&self.lq.api_key);
        }

        // Compute values that borrow &self before taking &mut self.settings
        let section_id = self.current_section().id.to_owned();

        // Write directly to settings data — no clones needed
        let s = self.settings.data_mut();
        s.browse_section = section_id;
        s.last_view = match self.current_view {
            View::Browse => "browse",
            View::Library => "library",
        }
        .to_owned();
        s.library_sort = match self.library.sort_mode {
            LibrarySortMode::Newest => "newest",
            LibrarySortMode::Oldest => "oldest",
            LibrarySortMode::Longest => "longest",
            LibrarySortMode::Shortest => "shortest",
            LibrarySortMode::Title => "title",
        }
        .to_owned();
        s.lingq_api_key.clear(); // API key stored separately
        s.lingq_collection_id = self.lq.selected_collection;
        s.browse_only_new = self.browse.only_new;
        s.browse_date_from.clone_from(&self.browse.date_from);
        s.browse_date_to.clone_from(&self.browse.date_to);
        s.bulk_max_articles.clone_from(&self.bulk.max_articles);
        s.bulk_per_section_cap.clone_from(&self.bulk.per_section_cap);
        s.bulk_stop_after_old.clone_from(&self.bulk.stop_after_old);
        s.library_only_not_uploaded = self.library.only_not_uploaded;
        s.library_min_words.clone_from(&self.library.min_words);
        s.library_max_words.clone_from(&self.library.max_words);
        s.lingq_language.clone_from(&self.lq.language);
        s.lingq_only_not_uploaded = self.lq.only_not_uploaded;
        s.lingq_min_words.clone_from(&self.lq.min_words);
        s.lingq_max_words.clone_from(&self.lq.max_words);
        s.show_library_filters = self.library.show_filters;
        s.show_upload_tools = self.library.show_upload_tools;
        s.preview_wide = self.library.preview_wide;
        s.auto_fetch_on_startup = self.bulk.auto_fetch_on_startup;

        let _ = self.settings.save();
    }

    pub(super) fn heading_labels(&self) -> Vec<String> {
        let mut labels = vec!["All headings".to_owned()];
        let mut seen = HashSet::new();
        for article in &self.library.articles {
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

    pub(super) fn section_labels(&self) -> Vec<String> {
        let mut labels = vec!["All sections".to_owned()];
        let mut seen = HashSet::new();
        for article in &self.library.articles {
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

    pub(super) fn filtered_browse_articles(&self) -> Vec<&ArticleSummary> {
        self.browse.articles
            .iter()
            .filter(|article| !self.browse.only_new || !self.browse.saved_urls.contains(&article.url))
            .collect()
    }

    pub(super) fn filtered_library_articles(&self) -> Vec<&StoredArticleMeta> {
        // Most filtering and sorting is now done in SQL (see build_library_query).
        // Only heading filtering remains here since it maps sections to app-level categories.
        let heading = self.library.heading.trim();

        self.library.articles
            .iter()
            .filter(|article| {
                if heading != "All headings" && !heading.is_empty() {
                    if section_heading(&article.section) != heading {
                        return false;
                    }
                }
                true
            })
            .collect()
    }

    pub(super) fn matches_lingq_upload_filters(&self, article: &StoredArticleMeta) -> bool {
        if self.lq.only_not_uploaded && article.uploaded_to_lingq {
            return false;
        }
        if let Ok(Some(min_words)) = parse_optional_i64(&self.lq.min_words) {
            if article.word_count < min_words {
                return false;
            }
        }
        if let Ok(Some(max_words)) = parse_optional_i64(&self.lq.max_words) {
            if article.word_count > max_words {
                return false;
            }
        }
        true
    }

    pub(super) fn sync_to_window(&mut self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };

        // ── Scalar properties (always synced — cheap) ──
        window.set_page_index(if self.current_view == View::Browse { 0 } else { 1 });
        window.set_browse_section_index(self.browse.section_index as i32);
        window.set_browse_only_new(self.browse.only_new);
        window.set_browse_date_from(self.browse.date_from.clone().into());
        window.set_browse_date_to(self.browse.date_to.clone().into());
        window.set_bulk_max_articles(self.bulk.max_articles.clone().into());
        window.set_bulk_per_section_cap(self.bulk.per_section_cap.clone().into());
        window.set_bulk_stop_after_old(self.bulk.stop_after_old.clone().into());
        window.set_auto_fetch_on_startup(self.bulk.auto_fetch_on_startup);
        window.set_library_search(self.library.search.clone().into());
        window.set_library_sort_index(self.library.sort_mode.index());
        window.set_library_only_not_uploaded(self.library.only_not_uploaded);
        window.set_library_min_words(self.library.min_words.clone().into());
        window.set_library_max_words(self.library.max_words.clone().into());
        window.set_show_library_filters(self.library.show_filters);
        window.set_show_upload_tools(self.library.show_upload_tools);
        window.set_lingq_min_words(self.lq.min_words.clone().into());
        window.set_lingq_max_words(self.lq.max_words.clone().into());
        window.set_lingq_only_not_uploaded(self.lq.only_not_uploaded);
        window.set_lingq_connected(!self.lq.api_key.trim().is_empty());
        window.set_show_lingq_settings(self.lq.show_settings);
        window.set_lingq_token(self.lq.api_key.clone().into());
        window.set_lingq_username(self.lq.username.clone().into());
        window.set_lingq_password(self.lq.password.clone().into());
        window.set_status_message(self.status_message.clone().into());
        window.set_preview_wide(self.library.preview_wide);
        window.set_taz_search_query(self.browse.search_query.clone().into());
        window.set_queue_label(if self.job_queue.is_empty() {
            SharedString::new()
        } else {
            SharedString::from(format!("{} queued job(s)", self.job_queue.len()))
        });

        // ── Browse models (only rebuilt when browse data changes) ──
        if self.dirty.browse {
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
                    selected: self.browse.selected.contains(&article.url),
                    saved: self.browse.saved_urls.contains(&article.url),
                })
                .collect::<Vec<_>>();
            window.set_browse_rows(ModelRc::from(Rc::new(VecModel::from(browse_rows))));
            let browse_selected = filtered_browse
                .iter()
                .filter(|article| self.browse.selected.contains(&article.url))
                .count();
            let browse_summary = format!(
                "Showing {} article(s) from {}. {} selected. Limit {}.{}",
                filtered_browse.len(),
                self.current_section().label,
                browse_selected,
                self.browse.limit,
                self.browse.report
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
                    selected: self.bulk.selected_sections.contains(section.id),
                })
                .collect::<Vec<_>>();
            window.set_bulk_section_rows(ModelRc::from(Rc::new(VecModel::from(bulk_rows))));
            let bulk_count = self.bulk.selected_sections.len();
            window.set_bulk_sections_label(
                format!("Sections ({})", bulk_count).into(),
            );
        }

        // ── Stats cards ──
        if self.dirty.stats {
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
        }

        // ── Library models (only rebuilt when library data or selection changes) ──
        if self.dirty.library {
            self.library.cached_heading_labels = self.heading_labels();
            self.library.cached_section_labels = self.section_labels();
            let sort_labels = LibrarySortMode::all()
                .into_iter()
                .map(|mode| SharedString::from(mode.label()))
                .collect::<Vec<_>>();
            let heading_index = index_of_label(&self.library.cached_heading_labels, &self.library.heading);
            let section_index = index_of_label(&self.library.cached_section_labels, &self.library.section);
            window.set_heading_labels(ModelRc::from(Rc::new(VecModel::from(
                self.library.cached_heading_labels
                    .iter()
                    .cloned()
                    .map(SharedString::from)
                    .collect::<Vec<_>>(),
            ))));
            window.set_section_labels(ModelRc::from(Rc::new(VecModel::from(
                self.library.cached_section_labels
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
                    difficulty: article.difficulty as i32,
                    url: article.url.clone().into(),
                    uploaded: article.uploaded_to_lingq,
                    paywalled: article.paywalled,
                    selected: self.lq.selected_articles.contains(&article.id),
                    confirming_delete: self.library.delete_confirm_id == Some(article.id),
                    viewing: self.library.selected_article_id == Some(article.id),
                })
                .collect::<Vec<_>>();
            window.set_library_rows(ModelRc::from(Rc::new(VecModel::from(library_rows))));
            let uploadable_count = filtered_library
                .iter()
                .filter(|article| {
                    self.lq.selected_articles.contains(&article.id)
                        && self.matches_lingq_upload_filters(article)
                })
                .count();
            let selected_suffix = if uploadable_count > 0 {
                format!(" {} selected for upload.", uploadable_count)
            } else if !self.lq.selected_articles.is_empty() {
                " Selection hidden by upload filters.".to_owned()
            } else {
                String::new()
            };
            window.set_library_summary(
                format!(
                    "Showing {} saved article(s) after filters.{selected_suffix}",
                    filtered_library.len()
                )
                .into(),
            );
            window.set_library_count(if self.library.articles.is_empty() {
                SharedString::new()
            } else {
                SharedString::from(self.library.articles.len().to_string())
            });
        }

        // ── Preview pane ──
        if self.dirty.preview || self.dirty.library {
            if let Some(article) = &self.library.preview_article {
                window.set_preview_has_article(true);
                window.set_preview_title(article.title.clone().into());
                window.set_preview_meta(
                    format!(
                        "{} | {} words | {} | {}{}",
                        article.section,
                        article.word_count,
                        article.date,
                        if article.uploaded_to_lingq {
                            "On LingQ"
                        } else {
                            "Not on LingQ"
                        },
                        if article.paywalled {
                            " | Paywalled"
                        } else {
                            ""
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
        }

        // ── Collections ──
        if self.dirty.collections {
            let mut collection_labels = vec![SharedString::from("No course selected")];
            collection_labels.extend(self.lq.collections.iter().map(|collection| {
                SharedString::from(format!(
                    "{} ({})",
                    collection.title, collection.lessons_count
                ))
            }));
            let collection_index = self
                .lq.selected_collection
                .and_then(|selected| {
                    self.lq.collections
                        .iter()
                        .position(|collection| collection.id == selected)
                })
                .map(|index| index + 1)
                .unwrap_or(0);
            window.set_collection_labels(ModelRc::from(Rc::new(VecModel::from(collection_labels))));
            window.set_lingq_collection_index(collection_index as i32);
        }

        // ── Progress bar (always synced — cheap and changes frequently) ──
        window.set_progress_visible(self.progress.is_some() || self.save_progress.is_some());
        let active_progress = self.progress.as_ref().or(self.save_progress.as_ref());
        let progress_value = active_progress
            .map(|p| {
                if p.total == 0 {
                    0.0
                } else {
                    (p.completed as f32 / p.total as f32).clamp(0.0, 1.0)
                }
            })
            .unwrap_or(0.0);
        window.set_progress_label(
            active_progress
                .map(|p| {
                    let elapsed = self
                        .current_job_started_at
                        .map(|started| started.elapsed().as_secs())
                        .unwrap_or(0);
                    format!(
                        "{} ({} / {}, {}s)",
                        p.label, p.completed, p.total, elapsed
                    )
                })
                .unwrap_or_default()
                .into(),
        );
        window.set_progress_value(progress_value);

        if self.dirty.activity {
            let activity_rows = self
                .activity
                .iter()
                .map(|entry| ActivityRow {
                    kind: entry.kind.clone().into(),
                    message: entry.message.clone().into(),
                    detail: entry.detail.clone().into(),
                })
                .collect::<Vec<_>>();
            window.set_activity_rows(ModelRc::from(Rc::new(VecModel::from(activity_rows))));

            let failed_rows = self
                .failed_items
                .iter()
                .rev()
                .map(|failure| FailedRow {
                    label: failure.label.clone().into(),
                    detail: failure.detail.clone().into(),
                    retryable: failure.retry.is_some(),
                })
                .collect::<Vec<_>>();
            window.set_failed_rows(ModelRc::from(Rc::new(VecModel::from(failed_rows))));
        }

        // Clear all dirty flags after sync
        self.dirty.clear();
    }
}
