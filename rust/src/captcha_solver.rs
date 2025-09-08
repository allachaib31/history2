use serde::{Deserialize, Serialize};
use std::time::Duration;
use anyhow::Result;

const API_KEY: &str = "aad614ddfe71bf02f53c9a1d761cc432";

#[derive(Serialize)]
struct CreateTaskRequest<'a> {
    #[serde(rename = "clientKey")]
    client_key: &'a str,
    task: Task<'a>,
}

#[derive(Serialize)]
struct Task<'a> {
    #[serde(rename = "type")]
    task_type: &'a str,
    #[serde(rename = "websiteURL")]
    website_url: &'a str,
    #[serde(rename = "websiteKey")]
    website_key: &'a str,
    #[serde(rename = "isInvisible")]
    is_invisible: bool,
}

#[derive(Deserialize)]
struct CreateTaskResponse {
    #[serde(rename = "errorId")]
    error_id: i32,
    #[serde(rename = "taskId")]
    // --- FIX: Changed from Option<i32> to Option<i64> ---
    task_id: Option<i64>,
}

#[derive(Serialize)]
struct GetTaskResultRequest<'a> {
    #[serde(rename = "clientKey")]
    client_key: &'a str,
    #[serde(rename = "taskId")]
    // --- FIX: Changed from i32 to i64 ---
    task_id: i64,
}

#[derive(Deserialize)]
struct GetTaskResultResponse {
    #[serde(rename = "errorId")]
    error_id: i32,
    status: String,
    solution: Option<Solution>,
}

#[derive(Deserialize)]
struct Solution {
    #[serde(rename = "gRecaptchaResponse")]
    g_recaptcha_response: String,
}

pub struct CaptchaSolver {
    client: reqwest::Client,
}

impl CaptchaSolver {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }

    pub async fn solve(&self, website_url: &str, website_key: &str) -> Result<String> {
        let create_task_payload = CreateTaskRequest {
            client_key: API_KEY,
            task: Task {
                task_type: "RecaptchaV2TaskProxyless",
                website_url,
                website_key,
                is_invisible: true,
            },
        };

        let resp: CreateTaskResponse = self.client.post("https://api.2captcha.com/createTask")
            .json(&create_task_payload)
            .send().await?
            .json().await?;

        if resp.error_id != 0 || resp.task_id.is_none() {
            return Err(anyhow::anyhow!("2Captcha: Failed to create task"));
        }
        let task_id = resp.task_id.unwrap();

        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let get_result_payload = GetTaskResultRequest {
                client_key: API_KEY,
                task_id,
            };

            let result_resp: GetTaskResultResponse = self.client.post("https://api.2captcha.com/getTaskResult")
                .json(&get_result_payload)
                .send().await?
                .json().await?;

            if result_resp.error_id != 0 {
                return Err(anyhow::anyhow!("2Captcha: Failed to get task result"));
            }

            if result_resp.status == "ready" {
                return Ok(result_resp.solution.unwrap().g_recaptcha_response);
            }
        }
    }
}