use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
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

pub struct LingqClient {
    client: Client,
}

impl LingqClient {
    pub fn new() -> Result<Self> {
        let client = Client::builder()
            .user_agent("taz_lingq_tool/0.1.0")
            .build()
            .context("failed to build LingQ HTTP client")?;

        Ok(Self { client })
    }

    pub fn login(&self, username: &str, password: &str) -> Result<LoginResponse> {
        let params = [("username", username), ("password", password)];
        let response = self
            .client
            .post(LINGQ_AUTH)
            .form(&params)
            .send()
            .context("LingQ login request failed")?;

        let response = response
            .error_for_status()
            .context("LingQ rejected the username/password login")?;
        let payload: LingqTokenResponse = response
            .json()
            .context("failed to parse LingQ login response")?;

        let token = payload
            .token
            .filter(|token| !token.trim().is_empty())
            .context("LingQ login succeeded but no token was returned")?;

        Ok(LoginResponse { token })
    }

    pub fn get_collections(&self, api_key: &str, language_code: &str) -> Result<Vec<Collection>> {
        let response = self
            .client
            .get(format!("{}/{}/collections/my/", LINGQ_BASE, language_code))
            .header("Authorization", format!("Token {api_key}"))
            .send()
            .context("LingQ collections request failed")?;

        let response = response
            .error_for_status()
            .context("LingQ rejected the API key or collections request")?;
        let collections: LingqCollectionsResponse = response
            .json()
            .context("failed to parse LingQ collections response")?;

        Ok(collections
            .results
            .into_iter()
            .map(|row| Collection {
                id: row.id,
                title: row.title,
                lessons_count: row.lessons_count.or(row.lessons_count_alt).unwrap_or(0),
            })
            .collect())
    }

    pub fn upload_lesson(&self, request: &UploadRequest) -> Result<UploadResponse> {
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

        let response = self
            .client
            .post(format!("{}/{}/lessons/", LINGQ_BASE, request.language_code))
            .header("Authorization", format!("Token {}", request.api_key))
            .json(&payload)
            .send()
            .context("LingQ upload request failed")?;

        let response = response
            .error_for_status()
            .context("LingQ rejected the lesson upload")?;
        let lesson: LingqLessonResponse = response
            .json()
            .context("failed to parse LingQ upload response")?;

        Ok(UploadResponse {
            lesson_id: lesson.id,
            lesson_url: format!(
                "https://www.lingq.com/{}/learn/lesson/{}/",
                request.language_code, lesson.id
            ),
        })
    }
}

fn normalize_text(text: &str) -> String {
    text.split("\n\n")
        .map(|paragraph| paragraph.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|paragraph| !paragraph.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}
