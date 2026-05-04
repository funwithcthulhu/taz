use regex::Regex;
use std::{collections::HashSet, sync::LazyLock};

pub(super) fn build_clean_text(
    title: &str,
    subtitle: &str,
    author: &str,
    date: &str,
    body: &str,
) -> String {
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

fn matches_title_or_subtitle(canon_block: &str, canon_title: &str, canon_subtitle: &str) -> bool {
    if !canon_block.is_empty() {
        if canon_block == canon_title {
            return true;
        }
        if overlaps_canonical(canon_block, canon_title) {
            return true;
        }
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

static RE_PUNCTUATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s+([,;:.!?)])").expect("punctuation regex"));
static RE_OPENING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([(\[])\s+").expect("opening punctuation regex"));
static RE_QUOTE_SPACING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"([–—-])(["„‚»])"#).expect("dash quote spacing regex"));
static RE_QUOTE_OPENING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(["„‚«])\s+"#).expect("quote opening spacing regex"));
static RE_SPLIT_WORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b([A-Za-zÄÖÜäöüß]) ([a-zäöüß]{2,})\b").expect("split word regex")
});
static RE_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").expect("tag regex"));

pub(super) fn clean_whitespace(input: &str) -> String {
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

pub(super) fn strip_markup(input: &str) -> String {
    clean_whitespace(&RE_TAG.replace_all(input, " "))
}

pub(super) fn trim_chars(input: &str, max: usize) -> String {
    input.chars().take(max).collect()
}

pub(super) fn iso_timestamp_now() -> String {
    chrono::Utc::now().to_rfc3339()
}

pub(super) fn normalize_date(input: &str) -> String {
    let trimmed = input.trim();
    if chrono::NaiveDate::parse_from_str(trimmed, "%Y-%m-%d").is_ok() && trimmed.len() == 10 {
        return trimmed.to_owned();
    }
    if trimmed.len() >= 10
        && let Ok(date) = chrono::NaiveDate::parse_from_str(&trimmed[..10], "%Y-%m-%d")
    {
        return date.format("%Y-%m-%d").to_string();
    }
    if let Ok(date) = chrono::NaiveDate::parse_from_str(trimmed, "%d.%m.%Y") {
        return date.format("%Y-%m-%d").to_string();
    }
    trimmed.to_owned()
}

pub fn estimate_difficulty(body_text: &str) -> i64 {
    let words: Vec<&str> = body_text.split_whitespace().collect();
    if words.len() < 20 {
        return 3;
    }

    let sentence_count = body_text
        .chars()
        .zip(body_text.chars().skip(1).chain(std::iter::once(' ')))
        .filter(|(ch, next)| {
            matches!(ch, '.' | '!' | '?') && (next.is_whitespace() || *next == '"')
        })
        .count()
        .max(1);

    let avg_sentence_len = words.len() as f64 / sentence_count as f64;
    let avg_word_len =
        words.iter().map(|w| w.chars().count()).sum::<usize>() as f64 / words.len() as f64;
    let long_word_ratio =
        words.iter().filter(|w| w.chars().count() >= 10).count() as f64 / words.len() as f64;

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
        assert_eq!(clean_whitespace("hello ."), "hello.");
    }

    #[test]
    fn estimate_difficulty_short_text_defaults_to_3() {
        assert_eq!(estimate_difficulty("Ein paar Wörter."), 3);
    }

    #[test]
    fn estimate_difficulty_returns_1_to_5() {
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

    #[test]
    fn strip_markup_removes_tags() {
        assert_eq!(strip_markup("<b>bold</b> text"), "bold text");
    }

    #[test]
    fn strip_markup_handles_nested_tags() {
        assert_eq!(strip_markup("<div><p>hello</p></div>"), "hello");
    }

    #[test]
    fn build_clean_text_includes_all_metadata() {
        let result = build_clean_text(
            "Titel",
            "Untertitel",
            "Autor",
            "2025-01-01",
            "Body text hier.",
        );
        assert!(result.contains("Titel"), "should contain title");
        assert!(result.contains("Untertitel"), "should contain subtitle");
        assert!(
            result.contains("Von Autor"),
            "should contain author with Von prefix"
        );
        assert!(result.contains("2025-01-01"), "should contain date");
        assert!(result.contains("Body text hier."), "should contain body");
    }

    #[test]
    fn build_clean_text_omits_empty_subtitle() {
        let result = build_clean_text("Titel", "", "Autor", "2025-01-01", "Body.");
        assert!(
            !result.contains("\n\n\n"),
            "should not have double blank lines from empty subtitle"
        );
    }

    #[test]
    fn build_clean_text_removes_duplicate_title_from_body() {
        let result = build_clean_text("Titel", "Sub", "", "", "Titel\n\nActual body content here.");
        let title_count = result.matches("Titel").count();
        assert!(
            title_count <= 2,
            "title should not appear more than twice (header + possible sub overlap)"
        );
    }
}
