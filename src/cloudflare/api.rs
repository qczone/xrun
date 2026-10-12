use crate::error::ErrorCode;
use anyhow::{Context, Result, bail};
use reqwest::{Client, Method, StatusCode};
use serde_json::{Value, json};
use std::time::Duration;

pub(super) struct Api {
    client: Client,
    origin: String,
    token: String,
    secrets: Vec<String>,
}

impl Api {
    pub(super) fn new(token: String) -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(90))
                .build()?,
            origin: "https://api.cloudflare.com/client/v4".into(),
            token,
            secrets: vec![],
        })
    }

    #[cfg(test)]
    pub(super) fn test_origin(mut self, origin: String) -> Self {
        self.origin = origin;
        self
    }

    pub(super) fn protect(&mut self, secret: &str) {
        self.secrets.push(secret.to_owned());
    }

    pub(super) async fn json(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        self.request(method, path, body, None, false)
            .await
            .map(|value| value.expect("required API result"))
    }

    pub(super) async fn optional(&self, path: &str) -> Result<Option<Value>> {
        self.request(Method::GET, path, None, None, true).await
    }

    pub(super) async fn worker(&self, account: &str, name: &str) -> Result<Option<Value>> {
        let scripts = self
            .json(
                Method::GET,
                &format!("/accounts/{account}/workers/scripts"),
                None,
            )
            .await?;
        Ok(scripts
            .as_array()
            .context("Cloudflare Worker list")?
            .iter()
            .find(|worker| worker["id"] == name)
            .cloned())
    }

    pub(super) async fn upload(&self, path: &str, metadata: &Value, source: &str) -> Result<()> {
        let boundary = format!("xrun-{}", uuid::Uuid::new_v4().simple());
        let body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"metadata\"\r\n\
             Content-Type: application/json\r\n\r\n{metadata}\r\n\
             --{boundary}\r\nContent-Disposition: form-data; name=\"relay.mjs\"; filename=\"relay.mjs\"\r\n\
             Content-Type: application/javascript+module\r\n\r\n{source}\r\n--{boundary}--\r\n"
        );
        self.request(
            Method::PUT,
            path,
            None,
            Some((format!("multipart/form-data; boundary={boundary}"), body)),
            false,
        )
        .await?;
        Ok(())
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        upload: Option<(String, String)>,
        missing_ok: bool,
    ) -> Result<Option<Value>> {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.origin))
            .bearer_auth(&self.token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        if let Some((content_type, body)) = upload {
            request = request
                .header(reqwest::header::CONTENT_TYPE, content_type)
                .body(body);
        }
        let mut response = request.send().await.context("contact Cloudflare API")?;
        let status = response.status();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if bytes.len() + chunk.len() > 1024 * 1024 {
                bail!(ErrorCode::InvalidBody.error("Cloudflare API response exceeds 1 MiB"))
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes).unwrap_or(json!({}));
        if missing_ok
            && matches!(status, StatusCode::NOT_FOUND | StatusCode::BAD_REQUEST)
            && value["success"] == false
            && value["errors"].as_array().is_some_and(|errors| {
                !errors.is_empty() && errors.iter().all(|error| error["code"] == 10007)
            })
        {
            return Ok(None);
        }
        if !status.is_success() || value["success"] != true {
            let mut details = value["errors"]
                .as_array()
                .map(|errors| {
                    errors
                        .iter()
                        .take(3)
                        .map(|error| {
                            format!(
                                "{}: {}",
                                error["code"],
                                error["message"].as_str().unwrap_or("request failed")
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("; ")
                })
                .unwrap_or_default();
            for secret in std::iter::once(&self.token).chain(&self.secrets) {
                if !secret.is_empty() {
                    details = details.replace(secret, "[redacted]");
                }
            }
            let code = if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
                ErrorCode::Unauthenticated
            } else {
                ErrorCode::HttpError
            };
            bail!(code.error(format!("Cloudflare API HTTP {status}: {details}")))
        }
        Ok(Some(value["result"].clone()))
    }
}
