use regex::Regex;
use std::sync::LazyLock;

static ARTICLE_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:%21|!)(\d+)(?:/|$)").expect("article id regex"));

/// Return a stable article key for a taz.de URL.
///
/// We prefer the numeric article id embedded in the URL because taz slugs can
/// change over time while the `!1234567` identifier stays stable. If no numeric
/// id is present, fall back to a normalized URL key so the caller still gets a
/// deterministic non-empty identity value.
pub fn article_key_from_url(url: &str) -> String {
    if let Some(id) = article_numeric_id(url) {
        return id;
    }

    format!("url:{}", normalized_url_key(url))
}

pub fn article_numeric_id(url: &str) -> Option<String> {
    ARTICLE_ID_RE
        .captures(url)
        .and_then(|captures| captures.get(1))
        .map(|matched| matched.as_str().to_owned())
}

pub fn normalized_url_key(url: &str) -> String {
    let trimmed = url.trim();
    let without_fragment = trimmed.split('#').next().unwrap_or(trimmed);
    let without_query = without_fragment
        .split('?')
        .next()
        .unwrap_or(without_fragment);

    without_query
        .trim_end_matches('/')
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("www.")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn article_key_prefers_numeric_id() {
        assert_eq!(
            article_key_from_url("https://taz.de/Sonniges-Wochenende/!6175897/"),
            "6175897"
        );
    }

    #[test]
    fn article_key_supports_percent_encoded_bang() {
        assert_eq!(
            article_key_from_url("https://taz.de/Foo/%216175897/"),
            "6175897"
        );
    }

    #[test]
    fn article_key_falls_back_to_normalized_url() {
        assert_eq!(
            article_key_from_url("https://taz.de/Foo/Bar/?x=1#fragment"),
            "url:taz.de/foo/bar"
        );
    }
}
