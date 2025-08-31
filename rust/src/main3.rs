use anyhow::Result;
// REMOVED: Unused import: use async_trait::async_trait;
use chromiumoxide::{Browser, BrowserConfig};
use chrono::{DateTime, Duration, Utc};
use crossbeam_channel::{unbounded, Receiver, Sender};
use dashmap::DashMap;
use eframe::egui;
use parking_lot::{Mutex, RwLock};
use regex::Regex;
use reqwest::{Client, Proxy};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration as StdDuration;
use std::{fs, string};
use tokio::sync::{Barrier, RwLock as TokioRwLock, Semaphore};
use tokio::time::sleep; // REMOVED: Unused import `interval`
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{error, info, warn}; // REMOVED: Unused import `debug`
use url::Url;
// REMOVED: Unused import: use uuid::Uuid;
use futures_util::{SinkExt, StreamExt};

// Constants
const TOKENS_FILE: &str = "tokens.json";
const LOGIN_TIMEOUT: u64 = 60000;
const NAVIGATION_TIMEOUT: u64 = 60000;
const API_TIMEOUT: u64 = 60;
const MAX_RETRIES: u32 = 3;
const WORKSPACE_KEY: &str = "3d443a0c-83b8-4a11-8c57-3db9d116ef76";

// Data structures
#[derive(Debug, Clone, Serialize, Deserialize)]
struct User {
    email: String,
    password: String,
    #[serde(rename = "type")]
    user_type: String,
    proxy: Option<String>,
    status: String,
    #[serde(skip)]
    expire_time: Arc<AtomicUsize>, // Change this to Arc<AtomicUsize>
    seats_booked: usize,
    last_update: String,
    logs: Vec<String>,
    #[serde(skip)]
    bot_instance: Option<Arc<WebookBot>>,
    #[serde(skip)]
    assigned_seats: Vec<String>,
    #[serde(skip)]
    save_seat_assignment: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TokenData {
    access_token: String,
    refresh_token: Option<String>,
    token: String,
    expires_at: i64, // Unix timestamp in milliseconds
    user_id: Option<String>,
    savedAt: i64, // Unix timestamp in milliseconds
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SharedEventData {
    chart_key: Option<String>,
    event_key: Option<String>,
    channel_key_common: Option<Vec<String>>,
    channel_key: Option<HashMap<String, Vec<String>>>,
    event_id: Option<String>,
    home_team: Option<TeamInfo>,
    away_team: Option<TeamInfo>,
    channels: Option<Vec<String>>,
    season_structure: Option<String>,
    chart_token: Option<String>,
    commit_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TeamInfo {
    #[serde(rename = "_id")]
    id: String,
    name: String,
}

// Token Manager
struct TokenManager {
    tokens: Arc<RwLock<HashMap<String, TokenData>>>,
    tokens_file: PathBuf,
}

impl TokenManager {
    fn new() -> Self {
        let tokens_file = PathBuf::from(TOKENS_FILE);
        let tokens = Self::load_tokens(&tokens_file);

        Self {
            tokens: Arc::new(RwLock::new(tokens)),
            tokens_file,
        }
    }

    fn load_tokens(path: &PathBuf) -> HashMap<String, TokenData> {
        println!("Loading tokens from: {:?}", path);
        if !path.exists() {
            println!("Tokens file does not exist!");
            return HashMap::new();
        }

        match fs::read_to_string(path) {
            Ok(content) => {
                println!("Loaded tokens file content ({} bytes)", content.len());
                match serde_json::from_str::<HashMap<String, TokenData>>(&content) {
                    Ok(tokens) => {
                        println!("Successfully parsed {} tokens", tokens.len());
                        tokens
                    }
                    Err(e) => {
                        println!("Failed to parse tokens JSON: {}", e);
                        HashMap::new()
                    }
                }
            }
            Err(e) => {
                println!("Failed to read tokens file: {}", e);
                HashMap::new()
            }
        }
    }

    fn save_tokens(&self) -> Result<()> {
        let tokens = self.tokens.read();
        let content = serde_json::to_string_pretty(&*tokens)?;
        fs::write(&self.tokens_file, content)?;
        Ok(())
    }

    fn get_valid_token(&self, email: &str) -> Option<TokenData> {
        println!("Looking for token for email: {}", email);
        let tokens = self.tokens.read();
        println!("Total tokens loaded: {}", tokens.len());

        if let Some(token) = tokens.get(email) {
            println!(
                "Found token for {}, expires_at: {}",
                email, token.expires_at
            );
            let now_millis = Utc::now().timestamp_millis();
            let buffer_millis = 60 * 60 * 1000; // 1 hour in milliseconds
            let is_valid = token.expires_at > (now_millis + buffer_millis);
            println!("Current time: {}, Token valid? {}", now_millis, is_valid);

            if is_valid {
                Some(token.clone())
            } else {
                println!("Token expired");
                None
            }
        } else {
            println!("No token found for email: {}", email);
            None
        }
    }

    /*fn store_token(&self, email: String, token_data: HashMap<String, Value>) -> Result<()> {
        let expires_in = token_data
            .get("token_expires_in")
            .and_then(|v| v.as_i64())
            .unwrap_or(604800);

        let token = TokenData {
            access_token: token_data
                .get("access_token")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            refresh_token: token_data
                .get("refresh_token")
                .and_then(|v| v.as_str())
                .map(String::from),
            token: token_data
                .get("token")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            expires_at: Utc::now() + Duration::seconds(expires_in),
            user_id: token_data
                .get("_id")
                .and_then(|v| v.as_str())
                .map(String::from),
            created_at: Utc::now(),
        };

        self.tokens.write().insert(email, token);
        self.save_tokens()?;
        Ok(())
    }*/
}

// WebookBot Implementation
struct WebookBot {
    email: String,
    password: String,
    headless: bool,
    proxy: Option<String>,
    token_manager: Arc<TokenManager>,
    client: Client,
    browser_id: Arc<Mutex<Option<String>>>,
    webook_hold_token: Arc<Mutex<Option<String>>>,
    expire_time: Arc<AtomicUsize>,
    shared_data: Arc<TokioRwLock<SharedEventData>>,
}
// Add this implementation right after the WebookBot struct

impl std::fmt::Debug for WebookBot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebookBot")
            .field("email", &self.email)
            .field("headless", &self.headless)
            .field("proxy", &self.proxy)
            .finish_non_exhaustive() // Use this for structs with non-Debug fields
    }
}

impl WebookBot {
    fn new(
        email: String,
        password: String,
        headless: bool,
        proxy: Option<String>,
        shared_data: Arc<TokioRwLock<SharedEventData>>,
    ) -> Result<Self> {
        let mut client_builder = Client::builder()
            .timeout(StdDuration::from_secs(API_TIMEOUT))
            .danger_accept_invalid_certs(true)
            .pool_max_idle_per_host(10)
            .pool_idle_timeout(StdDuration::from_secs(5));

        if let Some(proxy_url) = &proxy {
            let proxy = Proxy::all(proxy_url)?;
            client_builder = client_builder.proxy(proxy);
        }

        let client = client_builder.build()?;

        Ok(Self {
            email,
            password,
            headless,
            proxy,
            token_manager: Arc::new(TokenManager::new()),
            client,
            browser_id: Arc::new(Mutex::new(None)),
            webook_hold_token: Arc::new(Mutex::new(None)),
            expire_time: Arc::new(AtomicUsize::new(600)),
            shared_data,
        })
    }

    async fn update_profile(&self, favorite_team_id: &str) -> Result<bool> {
        let token_data = self
            .token_manager
            .get_valid_token(&self.email)
            .ok_or_else(|| anyhow::anyhow!("No valid token for {}", self.email))?;

        let url = "https://api.webook.com/api/v2/update-profile?lang=en";
        let payload = json!({
            "favorite_team": favorite_team_id
        });

        let response = self
            .client
            .post(url)
            .header("accept", "application/json")
            .header(
                "authorization",
                format!("Bearer {}", token_data.access_token),
            )
            .header("content-type", "application/json")
            .header("token", token_data.token)
            .json(&payload)
            .send()
            .await?;

        Ok(response.status().is_success())
    }

    async fn extract_seatsio_chart_data(&self) -> Result<()> {
        let url = "https://cdn-eu.seatsio.net/chart.js";
        let response = self.client.get(url).send().await?;
        let content = response.text().await?;

        let chart_token_regex = Regex::new(r#"seatsio\.chartToken\s*=\s*["']([^"']+)["']"#)?;
        let commit_hash_regex = Regex::new(r#"seatsio\.commitHash\s*=\s*["']([^"']+)["']"#)?;

        let mut shared_data = self.shared_data.write().await;

        if let Some(caps) = chart_token_regex.captures(&content) {
            shared_data.chart_token = Some(caps[1].to_string());
            info!("Found chartToken: {}", &caps[1]);
        }

        if let Some(caps) = commit_hash_regex.captures(&content) {
            shared_data.commit_hash = Some(caps[1].to_string());
            info!("Found commitHash: {}", &caps[1]);
        }
        println!("{:?}", shared_data);
        Ok(())
    }

    async fn send_event_detail_request(&self, event_id: &str) -> Result<()> {
        println!("send_event_detail_request");

        //println!("Getting token for email: {}", self.email);
        let token_data = self
            .token_manager
            .get_valid_token(&self.email)
            .ok_or_else(|| anyhow::anyhow!("No valid token"))?;
        //println!("Token data: {:?}", token_data);

        let url = format!("https://api.webook.com/api/v2/event-detail/{}", event_id);
        //println!("Making request to: {}", url);

        let response = self
            .client
            .get(&url)
            .query(&[("lang", "en"), ("visible_in", "rs")])
            .header("token", token_data.token)
            .header("Accept", "application/json")
            .header("Origin", "https://webook.com")
            .send()
            .await?;

        println!("Got response with status: {}", response.status());
        let data: Value = response.json().await?;
        println!("Response data: {:?}", data);

        let mut shared_data = self.shared_data.write().await;

        if let Some(event_data) = data.get("data") {
            if let Some(seats_io) = event_data.get("seats_io") {
                shared_data.chart_key = seats_io
                    .get("chart_key")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                shared_data.event_key = seats_io
                    .get("event_key")
                    .and_then(|v| v.as_str())
                    .map(String::from);
            }

            if let Some(channel_keys) = event_data.get("channel_keys") {
                shared_data.channel_key_common = channel_keys
                    .get("common")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    });

                shared_data.channel_key = serde_json::from_value(channel_keys.clone()).ok();
            }

            shared_data.event_id = event_data
                .get("_id")
                .and_then(|v| v.as_str())
                .map(String::from);
            shared_data.home_team = event_data
                .get("home_team")
                .and_then(|v| serde_json::from_value(v.clone()).ok());
            shared_data.away_team = event_data
                .get("away_team")
                .and_then(|v| serde_json::from_value(v.clone()).ok());
        }
        //println!("{:?}", shared_data);
        Ok(())
    }

    async fn get_rendering_info(&self) -> Result<()> {
        self.generate_browser_id();
        println!("get_rendering_info");

        let shared_data = self.shared_data.read().await;
        let event_key = shared_data
            .event_key
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Event key not available"))?;

        let url = format!(
            "https://cdn-eu.seatsio.net/system/public/{}/rendering-info",
            WORKSPACE_KEY
        );

        let signature = self.generate_signature("").await?;

        let response = self
            .client
            .get(&url)
            .query(&[("event_key", event_key)])
            .header("accept", "*/*")
            .header("x-client-tool", "Renderer")
            .header("x-request-origin", "webook.com")
            .header("x-signature", signature)
            .send()
            .await?;

        let data: Value = response.json().await?;
        drop(shared_data);

        let mut shared_data = self.shared_data.write().await;

        if let Some(season) = data.get("seasonStructure") {
            shared_data.season_structure = season
                .get("topLevelSeasonKey")
                .and_then(|v| v.as_str())
                .map(String::from);
        }

        let mut all_objects = Vec::new();
        if let Some(channels) = data.get("channels").and_then(|v| v.as_array()) {
            for channel in channels {
                if let Some(objects) = channel.get("objects").and_then(|v| v.as_array()) {
                    for obj in objects {
                        if let Some(obj_str) = obj.as_str() {
                            all_objects.push(obj_str.to_string());
                        }
                    }
                }
            }
        }
        shared_data.channels = Some(all_objects);
        println!("{:?}", shared_data);
        Ok(())
    }

    fn generate_browser_id(&self) {
        let mut browser_id = self.browser_id.lock();
        if browser_id.is_none() {
            let id = format!("{:016x}", rand::random::<u64>());
            *browser_id = Some(id);
        }
    }

    async fn generate_signature(&self, request_body: &str) -> Result<String> {
        let shared_data = self.shared_data.read().await;
        let chart_token = shared_data
            .chart_token
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("chartToken required for signature"))?;

        let reversed_token: String = chart_token.chars().rev().collect();
        let data_to_hash = format!("{}{}", reversed_token, request_body);

        let mut hasher = Sha256::new();
        hasher.update(data_to_hash.as_bytes());
        let result = hasher.finalize();

        Ok(hex::encode(result))
    }

    
    async fn get_webook_hold_token(&self) -> Result<()> {
        let token_data = self
            .token_manager
            .get_valid_token(&self.email)
            .ok_or_else(|| anyhow::anyhow!("No valid token"))?;
        
        let shared_data = self.shared_data.read().await;
        let event_id = shared_data
            .event_id
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Event ID not available"))?;
        println!("get_webook_hold_token");

        let url = "https://api.webook.com/api/v2/seats/hold-token?lang=en";
        let payload = json!({
            "event_id": event_id,
            "lang": "en"
        });

        let response = self
            .client
            .post(url)
            .header(
                "Authorization",
                format!("Bearer {}", token_data.access_token),
            )
            .header("token", token_data.token)
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await?;

        if response.status().is_success() {
            let data: Value = response.json().await?;
            println!("{:?}", data);
            if let Some(token) = data
                .get("data")
                .and_then(|d| d.get("token"))
                .and_then(|t| t.as_str())
            {
                *self.webook_hold_token.lock() = Some(token.to_string());
                info!("Webook hold token received: {}", token);
            }
        }
        println!("get_webook_hold_token2");
        Ok(())
    }

    async fn get_expire_time(&self) -> Result<()> {
        let hold_token = self
            .webook_hold_token
            .lock()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Hold token not available"))?;
        println!("{:?}", hold_token);

        let signature = self.generate_signature("").await?;
        let browser_id = self
            .browser_id
            .lock()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Browser ID not generated"))?;

        let url = format!(
            "https://cdn-eu.seatsio.net/system/public/{}/hold-tokens/{}",
            WORKSPACE_KEY, hold_token
        );

        let response = self
            .client
            .get(&url)
            .header("x-browser-id", browser_id)
            .header("x-client-tool", "Renderer")
            .header("x-signature", signature)
            .send()
            .await?;

        if response.status().is_success() {
            let data: Value = response.json().await?;
            println!("{:?}", data);
            if let Some(expire_time) = data.get("expiresInSeconds").and_then(|v| v.as_u64()) {
                self.expire_time
                    .store(expire_time as usize, Ordering::Relaxed);
            }
        } else {
            println!("{:?}", response)
        }

        Ok(())
    }

    async fn take_seat(&self, seat_id: &str, channel_keys: Vec<String>) -> Result<bool> {
        let shared_data = self.shared_data.read().await;
        let event_key = shared_data
            .event_key
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Event key not available"))?;

        let hold_token = self
            .webook_hold_token
            .lock()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Hold token not available"))?;

        let request_body = json!({
            "events": [event_key],
            "holdToken": hold_token,
            "objects": [{"objectId": seat_id}],
            "channelKeys": channel_keys,
            "validateEventsLinkedToSameChart": true
        });

        let request_body_str = serde_json::to_string(&request_body)?;
        let signature = self.generate_signature(&request_body_str).await?;
        let browser_id = self
            .browser_id
            .lock()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Browser ID not generated"))?;

        let url = format!(
            "https://cdn-eu.seatsio.net/system/public/{}/events/groups/actions/hold-objects",
            WORKSPACE_KEY
        );

        let response = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .header("x-browser-id", browser_id)
            .header("x-client-tool", "Renderer")
            .header("x-signature", signature)
            .body(request_body_str)
            .send()
            .await?;

        Ok(response.status().is_success())
    }

    async fn release_seat(&self, seat_id: &str) -> Result<bool> {
        let shared_data = self.shared_data.read().await;
        let event_key = shared_data
            .event_key
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Event key not available"))?;

        let hold_token = self
            .webook_hold_token
            .lock()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Hold token not available"))?;

        let request_body = json!({
            "events": [event_key],
            "holdToken": hold_token,
            "objects": [{"objectId": seat_id}],
            "validateEventsLinkedToSameChart": true
        });

        let request_body_str = serde_json::to_string(&request_body)?;
        let signature = self.generate_signature(&request_body_str).await?;
        let browser_id = self
            .browser_id
            .lock()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Browser ID not generated"))?;

        let url = format!(
            "https://cdn-eu.seatsio.net/system/public/{}/events/groups/actions/release-held-objects",
            WORKSPACE_KEY
        );

        let response = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .header("x-browser-id", browser_id)
            .header("x-client-tool", "Renderer")
            .header("x-signature", signature)
            .body(request_body_str)
            .send()
            .await?;

        Ok(response.status().is_success())
    }

    /* async fn login_with_browser(&self) -> Result<TokenData> {
        // Check for existing valid token
        if let Some(token) = self.token_manager.get_valid_token(&self.email) {
            info!("Using existing valid token for {}", self.email);
            return Ok(token);
        }

        info!("Starting browser login for {}", self.email);

        let mut config = BrowserConfig::builder();
        if !self.headless {
            // Correction: Check for !self.headless to show browser
            config = config.with_head();
        }

        if let Some(proxy_url) = &self.proxy {
            // FIX 1: The method is `arg`, not `proxy_server`.
            config = config.arg(format!("--proxy-server={}", proxy_url));
        }

        let (browser, mut handler) =
            Browser::launch(config.build().map_err(|e| anyhow::anyhow!(e))?).await?;

        tokio::spawn(async move {
            while let Some(event) = handler.next().await {
                if let Err(e) = event {
                    error!("Browser event error: {}", e);
                }
            }
        });

        let page = browser.new_page("https://webook.com/en/login").await?;

        // Wait for page load
        page.wait_for_navigation().await?;
        sleep(StdDuration::from_secs(2)).await;

        // Handle cookie consent
        if let Ok(cookie_button) = page.find_element("button:has-text('Accept all')").await {
            cookie_button.click().await?;
            sleep(StdDuration::from_millis(1000)).await;
        }

        // Fill login form
        let email_input = page
            .find_element("input[data-testid='auth_login_email_input']")
            .await?;
        email_input.click().await?;
        email_input.type_str(&self.email).await?;

        sleep(StdDuration::from_millis(500)).await;

        let password_input = page
            .find_element("input[data-testid='auth_login_password_input']")
            .await?;
        password_input.click().await?;
        password_input.type_str(&self.password).await?;

        sleep(StdDuration::from_millis(500)).await;

        // Submit login
        let login_button = page
            .find_element("button[data-testid='auth_login_submit_button']")
            .await?;
        login_button.click().await?;

        // Wait for navigation
        page.wait_for_navigation().await?;
        sleep(StdDuration::from_secs(3)).await;

        // FIX 2 & 3: Correctly extract tokens from localStorage
        let token = page
            .evaluate("localStorage.getItem('token')")
            .await?
            .into_value::<Option<String>>()?
            .unwrap_or_default();

        let access_token = page
            .evaluate("localStorage.getItem('access_token')")
            .await?
            .into_value::<Option<String>>()?
            .unwrap_or_default();

        // browser.close().await?;

        let token_data = TokenData {
            access_token,
            refresh_token: None,
            token,
            expires_at: Utc::now() + Duration::days(7),
            user_id: None,
            created_at: Utc::now(),
        };

        self.token_manager
            .tokens
            .write()
            .insert(self.email.clone(), token_data.clone());
        self.token_manager.save_tokens()?;

        Ok(token_data)
    }*/
}

// Seat Scanner
struct SeatScanner {
    workspace_key: String,
    event_key: Arc<RwLock<Option<String>>>,
    season_structure: Arc<RwLock<Option<String>>>,
    is_running: Arc<AtomicBool>,
    users: Arc<RwLock<Vec<Arc<User>>>>,
    ready_user_queue: Arc<Mutex<VecDeque<PreparedRequest>>>,
    selected_sections: Arc<RwLock<Vec<String>>>,
    reserved_seats: Arc<DashMap<String, Vec<String>>>,
}

#[derive(Clone)]
struct PreparedRequest {
    user_email: String,
    url: String,
    headers: HashMap<String, String>,
    body_template: Value,
    chart_token: String,
    client: Client,
}

impl SeatScanner {
    fn new() -> Self {
        Self {
            workspace_key: WORKSPACE_KEY.to_string(),
            event_key: Arc::new(RwLock::new(None)),
            season_structure: Arc::new(RwLock::new(None)),
            is_running: Arc::new(AtomicBool::new(false)),
            users: Arc::new(RwLock::new(Vec::new())),
            ready_user_queue: Arc::new(Mutex::new(VecDeque::new())),
            selected_sections: Arc::new(RwLock::new(Vec::new())),
            reserved_seats: Arc::new(DashMap::new()),
        }
    }

    async fn set_event_key(&self, event_key: String, season_structure: Option<String>) {
        *self.event_key.write() = Some(event_key);
        *self.season_structure.write() = season_structure;
    }

    async fn set_users(&self, users: Vec<Arc<User>>) {
        *self.users.write() = users.clone();

        let mut queue = self.ready_user_queue.lock();
        queue.clear();

        for user in users {
            if user.user_type == "*" || user.user_type == "+" {
                if let Some(bot) = &user.bot_instance {
                    if let Ok(prepared) = self.prepare_booking_request(bot, &user.email).await {
                        queue.push_back(prepared);
                    }
                }
            }
        }

        info!("🚀 {} users ready in high-speed queue", queue.len());
    }

    async fn prepare_booking_request(
        &self,
        bot: &Arc<WebookBot>,
        email: &str,
    ) -> Result<PreparedRequest> {
        let shared_data = bot.shared_data.read().await;
        let chart_token = shared_data
            .chart_token
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Missing chart token"))?;
        let event_key = shared_data
            .event_key
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Missing event key"))?;

        bot.generate_browser_id();
        let browser_id = bot
            .browser_id
            .lock()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Browser ID not generated"))?;

        let body_template = json!({
            "events": [event_key],
            "holdToken": bot.webook_hold_token.lock().clone(),
            "objects": [{"objectId": "{SEAT_ID}"}],
            "channelKeys": ["NO_CHANNEL"],
            "validateEventsLinkedToSameChart": true
        });

        let mut headers = HashMap::new();
        headers.insert("accept".to_string(), "*/*".to_string());
        headers.insert("content-type".to_string(), "application/json".to_string());
        headers.insert("x-browser-id".to_string(), browser_id);
        headers.insert("x-client-tool".to_string(), "Renderer".to_string());

        Ok(PreparedRequest {
            user_email: email.to_string(),
            url: format!(
                "https://cdn-eu.seatsio.net/system/public/{}/events/groups/actions/hold-objects",
                self.workspace_key
            ),
            headers,
            body_template,
            chart_token: chart_token.clone(),
            client: bot.client.clone(),
        })
    }

    async fn start_scanning(&self) {
        if self.is_running.load(Ordering::Relaxed) {
            info!("Scanner already running");
            return;
        }

        self.is_running.store(true, Ordering::Relaxed);
        info!("Starting seat scanner...");

        let scanner = self.clone();
        tokio::spawn(async move {
            scanner.run_websocket().await;
        });
    }

    async fn stop_scanning(&self) {
        self.is_running.store(false, Ordering::Relaxed);
        info!("Scanner stopped");
    }

    async fn run_websocket(&self) {
        while self.is_running.load(Ordering::Relaxed) {
            match self.connect_websocket().await {
                Ok(_) => info!("WebSocket connection closed"),
                Err(e) => error!("WebSocket error: {}", e),
            }

            if self.is_running.load(Ordering::Relaxed) {
                info!("Reconnecting in 5 seconds...");
                sleep(StdDuration::from_secs(5)).await;
            }
        }
    }

    async fn connect_websocket(&self) -> Result<()> {
        let url = "wss://messaging-eu.seatsio.net/ws";
        let (ws_stream, _) = connect_async(url).await?;
        let (write, mut read) = ws_stream.split();
        let write = Arc::new(tokio::sync::Mutex::new(write));

        info!("✓ WebSocket connected");

        // Subscribe to channels
        self.subscribe_to_channels(&write).await?;

        while let Some(msg) = read.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    if let Ok(data) = serde_json::from_str::<Value>(&text) {
                        self.process_message(data).await;
                    }
                }
                Err(e) => {
                    error!("WebSocket message error: {}", e);
                    break;
                }
                _ => {}
            }
        }

        Ok(())
    }

    // FIX 4: Correct the `Send` error by releasing the read lock before an `.await`
    async fn subscribe_to_channels(
        &self,
        write: &Arc<
            tokio::sync::Mutex<
                futures_util::stream::SplitSink<
                    tokio_tungstenite::WebSocketStream<
                        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
                    >,
                    Message,
                >,
            >,
        >,
    ) -> Result<()> {
        let workspace_sub = json!({
            "type": "SUBSCRIBE",
            "data": {"channel": self.workspace_key}
        });

        write
            .lock()
            .await
            .send(Message::Text(serde_json::to_string(&workspace_sub)?))
            .await?;
        info!("📡 Subscribed to workspace channel");

        // Read the event key into a local variable to release the lock before .await
        let event_key_clone = { self.event_key.read().clone() };

        if let Some(event_key) = event_key_clone {
            let event_channel = format!("{}-{}", self.workspace_key, event_key);
            let event_sub = json!({
                "type": "SUBSCRIBE",
                "data": {"channel": event_channel}
            });

            // Now the await call happens without holding the lock
            write
                .lock()
                .await
                .send(Message::Text(serde_json::to_string(&event_sub)?))
                .await?;
            info!("📡 Subscribed to event channel: {}", event_channel);
        }

        Ok(())
    }

    async fn process_message(&self, msg: Value) {
        if msg.get("type").and_then(|v| v.as_str()) != Some("MESSAGE_PUBLISHED") {
            return;
        }

        if let Some(message) = msg.get("message") {
            if let Some(body) = message.get("body") {
                if let Some(status) = body.get("status").and_then(|v| v.as_str()) {
                    if let Some(seat_id) = body.get("objectLabelOrUuid").and_then(|v| v.as_str()) {
                        if status == "free" {
                            self.handle_seat_free(seat_id).await;
                        }
                    }
                }
            }
        }
    }

    async fn handle_seat_free(&self, seat_id: &str) {
        if !self.is_seat_in_selected_sections(seat_id).await {
            return;
        }

        let mut queue = self.ready_user_queue.lock();
        if let Some(prepared_request) = queue.pop_front() {
            let seat_id = seat_id.to_string();
            let mut request_for_task = prepared_request.clone(); // Clone the request for the task

            tokio::spawn(async move {
                if let Err(e) = Self::fire_prepared_request(&mut request_for_task, &seat_id).await {
                    warn!("Failed to book seat {}: {}", seat_id, e);
                }
            });

            queue.push_back(prepared_request); // Push the original request back
        }
    }

    async fn is_seat_in_selected_sections(&self, seat_id: &str) -> bool {
        let sections = self.selected_sections.read();
        sections.iter().any(|prefix| seat_id.starts_with(prefix))
    }

    async fn fire_prepared_request(prepared: &mut PreparedRequest, seat_id: &str) -> Result<()> {
        let mut body = prepared.body_template.clone();
        body["objects"][0]["objectId"] = json!(seat_id);

        let body_str = serde_json::to_string(&body)?;

        // Generate signature
        let reversed_token: String = prepared.chart_token.chars().rev().collect();
        let data_to_hash = format!("{}{}", reversed_token, body_str);
        let mut hasher = Sha256::new();
        hasher.update(data_to_hash.as_bytes());
        let signature = hex::encode(hasher.finalize());

        let mut headers = prepared.headers.clone();
        headers.insert("x-signature".to_string(), signature);

        let mut request = prepared.client.post(&prepared.url);
        for (key, value) in headers {
            request = request.header(key, value);
        }

        let response = request
            .body(body_str)
            .timeout(StdDuration::from_secs(2))
            .send()
            .await?;

        if response.status().is_success() {
            info!("✅ SUCCESS [{}] booked {}", prepared.user_email, seat_id);
        }

        Ok(())
    }

    fn clone(&self) -> Self {
        Self {
            workspace_key: self.workspace_key.clone(),
            event_key: self.event_key.clone(),
            season_structure: self.season_structure.clone(),
            is_running: self.is_running.clone(),
            users: self.users.clone(),
            ready_user_queue: self.ready_user_queue.clone(),
            selected_sections: self.selected_sections.clone(),
            reserved_seats: self.reserved_seats.clone(),
        }
    }
}

// GUI Application
struct BotApp {
    users: Arc<RwLock<Vec<Arc<User>>>>,
    proxies: Vec<String>,
    event_url: String,
    seats: String,
    dseats: String,
    selected_team: String,
    selected_sections: Arc<RwLock<Vec<String>>>,
    shared_event_data: Arc<TokioRwLock<SharedEventData>>,
    scanner: Arc<SeatScanner>,
    runtime: Arc<tokio::runtime::Runtime>,
    ui_receiver: Receiver<UiMessage>,
    ui_sender: Sender<UiMessage>,
    bot_running: Arc<AtomicBool>,
    browser_semaphore: Arc<Semaphore>,
    cached_sections: Arc<RwLock<Vec<String>>>, // Add this line
    bot_instances: Arc<RwLock<HashMap<usize, Arc<WebookBot>>>>,
}

#[derive(Debug, Clone)]
enum UiMessage {
    UpdateUser {
        index: usize,
        field: String,
        value: String,
    },
    UpdateStatus(String),
    RefreshTable,
}

impl BotApp {
    fn new() -> Self {
        let (tx, rx) = unbounded();
        let runtime = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(16)
                .enable_all()
                .build()
                .unwrap(),
        );

        let app = Self {
            users: Arc::new(RwLock::new(Vec::new())),
            proxies: Vec::new(),
            event_url: String::new(),
            seats: "1".to_string(),
            dseats: "1".to_string(),
            selected_team: "home".to_string(),
            selected_sections: Arc::new(RwLock::new(Vec::new())),
            shared_event_data: Arc::new(TokioRwLock::new(SharedEventData {
                chart_key: None,
                event_key: None,
                channel_key_common: None,
                channel_key: None,
                event_id: None,
                home_team: None,
                away_team: None,
                channels: None,
                season_structure: None,
                chart_token: None,
                commit_hash: None,
            })),
            scanner: Arc::new(SeatScanner::new()),
            cached_sections: Arc::new(RwLock::new(Vec::new())),
            runtime: runtime.clone(),
            ui_receiver: rx,
            ui_sender: tx.clone(),
            bot_running: Arc::new(AtomicBool::new(false)),
            browser_semaphore: Arc::new(Semaphore::new(1)),
            bot_instances: Arc::new(RwLock::new(HashMap::new())),
        };
        
        // Start countdown timer after creating app
        app.start_countdown_timer();
        app
    }
    fn load_users(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("CSV files", &["csv"])
            .pick_file()
        {
            match self.read_users_csv(path) {
                Ok(users) => {
                    *self.users.write() = users.into_iter().map(Arc::new).collect();
                    self.assign_proxies();
                    self.ui_sender.send(UiMessage::RefreshTable).unwrap();
                    info!("Loaded {} users", self.users.read().len());
                }
                Err(e) => error!("Failed to load users: {}", e),
            }
        }
    }

    fn read_users_csv(&self, path: PathBuf) -> Result<Vec<User>> {
        let mut users = Vec::new();
        let mut reader = csv::Reader::from_path(path)?;

        for result in reader.deserialize() {
            let record: HashMap<String, String> = result?;

            let user = User {
                email: record.get("email").cloned().unwrap_or_default(),
                password: record.get("password").cloned().unwrap_or_default(),
                user_type: record.get("type").cloned().unwrap_or("*".to_string()),
                proxy: None,
                status: "Ready".to_string(),
                expire_time: Arc::new(AtomicUsize::new(0)), // Initialize with AtomicUsize
                seats_booked: 0,
                last_update: Utc::now().format("%H:%M:%S").to_string(),
                logs: Vec::new(),
                bot_instance: None,
                assigned_seats: Vec::new(),
                save_seat_assignment: Vec::new(),
            };

            if !user.email.is_empty() && !user.password.is_empty() {
                users.push(user);
            }
        }

        Ok(users)
    }

    fn load_proxies(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Text files", &["txt"])
            .pick_file()
        {
            match fs::read_to_string(path) {
                Ok(content) => {
                    self.proxies = content
                        .lines()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    self.assign_proxies();
                    info!("Loaded {} proxies", self.proxies.len());
                }
                Err(e) => error!("Failed to load proxies: {}", e),
            }
        }
    }

    fn assign_proxies(&mut self) {
        if self.proxies.is_empty() {
            return;
        }

        let mut users = self.users.write();
        for (i, user) in users.iter_mut().enumerate() {
            if let Some(user) = Arc::get_mut(user) {
                user.proxy = Some(self.proxies[i % self.proxies.len()].clone());
            }
        }
    }
    // NEW, CORRECT VERSION
    fn start_countdown_timer(&self) {
        let ui_sender = self.ui_sender.clone();
        let users = self.users.clone();

        std::thread::spawn(move || {
            loop {
                // Wait for one second at the start of the loop
                std::thread::sleep(std::time::Duration::from_secs(1));
                
                let mut needs_repaint = false;
                { // Scoped to release the read lock quickly
                    let users_guard = users.read();
                    for user in users_guard.iter() {
                        // Load the current time
                        let current_expire = user.expire_time.load(Ordering::Relaxed);
                        // If it's greater than 0, decrement it
                        if current_expire > 0 {
                            user.expire_time.store(current_expire - 1, Ordering::Relaxed);
                            needs_repaint = true;
                        }
                    }
                } // users_guard lock is released here

                // If any timer was updated, send ONE message to the UI to refresh
                if needs_repaint {
                    let _ = ui_sender.send(UiMessage::RefreshTable);
                }
            }
        });
    }
    
    fn start_bot(&self) {
        if self.bot_running.load(Ordering::Relaxed) {
            return;
        }
    
        self.bot_running.store(true, Ordering::Relaxed);
        
        let users = self.users.clone();
        let event_url = self.event_url.clone();
        let seats = self.seats.parse::<usize>().unwrap_or(1);
        let dseats = self.dseats.parse::<usize>().unwrap_or(1);
        let shared_data = self.shared_event_data.clone();
        let scanner = self.scanner.clone();
        let ui_sender = self.ui_sender.clone();
        let browser_semaphore = self.browser_semaphore.clone();
        let bot_instances = self.bot_instances.clone(); // Add this line
        let runtime = self.runtime.clone();
        let cached_sections = self.cached_sections.clone();
    
        std::thread::spawn(move || {
            runtime.block_on(async {
                let users_read = users.read();
                let mut handles = Vec::new();
    
                for (i, user) in users_read.iter().enumerate() {
                    let user = user.clone();
                    let event_url = event_url.clone();
                    let shared_data = shared_data.clone();
                    let scanner = scanner.clone();
                    let ui_sender = ui_sender.clone();
                    let browser_semaphore = browser_semaphore.clone();
                    let bot_instances = bot_instances.clone(); // Add this line
                    let cached_sections = cached_sections.clone();
                    let handle = tokio::spawn(async move {
                        Self::worker_thread(
                            i,
                            user,
                            event_url,
                            seats,
                            dseats,
                            shared_data,
                            scanner,
                            ui_sender,
                            browser_semaphore,
                            bot_instances, // Add this parameter
                            cached_sections
                        ).await;
                    });
    
                    handles.push(handle);
                }
    
                tokio::time::sleep(StdDuration::from_secs(5)).await;
                
                let shared_data = shared_data.read().await;
                if let Some(event_key) = &shared_data.event_key {
                    scanner.set_event_key(event_key.clone(), shared_data.season_structure.clone()).await;
                    scanner.start_scanning().await;
                }
            });
        });
    }

    async fn worker_thread(
        index: usize,
        user: Arc<User>,
        event_url: String,
        _seats: usize,
        _dseats: usize,
        shared_data: Arc<TokioRwLock<SharedEventData>>,
        scanner: Arc<SeatScanner>,
        ui_sender: Sender<UiMessage>,
        browser_semaphore: Arc<Semaphore>,
        bot_instances: Arc<RwLock<HashMap<usize, Arc<WebookBot>>>>,
        cached_sections: Arc<RwLock<Vec<String>>>, // <-- Add this parameter
    ) {
        let _permit = browser_semaphore.acquire().await.unwrap();

        ui_sender.send(UiMessage::UpdateUser {
            index,
            field: "status".to_string(),
            value: "Logging in...".to_string(),
        }).unwrap();
    
        let bot = Arc::new(
            WebookBot::new(
                user.email.clone(),
                user.password.clone(),
                true,
                user.proxy.clone(),
                shared_data.clone(),
            )
            .unwrap(),
        );
        {
            let mut bots = bot_instances.write();
            bots.insert(index, bot.clone());
        }

        ui_sender
        .send(UiMessage::UpdateUser {
            index,
            field: "status".to_string(),
            value: "Login successful".to_string(),
        })
        .unwrap();

        drop(_permit);

        // Initialize shared data ONLY ONCE, using a static flag
        use std::sync::Once;
        static INIT: Once = Once::new();
        
        let is_first_thread = INIT.is_completed(); // Check if it's already run
    
        INIT.call_once(|| {
            let bot_clone = bot.clone();
            let event_url_clone = event_url.clone();
            let shared_data_clone = shared_data.clone();
            let cached_sections_clone = cached_sections.clone(); // Clone for the task
            let ui_sender_clone = ui_sender.clone(); // Clone for the task
            
            tokio::spawn(async move {
                info!("First thread initializing shared data...");
                // These can fail, so handle the results gracefully
                if let Err(e) = bot_clone.extract_seatsio_chart_data().await {
                    error!("Failed to extract chart data: {}", e); return;
                }
                if let Err(e) = bot_clone.send_event_detail_request(&extract_event_id(&event_url_clone)).await {
                     error!("Failed to get event details: {}", e); return;
                }
                if let Err(e) = bot_clone.get_rendering_info().await {
                     error!("Failed to get rendering info: {}", e); return;
                }
                
                // --- THIS IS THE NEW PART ---
                // Now that data is fetched, update the UI cache
                let data = shared_data_clone.read().await;
                if let Some(channels) = &data.channels {
                    let mut prefixes: Vec<String> = channels
                        .iter()
                        .filter_map(|ch| ch.split('-').next().map(String::from))
                        .collect::<std::collections::HashSet<_>>()
                        .into_iter()
                        .collect();
                    prefixes.sort();
                    
                    // Write the processed sections to the cache
                    *cached_sections_clone.write() = prefixes;
                    
                    // Tell the UI it needs to redraw itself
                    let _ = ui_sender_clone.send(UiMessage::RefreshTable);
                    info!("✅ Shared data initialized and UI cache populated.");
                }
            });
        });
    
        // If another thread is already initializing, wait a bit for it to finish
        if is_first_thread {
            tokio::time::sleep(StdDuration::from_secs(5)).await;
        }
    
        let _ = bot.get_webook_hold_token().await;
        let _ = bot.get_expire_time().await;
    
        let expire_time = bot.expire_time.load(Ordering::Relaxed);
        user.expire_time.store(expire_time, Ordering::Relaxed);
        ui_sender.send(UiMessage::UpdateUser {
            index,
            field: "expire_time".to_string(),
            value: expire_time.to_string(),
        }).unwrap();
    
        ui_sender
            .send(UiMessage::UpdateUser {
                index,
                field: "status".to_string(),
                value: "Ready".to_string(),
            })
            .unwrap();
    
        println!("User {} is ready", index);
    }
    fn stop_bot(&self) {
        self.bot_running.store(false, Ordering::Relaxed);
        let scanner = self.scanner.clone();
        self.runtime.spawn(async move {
            scanner.stop_scanning().await;
        });
        info!("Stop signal sent to bot.");
    }

    fn fill_seats(&self) {
        let seats_per_user = self.seats.parse::<usize>().unwrap_or(1);
        let selected_sections = self.selected_sections.read().clone();
    
        let runtime = self.runtime.clone();
        let users = self.users.clone();
        let shared_data = self.shared_event_data.clone();
        let bot_instances = self.bot_instances.clone(); // Add this
    
        runtime.spawn(async move {
            let shared_data = shared_data.read().await;
            let empty_vec = Vec::new();
            let channels = shared_data.channels.as_ref().unwrap_or(&empty_vec);
    
            let available_seats: Vec<String> = channels
                .iter()
                .filter(|ch| {
                    selected_sections
                        .iter()
                        .any(|prefix| ch.starts_with(prefix))
                })
                .cloned()
                .collect();
    
            let mut seat_index = 0;
    
            // Get bot instances from HashMap instead of User struct
            let assignments = {
                let users_guard = users.read();
                let bots_guard = bot_instances.read();
                let mut collected = Vec::new();
                
                for (i, user) in users_guard.iter().enumerate() {
                    if user.user_type == "*" {
                        if let Some(bot) = bots_guard.get(&i) {
                            let mut user_seats = Vec::new();
                            for _ in 0..seats_per_user {
                                if seat_index < available_seats.len() {
                                    user_seats.push(available_seats[seat_index].clone());
                                    seat_index += 1;
                                } else {
                                    break;
                                }
                            }
                            if !user_seats.is_empty() {
                                collected.push((bot.clone(), user_seats));
                            }
                        }
                    }
                }
                collected
            };
    
            for (bot, seats) in assignments {
                for seat in seats {
                    let _ = bot.take_seat(&seat, vec!["NO_CHANNEL".to_string()]).await;
                }
            }
        });
    }

    fn create_section_checkboxes(&mut self, ui: &mut egui::Ui) {
        let shared_data = self.runtime.block_on(async {
            self.shared_event_data.read().await.clone()
        });
        
        if let Some(channels) = &shared_data.channels {
            // Extract unique prefixes
            let mut prefixes: Vec<String> = channels
                .iter()
                .filter_map(|ch| ch.split('-').next().map(String::from))
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            prefixes.sort();
            
            ui.horizontal(|ui| {
                ui.label("Sections:");
                for prefix in prefixes {
                    let mut selected = self.selected_sections.read().contains(&prefix);
                    if ui.checkbox(&mut selected, &prefix).changed() {
                        let mut sections = self.selected_sections.write();
                        if selected {
                            if !sections.contains(&prefix) {
                                sections.push(prefix);
                            }
                        } else {
                            sections.retain(|s| s != &prefix);
                        }
                    }
                }
            });
        }
    }
}

impl Default for BotApp {
    fn default() -> Self {
        Self::new()
    }
}

fn extract_event_id(url: &str) -> String {
    if let Ok(parsed) = Url::parse(url) {
        let path_segments: Vec<&str> = parsed
            .path_segments()
            .map(|segments| segments.collect())
            .unwrap_or_default();

        if let Some(pos) = path_segments.iter().position(|&s| s == "events") {
            if pos + 1 < path_segments.len() {
                return path_segments[pos + 1].to_string();
            }
        }
    }
    "unknown-event-id".to_string()
}

impl eframe::App for BotApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Process UI messages
        while let Ok(msg) = self.ui_receiver.try_recv() {
            match msg {
                UiMessage::UpdateUser { index, field, value } => {
                    if let Some(user) = self.users.read().get(index) {
                        match field.as_str() {
                            "status" => {
                                if let Some(user) = Arc::get_mut(&mut self.users.write()[index]) {
                                    user.status = value;
                                }
                            }
                            "expire_time" => {
                                let expire_val = value.parse().unwrap_or(0);
                                user.expire_time.store(expire_val, Ordering::Relaxed);
                            }
                            "seats_booked" => {
                                if let Some(user) = Arc::get_mut(&mut self.users.write()[index]) {
                                    user.seats_booked = value.parse().unwrap_or(0);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                UiMessage::UpdateStatus(status) => {
                    info!("{}", status);
                }
                UiMessage::RefreshTable => {
                    ctx.request_repaint();
                }
            }
        }

        // Top panel with controls
        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Load Users CSV").clicked() {
                    self.load_users();
                }

                if ui.button("Load Proxies").clicked() {
                    self.load_proxies();
                }

                ui.separator();

                ui.label("Event URL:");
                ui.text_edit_singleline(&mut self.event_url);

                ui.label("Seats:");
                ui.add(egui::TextEdit::singleline(&mut self.seats).desired_width(50.0));

                ui.label("DSeats:");
                ui.add(egui::TextEdit::singleline(&mut self.dseats).desired_width(50.0));

                ui.separator();

                if !self.bot_running.load(Ordering::Relaxed) {
                    if ui.button("Start Bot").clicked() {
                        self.start_bot();
                    }
                } else {
                    if ui.button("Stop Bot").clicked() {
                        self.stop_bot();
                    }
                }

                if ui.button("Fill Seats").clicked() {
                    self.fill_seats();
                }
            });
        });

        // Team selection
        egui::TopBottomPanel::top("team_panel").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Favorite Team:");
                egui::ComboBox::from_label("")
                    .selected_text(&self.selected_team)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.selected_team,
                            "home".to_string(),
                            "Home Team",
                        );
                        ui.selectable_value(
                            &mut self.selected_team,
                            "away".to_string(),
                            "Away Team",
                        );
                    });
            });
        });

        // Section checkboxes
        // In eframe::App for BotApp, under "Section checkboxes"
        // OLD, BLOCKING CODE
        // In eframe::App for BotApp, under "Section checkboxes"
        // NEW, NON-BLOCKING CODE
        egui::TopBottomPanel::top("sections_panel").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Sections:");
                
                // Read from the non-blocking cache. This is very fast.
                let cached_sections = self.cached_sections.read();
                
                if cached_sections.is_empty() {
                    ui.label("Waiting for event data...");
                } else {
                    let mut selected_sections = self.selected_sections.write();
                    for prefix in cached_sections.iter() {
                        let mut is_selected = selected_sections.contains(prefix);
                        if ui.checkbox(&mut is_selected, prefix).changed() {
                            if is_selected {
                                if !selected_sections.contains(prefix) {
                                    selected_sections.push(prefix.clone());
                                }
                            } else {
                                selected_sections.retain(|s| s != prefix);
                            }
                        }
                    }
                }
            });
        });

        // Main content - Users table
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::both().show(ui, |ui| {
                use egui_extras::{Column, TableBuilder};

                let table = TableBuilder::new(ui)
                    .striped(true)
                    .resizable(true)
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .column(Column::initial(200.0).clip(true)) // Email
                    .column(Column::initial(60.0)) // Type
                    .column(Column::initial(150.0).clip(true)) // Proxy
                    .column(Column::initial(100.0)) // Status
                    .column(Column::initial(80.0)) // Expire
                    .column(Column::initial(80.0)) // Seats
                    .column(Column::initial(100.0)) // Last Update
                    .column(Column::initial(60.0)) // Pay
                    .min_scrolled_height(0.0);

                table
                    .header(20.0, |mut header| {
                        header.col(|ui| {
                            ui.strong("Email");
                        });
                        header.col(|ui| {
                            ui.strong("Type");
                        });
                        header.col(|ui| {
                            ui.strong("Proxy");
                        });
                        header.col(|ui| {
                            ui.strong("Status");
                        });
                        header.col(|ui| {
                            ui.strong("Expire");
                        });
                        header.col(|ui| {
                            ui.strong("Seats");
                        });
                        header.col(|ui| {
                            ui.strong("Last Update");
                        });
                        header.col(|ui| {
                            ui.strong("Pay");
                        });
                    })
                    .body(|body| {
                        let users = self.users.read();
                        body.rows(25.0, users.len(), |mut row| {
                            let row_index = row.index();
                            if let Some(user) = users.get(row_index) {
                                row.col(|ui| {
                                    ui.label(&user.email);
                                });
                                row.col(|ui| {
                                    ui.label(&user.user_type);
                                });
                                row.col(|ui| {
                                    ui.label(user.proxy.as_deref().unwrap_or("None"));
                                });
                                row.col(|ui| {
                                    ui.label(&user.status);
                                });
                                // In the users table body where you display expire time:
                                row.col(|ui| { 
                                    ui.label(format!("{}", user.expire_time.load(Ordering::Relaxed))); 
                                });
                                row.col(|ui| {
                                    ui.label(format!("{}", user.seats_booked));
                                });
                                row.col(|ui| {
                                    ui.label(&user.last_update);
                                });
                                row.col(|ui| {
                                    if ui.button("Pay").clicked() {
                                        self.handle_payment(row_index);
                                    }
                                });
                            }
                        });
                    });
            });
        });

        // Status bar
        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!(
                    "Users: {} | Proxies: {} | Bot: {}",
                    self.users.read().len(),
                    self.proxies.len(),
                    if self.bot_running.load(Ordering::Relaxed) {
                        "Running"
                    } else {
                        "Stopped"
                    }
                ));
            });
        });

        // Auto-refresh
        ctx.request_repaint_after(StdDuration::from_millis(100));
    }
}

impl BotApp {
    fn handle_payment(&self, user_index: usize) {
        let users = self.users.clone();
        let event_url = self.event_url.clone();
        let runtime = self.runtime.clone();
        let ui_sender = self.ui_sender.clone();

        // REPLACE the entire tokio::spawn block in handle_payment with this:
        runtime.spawn(async move {
            // Extract the data needed first to release the lock
            let user_data = {
                users.read().get(user_index).map(|u| {
                    (
                        u.email.clone(),
                        u.password.clone(),
                        event_url.clone(),
                        u.proxy.clone(),
                    )
                })
            }; // Lock on `users` is released here

            if let Some((email, password, event_url, proxy)) = user_data {
                let payload = json!({
                    "email": email,
                    "password": password,
                    "eventsUrl": event_url,
                    "proxyUrl": proxy.as_deref().unwrap_or("")
                });

                let client = Client::new();
                match client
                    .post("http://localhost:3000/getPayment")
                    .json(&payload)
                    .timeout(StdDuration::from_secs(30))
                    .send()
                    .await
                {
                    Ok(response) => {
                        let status = if response.status().is_success() {
                            "Payment completed"
                        } else {
                            "Payment failed"
                        };

                        let _ = ui_sender.send(UiMessage::UpdateUser {
                            index: user_index,
                            field: "status".to_string(),
                            value: status.to_string(),
                        });
                    }
                    Err(e) => {
                        let _ = ui_sender.send(UiMessage::UpdateUser {
                            index: user_index,
                            field: "status".to_string(),
                            value: format!("Payment error: {}", e),
                        });
                    }
                }
            }
        });
    }

}

fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    // Run the GUI application
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1700.0, 800.0])
            .with_title("Webook Bot - Rust Edition"),
        ..Default::default()
    };

    eframe::run_native(
        "Webook Bot",
        options,
        Box::new(|_cc| Ok(Box::new(BotApp::new()))),
    )
    .map_err(|e| anyhow::anyhow!("Failed to run application: {}", e))
}
