use super::*;

pub(super) fn wire_callbacks(window: &AppWindow, state: &Rc<RefCell<AppState>>) {
    let state_clone = state.clone();
    window.on_switch_page(move |index| {
        let mut app = state_clone.borrow_mut();
        app.current_view = if index == 0 { View::Browse } else { View::Library };
        app.dirty = DirtyFlags::all();
        app.save_settings();
        app.sync_to_window();
    });

    let state_clone = state.clone();
    window.on_browse_set_section_index(move |index| {
        let mut app = state_clone.borrow_mut();
        app.browse.section_index = index.max(0) as usize;
        app.save_settings();
    });

    let state_clone = state.clone();
    window.on_browse_set_only_new(move |checked| {
        let mut app = state_clone.borrow_mut();
        app.browse.only_new = checked;
        app.save_settings();
        app.dirty.browse = true;
        app.sync_to_window();
    });

    let state_clone = state.clone();
    window.on_browse_refresh(move || {
        let mut app = state_clone.borrow_mut();
        app.pull_inputs_from_window();
        app.browse.limit = 80;
        app.load_browse();
    });

    let state_clone = state.clone();
    window.on_browse_load_more(move || {
        let mut app = state_clone.borrow_mut();
        app.pull_inputs_from_window();
        app.browse.limit = app.browse.limit.saturating_add(80);
        app.load_browse();
    });

    let state_clone = state.clone();
    window.on_browse_set_date_from(move |value| {
        let mut app = state_clone.borrow_mut();
        app.browse.date_from = value.to_string();
        app.save_settings();
    });

    let state_clone = state.clone();
    window.on_browse_set_date_to(move |value| {
        let mut app = state_clone.borrow_mut();
        app.browse.date_to = value.to_string();
        app.save_settings();
    });

    let state_clone = state.clone();
    window.on_browse_toggle_article(move |index| {
        let mut app = state_clone.borrow_mut();
        if let Some(url) = app.browse.articles.get(index as usize).map(|article| article.url.clone()) {
            if !app.browse.selected.insert(url.clone()) {
                app.browse.selected.remove(&url);
            }
        }
        app.dirty.browse = true;
        app.sync_to_window();
    });

    let state_clone = state.clone();
    window.on_browse_select_all_new(move || {
        let mut app = state_clone.borrow_mut();
        app.browse.selected.clear();
        let urls = app
            .filtered_browse_articles()
            .into_iter()
            .map(|article| article.url.clone())
            .collect::<Vec<_>>();
        app.browse.selected.extend(urls);
        app.dirty.browse = true;
        app.sync_to_window();
    });

    let state_clone = state.clone();
    window.on_browse_clear_selection(move || {
        let mut app = state_clone.borrow_mut();
        app.browse.selected.clear();
        app.dirty.browse = true;
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
            if !app.bulk.selected_sections.insert(section.id.to_owned()) {
                app.bulk.selected_sections.remove(section.id);
            }
        }
        app.dirty.browse = true;
        app.sync_to_window();
    });

    let state_clone = state.clone();
    window.on_browse_bulk_select_all(move || {
        let mut app = state_clone.borrow_mut();
        app.bulk.selected_sections = app.sections.iter().map(|s| s.id.to_owned()).collect();
        app.dirty.browse = true;
        app.sync_to_window();
    });

    let state_clone = state.clone();
    window.on_browse_bulk_clear(move || {
        let mut app = state_clone.borrow_mut();
        app.bulk.selected_sections.clear();
        app.dirty.browse = true;
        app.sync_to_window();
    });

    let state_clone = state.clone();
    window.on_browse_run_bulk_fetch(move || {
        let mut app = state_clone.borrow_mut();
        app.pull_inputs_from_window();
        app.save_settings();
        app.run_bulk_fetch();
    });

    let state_clone = state.clone();
    window.on_library_apply_filters(move || {
        let mut app = state_clone.borrow_mut();
        app.pull_inputs_from_window();
        app.save_settings();
        app.load_library();
    });

    let state_clone = state.clone();
    window.on_library_set_only_not_uploaded(move |checked| {
        let mut app = state_clone.borrow_mut();
        app.library.only_not_uploaded = checked;
        app.save_settings();
        app.sync_to_window();
    });

    let state_clone = state.clone();
    window.on_library_set_min_words(move |value| {
        let mut app = state_clone.borrow_mut();
        app.library.min_words = value.to_string();
        app.save_settings();
    });

    let state_clone = state.clone();
    window.on_library_set_max_words(move |value| {
        let mut app = state_clone.borrow_mut();
        app.library.max_words = value.to_string();
        app.save_settings();
    });

    let state_clone = state.clone();
    window.on_library_set_duplicate_only(move |checked| {
        let mut app = state_clone.borrow_mut();
        app.library.duplicate_only = checked;
        app.save_settings();
        app.load_library();
    });

    let state_clone = state.clone();
    window.on_library_apply_preset(move |index| {
        let mut app = state_clone.borrow_mut();
        app.apply_filter_preset(index.max(0) as usize);
    });

    let state_clone = state.clone();
    window.on_set_article_density(move |index| {
        let mut app = state_clone.borrow_mut();
        app.article_density = ArticleDensity::from_index(index);
        app.save_settings();
        app.dirty.library = true;
        app.dirty.browse = true;
        app.sync_to_window();
    });

    let state_clone = state.clone();
    window.on_open_library_folder(move || {
        state_clone.borrow_mut().open_library_folder();
    });

    let state_clone = state.clone();
    window.on_library_toggle_select(move |index| {
        let mut app = state_clone.borrow_mut();
        if let Some(article_id) = app
            .filtered_library_articles()
            .get(index as usize)
            .map(|article| article.id)
        {
            app.toggle_upload_selection(article_id);
        }
        app.sync_to_window();
    });

    let state_clone = state.clone();
    window.on_library_select_article(move |index| {
        let mut app = state_clone.borrow_mut();
        let id = app.filtered_library_articles().get(index as usize).map(|a| a.id);
        app.select_article(id);
        app.sync_to_window();
    });

    let state_clone = state.clone();
    window.on_library_delete_article(move |index| {
        let mut app = state_clone.borrow_mut();
        if let Some(article) = app.filtered_library_articles().get(index as usize) {
            let article_id = article.id;
            if app.library.delete_confirm_id == Some(article_id) {
                app.library.delete_confirm_id = None;
                {
                    let _ = app.db.delete_article(article_id);
                    app.lq.selected_articles.remove(&article_id);
                    if app.library.selected_article_id == Some(article_id) {
                        app.library.selected_article_id = None;
                    }
                    app.refresh_saved_urls();
                    app.refresh_stats();
                    app.load_library();
                    app.set_status("Deleted article from the local library.");
                }
            } else {
                app.library.delete_confirm_id = Some(article_id);
                app.dirty.library = true;
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
        app.lq.selected_articles.extend(article_ids);
        app.dirty.library = true;
        app.sync_to_window();
    });

    let state_clone = state.clone();
    window.on_library_clear_upload_selection(move || {
        let mut app = state_clone.borrow_mut();
        app.lq.selected_articles.clear();
        app.dirty.library = true;
        app.sync_to_window();
    });

    let state_clone = state.clone();
    window.on_library_upload_selected(move || {
        let mut app = state_clone.borrow_mut();
        app.pull_inputs_from_window();
        app.upload_selected();
    });

    let state_clone = state.clone();
    window.on_library_delete_selected(move || {
        let mut app = state_clone.borrow_mut();
        app.delete_selected();
    });

    let state_clone = state.clone();
    window.on_lingq_open_settings(move || {
        let mut app = state_clone.borrow_mut();
        app.lq.show_settings = true;
        app.sync_to_window();
    });

    let state_clone = state.clone();
    window.on_lingq_set_only_not_uploaded(move |checked| {
        let mut app = state_clone.borrow_mut();
        app.lq.only_not_uploaded = checked;
        app.save_settings();
        app.sync_to_window();
    });

    let state_clone = state.clone();
    window.on_lingq_set_min_words(move |value| {
        let mut app = state_clone.borrow_mut();
        app.lq.min_words = value.to_string();
        app.save_settings();
    });

    let state_clone = state.clone();
    window.on_lingq_set_max_words(move |value| {
        let mut app = state_clone.borrow_mut();
        app.lq.max_words = value.to_string();
        app.save_settings();
    });

    let state_clone = state.clone();
    window.on_lingq_close_settings(move || {
        let mut app = state_clone.borrow_mut();
        app.lq.show_settings = false;
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
        app.lq.api_key.clear();
        app.lq.username.clear();
        app.lq.password.clear();
        app.lq.collections.clear();
        app.lq.selected_collection = None;
        app.save_settings();
        app.set_status("Disconnected LingQ.");
        app.sync_to_window();
    });

    // Enter key in library search applies filters (re-queries SQLite)
    let state_clone = state.clone();
    window.on_library_search_accepted(move || {
        let mut app = state_clone.borrow_mut();
        app.pull_inputs_from_window();
        app.save_settings();
        app.load_library();
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
        app.browse.date_from = from.format("%Y-%m-%d").to_string();
        app.browse.date_to = to.format("%Y-%m-%d").to_string();
        app.sync_to_window();
    });

    let state_clone = state.clone();
    window.on_taz_set_search_query(move |value| {
        let mut app = state_clone.borrow_mut();
        app.browse.search_query = value.to_string();
    });

    // taz.de search
    let state_clone = state.clone();
    window.on_taz_search(move || {
        let mut app = state_clone.borrow_mut();
        app.pull_inputs_from_window();
        app.run_taz_search();
    });

    // Cancel running background operation
    let state_clone = state.clone();
    window.on_cancel_operation(move || {
        let mut app = state_clone.borrow_mut();
        if app.cancel_active_job() {
            app.set_status("Cancelled current background job.");
            app.cancel_flag.store(false, Ordering::Relaxed);
        } else {
            app.set_status("No cancellable background job is running.");
        }
    });

    let state_clone = state.clone();
    window.on_retry_failed_items(move || {
        state_clone.borrow_mut().retry_failed_items();
    });

    let state_clone = state.clone();
    window.on_clear_failed_items(move || {
        state_clone.borrow_mut().clear_failed_items();
    });

    // Keyboard navigation: move selection up in library list
    let state_clone = state.clone();
    window.on_library_key_up(move || {
        let mut app = state_clone.borrow_mut();
        let filtered = app.filtered_library_articles();
        if filtered.is_empty() {
            return;
        }
        let current_pos = app
            .library.selected_article_id
            .and_then(|id| filtered.iter().position(|a| a.id == id));
        let new_pos = match current_pos {
            Some(0) | None => 0,
            Some(pos) => pos - 1,
        };
        if let Some(article) = filtered.get(new_pos) {
            let new_id = article.id;
            drop(filtered);
            app.select_article(Some(new_id));
            app.sync_to_window();
        }
    });

    // Keyboard navigation: move selection down in library list
    let state_clone = state.clone();
    window.on_library_key_down(move || {
        let mut app = state_clone.borrow_mut();
        let filtered = app.filtered_library_articles();
        if filtered.is_empty() {
            return;
        }
        let last = filtered.len() - 1;
        let current_pos = app
            .library.selected_article_id
            .and_then(|id| filtered.iter().position(|a| a.id == id));
        let new_pos = match current_pos {
            Some(pos) if pos < last => pos + 1,
            Some(pos) => pos,
            None => 0,
        };
        if let Some(article) = filtered.get(new_pos) {
            let new_id = article.id;
            drop(filtered);
            app.select_article(Some(new_id));
            app.sync_to_window();
        }
    });

    // Keyboard navigation: toggle upload selection for currently viewed article
    let state_clone = state.clone();
    window.on_library_key_space(move || {
        let mut app = state_clone.borrow_mut();
        if let Some(id) = app.library.selected_article_id {
            app.toggle_upload_selection(id);
            app.sync_to_window();
        }
    });

    // Toggle preview pane width
    let state_clone = state.clone();
    window.on_toggle_preview_width(move || {
        let mut app = state_clone.borrow_mut();
        app.library.preview_wide = !app.library.preview_wide;
        app.save_settings();
        app.sync_to_window();
    });
}
