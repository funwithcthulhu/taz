use anyhow::{Context, Result, bail};
use log::info;
use reqwest::Client;
use serde::Deserialize;

const LINGQ_BASE: &str = "https://www.lingq.com/api/v3";
const LINGQ_AUTH: &str = "https://www.lingq.com/api/v2/api-token-auth/";

#[derive(Debug, Clone)]
pub struct Collection {
    pub id: i64,
    pub title: String,
    pub lessons_count: i64,
}

#[derive(Debug, Clone)]
pub struct RemoteLesson {
    pub id: i64,
    pub title: String,
    pub original_url: Option<String>,
    pub lesson_url: String,
}

#[derive(Debug, Clone)]
pub struct UploadRequest {
    pub api_key: String,
    pub language_code: String,
    pub collection_id: Option<i64>,
    pub title: String,
    pub text: String,
    pub original_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UploadResponse {
    pub lesson_id: i64,
    pub lesson_url: String,
}

#[derive(Debug, Clone)]
pub struct LoginResponse {
    pub token: String,
}

#[derive(Deserialize)]
struct LingqLessonResponse {
    id: i64,
}

#[derive(Deserialize)]
struct LingqTokenResponse {
    token: Option<String>,
}

#[derive(Deserialize)]
struct LingqCollectionsResponse {
    results: Vec<LingqCollectionRow>,
    next: Option<String>,
}

#[derive(Deserialize)]
struct LingqCollectionRow {
    id: i64,
    title: String,
    #[serde(rename = "lessonsCount")]
    lessons_count: Option<i64>,
    #[serde(rename = "lessons_count")]
    lessons_count_alt: Option<i64>,
}

#[derive(Deserialize)]
struct LingqLessonPage {
    results: Option<Vec<serde_json::Value>>,
    lessons: Option<Vec<serde_json::Value>>,
    items: Option<Vec<serde_json::Value>>,
    next: Option<String>,
}

#[derive(Clone)]
pub struct LingqClient {
    client: Client,
}

impl LingqClient {
    pub fn new() -> Result<Self> {
        let client = Client::builder()
            .user_agent(format!(
                "{}/{}",
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION")
            ))
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .context("failed to build LingQ HTTP client")?;

        Ok(Self { client })
    }

    pub async fn login(&self, username: &str, password: &str) -> Result<LoginResponse> {
        info!("LingQ login attempt for user: {username}");
        let params = [("username", username), ("password", password)];
        let response = self
            .client
            .post(LINGQ_AUTH)
            .form(&params)
            .send()
            .await
            .context("LingQ login request failed")?;

        let response = response
            .error_for_status()
            .context("LingQ rejected the username/password login")?;
        let payload: LingqTokenResponse = response
            .json()
            .await
            .context("failed to parse LingQ login response")?;

        let token = payload
            .token
            .filter(|token| !token.trim().is_empty())
            .context("LingQ login succeeded but no token was returned")?;

        Ok(LoginResponse { token })
    }

    pub async fn get_collections(
        &self,
        api_key: &str,
        language_code: &str,
    ) -> Result<Vec<Collection>> {
        let mut all_collections = Vec::new();
        let mut url = Some(format!("{}/{}/collections/my/", LINGQ_BASE, language_code));
        let max_pages = 20;
        let mut page = 0;

        while let Some(current_url) = url.take() {
            page += 1;
            if page > max_pages {
                break;
            }

            let mut auth = reqwest::header::HeaderValue::from_str(&format!("Token {api_key}"))
                .context("invalid API key characters")?;
            auth.set_sensitive(true);
            let response = self
                .client
                .get(&current_url)
                .header("Authorization", auth)
                .send()
                .await
                .context("LingQ collections request failed")?;

            let response = response
                .error_for_status()
                .context("LingQ rejected the API key or collections request")?;
            let page_data: LingqCollectionsResponse = response
                .json()
                .await
                .context("failed to parse LingQ collections response")?;

            all_collections.extend(page_data.results.into_iter().map(|row| Collection {
                id: row.id,
                title: row.title,
                lessons_count: row.lessons_count.or(row.lessons_count_alt).unwrap_or(0),
            }));

            url = page_data.next;
        }

        Ok(all_collections)
    }

    pub async fn get_collection_lessons(
        &self,
        api_key: &str,
        language_code: &str,
        collection_id: i64,
    ) -> Result<Vec<RemoteLesson>> {
        let candidate_urls = [
            format!(
                "{}/{}/collections/{}/lessons/",
                LINGQ_BASE, language_code, collection_id
            ),
            format!(
                "{}/{}/lessons/?collection={}",
                LINGQ_BASE, language_code, collection_id
            ),
        ];

        let mut last_error = None;
        for url in candidate_urls {
            match self.fetch_lesson_pages(api_key, language_code, &url).await {
                Ok(lessons) => return Ok(lessons),
                Err(err) => last_error = Some(err),
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("LingQ lesson lookup failed")))
    }

    async fn fetch_lesson_pages(
        &self,
        api_key: &str,
        language_code: &str,
        first_url: &str,
    ) -> Result<Vec<RemoteLesson>> {
        let mut lessons = Vec::new();
        let mut url = Some(first_url.to_owned());
        let max_pages = 50;
        let mut page = 0;

        while let Some(current_url) = url.take() {
            page += 1;
            if page > max_pages {
                break;
            }

            let mut auth = reqwest::header::HeaderValue::from_str(&format!("Token {api_key}"))
                .context("invalid API key characters")?;
            auth.set_sensitive(true);
            let response = self
                .client
                .get(&current_url)
                .header("Authorization", auth)
                .send()
                .await
                .context("LingQ lessons request failed")?;

            let response = response
                .error_for_status()
                .context("LingQ rejected the lessons request")?;
            let value: serde_json::Value = response
                .json()
                .await
                .context("failed to parse LingQ lessons response")?;

            let (mut rows, next) = extract_lesson_rows(value)?;
            lessons.extend(
                rows.drain(..)
                    .filter_map(|row| parse_remote_lesson(row, language_code)),
            );
            url = next;
        }

        Ok(lessons)
    }

    pub async fn upload_lesson(&self, request: &UploadRequest) -> Result<UploadResponse> {
        info!("Uploading lesson to LingQ: {}", request.title);
        let normalized_text = normalize_text(&request.text);
        if normalized_text.trim().is_empty() {
            bail!("lesson text is empty");
        }

        let mut payload = serde_json::json!({
            "title": request.title,
            "text": normalized_text,
            "status": "private",
        });

        if let Some(collection_id) = request.collection_id {
            payload["collection"] = serde_json::json!(collection_id);
        }

        if let Some(original_url) = &request.original_url {
            payload["original_url"] = serde_json::json!(original_url);
        }

        let mut auth =
            reqwest::header::HeaderValue::from_str(&format!("Token {}", request.api_key))
                .context("invalid API key characters")?;
        auth.set_sensitive(true);
        let response = self
            .client
            .post(format!("{}/{}/lessons/", LINGQ_BASE, request.language_code))
            .header("Authorization", auth)
            .json(&payload)
            .send()
            .await
            .context("LingQ upload request failed")?;

        let response = response
            .error_for_status()
            .context("LingQ rejected the lesson upload")?;
        let lesson: LingqLessonResponse = response
            .json()
            .await
            .context("failed to parse LingQ upload response")?;

        Ok(UploadResponse {
            lesson_id: lesson.id,
            lesson_url: format!(
                "https://www.lingq.com/{}/learn/lesson/{}/",
                request.language_code, lesson.id
            ),
        })
    }
    /// Update an existing lesson on LingQ (PATCH). Useful when article text
    /// has been re-fetched with better content or the article was previously
    /// paywalled and is now available.
    pub async fn update_lesson(
        &self,
        request: &UploadRequest,
        lesson_id: i64,
    ) -> Result<UploadResponse> {
        info!("Updating LingQ lesson {}: {}", lesson_id, request.title);
        let normalized_text = normalize_text(&request.text);
        if normalized_text.trim().is_empty() {
            bail!("lesson text is empty");
        }

        let mut payload = serde_json::json!({
            "title": request.title,
            "text": normalized_text,
        });

        if let Some(original_url) = &request.original_url {
            payload["original_url"] = serde_json::json!(original_url);
        }

        let mut auth =
            reqwest::header::HeaderValue::from_str(&format!("Token {}", request.api_key))
                .context("invalid API key characters")?;
        auth.set_sensitive(true);
        let response = self
            .client
            .patch(format!(
                "{}/{}/lessons/{}/",
                LINGQ_BASE, request.language_code, lesson_id
            ))
            .header("Authorization", auth)
            .json(&payload)
            .send()
            .await
            .context("LingQ update request failed")?;

        let response = response
            .error_for_status()
            .context("LingQ rejected the lesson update")?;
        let lesson: LingqLessonResponse = response
            .json()
            .await
            .context("failed to parse LingQ update response")?;

        Ok(UploadResponse {
            lesson_id: lesson.id,
            lesson_url: format!(
                "https://www.lingq.com/{}/learn/lesson/{}/",
                request.language_code, lesson.id
            ),
        })
    }
}

fn extract_lesson_rows(
    value: serde_json::Value,
) -> Result<(Vec<serde_json::Value>, Option<String>)> {
    if let Some(array) = value.as_array() {
        return Ok((array.clone(), None));
    }

    let page: LingqLessonPage =
        serde_json::from_value(value).context("failed to decode LingQ lesson page")?;
    let rows = page
        .results
        .or(page.lessons)
        .or(page.items)
        .unwrap_or_default();
    Ok((rows, page.next))
}

fn parse_remote_lesson(value: serde_json::Value, language_code: &str) -> Option<RemoteLesson> {
    let id = value
        .get("id")
        .or_else(|| value.get("pk"))
        .and_then(|field| field.as_i64())?;
    let title = value
        .get("title")
        .and_then(|field| field.as_str())
        .unwrap_or("")
        .trim()
        .to_owned();
    if title.is_empty() {
        return None;
    }

    let original_url = value
        .get("original_url")
        .or_else(|| value.get("originalUrl"))
        .or_else(|| value.get("source_url"))
        .or_else(|| value.get("sourceUrl"))
        .and_then(|field| field.as_str())
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(str::to_owned);
    let lesson_url = value
        .get("url")
        .or_else(|| value.get("lesson_url"))
        .or_else(|| value.get("lessonUrl"))
        .and_then(|field| field.as_str())
        .filter(|url| url.starts_with("http"))
        .map(str::to_owned)
        .unwrap_or_else(|| {
            format!(
                "https://www.lingq.com/{}/learn/lesson/{}/",
                language_code, id
            )
        });

    Some(RemoteLesson {
        id,
        title,
        original_url,
        lesson_url,
    })
}

fn normalize_text(text: &str) -> String {
    text.split("\n\n")
        .map(|paragraph| paragraph.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|paragraph| !paragraph.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_text_collapses_whitespace_within_paragraphs() {
        assert_eq!(normalize_text("hello   world"), "hello world");
    }

    #[test]
    fn normalize_text_preserves_paragraph_breaks() {
        assert_eq!(
            normalize_text("para one\n\npara two"),
            "para one\n\npara two"
        );
    }

    #[test]
    fn normalize_text_strips_empty_paragraphs() {
        assert_eq!(normalize_text("hello\n\n\n\nworld"), "hello\n\nworld");
    }

    #[test]
    fn normalize_text_empty_input() {
        assert_eq!(normalize_text(""), "");
    }

    #[test]
    fn normalize_text_only_whitespace() {
        assert_eq!(normalize_text("   \n\n   \n\n   "), "");
    }
}
