#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use anyhow::Result;
use eframe::egui;
#[cfg(unix)]
use libc::{rlimit, setrlimit, RLIMIT_NOFILE};
use log::info;
use parking_lot::Mutex;
use regex::Regex;
use reqwest::{Client, Proxy};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use url::Url;
mod telegram_manager;
use telegram_manager::TelegramManager;
mod captcha_solver;
use captcha_solver::CaptchaSolver;
use std::process::{Child, Command, Stdio};
use telegram_manager::TelegramMessage;
use telegram_manager::TelegramRequest;
use tokio::sync::oneshot;

const WORKSPACE_KEY: &str = "3d443a0c-83b8-4a11-8c57-3db9d116ef76";
#[derive(Clone)] // <-- ADD THIS LINE
struct PreparedSeatRequest {
    url: String,
    json_payload_template: String,
    headers: reqwest::header::HeaderMap,
    client: reqwest::Client,
}

#[derive(Clone)]
struct PreparedTransferRequest {
    url: String,
    headers: reqwest::header::HeaderMap,
    body: String,
    client: reqwest::Client,
}
// Add these structs somewhere near the top of your main.rs file

#[derive(Serialize)]
struct ParallelPaymentUserPayload {
    #[serde(rename = "userIndex")]
    user_index: usize,
    seats: Vec<String>,
    #[serde(rename = "holdToken")]
    hold_token: String,
}

#[derive(Serialize)]
struct ParallelPaymentRequest {
    users: Vec<ParallelPaymentUserPayload>,
}

#[derive(Deserialize, Debug)]
struct ParallelPaymentResponseUser {
    #[serde(rename = "userIndex")]
    user_index: usize,
    selectable: Vec<Value>, // This will hold the rich seat data from Node.js
}
// Add this enum near the top of main.rs
enum SeatTakeStatus {
    Success,
    Failure(Option<reqwest::StatusCode>),
}
pub struct WebbookBot {
    // Form fields
    section_input: String, 
    event_url: String,
    seats: String,
    d_seats: String,
    favorite_team: String,
    sections_status: String,
    select_all_users: bool,
    // User data
    users: Vec<User>,
    proxies: Vec<String>,
    selected_sections_shared: Arc<Mutex<HashMap<String, bool>>>,
    // Status
    bot_running: bool,

    // UI State
    teams: Vec<String>,

    // Runtime
    rt: Option<tokio::runtime::Runtime>,
    bot_manager: Option<Arc<BotManager>>,
    selected_sections: HashMap<String, bool>,
    status_receiver: Option<mpsc::UnboundedReceiver<String>>,
    expire_receiver: Option<mpsc::UnboundedReceiver<(usize, usize)>>,
    assigned_seats_sender: Option<mpsc::UnboundedSender<(usize, String)>>, // ADD THIS LINE
    bot_manager_receiver: Option<mpsc::UnboundedReceiver<Arc<BotManager>>>,
    assigned_seats_receiver: Option<mpsc::UnboundedReceiver<(usize, String)>>, // ADD THIS
    ctx: Option<egui::Context>,
    shared_data: Option<Arc<RwLock<SharedData>>>,
    scanner_running: bool, // Indices of ready users
    // ADD these fields:
    reserved_seats_map: Arc<Mutex<HashMap<String, Vec<String>>>>, // holdTokenHash -> seats
    ready_scanner_users: Arc<Mutex<Vec<usize>>>,                  // Indices of ready users
    next_user_index: Arc<AtomicUsize>,
    show_transfer_modal: bool,
    transfer_from_user: String,
    transfer_to_user: String,
    show_logs_modal: bool,
    // In struct WebbookBot
    selected_user_index: Option<usize>,
    log_sender: Option<mpsc::UnboundedSender<(usize, String)>>,
    log_receiver: Option<mpsc::UnboundedReceiver<(usize, String)>>,
    telegram_sender: Option<mpsc::UnboundedSender<TelegramMessage>>, // --- ADD THIS ---
    transfer_ignore_list: Arc<Mutex<HashSet<String>>>,
    node_process: Option<Child>, // ADD THIS for Node.js process management
    token_file_path: String,
    expiry_warnings_sent: HashMap<usize, bool>,
    pending_scanner_seats: Arc<Mutex<HashMap<usize, usize>>>,
    show_telegram_dialog: bool,
    telegram_dialog_type: Option<TelegramRequest>,
    telegram_input: String,
    telegram_request_receiver: Option<mpsc::UnboundedReceiver<TelegramRequest>>,
    telegram_response_sender: Option<mpsc::UnboundedSender<String>>,
    payment_status_sender: Option<mpsc::UnboundedSender<(usize, usize)>>, // (user_index, paid_count)
    payment_status_receiver: Option<mpsc::UnboundedReceiver<(usize, usize)>>,
    paid_users: Arc<Mutex<HashSet<usize>>>,
    payment_in_progress: Arc<Mutex<HashSet<usize>>>, // <-- ADD THIS LINE
    // Add these two fields to the WebbookBot struct
    paid_seats_query_sender: mpsc::UnboundedSender<(usize, oneshot::Sender<Vec<String>>)>,
    paid_seats_query_receiver: mpsc::UnboundedReceiver<(usize, oneshot::Sender<Vec<String>>)>,
    cooldown_until: Arc<Mutex<HashMap<usize, std::time::Instant>>>,
}

#[derive(Clone, Deserialize)]
struct User {
    #[serde(rename = "type")]
    user_type: String,
    email: String,
    password: String,
    #[serde(skip)]
    proxy: String,
    #[serde(skip)]
    status: String,
    #[serde(skip)]
    expire: String,
    #[serde(skip)]
    seats: String,
    #[serde(skip)]
    last_update: String,
    #[serde(skip)]
    pay_status: String,
    #[serde(skip)]
    target_seats: Vec<String>, // Seats to attempt taking
    #[serde(skip)]
    assigned_seats: Vec<String>,
    #[serde(skip)]
    held_seats: Vec<String>,
    #[serde(skip)]
    logs: Vec<String>,
    #[serde(skip)] // Add this
    max_seats: usize, // Add this
    #[serde(default)] // This will default to `false` if the column is missing in the CSV
    payment_completed: bool,
    #[serde(skip)] // <-- ADD THIS LINE
    selected: bool,
    #[serde(skip)]
    paid_seats: Vec<String>, // Track which seats are already paid for
    #[serde(skip)] 
    remaining_seat_limit: usize, 
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct TokenData {
    token: String,
    refresh_token: String,
    access_token: String,
    #[serde(rename = "UtmWbkWaSessionID")]
    session_id: String,
    #[serde(rename = "savedAt")]
    saved_at: u64,
    expires_at: u64,
}

#[derive(Debug, Default)]
struct SharedData {
    chart_key: Option<String>,
    event_key: Option<String>,
    channel_key_common: Option<Vec<String>>,
    channel_key: Option<Value>,
    event_id: Option<String>,
    home_team: Option<Value>,
    away_team: Option<Value>,
    channels: Option<Vec<String>>,
    season_structure: Option<String>,
    chart_token: Option<String>,
    commit_hash: Option<String>,
    favorite_team: Option<String>,
    recaptcha_site_key: Option<String>,
}

struct TokenManager {
    tokens: HashMap<String, TokenData>,
}

#[derive(Clone)] // Add this line
struct BotUser {
    email: String,
    proxy_url: String,
    client: Client,
    shared_data: Arc<RwLock<SharedData>>,
    token_manager: Arc<TokenManager>,
    webook_hold_token: Arc<Mutex<Option<String>>>,
    expire_time: Arc<AtomicUsize>,
    browser_id: Arc<Mutex<Option<String>>>,
    held_seats: Arc<Mutex<Vec<String>>>,
    prepared_request: Arc<Mutex<Option<PreparedSeatRequest>>>, // ADD THIS
    signature_calculator: Arc<SignatureCalculator>,
}

#[derive(Debug, Deserialize)]
struct ScannerMessage {
    #[serde(rename = "type")]
    message_type: String,
    message: MessageData,
}

#[derive(Debug, Deserialize)]
struct MessageData {
    body: MessageBody,
}

#[derive(Debug, Deserialize)]
struct MessageBody {
    #[serde(rename = "objectLabelOrUuid")]
    object_label: Option<String>,
    status: Option<String>,
    #[serde(rename = "holdTokenHash")]
    hold_token_hash: Option<String>,
    #[serde(rename = "messageType")]
    message_type: Option<String>,
    #[serde(rename = "holdToken")]
    hold_token: Option<String>,
}
struct BotManager {
    shared_data: Arc<RwLock<SharedData>>,
    token_manager: Arc<TokenManager>,
    users: Vec<BotUser>,
    event_url: String,
}

pub struct SignatureCalculator {
    // The sender side of a channel to send jobs TO the crypto thread.
    sender: mpsc::Sender<(String, oneshot::Sender<String>)>,
}

impl SignatureCalculator {
    pub fn new() -> Self {
        let (sender, mut receiver) =
            mpsc::channel::<(String, oneshot::Sender<String>)>(100); // Buffer of 100 jobs

        // Spawn a new OS thread, not a tokio task. This is for CPU-bound work.
        std::thread::spawn(move || {
            // The thread's main loop. It just waits for jobs.
            while let Some((payload, response_sender)) = receiver.blocking_recv() {
                // Perform the expensive SHA256 calculation here.
                let mut hasher = Sha256::new();
                hasher.update(payload.as_bytes());
                let result = hasher.finalize();
                let signature = hex::encode(result);

                // Send the result back to the waiting async task.
                let _ = response_sender.send(signature);
            }
        });

        Self { sender }
    }

    // The async function that our bot will call.
    // It sends the job to the thread and waits for the result without blocking.
    pub async fn calculate(&self, payload: String) -> Result<String> {
        // Create a "one-time" channel to get the response back.
        let (response_sender, response_receiver) = oneshot::channel();

        // Send the payload and the response sender to the crypto thread.
        self.sender
            .send((payload, response_sender))
            .await
            .map_err(|e| anyhow::anyhow!("Failed to send to crypto thread: {}", e))?;

        // Await the result from the crypto thread.
        // This pauses the current task, but the Tokio runtime is free to work on other things.
        let signature = response_receiver
            .await
            .map_err(|e| anyhow::anyhow!("Failed to receive from crypto thread: {}", e))?;
            
        Ok(signature)
    }
}
// ADD THIS ENTIRE BLOCK
impl Drop for WebbookBot {
    fn drop(&mut self) {
        println!("Application closing, stopping Node.js process...");
        self.stop_node_process();
    }
}
impl Default for User {
    fn default() -> Self {
        Self {
            email: String::new(),
            user_type: "*".to_string(),
            password: String::new(),
            proxy: String::new(),
            status: "Ready".to_string(),
            expire: "0".to_string(),
            seats: "0".to_string(),
            last_update: "10:37:47".to_string(),
            pay_status: "Pay".to_string(),
            assigned_seats: Vec::new(),
            target_seats: Vec::new(),
            held_seats: Vec::new(),
            logs: Vec::new(),
            max_seats: 0, // Add this
            payment_completed: false,
            selected: false,
            paid_seats: Vec::new(),
            remaining_seat_limit: 0,
        }
    }
}

impl TokenManager {
    fn new() -> Self {
        Self {
            tokens: HashMap::new(),
        }
    }

    fn load_from_file(&mut self, path: &str) -> Result<()> {
        let content = fs::read_to_string(path)?;
        self.tokens = serde_json::from_str(&content)?;
        Ok(())
    }

    fn get_valid_token(&self, email: &str) -> Option<&TokenData> {
        self.tokens.get(email).filter(|token| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;
            token.expires_at > now
        })
    }
}

impl BotUser {
        // In impl BotUser
    fn new(
        email: String,
        proxy_url: String,
        shared_data: Arc<RwLock<SharedData>>,
        token_manager: Arc<TokenManager>,
        // --- NEW PARAMETER ---
        signature_calculator: Arc<SignatureCalculator>,
    ) -> Result<Self> {
        let no_proxy = reqwest::NoProxy::from_string("localhost,127.0.0.1");

        // This client builder is already perfect for connection pinning.
        // Each user gets their own client, which maintains its own pool of warm connections.
        let mut client_builder = Client::builder()
            .pool_idle_timeout(Some(std::time::Duration::from_secs(90)))
            .pool_max_idle_per_host(100)
            .http2_keep_alive_interval(Some(std::time::Duration::from_secs(60)))
            .http2_keep_alive_timeout(std::time::Duration::from_secs(20))
            .connect_timeout(std::time::Duration::from_secs(60))
            .timeout(std::time::Duration::from_secs(90))
            .tcp_keepalive(std::time::Duration::from_secs(60))
            .tcp_nodelay(true);

        if !proxy_url.is_empty() && proxy_url != "No proxy" {
            let proxy = Proxy::all(&proxy_url)?.no_proxy(no_proxy);
            client_builder = client_builder.proxy(proxy);
        }

        let client = client_builder.build()?;

        Ok(Self {
            email,
            proxy_url,
            client,
            shared_data,
            token_manager,
            webook_hold_token: Arc::new(Mutex::new(None)),
            expire_time: Arc::new(AtomicUsize::new(usize::MAX)),
            browser_id: Arc::new(Mutex::new(None)),
            held_seats: Arc::new(Mutex::new(Vec::new())),
            prepared_request: Arc::new(Mutex::new(None)),
            // --- ASSIGN THE NEW FIELD ---
            signature_calculator,
        })
    }
    
        // In impl BotUser
// In impl BotUser
// In impl BotUser
async fn prewarm_connection(&self) -> Result<()> {
    // --- THIS IS THE NEW "MIMIC" METHOD ---

    // 1. Get all the data needed to build a legitimate `get_expire_time` request.
    let hold_token = self
        .webook_hold_token
        .lock()
        .clone()
        .ok_or_else(|| anyhow::anyhow!("Hold token not available for pre-warm"))?;

    let browser_id = self
        .browser_id
        .lock()
        .clone()
        .ok_or_else(|| anyhow::anyhow!("Browser ID not generated for pre-warm"))?;

    // 2. Generate a valid signature, just like the real request does.
    let signature = self.generate_signature("").await?;

    // 3. Build the full, specific URL that we know works.
    let url = format!(
        "https://cdn-eu.seatsio.net/system/public/{}/hold-tokens/{}",
        WORKSPACE_KEY, hold_token
    );

    // 4. Send the request. We don't care about the response body, only that it succeeds
    // at the network level, which forces the connection to open.
    let response = self
        .client
        .get(&url)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0.0.0 Safari/537.36")
        .header("x-browser-id", browser_id)
        .header("x-client-tool", "Renderer")
        .header("x-signature", signature)
        .send()
        .await;

    // 5. Check the result. As long as we didn't get a network error (like a proxy failure),
    // the connection was established. We can even ignore HTTP errors like 403 here,
    // because the TCP/TLS handshake was still completed.
    match response {
        Ok(_) => {
            println!("🔥 Connection pre-warmed successfully for user {}", self.email);
            Ok(())
        }
        Err(e) => {
            // This will only trigger on a major network or proxy error.
            println!(
                "⚠️  Pre-warm FAILED for {} with a network error: {}",
                self.email, e
            );
            Err(e.into())
        }
    }
}
    // Add this new function to impl BotUser
    async fn refresh_token_with_retries(
        &self,
        log_sender: &mpsc::UnboundedSender<(usize, String)>,
        user_index: usize,
        channel_keys: Vec<String>,
    ) -> bool {
        // Step 1: Try to get hold token (3 attempts)
        let mut hold_token_success = false;
        for attempt in 1..=3 {
            log_sender
                .send((
                    user_index,
                    format!("🔄 Getting hold token (Attempt {}/3)...", attempt),
                ))
                .ok();
                
            match self.get_webook_hold_token().await {
                Ok(_) => {
                    log_sender
                        .send((user_index, "✅ Hold token acquired successfully".to_string()))
                        .ok();
                    hold_token_success = true;
                    break;
                }
                Err(e) => {
                    log_sender
                        .send((
                            user_index,
                            format!("❌ Hold token attempt {}/3 failed: {}", attempt, e),
                        ))
                        .ok();
                    if attempt < 3 {
                        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    }
                }
            }
        }
        
        if !hold_token_success {
            log_sender
                .send((
                    user_index,
                    "❌ All hold token attempts failed. Starting 600s countdown.".to_string(),
                ))
                .ok();
            self.expire_time.store(600, Ordering::Relaxed);
            return false;
        }
        
        // Step 2: Prepare request template
        if let Ok(template) = self.prepare_request_template(channel_keys).await {
            *self.prepared_request.lock() = Some(template);
            log_sender
                .send((user_index, "✅ Request template prepared".to_string()))
                .ok();
        }
        
        // Step 3: Try to get expire time (3 attempts)
        let mut expire_success = false;
        for attempt in 1..=3 {
            log_sender
                .send((
                    user_index,
                    format!("⏰ Getting expire time (Attempt {}/3)...", attempt),
                ))
                .ok();
                
            match self.get_expire_time().await {
                Ok(_) => {
                    log_sender
                        .send((user_index, "✅ Expire time acquired successfully".to_string()))
                        .ok();
                    expire_success = true;
                    break;
                }
                Err(e) => {
                    log_sender
                        .send((
                            user_index,
                            format!("❌ Expire time attempt {}/3 failed: {}", attempt, e),
                        ))
                        .ok();
                    if attempt < 3 {
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                }
            }
        }
        
        if !expire_success {
            log_sender
                .send((
                    user_index,
                    "⚠️ Expire time failed. Starting 600s countdown.".to_string(),
                ))
                .ok();
            self.expire_time.store(600, Ordering::Relaxed);
        }
        
        true // Hold token was successful
    }
    // ADD THIS FUNCTION INSIDE `impl BotUser`
    async fn update_profile(&self, favorite_team_id: &str) -> Result<()> {
        let token_data = self
            .token_manager
            .get_valid_token(&self.email)
            .ok_or_else(|| anyhow::anyhow!("No valid token for profile update"))?;

        let url = "https://api.webook.com/api/v2/update-profile?lang=en";

        let payload = json!({
            "favorite_team": favorite_team_id
        });

        let response = self
            .client
            .post(url)
            .header(
                "Authorization",
                format!("Bearer {}", token_data.access_token),
            )
            .header("token", &token_data.token)
            .header("content-type", "application/json")
            .header(
                "user-agent",
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
            )
            .json(&payload)
            .send()
            .await?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "Update profile failed with status {}",
                response.status()
            ))
        }
    }
    // In impl BotUser
    async fn find_seats(&self) -> Result<Value> {
        let hold_token = self
            .webook_hold_token
            .lock()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Hold token not available for findSeats"))?;

        let held_seats = self.held_seats.lock().clone();

        let payload = json!({
            "SeatsTaking": held_seats,
            "holdToken": hold_token,
        });

        let resp = self
            .client
            .post("http://localhost:3000/findSeats")
            .timeout(std::time::Duration::from_secs(600))
            .json(&payload)
            .send()
            .await?;

        // The response from /findSeats is an array of objects, each with a "seat" key.
        // We need to extract the value of each "seat" key into a new array.
        let seat_info_array: Vec<Value> = resp.json().await?;
        let selected_seats: Vec<Value> = seat_info_array
            .into_iter()
            .filter_map(|v| v.get("seat").cloned())
            .collect();

        // Convert the final array back to a serde_json::Value
        Ok(serde_json::to_value(selected_seats)?)
    }

        // This function is part of `impl BotUser`
    async fn prepare_request_template(
        &self,
        channel_keys: Vec<String>,
    ) -> Result<PreparedSeatRequest> {
        // Get the user's unique hold token, which is essential for the request.
        let hold_token = self
            .webook_hold_token
            .lock()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("No hold token"))?;

        // Access the shared event data (like event key, team info, etc.).
        let shared_data = self.shared_data.read().await;

        // --- Channel Key Pre-Building ---
        // Build the final, complete list of channel keys. This is done only once here.
        let mut final_channel_keys = vec!["NO_CHANNEL".to_string()];

        // Add common keys that apply to everyone.
        if let Some(common_keys) = &shared_data.channel_key_common {
            final_channel_keys.extend(common_keys.clone());
        }

        // Add team-specific keys based on the user's favorite team.
        if let Some(favorite_team) = &shared_data.favorite_team {
            if let Some(channel_key_obj) = &shared_data.channel_key {
                let team_to_use = match favorite_team.as_str() {
                    "home" => &shared_data.home_team,
                    "away" => &shared_data.away_team,
                    _ => &shared_data.home_team,
                };

                if let Some(team) = team_to_use {
                    if let Some(team_id) = team.get("_id").and_then(|v| v.as_str()) {
                        if let Some(team_keys) =
                            channel_key_obj.get(team_id).and_then(|v| v.as_array())
                        {
                            for key in team_keys {
                                if let Some(key_str) = key.as_str() {
                                    final_channel_keys.push(key_str.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }

        // --- Body Template Pre-Building ---
        // Create the JSON structure, inserting the final channel keys and the seat placeholder.
        let request_body_template = json!({
            "events": if let Some(event_key) = &shared_data.event_key {
                vec![event_key.clone()]
            } else {
                return Err(anyhow::anyhow!("No event key available"));
            },
            "holdToken": hold_token,
            "objects": [{"objectId": "__SEAT_ID_PLACEHOLDER__"}], // The critical placeholder
            "channelKeys": final_channel_keys, // The pre-built keys are baked in
            "validateEventsLinkedToSameChart": true
        });

        // Convert the JSON to a simple String. This is the template.
        let json_payload_template = request_body_template.to_string();

        // --- Headers Pre-Building ---
        // Create the HeaderMap with all static headers that do not change.
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("accept", "*/*".parse()?);
        headers.insert("content-type", "application/json".parse()?);
        headers.insert("x-client-tool", "Renderer".parse()?);
        if let Some(browser_id) = self.browser_id.lock().as_ref() {
            headers.insert("x-browser-id", browser_id.parse()?);
        }

        // Return the completed, launch-ready template.
        Ok(PreparedSeatRequest {
            url: format!(
                "https://cdn-eu.seatsio.net/system/public/{}/events/groups/actions/hold-objects",
                WORKSPACE_KEY
            ),
            json_payload_template,
            headers,
            client: self.client.clone(),
        })
    }

    async fn release_seat(&self, seat_id: &str, event_keys: &[String]) -> Result<bool> {
        let hold_token = self
            .webook_hold_token
            .lock()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("No hold token available"))?;

        let request_body = json!({
            "events": event_keys,
            "holdToken": hold_token,
            "objects": [{"objectId": seat_id}],
            "validateEventsLinkedToSameChart": true
        });

        let request_body_str = request_body.to_string();
        let signature = self.generate_signature(&request_body_str).await?;

        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("accept", "*/*".parse()?);
        headers.insert("content-type", "application/json".parse()?);
        headers.insert(
            "user-agent",
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36".parse()?,
        );
        headers.insert("x-client-tool", "Renderer".parse()?);
        headers.insert("x-signature", signature.parse()?);

        if let Some(browser_id) = self.browser_id.lock().as_ref() {
            headers.insert("x-browser-id", browser_id.parse()?);
        }

        if let Some(commit_hash) = &self.shared_data.read().await.commit_hash {
            let referer = format!(
                "https://cdn-eu.seatsio.net/static/version/seatsio-ui-prod-00384-f7t/chart-renderer/chartRendererIframe.html?environment=PROD&commit_hash={}",
                commit_hash
            );
            headers.insert("referer", referer.parse()?);
        }

        let url = format!(
            "https://cdn-eu.seatsio.net/system/public/{}/events/groups/actions/release-held-objects",
            WORKSPACE_KEY
        );

        let response = self
            .client
            .post(&url)
            .headers(headers)
            .body(request_body_str)
            .send()
            .await?;

        let success = response.status() == 200 || response.status() == 204;

        if success {
            // Remove from held seats
            let mut held = self.held_seats.lock();
            held.retain(|s| s != seat_id);
            println!("Released seat: {}", seat_id);
        }

        Ok(success)
    }

    // In impl BotUser
    // In impl BotUser
    async fn retake_seats(
        &self,
        seats_to_retake: Vec<String>,
        _channel_keys: Vec<String>, // No longer needed
        _event_keys: Vec<String>,   // No longer needed
        assigned_seats_sender: mpsc::UnboundedSender<(usize, String)>,
        user_index: usize,
        max_seats: usize, // <-- ADD THIS PARAMETER
    ) -> Result<()> {
        // Located in: impl BotUser
        if seats_to_retake.is_empty() {
            return Ok(());
        }

        println!(
            "User {}: Starting ULTRA-FAST retake for {} seats",
            self.email,
            seats_to_retake.len()
        );

        // Create a collection of tasks, one for each seat.
        let tasks = seats_to_retake
            .into_iter()
            .map(|seat| {
                // Clone all necessary data for the new concurrent task.
                let bot_user_clone = self.clone();
                let assigned_seats_sender_clone = assigned_seats_sender.clone();
                let seat_id_clone = seat.clone();

                // Spawn a new task for this single seat.
               // Replace the entire tokio::spawn block with this one
                tokio::spawn(async move {
                    if let Ok(SeatTakeStatus::Success) = take_seat_direct_final(&bot_user_clone, &seat_id_clone).await {
                        // --- START OF FIX ---
                        
                        let mut seat_was_added = false;

                        // Create a new scope `{}` to ensure the lock is dropped immediately after.
                        {
                            let mut held_seats = bot_user_clone.held_seats.lock();

                            // First, decide if we are within the limit.
                            if max_seats == 0 || held_seats.len() < max_seats {
                                // If so, add the seat immediately while we have the lock.
                                held_seats.push(seat_id_clone.clone());
                                seat_was_added = true;
                            }
                            // The lock on `held_seats` is automatically released here at the end of the scope.
                        }

                        // Now, act on our decision *after* the lock has been released.
                        if seat_was_added {
                            // The seat was added successfully, now we can update the UI.
                            assigned_seats_sender_clone
                                .send((user_index, seat_id_clone.clone()))
                                .ok();
                            println!("🚀 RETAKE SUCCESS: {} by {}", seat, bot_user_clone.email);
                        } else {
                            // We were over the limit. Now it's safe to perform async operations.
                            println!("🔶 Over limit during retake! Releasing seat {}", seat_id_clone);
                            let event_key = bot_user_clone.shared_data.read().await.event_key.clone().unwrap_or_default();
                            bot_user_clone.release_seat(&seat_id_clone, &[event_key]).await.ok();
                        }
                        // --- END OF FIX ---
                    } else {
                        println!("✗ RETAKE FAILED for {}: {}", seat, bot_user_clone.email);
                    }
                })
            })
            .collect::<Vec<_>>();

        // Wait for all the seat-taking HTTP requests to complete.
        futures::future::join_all(tasks).await;

        println!("User {}: All retake attempts completed.", self.email);

        Ok(())
    }
    async fn fill_assigned_seats(
        &self,
        target_seats: Vec<String>,
        channel_keys: Vec<String>,
        event_keys: Vec<String>,
        _status_sender: mpsc::UnboundedSender<String>, // No longer needed for logging
        assigned_seats_sender: mpsc::UnboundedSender<(usize, String)>,
        log_sender: mpsc::UnboundedSender<(usize, String)>, // Add this
        user_index: usize,
    ) -> Result<()> {
        if target_seats.is_empty() {
            return Ok(());
        }

        log_sender
            .send((
                user_index,
                format!(
                    "🎯 Starting to take {} seats manually...",
                    target_seats.len()
                ),
            ))
            .ok();

        // Prepare base headers once (optimization)
        let mut base_headers = reqwest::header::HeaderMap::new();
        base_headers.insert("accept", "*/*".parse()?);
        base_headers.insert(
            "accept-language",
            "ar,en-US;q=0.9,en;q=0.8,fr;q=0.7".parse()?,
        );
        base_headers.insert("content-type", "application/json".parse()?);
        base_headers.insert("origin", "https://cdn-eu.seatsio.net".parse()?);
        base_headers.insert("priority", "u=1, i".parse()?);

        // Add browser ID
        let browser_id = self
            .browser_id
            .lock()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Browser ID not available"))?;
        base_headers.insert("x-browser-id", browser_id.parse()?);
        base_headers.insert("x-client-tool", "Renderer".parse()?);

        // Add commit hash to referer
        let shared_data = self.shared_data.read().await;
        if let Some(commit_hash) = &shared_data.commit_hash {
            let referer = format!(
                "https://cdn-eu.seatsio.net/static/version/seatsio-ui-prod-00384-f7t/chart-renderer/chartRendererIframe.html?environment=PROD&commit_hash={}",
                commit_hash
            );
            base_headers.insert("referer", referer.parse()?);
        }

        let mut successful_seats = 0;
        let total_seats = target_seats.len();

        // In BotUser::fill_assigned_seats, after you define `base_headers`...

        // 1. Create a collection of tasks, one for each seat.
        let tasks = target_seats
            .into_iter()
            .map(|seat| {
                // 2. Clone all necessary data for the new concurrent task.
                // Your BotUser struct already derives Clone, which is perfect.
                let bot_user_clone = self.clone();
                let channel_keys_clone = channel_keys.clone();
                let event_keys_clone = event_keys.clone();
                let base_headers_clone = base_headers.clone();
                let assigned_seats_sender_clone = assigned_seats_sender.clone();
                //let status_sender_clone = status_sender.clone();
                let log_sender_clone = log_sender.clone(); // ADD THIS

                // 3. Spawn a new task for this single seat. This happens almost instantly.
                tokio::spawn(async move {
                    match bot_user_clone
                        .take_single_seat(
                            &seat,
                            &channel_keys_clone,
                            &event_keys_clone,
                            &base_headers_clone,
                        )
                        .await
                    {
                        Ok(true) => {
                            assigned_seats_sender_clone
                                .send((user_index, seat.clone()))
                                .ok();
                            log_sender_clone
                                .send((user_index, format!("✅ Manual Take: {}", seat)))
                                .ok(); // ADD THIS
                        }
                        Ok(false) => {
                            log_sender_clone
                                .send((user_index, format!("✗ Seat Not Available: {}", seat)))
                                .ok(); // ADD THIS
                        }
                        Err(e) => {
                            log_sender_clone
                                .send((user_index, format!("❌ Error taking {}: {}", seat, e)))
                                .ok(); // ADD THIS
                        }
                    }
                })
            })
            .collect::<Vec<_>>(); // Collect all the spawned tasks into a vector.

        futures::future::join_all(tasks).await;
        log_sender
            .send((
                user_index,
                "🏁 All manual seat attempts completed.".to_string(),
            ))
            .ok();
        Ok(())
    }

    async fn take_single_seat(
        &self,
        seat: &str,
        channel_keys: &[String], // This parameter can be ignored now
        event_keys: &[String],
        base_headers: &reqwest::header::HeaderMap,
    ) -> Result<bool> {
        let hold_token = self
            .webook_hold_token
            .lock()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("No hold token available"))?;

        // BUILD CHANNEL KEYS THE SAME WAY
        let shared_data = self.shared_data.read().await;
        let mut final_channel_keys = vec!["NO_CHANNEL".to_string()];

        if let Some(common_keys) = &shared_data.channel_key_common {
            final_channel_keys.extend(common_keys.clone());
        }

        if let Some(favorite_team) = &shared_data.favorite_team {
            if let Some(channel_key_obj) = &shared_data.channel_key {
                let team_to_use = match favorite_team.as_str() {
                    "home" => &shared_data.home_team,
                    "away" => &shared_data.away_team,
                    _ => &shared_data.home_team,
                };

                if let Some(team) = team_to_use {
                    if let Some(team_id) = team.get("_id").and_then(|v| v.as_str()) {
                        if let Some(team_keys) =
                            channel_key_obj.get(team_id).and_then(|v| v.as_array())
                        {
                            for key in team_keys {
                                if let Some(key_str) = key.as_str() {
                                    final_channel_keys.push(key_str.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }

        let request_body = json!({
            "events": event_keys,
            "holdToken": hold_token,
            "objects": [{"objectId": seat}],
            "channelKeys": final_channel_keys,  // USE THE PROPERLY BUILT KEYS
            "validateEventsLinkedToSameChart": true
        });
        //println!("{:?}",request_body);
        let request_body_str = request_body.to_string();
        println!("{:?}", request_body);
        let signature = self.generate_signature(&request_body_str).await?;

        let mut headers = base_headers.clone();
        headers.insert("x-signature", signature.parse()?);

        let url = format!(
            "https://cdn-eu.seatsio.net/system/public/{}/events/groups/actions/hold-objects",
            WORKSPACE_KEY
        );

        let response = self
            .client
            .post(&url)
            .headers(headers)
            .body(request_body_str)
            .send()
            .await?;
        let status = response.status();
        //let response_text = response.text().await?;
        //println!("Response status: {:?}", response_text);
        let success = status == 200 || status == 204; // Only these are success

        if success {
            self.held_seats.lock().push(seat.to_string());
            println!(
                "✓ User {}: Successfully took seat {} (Status: {})",
                self.email, seat, status
            );
        } else {
            println!(
                "✗ User {}: Failed to take seat {} - Status: {}",
                self.email, seat, status
            );
        }

        Ok(success)
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
        println!("get_webook_hold_token for {}", self.email);

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
            .header("token", &token_data.token)
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
        Ok(())
    }

    async fn get_expire_time(&self) -> Result<()> {
        let hold_token = self
            .webook_hold_token
            .lock()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Hold token not available"))?;
        println!("Getting expire time for {}: {}", self.email, hold_token);

        let signature = self.generate_signature("").await?;
        let browser_id = self
            .browser_id
            .lock()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Browser ID not generated"))?;
        println!("{:?}", browser_id);
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
            println!("Error response: {:?}", response);
        }

        Ok(())
    }

    fn generate_browser_id(&self) {
        let mut browser_id = self.browser_id.lock();
        if browser_id.is_none() {
            let id = format!("{:016x}", rand::random::<u64>());
            *browser_id = Some(id);
        }
    }

        // In impl BotUser
    async fn generate_signature(&self, request_body: &str) -> Result<String> {
        // Get the chart token, as before.
        let shared_data = self.shared_data.read().await;
        let chart_token = shared_data
            .chart_token
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("chartToken required for signature"))?;

        let reversed_token: String = chart_token.chars().rev().collect();
        let data_to_hash = format!("{}{}", reversed_token, request_body);

        // --- THE CHANGE ---
        // Instead of calculating here, we send the job to our dedicated crypto thread
        // and await the result without blocking the tokio runtime.
        self.signature_calculator.calculate(data_to_hash).await
    }
}

// This is a global function
// REPLACE the entire take_seat_direct_final function with this
async fn take_seat_direct_final(bot_user: &BotUser, seat_id: &str) -> Result<SeatTakeStatus> {
    let prepared = bot_user
        .prepared_request
        .lock()
        .clone()
        .ok_or_else(|| anyhow::anyhow!("Request template not ready"))?;

    let final_payload = prepared
        .json_payload_template
        .replace("__SEAT_ID_PLACEHOLDER__", seat_id);

    let signature = bot_user.generate_signature(&final_payload).await?;

    let mut headers = prepared.headers.clone();
    headers.insert("x-signature", signature.parse()?);

    let response = prepared
        .client
        .post(&prepared.url)
        .headers(headers)
        .body(final_payload)
        .send()
        .await?;
        
    let status = response.status();
    if status.is_success() {
        Ok(SeatTakeStatus::Success)
    } else {
        // Now we return the specific status code on failure
        Ok(SeatTakeStatus::Failure(Some(status)))
    }
}
fn extract_event_key(event_url: &str) -> anyhow::Result<String> {
    let parsed = Url::parse(event_url)?;
    let segments: Vec<&str> = parsed
        .path_segments()
        .ok_or_else(|| anyhow::anyhow!("Invalid URL path"))?
        .collect();

    if segments.len() < 2 {
        anyhow::bail!("Not enough URL segments");
    }
    println!("{:?}", segments);
    Ok(segments[segments.len() - 2].to_string())
}

impl BotManager {
    // In impl BotManager
    async fn new(
        users_data: Vec<(String, Option<String>)>,
        event_url: String,
        shared_data: Arc<RwLock<SharedData>>,
    ) -> Result<Self> {
        let mut token_manager = TokenManager::new();
        if let Err(e) = token_manager.load_from_file("tokens.json") {
            println!("Warning: Could not load tokens.json: {}", e);
        }
        let token_manager = Arc::new(token_manager);

        // --- FIX Part 1: Create the SignatureCalculator once for the whole application ---
        let signature_calculator = Arc::new(SignatureCalculator::new());

        let mut users = Vec::new();
        for (email, proxy) in users_data {
            let proxy_string = proxy.unwrap_or_else(|| "No proxy".to_string());
            
            // --- FIX Part 2: Pass the calculator to every new BotUser ---
            // We clone the Arc, which is very cheap and just increases the reference count.
            let user = BotUser::new(
                email,
                proxy_string,
                shared_data.clone(),
                token_manager.clone(),
                signature_calculator.clone(), // Add the missing 5th argument here
            )?;
            users.push(user);
        }

        Ok(Self {
            shared_data,
            token_manager,
            users,
            event_url,
        })
    }
    // In impl BotManager
    // In impl BotManager
    async fn extract_recaptcha_site_key(&self) -> Result<()> {
        // --- START OF FIX ---
        // Create a new client with a long timeout specifically for the local server,
        // as Puppeteer can be slow to start.
        let local_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300)) // Generous 90-second timeout
            .build()?;
        // --- END OF FIX ---

        // Use the new local_client for this request instead of the user's client
        let resp = local_client
            .get("http://localhost:3000/extractcaptcha")
            .send()
            .await?;

        #[derive(Deserialize)]
        struct SiteKeyResponse {
            success: bool,
            #[serde(rename = "siteKey")]
            site_key: Option<String>,
            error: Option<String>,
        }

        let data: SiteKeyResponse = resp.json().await?;

        if data.success {
            if let Some(key) = data.site_key {
                self.shared_data.write().await.recaptcha_site_key = Some(key);
                Ok(())
            } else {
                Err(anyhow::anyhow!("Site key not found in successful response"))
            }
        } else {
            Err(anyhow::anyhow!(
                "Failed to get site key: {}",
                data.error.unwrap_or_default()
            ))
        }
    }
    async fn extract_seatsio_chart_data(&self) -> Result<()> {
        let url = "https://cdn-eu.seatsio.net/chart.js";
        let client = &self.users[0].client; // Use first user's client
        let response = client.get(url).send().await?;
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
        println!("Chart data extracted: {:?}", shared_data);
        Ok(())
    }

    async fn send_event_detail_request(&self, event_id: &str) -> Result<()> {
        println!("send_event_detail_request {}", event_id);

        let first_user = &self.users[0]; // Use first user
        let token_data = self
            .token_manager
            .get_valid_token(&first_user.email)
            .ok_or_else(|| anyhow::anyhow!("No valid token"))?;

        let url = format!("https://api.webook.com/api/v2/event-detail/{}", event_id);

        let response = first_user
            .client
            .get(&url)
            .query(&[("lang", "en"), ("visible_in", "rs")])
            .header("token", &token_data.token)
            .header("Accept", "application/json")
            .header("Origin", "https://webook.com")
            .send()
            .await?;

        println!("Got response with status: {}", response.status());
        let data: Value = response.json().await?;
        println!("Event detail data: {:?}", data);

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

                shared_data.channel_key = Some(channel_keys.clone());
            }

            shared_data.event_id = event_data
                .get("_id")
                .and_then(|v| v.as_str())
                .map(String::from);
            shared_data.home_team = event_data.get("home_team").cloned();
            shared_data.away_team = event_data.get("away_team").cloned();
        }
        Ok(())
    }

    async fn get_rendering_info(&self) -> Result<()> {
        let first_user = &self.users[0];
        //first_user.generate_browser_id();
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
        println!("Rendering info URL: {}", event_key);
        let signature = first_user.generate_signature("").await?;

        let response = first_user
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
        all_objects.reverse();

        shared_data.channels = Some(all_objects);
        //println!("Rendering info: {:?}", shared_data);
        Ok(())
    }
    // In impl BotManager
    async fn start_bot(
        &self,
        indices_to_start: &[usize], // MODIFIED: Added this new parameter
        status_sender: mpsc::UnboundedSender<String>,
        log_sender: mpsc::UnboundedSender<(usize, String)>,
        channel_keys: Vec<String>,
    ) -> Result<()> {
        let mut user_tasks = Vec::new();

        // MODIFIED: Loop over the provided indices instead of all users
        for &user_index in indices_to_start {
            // Get the specific user by index, and skip if the index is somehow invalid
            let user = match self.users.get(user_index) {
                Some(u) => u,
                None => continue,
            };
            user.generate_browser_id();

            let user_clone = user.clone();
            let log_sender_clone = log_sender.clone();
            let channel_keys_clone = channel_keys.clone();
            let status_sender_clone = status_sender.clone();

            let task = tokio::spawn(async move {
                log_sender_clone
                    .send((user_index, "ðŸš€ Bot started".to_string()))
                    .ok();

                let shared_data = user_clone.shared_data.read().await;
                if let Some(favorite_team_name) = &shared_data.favorite_team {
                    let team_to_use = match favorite_team_name.as_str() {
                        "away" => &shared_data.away_team,
                        _ => &shared_data.home_team, // Default to home
                    };

                    // 2. Extract the team ID
                    if let Some(team_id) = team_to_use
                        .as_ref()
                        .and_then(|team| team.get("_id"))
                        .and_then(|id_val| id_val.as_str())
                    {
                        // 3. Call the update_profile function
                        match user_clone.update_profile(team_id).await {
                            Ok(_) => {
                                log_sender_clone
                                    .send((
                                        user_index,
                                        "✅ Profile updated with favorite team".to_string(),
                                    ))
                                    .ok();
                            }
                            Err(e) => {
                                log_sender_clone
                                    .send((
                                        user_index,
                                        format!("⚠️ Failed to update profile: {}", e),
                                    ))
                                    .ok();
                            }
                        }
                    }
                }
                // Drop the read lock so other tasks can access shared_data
                drop(shared_data);
                for attempt in 1..=100 {
                    log_sender_clone.send((user_index, format!("Attempt {} - Getting hold token", attempt))).ok();
    
                    match user_clone.get_webook_hold_token().await {
                        Ok(_) => {
                            log_sender_clone.send((user_index, "✅ Hold token received.".to_string())).ok();
    
                            if let Ok(template) = user_clone.prepare_request_template(channel_keys_clone.clone()).await {
                                *user_clone.prepared_request.lock() = Some(template);
                                log_sender_clone.send((user_index, "✅ Request template prepared.".to_string())).ok();
                            } else {
                                log_sender_clone.send((user_index, "❌ FAILED to prepare template".to_string())).ok();
                            }
    
                            // --- NEW LOGIC: PRE-WARM THE CONNECTION ---
                            // Right after the template is ready, establish the connection.
                            if let Err(e) = user_clone.prewarm_connection().await {
                                log_sender_clone.send((user_index, format!("⚠️ Failed to pre-warm connection: {}", e))).ok();
                            }
                            // --- END NEW LOGIC ---
    
                            status_sender_clone.send(format!("USER_READY:{}", user_index)).ok();
    
                            if user_clone.get_expire_time().await.is_err() {
                                log_sender_clone.send((user_index, "❌ Error getting expire time.".to_string())).ok();
                            } else {
                                log_sender_clone.send((user_index, "✅ Session verified. Ready to scan.".to_string())).ok();
                                return; // Exit loop on success
                            }
                        }
                        Err(e) => {
                            log_sender_clone.send((user_index, format!("❌ Attempt {} failed: {}", attempt, e))).ok();
                        }
                    }
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                }
            });
            user_tasks.push(task);
        }

        futures::future::join_all(user_tasks).await;
        Ok(())
    }
}

impl WebbookBot {
    pub fn new() -> Self {
        let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
        let (log_sender, log_receiver) = mpsc::unbounded_channel();
        let (telegram_sender, telegram_receiver) = mpsc::unbounded_channel::<TelegramMessage>();
        let (telegram_request_sender, telegram_request_receiver) = mpsc::unbounded_channel();
        let (telegram_response_sender, telegram_response_receiver) = mpsc::unbounded_channel(); // --- ADD THIS ---
        let selected_sections_shared = Arc::new(Mutex::new(HashMap::new()));
        let (payment_status_sender, payment_status_receiver) = mpsc::unbounded_channel::<(usize, usize)>();
    
        // --- ADD THIS BLOCK ---
        let (psq_tx, psq_rx) = mpsc::unbounded_channel();
        // --- ADD THIS BLOCK to start the Telegram client ---
        rt.spawn(async move {
            match TelegramManager::new(
                telegram_receiver, // CORRECT - this is the receiver variable
                telegram_request_sender,
                telegram_response_receiver,
            )
            .await
            {
                Ok(mut manager) => {
                    println!("Telegram Manager created. Running...");
                    if let Err(e) = manager.run().await {
                        println!("Telegram client error: {}", e);
                    }
                }
                Err(e) => {
                    println!("Failed to create Telegram Manager: {}", e);
                }
            }
        });
        let app = Self {
            section_input: String::new(),
            event_url: "".to_string(),
            seats: "1".to_string(),
            d_seats: "1".to_string(),
            favorite_team: "home".to_string(),
            sections_status: "Waiting for event data...".to_string(),
            bot_running: false,
            teams: vec![
                "home".to_string(),
                "away".to_string(),
                "neutral".to_string(),
            ],
            users: vec![],
            proxies: vec![],
            rt: Some(rt),
            bot_manager: None,
            status_receiver: None,
            select_all_users: false,
            selected_sections: HashMap::new(),
            selected_sections_shared: selected_sections_shared.clone(),
            shared_data: None, // ADD THIS LINE
            bot_manager_receiver: None,
            expire_receiver: None,         // This exists
            assigned_seats_receiver: None, // This exists
            assigned_seats_sender: None,
            ctx: None,
            scanner_running: false,
            reserved_seats_map: Arc::new(Mutex::new(HashMap::new())),
            ready_scanner_users: Arc::new(Mutex::new(Vec::new())), // <-- Corrected line
            next_user_index: Arc::new(AtomicUsize::new(0)),
            show_transfer_modal: false,
            selected_user_index: None,
            transfer_from_user: String::new(),
            transfer_to_user: String::new(),
            show_logs_modal: false,
            log_sender: Some(log_sender),
            log_receiver: Some(log_receiver),
            telegram_sender: Some(telegram_sender),
            transfer_ignore_list: Arc::new(Mutex::new(HashSet::new())),
            node_process: None,                         // ADD THIS
            token_file_path: "tokens.json".to_string(), // ADD THIS
            expiry_warnings_sent: HashMap::new(),
            pending_scanner_seats: Arc::new(Mutex::new(HashMap::new())),
            show_telegram_dialog: false,
            telegram_dialog_type: None,
            telegram_input: String::new(),
            telegram_request_receiver: Some(telegram_request_receiver),
            telegram_response_sender: Some(telegram_response_sender),
            payment_status_sender: Some(payment_status_sender),
            payment_status_receiver: Some(payment_status_receiver),
            paid_users: Arc::new(Mutex::new(HashSet::new())),
            payment_in_progress: Arc::new(Mutex::new(HashSet::new())), // <-- ADD THIS LINE
            
            // --- ADD THESE TWO LINES AT THE END OF THE STRUCT INITIALIZATION ---
            paid_seats_query_sender: psq_tx,
            paid_seats_query_receiver: psq_rx,
            cooldown_until: Arc::new(Mutex::new(HashMap::new())),

        };
        app
    }
    // in impl WebbookBot
    fn save_tokens(&mut self) {
        println!("Try to save token");
        if self.users.is_empty() {
            println!("No users loaded. Please load users first.");
            return;
        }

        if let Some(rt) = &self.rt {
            let users_clone = self.users.clone();
            let token_file_path = self.token_file_path.clone();
            let log_sender = self.log_sender.as_ref().unwrap().clone();

            rt.spawn(async move {
                // We still load all existing tokens once at the start
                // to avoid re-fetching tokens that are already valid.
                let mut existing_tokens: HashMap<String, TokenData> =
                    if let Ok(content) = fs::read_to_string(&token_file_path) {
                        serde_json::from_str(&content).unwrap_or_default()
                    } else {
                        HashMap::new()
                    };

                for (user_index, user) in users_clone.iter().enumerate() {
                    // Check if a valid token already exists in our in-memory map
                    if let Some(existing_token) = existing_tokens.get(&user.email) {
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_millis() as u64;

                        if existing_token.expires_at > now {
                            log_sender
                                .send((user_index, "Using existing valid token.".to_string()))
                                .ok();
                            continue; // Skip to the next user
                        }
                    }

                    // If no valid token, get a new one
                    log_sender
                        .send((user_index, "Getting new token...".to_string()))
                        .ok();

                    match Self::get_token_for_user(user).await {
                        Ok(new_token_data) => {
                            // --- START: MODIFIED SECTION ---
                            // 💾 Read, Modify, and Write the file for this single user

                            // Step 1: Read the latest version of the file.
                            let mut current_tokens: HashMap<String, TokenData> =
                                if let Ok(content) = fs::read_to_string(&token_file_path) {
                                    serde_json::from_str(&content).unwrap_or_default()
                                } else {
                                    HashMap::new()
                                };

                            // Step 2: Insert the new token data.
                            current_tokens.insert(user.email.clone(), new_token_data.clone());

                            // Step 3: Write everything back to the file.
                            if let Ok(json_data) = serde_json::to_string_pretty(&current_tokens) {
                                if let Err(e) = fs::write(&token_file_path, json_data) {
                                    println!("Failed to write token for {}: {}", user.email, e);
                                    log_sender
                                        .send((user_index, format!("🔴 FILE SAVE FAILED: {}", e)))
                                        .ok();
                                } else {
                                    log_sender
                                        .send((
                                            user_index,
                                            "✅ Token saved successfully to file.".to_string(),
                                        ))
                                        .ok();
                                    // Also update our in-memory map so we don't re-fetch it in this same run
                                    existing_tokens.insert(user.email.clone(), new_token_data);
                                }
                            }
                            // --- END: MODIFIED SECTION ---
                        }
                        Err(e) => {
                            log_sender
                                .send((user_index, format!("Token fetch failed: {}", e)))
                                .ok();
                        }
                    }
                }
                println!("All token saving attempts are complete.");
            });
        } else {
            println!("Tokio runtime not available.");
        }
    }
    async fn get_token_for_user(user: &User) -> Result<TokenData> {
        #[derive(Serialize)]
        struct LoginRequest {
            email: String,
            password: String,
            proxy: String,
        }

        #[derive(Deserialize)]
        struct LoginResponse {
            success: bool,
            access_token: String,
            refresh_token: String,
            token_expires_in: i64,
            token: String,
            id: String,
            #[serde(rename = "utm_wbk_wa_session_id")]
            session_id: String,
        }

        let login_req = LoginRequest {
            email: user.email.clone(),
            password: user.password.clone(),
            proxy: user.proxy.clone(),
        };

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()?;

        let response = client
            .post("http://localhost:3000/capturelogin")
            .json(&login_req)
            .send()
            .await?;

        if response.status() != 200 {
            let error_text = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("HTTP error: {}", error_text));
        }

        let login_resp: LoginResponse = response.json().await?;

        if !login_resp.success {
            return Err(anyhow::anyhow!("Login was not successful"));
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let expires_at = if login_resp.token_expires_in > 0 {
            now + (login_resp.token_expires_in as u64 * 1000)
        } else {
            now + (7 * 24 * 60 * 60 * 1000) // 7 days default
        };

        Ok(TokenData {
            token: login_resp.token,
            refresh_token: login_resp.refresh_token,
            access_token: login_resp.access_token,
            session_id: login_resp.session_id,
            saved_at: now,
            expires_at,
        })
    }
    fn start_node_process(&mut self) -> Result<()> {
        // Kill existing process if running
        if let Some(mut process) = self.node_process.take() {
            let _ = process.kill();
            println!("Killed existing Node.js process");
        }

        // Start new Node.js process
        let event_url = self.event_url.clone();

        let mut command = Command::new("node");
        command
            .arg("./script/newTest2.js")
            .arg(&event_url)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        match command.spawn() {
            Ok(child) => {
                println!("Node.js server started with PID: {}", child.id());
                self.node_process = Some(child);
                Ok(())
            }
            Err(e) => {
                let error_msg = format!(
                    "Failed to start Node.js server: {}\n\nMake sure Node.js is installed and the script is at '../newWebokboot/newTest2.js'", 
                    e
                );
                println!("{}", error_msg);
                Err(anyhow::anyhow!(error_msg))
            }
        }
    }

    // NEW: Stop Node.js process
    fn stop_node_process(&mut self) {
        if let Some(mut process) = self.node_process.take() {
            if let Err(e) = process.kill() {
                println!("Failed to kill Node.js process: {}", e);
            } else {
                println!("Node.js process stopped");
            }
        }
    }
    // In impl WebbookBot
    fn update_logs(&mut self) {
        // Step 1: Collect all available log messages into a temporary vector.
        let mut messages_to_log = Vec::new();
        if let Some(receiver) = &mut self.log_receiver {
            while let Ok(message) = receiver.try_recv() {
                messages_to_log.push(message);
            }
        }

        // Step 2: Now that the borrow on `log_receiver` is released,
        // iterate over the temporary vector and add the logs.
        for (user_index, message) in messages_to_log {
            self.add_log(user_index, &message);
        }
    }

    // This is the function that formats and appends the log, just like in Go.
    fn add_log(&mut self, user_index: usize, message: &str) {
        if let Some(user) = self.users.get_mut(user_index) {
            // Get current time in HH:MM:SS:mmm format (with milliseconds)
            let now = std::time::SystemTime::now();
            let since_epoch = now
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            let secs = since_epoch.as_secs();
            let millis = since_epoch.subsec_millis(); // ADD THIS LINE

            let timestamp_str = format!(
                "{:02}:{:02}:{:02}:{:03}", // CHANGE THIS - added {:03} for milliseconds
                (secs / 3600) % 24,
                (secs / 60) % 60,
                secs % 60,
                millis // ADD THIS - milliseconds with 3 digits, zero-padded
            );

            let log_entry = format!("[{}] {}", timestamp_str, message);
            user.logs.push(log_entry);
        }
    }

    // REPLACE the entire dispatch_seat_take_task function with this new version
    async fn dispatch_seat_take_task(
        seat_id: &str,
        log_prefix: &str,
        bot_manager: &Option<Arc<BotManager>>,
        ready_scanner_users: &Arc<Mutex<Vec<usize>>>,
        assigned_seats_sender: &mpsc::UnboundedSender<(usize, String)>,
        log_sender: &mpsc::UnboundedSender<(usize, String)>,
        telegram_sender: &mpsc::UnboundedSender<TelegramMessage>,
        ui_users: &Vec<User>,
        payment_in_progress: &Arc<Mutex<HashSet<usize>>>,
        next_user_index: &Arc<AtomicUsize>,
        cooldown_until: &Arc<Mutex<HashMap<usize, std::time::Instant>>>, // <-- NEW PARAMETER
    ) {
        let manager = match bot_manager {
            Some(m) => m,
            None => return,
        };

        let ready_users_indices = ready_scanner_users.lock();
        if ready_users_indices.is_empty() {
            return;
        }

        let start_index = next_user_index.load(Ordering::Relaxed);
        let num_ready_users = ready_users_indices.len();

        for i in 0..num_ready_users {
            let current_offset = (start_index + i) % num_ready_users;
            let actual_user_index = ready_users_indices[current_offset];

            // --- 1. NEW COOLDOWN CHECK ---
            let is_in_cooldown = {
                let cooldown_map = cooldown_until.lock();
                if let Some(end_time) = cooldown_map.get(&actual_user_index) {
                    std::time::Instant::now() < *end_time
                } else {
                    false
                }
            };
            if is_in_cooldown {
                continue; // Skip this user, they are in a timeout.
            }
            // --- END COOLDOWN CHECK ---

            let is_paying = payment_in_progress.lock().contains(&actual_user_index);
            if is_paying {
                continue;
            }

            if let (Some(ui_user), Some(bot_user)) = (
                ui_users.get(actual_user_index),
                manager.users.get(actual_user_index),
            ) {
                let is_below_limit =
                    ui_user.max_seats == 0 || bot_user.held_seats.lock().len() < ui_user.max_seats;

                if is_below_limit {
                    next_user_index.store((current_offset + 1) % num_ready_users, Ordering::Relaxed);

                    let bot_user_clone = bot_user.clone();
                    let sender_clone = assigned_seats_sender.clone();
                    let log_sender_clone = log_sender.clone();
                    let telegram_sender_clone = telegram_sender.clone();
                    let cooldown_until_clone = cooldown_until.clone(); // Clone Arc for the task
                    let seat_id_clone = seat_id.to_string();
                    let log_prefix_clone = log_prefix.to_string();
                    let user_email = ui_user.email.clone();
                    let max_seats_limit = ui_user.max_seats;

                    tokio::spawn(async move {
                        // --- 2. MODIFIED LOGIC TO HANDLE 403 ---
                        match take_seat_direct_final(&bot_user_clone, &seat_id_clone).await {
                            Ok(SeatTakeStatus::Success) => {
                                // This is the original success logic, no changes here
                                let was_released;
                                {
                                    let mut held_seats = bot_user_clone.held_seats.lock();
                                    held_seats.push(seat_id_clone.clone());
                                    if max_seats_limit > 0 && held_seats.len() > max_seats_limit {
                                        held_seats.pop();
                                        was_released = true;
                                        log_sender_clone.send((actual_user_index, format!("🔶 Over limit! Releasing seat {}", seat_id_clone))).ok();
                                        let bot_user_for_release = bot_user_clone.clone();
                                        let seat_id_for_release = seat_id_clone.clone();
                                        tokio::spawn(async move {
                                            let event_key = bot_user_for_release.shared_data.read().await.event_key.clone().unwrap_or_default();
                                            bot_user_for_release.release_seat(&seat_id_for_release, &[event_key]).await.ok();
                                        });
                                    } else {
                                        was_released = false;
                                    }
                                }
                                if !was_released {
                                    sender_clone.send((actual_user_index, seat_id_clone.clone())).ok();
                                    log_sender_clone.send((actual_user_index, format!("🚀 {}: Booked {}", log_prefix_clone, seat_id_clone))).ok();
                                    let total_held = bot_user_clone.held_seats.lock().len();
                                    let message = format!(
                                        "🎫 SEAT TAKING ✅ SUCCESS\n━━━━━━━━━━━━━━━━━\n🔧 Account: {}\n💺 Seat: {}\n📊 Total Held: {}/{}",
                                        user_email,
                                        seat_id_clone,
                                        total_held,
                                        if max_seats_limit == 0 { "∞".to_string() } else { max_seats_limit.to_string() }
                                    );
                                    telegram_sender_clone.send(TelegramMessage { text: message, chat_id: 4785839500 }).ok();
                                }
                            }
                            Ok(SeatTakeStatus::Failure(Some(status))) if status.as_u16() == 403 => {
                                // --- THIS IS THE NEW COOLDOWN LOGIC ---
                                log_sender_clone.send((actual_user_index, format!("🔴 Got 403 Forbidden for seat {}. Cooling down for 1 minute.", seat_id_clone))).ok();
                                let cooldown_duration = std::time::Duration::from_secs(60);
                                cooldown_until_clone.lock().insert(
                                    actual_user_index,
                                    std::time::Instant::now() + cooldown_duration
                                );
                            }
                            _ => {
                                // This handles other errors (e.g., 404 Not Found, 500 Server Error)
                                // We don't apply a cooldown for these.
                            }
                        }
                    });
                    return;
                }
            }
        }
    }
    
    fn show_logs_modal(&mut self, ctx: &egui::Context) {
        if let Some(user_index) = self.selected_user_index {
            // Get the user's current data
            if let Some(user) = self.users.get(user_index) {
                let mut is_open = true;
                egui::Window::new(&format!("Logs for {}", user.email))
                    .default_size([800.0, 300.0])
                    .resizable(true)
                    .open(&mut is_open) // Use `open` to handle the close button
                    .show(ctx, |ui| {
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .stick_to_bottom(true)
                            .show(ui, |ui| {
                                // --- START OF FIX ---
                                // Read directly from user.logs, which is always up to date
                                if user.logs.is_empty() {
                                    ui.label("No logs available");
                                } else {
                                    let all_logs = user.logs.join("\n");
                                    // Use a read-only TextEdit to display the live logs
                                    ui.add(
                                        egui::TextEdit::multiline(&mut all_logs.as_str())
                                            .font(egui::TextStyle::Monospace)
                                            .desired_width(f32::INFINITY),
                                    );
                                }
                                // --- END OF FIX ---
                            });
                    });

                if !is_open {
                    self.show_logs_modal = false;
                    self.selected_user_index = None;
                }
            } else {
                // If user index is somehow invalid, close the modal
                self.show_logs_modal = false;
                self.selected_user_index = None;
            }
        }
    }
    // In impl WebbookBot

    // Improved transfer modal with better UI (matches Go version)
    // In impl WebbookBot

    fn show_transfer_modal(&mut self, ctx: &egui::Context) {
        let mut close_modal = false;
        let mut transfer_details: Option<(usize, usize)> = None;

        egui::Window::new("Transfer Seats")
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                // --- CHANGE: Create one list of ALL users ---
                let all_users: Vec<(usize, &User)> = self.users.iter().enumerate().collect();

                // --- REMOVED the old separate 'star_users' and 'plus_users' lists ---

                // "From" user selection now uses the full list
                ui.horizontal(|ui| {
                    ui.label("From user:");
                    egui::ComboBox::from_id_source("from_user")
                        .selected_text(&self.transfer_from_user)
                        .width(350.0)
                        .show_ui(ui, |ui| {
                            for (idx, user) in &all_users {
                                let display_text =
                                    format!("{} (has {} seats)", user.email, user.held_seats.len());
                                if ui
                                    .selectable_label(
                                        self.transfer_from_user == user.email,
                                        &display_text,
                                    )
                                    .clicked()
                                {
                                    self.transfer_from_user = user.email.clone();
                                }
                            }
                        });
                });

                // "To" user selection also uses the full list
                ui.horizontal(|ui| {
                    ui.label("To user:");
                    egui::ComboBox::from_id_source("to_user")
                        .selected_text(&self.transfer_to_user)
                        .width(350.0)
                        .show_ui(ui, |ui| {
                            for (idx, user) in &all_users {
                                // --- CHANGE: Use the user's individual max_seats limit ---
                                let max_seats_display = if user.max_seats == 0 {
                                    "∞".to_string()
                                } else {
                                    user.max_seats.to_string()
                                };
                                let display_text = format!(
                                    "{} (has {}/{} seats)",
                                    user.email,
                                    user.held_seats.len(),
                                    max_seats_display
                                );
                                if ui
                                    .selectable_label(
                                        self.transfer_to_user == user.email,
                                        &display_text,
                                    )
                                    .clicked()
                                {
                                    self.transfer_to_user = user.email.clone();
                                }
                            }
                        });
                });

                ui.separator();

                // Transfer preview logic
                if !self.transfer_from_user.is_empty() && !self.transfer_to_user.is_empty() {
                    if let (Some(from_user), Some(to_user)) = (
                        self.users
                            .iter()
                            .find(|u| u.email == self.transfer_from_user),
                        self.users.iter().find(|u| u.email == self.transfer_to_user),
                    ) {
                        // --- CHANGE: Use the receiving user's actual max_seats ---
                        let to_user_limit = to_user.max_seats;
                        let can_receive = if to_user_limit > 0 {
                            to_user_limit.saturating_sub(to_user.held_seats.len())
                        } else {
                            from_user.held_seats.len() // Can receive all if limit is 0 (infinite)
                        };
                        let will_transfer = std::cmp::min(from_user.held_seats.len(), can_receive);

                        ui.label(format!("Will transfer {} seats", will_transfer));
                        if will_transfer == 0 {
                            ui.colored_label(egui::Color32::RED, "⚠️ No seats can be transferred!");
                        }
                    }
                }

                // Button logic (remains the same)
                ui.horizontal(|ui| {
                    let can_transfer = !self.transfer_from_user.is_empty()
                        && !self.transfer_to_user.is_empty()
                        && self.transfer_from_user != self.transfer_to_user;

                    if ui
                        .add_enabled(can_transfer, egui::Button::new("Transfer"))
                        .clicked()
                    {
                        let from_index = self
                            .users
                            .iter()
                            .position(|u| u.email == self.transfer_from_user);
                        let to_index = self
                            .users
                            .iter()
                            .position(|u| u.email == self.transfer_to_user);

                        if let (Some(from_idx), Some(to_idx)) = (from_index, to_index) {
                            transfer_details = Some((from_idx, to_idx));
                        }
                        close_modal = true;
                    }

                    if ui.button("Cancel").clicked() {
                        close_modal = true;
                    }
                });
            });

        if let Some((from_idx, to_idx)) = transfer_details {
            self.execute_ultra_fast_transfer(from_idx, to_idx);
        }

        if close_modal {
            self.show_transfer_modal = false;
            self.transfer_from_user.clear();
            self.transfer_to_user.clear();
        }
    }
    // NEW: Pre-build take request (like Go's prepareTakeSeatRequest)
    async fn prepare_take_request_for_transfer(
        to_user: &BotUser,
        seat_id: &str,
    ) -> Result<PreparedTransferRequest> {
        let hold_token = to_user
            .webook_hold_token
            .lock()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("No hold token available"))?;

        let shared_data = to_user.shared_data.read().await;

        // Build channel keys the same way as take_single_seat
        let mut final_channel_keys = vec!["NO_CHANNEL".to_string()];

        if let Some(common_keys) = &shared_data.channel_key_common {
            final_channel_keys.extend(common_keys.clone());
        }

        if let Some(favorite_team) = &shared_data.favorite_team {
            if let Some(channel_key_obj) = &shared_data.channel_key {
                let team_to_use = match favorite_team.as_str() {
                    "home" => &shared_data.home_team,
                    "away" => &shared_data.away_team,
                    _ => &shared_data.home_team,
                };

                if let Some(team) = team_to_use {
                    if let Some(team_id) = team.get("_id").and_then(|v| v.as_str()) {
                        if let Some(team_keys) =
                            channel_key_obj.get(team_id).and_then(|v| v.as_array())
                        {
                            for key in team_keys {
                                if let Some(key_str) = key.as_str() {
                                    final_channel_keys.push(key_str.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }

        let event_key = shared_data
            .event_key
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Event key not available"))?;

        let request_body = json!({
            "events": [event_key],
            "holdToken": hold_token,
            "objects": [{"objectId": seat_id}],
            "channelKeys": final_channel_keys,
            "validateEventsLinkedToSameChart": true
        });

        let body_str = request_body.to_string();
        let signature = to_user.generate_signature(&body_str).await?;

        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("accept", "*/*".parse()?);
        headers.insert("content-type", "application/json".parse()?);
        headers.insert(
            "user-agent",
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36".parse()?,
        );
        headers.insert("x-client-tool", "Renderer".parse()?);
        headers.insert("x-signature", signature.parse()?);

        if let Some(browser_id) = to_user.browser_id.lock().as_ref() {
            headers.insert("x-browser-id", browser_id.parse()?);
        }

        if let Some(commit_hash) = &shared_data.commit_hash {
            let referer = format!(
                "https://cdn-eu.seatsio.net/static/version/seatsio-ui-prod-00384-f7t/chart-renderer/chartRendererIframe.html?environment=PROD&commit_hash={}",
                commit_hash
            );
            headers.insert("referer", referer.parse()?);
        }

        Ok(PreparedTransferRequest {
            url: format!(
                "https://cdn-eu.seatsio.net/system/public/{}/events/groups/actions/hold-objects",
                WORKSPACE_KEY
            ),
            headers,
            body: body_str,
            client: to_user.client.clone(),
        })
    }

    // REPLACE execute_ultra_fast_transfer with this version that properly uses ignore list:
    fn execute_ultra_fast_transfer(&mut self, from_user_index: usize, to_user_index: usize) {
        let assigned_seats_sender = match &self.assigned_seats_sender {
            Some(sender) => sender.clone(),
            None => {
                println!("Cannot transfer: assigned_seats_sender is not available.");
                return;
            }
        };

        let log_sender = match &self.log_sender {
            Some(sender) => sender.clone(),
            None => {
                println!("Cannot transfer: log_sender is not available.");
                return;
            }
        };

        let telegram_sender = match &self.telegram_sender {
            Some(sender) => sender.clone(),
            None => {
                println!("Cannot transfer: telegram_sender is not available.");
                return;
            }
        };

        if let (Some(bot_manager), Some(rt)) = (&self.bot_manager, &self.rt) {
            let from_user = bot_manager.users[from_user_index].clone();
            let to_user = bot_manager.users[to_user_index].clone();
            let seats_to_transfer = from_user.held_seats.lock().clone();

            let to_user_seat_limit = self.users[to_user_index].max_seats;

            let from_user_email = from_user.email.clone();
            let to_user_email = to_user.email.clone();
            /*let d_seats_limit: usize = self.d_seats.parse().unwrap_or(0);
            let from_user_email = from_user.email.clone();
            let to_user_email = to_user.email.clone();*/

            let notify_sender = self.telegram_sender.clone();

            if seats_to_transfer.is_empty() {
                log_sender
                    .send((from_user_index, "❌ No seats to transfer".to_string()))
                    .ok();
                return;
            }

            // ADD SEATS TO IGNORE LIST BEFORE STARTING TRANSFER (CRITICAL FIX)
            let ignore_list = self.transfer_ignore_list.clone();
            {
                let mut list = ignore_list.lock();
                for seat in &seats_to_transfer {
                    list.insert(seat.clone());
                }
            }
            log_sender
                .send((
                    from_user_index,
                    format!(
                        "🚫 Added {} seats to scanner ignore list",
                        seats_to_transfer.len()
                    ),
                ))
                .ok();

            log_sender
                .send((
                    from_user_index,
                    "🔄 Starting ULTRA-FAST seat transfer...".to_string(),
                ))
                .ok();
            log_sender
                .send((to_user_index, "🔄 Receiving seats...".to_string()))
                .ok();

            rt.spawn(async move {
                log_sender
                    .send((
                        to_user_index,
                        "⚡ Starting atomic transfers (no pre-building needed)...".to_string(),
                    ))
                    .ok();

                // DO ATOMIC TRANSFERS - build requests fresh each time
                let mut successfully_transferred = 0;
                for seat_id in seats_to_transfer {
                    // Check seat limit
                    if to_user_seat_limit > 0
                        && to_user.held_seats.lock().len() >= to_user_seat_limit
                    {
                        log_sender
                            .send((
                                to_user_index,
                                format!(
                                    "⚠️ Reached limit of {} seats, stopping",
                                    to_user_seat_limit
                                ),
                            ))
                            .ok();
                        break;
                    }

                    // Build fresh request after release (with delay fix)
                    match Self::atomic_transfer_seat_ultra_fast(
                        &from_user,
                        &to_user,
                        &seat_id,
                        PreparedTransferRequest {
                            // dummy
                            url: String::new(),
                            headers: reqwest::header::HeaderMap::new(),
                            body: String::new(),
                            client: to_user.client.clone(),
                        },
                        &assigned_seats_sender,
                        &log_sender,
                        from_user_index,
                        to_user_index,
                    )
                    .await
                    {
                        Ok(true) => {
                            successfully_transferred += 1;
                            if let Some(sender) = notify_sender.clone() {
                                let final_seat_count = to_user.held_seats.lock().len();
                                let message = format!(
                                    "🔄 SEAT TRANSFER\n\
                                    ━━━━━━━━━━━━━━━━━\n\
                                    From: {}\n\
                                    To: {}\n\
                                    Seats Transferred: {}\n\
                                    Target Total: {}\n\
                                    ━━━━━━━━━━━━━━━━━",
                                    from_user_email,
                                    to_user_email,
                                    successfully_transferred,
                                    final_seat_count
                                );
                                sender
                                    .send(TelegramMessage {
                                        text: message,
                                        chat_id: 4940392737, // Transfer operations group
                                    })
                                    .ok();
                            }
                        }
                        Ok(false) | Err(_) => {
                            log_sender
                                .send((
                                    from_user_index,
                                    format!("❌ Transfer failed for {}, stopping", seat_id),
                                ))
                                .ok();
                            break;
                        }
                    }
                }

                // CLEAR IGNORE LIST AFTER TRANSFER COMPLETES (CRITICAL)
                ignore_list.lock().clear();
                log_sender
                    .send((
                        from_user_index,
                        "🚫 Cleared scanner ignore list".to_string(),
                    ))
                    .ok();

                // Final notification
                log_sender
                    .send((
                        from_user_index,
                        format!(
                            "📊 Ultra-fast transfer result: {} seats transferred",
                            successfully_transferred
                        ),
                    ))
                    .ok();
                let message = format!(
                    "🔄 SEAT TRANSFER COMPLETE\n\
                    ——————————————————\n\
                    From: {}\n\
                    To: {}\n\
                    Seats Transferred: {}\n\
                    Target Total: {}\n\
                    ——————————————————",
                    from_user.email,
                    to_user.email,
                    successfully_transferred,
                    to_user.held_seats.lock().len(),
                );
                telegram_sender
                    .send(TelegramMessage {
                        text: message,
                        chat_id: 4940392737,
                    })
                    .ok();
            });
        }
    }

    // ALSO UPDATE the atomic_transfer_seat_ultra_fast to have the 50ms delay fix:
    async fn atomic_transfer_seat_ultra_fast(
        from_user: &BotUser,
        to_user: &BotUser,
        seat_id: &str,
        _prepared_request: PreparedTransferRequest, // We'll rebuild this
        assigned_seats_sender: &mpsc::UnboundedSender<(usize, String)>,
        log_sender: &mpsc::UnboundedSender<(usize, String)>,
        from_user_index: usize,
        to_user_index: usize,
    ) -> Result<bool> {
        let event_keys = if let Ok(data) = from_user.shared_data.try_read() {
            if let Some(event_key) = &data.event_key {
                vec![event_key.clone()]
            } else {
                return Err(anyhow::anyhow!("Event key not found"));
            }
        } else {
            return Err(anyhow::anyhow!("Could not read shared data"));
        };

        // Step 1: Release the seat
        log_sender
            .send((from_user_index, format!("🔄 Releasing seat: {}", seat_id)))
            .ok();
        if !from_user.release_seat(seat_id, &event_keys).await? {
            return Err(anyhow::anyhow!("Failed to release seat"));
        }

        // Step 2: Small delay to let server process the release (CRITICAL FIX)
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Step 3: Build fresh take request AFTER release (not pre-built)
        log_sender
            .send((to_user_index, format!("⚡ Taking seat: {}", seat_id)))
            .ok();

        let fresh_request = match Self::prepare_take_request_for_transfer(to_user, seat_id).await {
            Ok(req) => req,
            Err(e) => {
                log_sender
                    .send((
                        to_user_index,
                        format!("❌ Failed to prepare fresh request: {}", e),
                    ))
                    .ok();
                return Err(e);
            }
        };

        let response = fresh_request
            .client
            .post(&fresh_request.url)
            .headers(fresh_request.headers)
            .body(fresh_request.body)
            .send()
            .await?;

        if response.status().is_success() {
            // Success - update seat tracking
            to_user.held_seats.lock().push(seat_id.to_string());

            // Update UI
            assigned_seats_sender
                .send((from_user_index, format!("REMOVE:{}", seat_id)))
                .ok();
            assigned_seats_sender
                .send((to_user_index, seat_id.to_string()))
                .ok();

            log_sender
                .send((to_user_index, format!("✅ ATOMIC SUCCESS: {}", seat_id)))
                .ok();
            return Ok(true);
        } else {
            let error_text = response.text().await.unwrap_or_default();
            log_sender
                .send((
                    to_user_index,
                    format!("❌ Take failed: {} - trying to restore", error_text),
                ))
                .ok();

            // Try to restore the seat to original user
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await; // Give server time

            if let Ok(restore_req) =
                Self::prepare_take_request_for_transfer(from_user, seat_id).await
            {
                let restore_response = restore_req
                    .client
                    .post(&restore_req.url)
                    .headers(restore_req.headers)
                    .body(restore_req.body)
                    .send()
                    .await;

                match restore_response {
                    Ok(resp) if resp.status().is_success() => {
                        from_user.held_seats.lock().push(seat_id.to_string());
                        assigned_seats_sender
                            .send((from_user_index, seat_id.to_string()))
                            .ok();
                        log_sender
                            .send((from_user_index, format!("✅ Restored: {}", seat_id)))
                            .ok();
                    }
                    _ => {
                        log_sender
                            .send((
                                from_user_index,
                                format!("⚠️ FAILED TO RESTORE: {}", seat_id),
                            ))
                            .ok();
                    }
                }
            }

            return Err(anyhow::anyhow!("Transfer failed"));
        }
    }

    fn build_channel_keys(&self) -> Vec<String> {
        let mut channel_keys = vec!["NO_CHANNEL".to_string()];

        if let Some(shared_data_arc) = &self.shared_data {
            if let Ok(shared_data) = shared_data_arc.try_read() {
                // Add common channel keys
                if let Some(common_keys) = &shared_data.channel_key_common {
                    channel_keys.extend(common_keys.clone());
                }

                // Add team-specific keys based on favorite team
                if let Some(channel_key_obj) = &shared_data.channel_key {
                    let team_to_use = match self.favorite_team.as_str() {
                        "home" => &shared_data.home_team,
                        "away" => &shared_data.away_team,
                        _ => &shared_data.home_team, // Default to home
                    };

                    if let Some(team) = team_to_use {
                        if let Some(team_id) = team.get("_id").and_then(|v| v.as_str()) {
                            if let Some(team_keys) =
                                channel_key_obj.get(team_id).and_then(|v| v.as_array())
                            {
                                for key in team_keys {
                                    if let Some(key_str) = key.as_str() {
                                        channel_keys.push(key_str.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        channel_keys
    }
    // In impl WebbookBot
    fn fill_seats(&mut self) {
        if !self.bot_running {
            println!("Please start the bot first");
            return;
        }

        if self.bot_manager.is_none() {
            println!("Bot manager not ready yet, please wait a few seconds and try again");
            return;
        }

        if let Some(bot_manager) = &self.bot_manager {
            let channel_keys = self.build_channel_keys();
            let event_keys = if let Some(shared_data_arc) = &self.shared_data {
                if let Ok(shared_data) = shared_data_arc.try_read() {
                    if let Some(event_key) = &shared_data.event_key {
                        vec![event_key.clone()]
                    } else {
                        println!("Event key not available");
                        return;
                    }
                } else {
                    return;
                }
            } else {
                return;
            };

            let assigned_seats_sender = match &self.assigned_seats_sender {
                Some(sender) => sender.clone(),
                None => {
                    println!("Error: assigned_seats_sender not initialized.");
                    return;
                }
            };
            let log_sender = match &self.log_sender {
                Some(sender) => sender.clone(),
                None => return,
            };

            if let Some(rt) = &self.rt {
                for i in 0..self.users.len() {
                    let user = &self.users[i];
                    // Located inside: impl WebbookBot -> fn fill_seats
                    // ... inside the for loop ...
                    if user.user_type == "*" && !user.target_seats.is_empty() {
                        if let Some(bot_user) = bot_manager.users.get(i) {
                            // --- START FIX: Respect user's seat limit ---
                            let current_held_count = bot_user.held_seats.lock().len();
                            let max_seats = user.max_seats;
                            
                            let seats_to_take = if max_seats > 0 {
                                max_seats.saturating_sub(current_held_count)
                            } else {
                                user.target_seats.len() // Unlimited seats, take all assigned
                            };

                            if seats_to_take == 0 {
                                continue; // Skip if user is already at their limit
                            }

                            // Take only the number of seats needed to reach the limit
                            let target_seats: Vec<String> = user.target_seats.iter().take(seats_to_take).cloned().collect();
                            // --- END FIX ---

                            let bot_user_clone = bot_user.clone();
                            let channel_keys_clone = channel_keys.clone();
                            let event_keys_clone = event_keys.clone();
                            let assigned_sender = assigned_seats_sender.clone();
                            let log_sender_clone = log_sender.clone();

                            rt.spawn(async move {
                                if let Err(e) = bot_user_clone
                                    .fill_assigned_seats(
                                        target_seats, // Use the new limited list
                                        channel_keys_clone,
                                        event_keys_clone,
                                        mpsc::unbounded_channel().0,
                                        assigned_sender,
                                        log_sender_clone,
                                        i,
                                    )
                                    .await
                                {
                                    println!(
                                        "Error in fill_assigned_seats for user {}: {}",
                                        bot_user_clone.email, e
                                    );
                                }
                            });
                        }
                    }
                }
            }
        }
    }

    // In impl WebbookBot
    fn get_seats_from_selected_sections(&self) -> Vec<String> {
        let mut all_seats = Vec::new();
        if let Some(shared_data_arc) = &self.shared_data {
            if let Ok(shared_data) = shared_data_arc.try_read() {
                if let Some(channels) = &shared_data.channels {
                    let selected_sections: Vec<String> = self
                        .selected_sections
                        .iter()
                        .filter_map(|(section, &is_selected)| {
                            if is_selected {
                                Some(section.clone())
                            } else {
                                None
                            }
                        })
                        .collect();

                    for seat in channels {
                        for selected_section in &selected_sections {
                            if seat.starts_with(selected_section) {
                                if seat.len() == selected_section.len()
                                    || seat
                                        .chars()
                                        .nth(selected_section.len())
                                        .map_or(false, |c| !c.is_ascii_digit())
                                {
                                    all_seats.push(seat.clone());
                                }
                            }
                        }
                    }
                }
            }
        }
        all_seats
    }

    // In impl WebbookBot

    // In impl WebbookBot
    fn distribute_seats_to_users(&mut self) {
        let available_seats = self.get_seats_from_selected_sections();
        let total_seats = available_seats.len();

        // --- MODIFICATION: Removed the "user.status != 'Waiting'" check ---
        let star_users: Vec<(usize, &mut User)> = self
            .users
            .iter_mut()
            .enumerate()
            .filter(|(_, user)| user.user_type == "*")
            .collect();
        // --- END MODIFICATION ---

        let user_count = star_users.len();

        if total_seats == 0 || user_count == 0 {
            println!("No seats available or no '*' users for distribution.");
            for user in self.users.iter_mut() {
                user.target_seats.clear();
            }
            return;
        }

        let seats_per_user: usize = self.seats.parse().unwrap_or(1);
        let actual_seats_per_user = if total_seats >= user_count * seats_per_user {
            seats_per_user
        } else {
            total_seats / user_count
        };

        println!(
            "Distributing {} seats among {} '*' users. ({} seats each)",
            total_seats, user_count, actual_seats_per_user
        );

        for (user_index, (_, user)) in star_users.into_iter().enumerate() {
            let start_index = user_index * actual_seats_per_user;
            let end_index =
                std::cmp::min(start_index + actual_seats_per_user, available_seats.len());

            if start_index < available_seats.len() {
                let target_seats: Vec<String> = available_seats[start_index..end_index].to_vec();
                user.target_seats = target_seats;
            } else {
                user.target_seats.clear();
            }
        }

        // Clear target seats for '+' users
        for user in self.users.iter_mut() {
            if user.user_type == "+" {
                user.target_seats.clear();
            }
        }
    }
    fn receive_bot_manager(&mut self) {
        if let Some(receiver) = &mut self.bot_manager_receiver {
            if let Ok(bot_manager_arc) = receiver.try_recv() {
                self.bot_manager = Some(bot_manager_arc);
                println!("Bot manager received and stored!");
            }
        }
    }
    fn update_countdowns(&mut self) {
        // Receive expire time updates from the async task
        if let Some(receiver) = &mut self.expire_receiver {
            while let Ok((user_index, expire_time)) = receiver.try_recv() {
                if user_index < self.users.len() {
                    self.users[user_index].expire = expire_time.to_string();

                    // ADD EXPIRY WARNING NOTIFICATION (like Go version)
                    if expire_time > 0 && expire_time < 100 {
                        // Check if we haven't already sent warning for this user
                        if !self.expiry_warnings_sent.get(&user_index).unwrap_or(&false) {
                            // Send expiry warning notification
                            if let Some(telegram_sender) = &self.telegram_sender {
                                let user = &self.users[user_index];
                                let message = format!(
                                    "⚠️ EXPIRY WARNING\n\
                                    ━━━━━━━━━━━━━━━━━\n\
                                    🔧 Account: {}\n\
                                    💺 Seats Held: {}\n\
                                    ⏰ Time Left: <100 seconds\n\
                                    ━━━━━━━━━━━━━━━━━",
                                    user.email,
                                    user.held_seats.len()
                                );
                                telegram_sender
                                    .send(TelegramMessage {
                                        text: message,
                                        chat_id: 4639502335, // Expiry group
                                    })
                                    .ok();
                            }

                            // Mark warning as sent for this user
                            self.expiry_warnings_sent.insert(user_index, true);
                        }
                    } else if expire_time >= 100 {
                        // Reset warning flag when token gets refreshed
                        self.expiry_warnings_sent.insert(user_index, false);
                    }

                    // Update status based on expire time
                    if expire_time == 0 {
                        self.users[user_index].status = "Expired".to_string();
                        // Reset warning flag on expiry
                        self.expiry_warnings_sent.insert(user_index, false);
                    } else if expire_time < 30 {
                        self.users[user_index].status = "Expiring Soon".to_string();
                    } else {
                        self.users[user_index].status = "Active".to_string();
                    }
                }
            }
        }
    }
    // In impl WebbookBot
    fn update_assigned_seats(&mut self) {
        if let Some(receiver) = &mut self.assigned_seats_receiver {
            while let Ok((user_index, seat_msg)) = receiver.try_recv() {
                if user_index < self.users.len() {
                    if seat_msg.is_empty() {
                        // Clear all seats (for token expiration)
                        self.users[user_index].held_seats.clear();
                    } else if seat_msg == "CLEAR_UNPAID" {
                        // NEW: Clear only unpaid seats, keep paid ones
                        let user = &mut self.users[user_index];
                        user.held_seats.retain(|seat| user.paid_seats.contains(seat));
                    } else if let Some(seat_to_remove) = seat_msg.strip_prefix("REMOVE:") {
                        // Remove a single seat (for transfers)
                        self.users[user_index]
                            .held_seats
                            .retain(|s| s != seat_to_remove);
                    } else {
                        // Add a single seat (for normal takes)
                        self.users[user_index].held_seats.push(seat_msg);
                    }
                    // Update the display count string
                    self.users[user_index].seats =
                        self.users[user_index].held_seats.len().to_string();
                }
            }
        }
    }

    fn load_users_csv(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("CSV files", &["csv"])
            .pick_file()
        {
            match fs::read_to_string(&path) {
                Ok(contents) => {
                    let mut reader = csv::Reader::from_reader(contents.as_bytes());
                    let mut new_users = Vec::new();

                    for result in reader.deserialize() {
                        match result {
                            Ok(mut user) => {
                                let mut user: User = user;
                                // Assign proxy only if proxies are available
                                user.proxy = if !self.proxies.is_empty() {
                                    self.proxies[new_users.len() % self.proxies.len()].clone()
                                } else {
                                    "No proxy".to_string() // Default when no proxies
                                };
                                user.status = "Ready".to_string();
                                user.expire = "0".to_string();
                                user.seats = "0".to_string();
                                user.last_update = "10:37:47".to_string();
                                user.pay_status = "Pay".to_string();
                                user.payment_completed = false;
                                user.selected = false;
                                new_users.push(user);
                            }
                            Err(e) => println!("Error parsing user: {}", e),
                        }
                    }

                    self.users = new_users;
                    println!("Loaded {} users from CSV", self.users.len());
                }
                Err(e) => println!("Error reading file: {}", e),
            }
        }
    }

    fn load_proxies(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Text files", &["txt"])
            .pick_file()
        {
            match fs::read_to_string(&path) {
                Ok(contents) => {
                    self.proxies = contents
                        .lines()
                        .filter(|line| !line.trim().is_empty())
                        .map(|line| line.trim().to_string())
                        .collect();

                    // Update user proxies only if proxies are loaded
                    if !self.proxies.is_empty() {
                        for (i, user) in self.users.iter_mut().enumerate() {
                            user.proxy = self.proxies[i % self.proxies.len()].clone();
                        }
                    }

                    println!("Loaded {} proxies", self.proxies.len());
                }
                Err(e) => println!("Error reading proxies file: {}", e),
            }
        }
    }

    // In impl WebbookBot
    fn start_bot(&mut self) {
        if self.users.is_empty() {
            println!("Please load users first");
            return;
        }
        /*if let Err(e) = self.start_node_process() {
            println!("Failed to start Node.js server: {}", e);
            return;
        }

        std::thread::sleep(std::time::Duration::from_secs(2));*/

        let star_user_limit: usize = self.seats.parse().unwrap_or(0);
        let plus_user_limit: usize = self.d_seats.parse().unwrap_or(0);
        for user in self.users.iter_mut() {
            if user.user_type == "*" {
                user.max_seats = star_user_limit;
            } else if user.user_type == "+" {
                user.max_seats = plus_user_limit;
            }
        }

        // --- MODIFICATION: Activate ALL '*' and '+' users immediately ---
        let users_to_activate_initially: Vec<usize> = self
            .users
            .iter()
            .enumerate()
            .filter(|(_, user)| user.user_type == "*" || user.user_type == "+")
            .map(|(i, _)| i)
            .collect();
        // --- END MODIFICATION ---

        let users_data: Vec<(String, Option<String>)> = self
            .users
            .iter()
            .map(|u| {
                let proxy = if u.proxy == "No proxy" || u.proxy.is_empty() {
                    None
                } else {
                    Some(u.proxy.clone())
                };
                (u.email.clone(), proxy)
            })
            .collect();

        let event_url = self.event_url.clone();
        let (assigned_seats_sender, assigned_seats_receiver) = mpsc::unbounded_channel();
        self.assigned_seats_sender = Some(assigned_seats_sender);
        self.assigned_seats_receiver = Some(assigned_seats_receiver);
        let (status_sender, status_receiver) = mpsc::unbounded_channel();
        let (expire_sender, expire_receiver) = mpsc::unbounded_channel::<(usize, usize)>();
        self.status_receiver = Some(status_receiver);
        self.expire_receiver = Some(expire_receiver);

        if let Some(rt) = &self.rt {
            let assigned_seats_sender_for_task =
                self.assigned_seats_sender.as_ref().unwrap().clone();
            let log_sender_for_task = self.log_sender.as_ref().unwrap().clone();
            let shared_data = Arc::new(RwLock::new(SharedData::default()));
            self.shared_data = Some(shared_data.clone());
            let favorite_team = self.favorite_team.clone();

            rt.spawn({
                let shared_data = shared_data.clone();
                async move {
                    shared_data.write().await.favorite_team = Some(favorite_team);
                }
            });

            let (bot_sender, bot_receiver) = mpsc::unbounded_channel();
            self.bot_manager_receiver = Some(bot_receiver);
            let channel_keys = self.build_channel_keys();
            let paid_users_clone = self.paid_users.clone();
            let users_data_clone = self.users.clone();
            let paid_seats_query_sender_for_loop = self.paid_seats_query_sender.clone();            rt.spawn(async move {
                match BotManager::new(users_data, event_url.clone(), shared_data).await {
                    Ok(bot_manager) => {
                        let bot_manager_arc = Arc::new(bot_manager);
                        bot_sender.send(bot_manager_arc.clone()).ok();

                        if bot_manager_arc.extract_seatsio_chart_data().await.is_err() {
                            return;
                        }
                        if let Ok(event_id) = extract_event_key(&event_url) {
                            if bot_manager_arc
                                .send_event_detail_request(&event_id)
                                .await
                                .is_err()
                            {
                                return;
                            }
                        } else {
                            return;
                        }
                        if bot_manager_arc.get_rendering_info().await.is_err() {
                            return;
                        }

                        // --- MODIFICATION: Activate all users in the background ---
                        let status_sender_clone = status_sender.clone();
                        let log_sender_clone = log_sender_for_task.clone();
                        let channel_keys_clone = channel_keys.clone();
                        let bot_manager_clone = bot_manager_arc.clone();
                        let users_to_activate_clone = users_to_activate_initially.clone();

                        tokio::spawn(async move {
                            if let Err(e) = bot_manager_clone
                                .start_bot(
                                    &users_to_activate_clone,
                                    status_sender_clone,
                                    log_sender_clone,
                                    channel_keys_clone,
                                )
                                .await
                            {
                                println!("Bot error during initial activation: {}", e);
                            }
                        });
                        // --- END MODIFICATION ---

                        // --- MODIFICATION: Simplified timer loop (removed second batch logic) ---
                        loop {
                            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

                            // Normal countdown and token refresh logic
                            for (i, user) in bot_manager_arc.users.iter().enumerate() {
                                let expire_time = user.expire_time.load(Ordering::Relaxed);
                                if expire_time == usize::MAX {
                                    continue;
                                }

                                let is_paid = paid_users_clone.lock().contains(&i);
                                if is_paid {
                                    continue;
                                }

                                if expire_time > 0 {
                                    let remaining = expire_time.saturating_sub(1);
                                    user.expire_time.store(remaining, Ordering::Relaxed);
                                    expire_sender.send((i, remaining)).ok();
                                } else if expire_time == 0 {
                                    // Token refresh logic (unchanged)
                                    user.expire_time.store(600, Ordering::Relaxed);
                                    let user_clone = user.clone();
                                    let assigned_seats_sender_clone =
                                        assigned_seats_sender_for_task.clone();
                                    let log_sender_clone_task = log_sender_for_task.clone();
                                    let bot_manager_clone = bot_manager_arc.clone();
                                    let channel_keys_clone = channel_keys.clone();
                                    let user_index = i;
                                    let users_data_clone = users_data_clone.clone();

                                    let paid_seats_query_sender_clone = paid_seats_query_sender_for_loop.clone();

                                    tokio::spawn(async move {
                                        // This `async move` block now takes ownership of the NEW clone,
                                        // leaving the original `paid_seats_query_sender_for_loop` untouched for the next loop iteration.
                                        
                                        // ... your "Ask for data" logic will now work correctly ...
                                        let (response_tx, response_rx) = oneshot::channel();
                                        paid_seats_query_sender_clone.send((user_index, response_tx)).ok();

                                        // Wait for the main thread to send back the up-to-date list.
                                        let ui_user_paid_seats = response_rx.await.unwrap_or_else(|_| {
                                            println!("Warning: Failed to receive paid seats list from main thread.");
                                            Vec::new()
                                        });
                                        // --- END ---
                                    
                                        let previously_held = {
                                            let mut held = user_clone.held_seats.lock();
                                            let all_seats = held.clone();
                                            
                                            // CRITICAL FIX: Preserve paid seats during token refresh
                                            if !ui_user_paid_seats.is_empty() {
                                                // Keep only paid seats, clear unpaid ones
                                                let kept_seats: Vec<String> = all_seats.iter()
                                                    .filter(|seat| ui_user_paid_seats.contains(seat))
                                                    .cloned()
                                                    .collect();
                                                
                                                *held = kept_seats.clone();
                                                
                                                log_sender_clone_task
                                                    .send((
                                                        user_index,
                                                        format!("💰 Preserved {} paid seats during refresh", kept_seats.len()),
                                                    ))
                                                    .ok();
                                                
                                                // Return only unpaid seats for potential retaking
                                                all_seats.into_iter()
                                                    .filter(|seat| !ui_user_paid_seats.contains(seat))
                                                    .collect()
                                            } else {
                                                // No paid seats - clear everything (original behavior)
                                                held.clear();
                                                log_sender_clone_task
                                                    .send((
                                                        user_index,
                                                        "🔄 No paid seats - clearing all seats for refresh".to_string(),
                                                    ))
                                                    .ok();
                                                all_seats
                                            }
                                        };
                                    
                                        log_sender_clone_task
                                            .send((
                                                user_index,
                                                format!(
                                                    "⏰ Token expired. Refreshing for {} unpaid seats...",
                                                    previously_held.len()
                                                ),
                                            ))
                                            .ok();
                                    
                                        // Clear unpaid seats from UI (paid seats are preserved)
                                        if previously_held.len() > 0 {
                                            assigned_seats_sender_clone
                                                .send((user_index, "CLEAR_UNPAID".to_string()))
                                                .ok();
                                        }
                                    
                                        if user_clone.get_webook_hold_token().await.is_ok() {
                                            user_clone.expire_time.store(600, Ordering::Relaxed);
                                            log_sender_clone_task
                                                .send((
                                                    user_index,
                                                    "✅ New token acquired.".to_string(),
                                                ))
                                                .ok();
                                    
                                            if let Ok(template) = user_clone
                                                .prepare_request_template(channel_keys_clone.clone())
                                                .await
                                            {
                                                *user_clone.prepared_request.lock() = Some(template);
                                            }
                                    
                                            // Only retake unpaid seats (not paid ones)
                                            if !previously_held.is_empty() {
                                                // --- START FIX: Respect seat limit during retake ---
                                                let ui_user = &users_data_clone[user_index];
                                                let max_seats = ui_user.max_seats;
                                                
                                                let seats_to_retake = if max_seats > 0 {
                                                    // Count how many seats are already held (e.g., paid ones that were preserved)
                                                    let already_held_count = user_clone.held_seats.lock().len();
                                                    let remaining_capacity = max_seats.saturating_sub(already_held_count);
                                                    // Only take enough `previously_held` seats to fill the remaining capacity
                                                    previously_held.into_iter().take(remaining_capacity).collect()
                                                } else {
                                                    previously_held // Unlimited seats, retake all
                                                };
                                                // --- END FIX ---
                                            
                                                if !seats_to_retake.is_empty() {
                                                    let event_keys = if let Some(key) =
                                                        bot_manager_clone
                                                            .shared_data
                                                            .read()
                                                            .await
                                                            .event_key
                                                            .as_ref()
                                                    {
                                                        vec![key.clone()]
                                                    } else {
                                                        return;
                                                    };
                                            
                                                    log_sender_clone_task
                                                        .send((
                                                            user_index,
                                                            format!("🔄 Attempting to retake {} unpaid seats (limit respected)", seats_to_retake.len()),
                                                        ))
                                                        .ok();
                                            
                                                    user_clone
                                                        .retake_seats(
                                                            seats_to_retake, // Use the new limited list
                                                            channel_keys_clone,
                                                            event_keys,
                                                            assigned_seats_sender_clone,
                                                            user_index,
                                                            ui_user.max_seats, // <-- ADD THE MISSING ARGUMENT HERE
                                                        )
                                                        .await
                                                        .ok();
                                                } else {
                                                    log_sender_clone_task
                                                        .send((
                                                            user_index,
                                                            "✅ Token refreshed - user is at seat limit, nothing to retake".to_string(),
                                                        ))
                                                        .ok();
                                                }
                                            } else {
                                                log_sender_clone_task
                                                    .send((
                                                        user_index,
                                                        "✅ Token refreshed - all seats were paid, nothing to retake".to_string(),
                                                    ))
                                                    .ok();
                                            }
                                        } else {
                                            log_sender_clone_task
                                                .send((
                                                    user_index,
                                                    "❌ FAILED to get new token.".to_string(),
                                                ))
                                                .ok();
                                            user_clone.expire_time.store(0, Ordering::Relaxed);
                                        }
                                    });
                                }
                            }
                        }
                    }
                    Err(e) => {
                        println!("Failed to create bot manager: {}", e);
                    }
                }
            });
        }
        self.bot_running = true;
    }
    // Add this new function inside the main `impl WebbookBot` block
    fn handle_paid_seats_queries(&mut self) {
        while let Ok((user_index, response_sender)) = self.paid_seats_query_receiver.try_recv() {
            let paid_seats = self.users.get(user_index)
                .map(|u| u.paid_seats.clone())
                .unwrap_or_default();
            
            // Send the current list of paid seats back to the background task
            let _ = response_sender.send(paid_seats);
        }
    }
    fn render_top_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            // File loading buttons
            if ui.button("Load Users CSV").clicked() {
                self.load_users_csv();
            }

            if ui.button("Load Proxies").clicked() {
                self.load_proxies();
            }
            if ui.button("Save Token").clicked() {
                self.save_tokens();
            }

            ui.separator();

            // Event URL
            ui.label("Event URL:");
            ui.add(egui::TextEdit::singleline(&mut self.event_url).desired_width(300.0));

            ui.separator();

            // Seats
            ui.label("Seats:");
            ui.add(egui::TextEdit::singleline(&mut self.seats).desired_width(40.0));

            ui.label("DSeats:");
            ui.add(egui::TextEdit::singleline(&mut self.d_seats).desired_width(40.0));

            ui.separator();

            // Control buttons
            let bot_button_text = if self.bot_running {
                "Stop Bot"
            } else {
                "Start Bot"
            };
            if ui.button(bot_button_text).clicked() {
                if self.bot_running {
                    self.bot_running = false;
                    self.stop_node_process();
                } else {
                    self.start_bot();
                }
            }

            if ui.button("Fill Seats").clicked() {
                self.fill_seats(); // ← Correct!
            }
            if ui.button("Transfer Seat").clicked() {
                self.show_transfer_modal = true;
            }
        });

        ui.separator();

        ui.separator();

        // Second row
        ui.horizontal(|ui| {
            ui.label("Favorite Team:");
            egui::ComboBox::from_label("")
                .selected_text(&self.favorite_team)
                .show_ui(ui, |ui| {
                    for team in &self.teams.clone() {
                        ui.selectable_value(&mut self.favorite_team, team.clone(), team);
                    }
                });
        });

        ui.separator();

        // In render_top_panel, replace the sections checkbox part:
        ui.vertical(|ui| {
            ui.label("Sections:");
        
            // ADD THIS NEW SECTION - Input field for quick section selection
            ui.horizontal(|ui| {
                ui.label("Quick select:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.section_input)
                        .hint_text("103, 102, W3, ...")
                        .desired_width(200.0)
                );
                if ui.button("Apply").clicked() {
                    self.apply_section_input();
                }
                if ui.button("Clear Input").clicked() {
                    self.section_input.clear();
                }
            });
        
            if let Some(shared_data_arc) = &self.shared_data {
                if let Ok(shared_data) = shared_data_arc.try_read() {
                    if let Some(channels) = &shared_data.channels {
                        // Only update sections when channels data changes
                        if self.selected_sections.is_empty() {
                            let mut unique_sections = std::collections::HashSet::new();
                            for seat in channels {
                                if let Some(prefix) = seat.split('-').next() {
                                    unique_sections.insert(prefix.to_string());
                                }
                            }
        
                            // Initialize checkboxes only once
                            for section in unique_sections {
                                self.selected_sections.insert(section, false);
                            }
                        }
                    }
                }
            }
        
            // Rest of the existing sections UI code...
            let mut sections_to_display: Vec<String> =
                self.selected_sections.keys().cloned().collect();
            sections_to_display.sort();
        
            let mut selection_changed = false;
            ui.horizontal(|ui| {
                if ui.button("Select All").clicked() {
                    for (_, is_selected) in self.selected_sections.iter_mut() {
                        *is_selected = true;
                    }
                    selection_changed = true;
                }
                if ui.button("Clear All").clicked() {
                    for (_, is_selected) in self.selected_sections.iter_mut() {
                        *is_selected = false;
                    }
                    selection_changed = true;
                }
            });
            let selected_count = self.selected_sections.values().filter(|&&v| v).count();
            ui.label(format!("Sections: {} selected", selected_count));
            
            // Create scrollable area with fixed height
            egui::ScrollArea::both()
                .id_source("sections_scroll")
                .max_height(200.0)
                .show(ui, |ui| {
                    // Use a grid for better layout control
                    egui::Grid::new("sections_grid")
                        .num_columns(20)
                        .spacing([5.0, 5.0])
                        .show(ui, |ui| {
                            for (index, section) in sections_to_display.iter().enumerate() {
                                if let Some(is_selected) = self.selected_sections.get_mut(section) {
                                    if ui.checkbox(is_selected, section).changed() {
                                        selection_changed = true;
                                    }
                                }
        
                                // End row after 20 items
                                if (index + 1) % 20 == 0 {
                                    ui.end_row();
                                }
                            }
                        });
                });
        
            // If selection changed, redistribute seats
            if selection_changed {
                println!("selection changed");
        
                // Update shared selections for scanner
                {
                    let mut shared = self.selected_sections_shared.lock();
                    *shared = self.selected_sections.clone();
                }
        
                self.distribute_seats_to_users();
        
                let has_selected = self.selected_sections.values().any(|&selected| selected);
                if has_selected && !self.scanner_running && self.bot_running {
                    self.start_scanner();
                } else if !has_selected && self.scanner_running {
                    self.scanner_running = false;
                }
            }
        });

        ui.separator();
    }
    // RENAME this function to update_ready_scanner_users_atomic
    fn update_ready_scanner_users_atomic(&self) {
        let mut ready_users = self.ready_scanner_users.lock();
        ready_users.clear();
    
        for (i, user) in self.users.iter().enumerate() {
            // Include users that are Active OR just reloaded (pay_status contains "Reloading")
            if (user.user_type == "*" || user.user_type == "+") && 
               (user.status == "Active" || user.pay_status == "Reloading...") {
                ready_users.push(i);
            }
        }
    }

    fn apply_section_input(&mut self) {
        if self.section_input.trim().is_empty() {
            return;
        }
    
        // Parse the input - split by comma and trim whitespace
        let sections_to_select: Vec<String> = self.section_input
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    
        if sections_to_select.is_empty() {
            return;
        }
    
        let mut found_sections = Vec::new();
        let mut not_found = Vec::new();
        let mut selection_changed = false;
    
        // Check each input section against available sections - EXACT MATCH ONLY
        for input_section in sections_to_select {
            let mut found = false;
            
            // Look for EXACT match only (case-insensitive)
            for (section_name, is_selected) in self.selected_sections.iter_mut() {
                if section_name.eq_ignore_ascii_case(&input_section) {
                    if !*is_selected {
                        *is_selected = true;
                        selection_changed = true;
                        found_sections.push(section_name.clone());
                    }
                    found = true;
                    break;
                }
            }
            
            if !found {
                not_found.push(input_section);
            }
        }
    
        // Log results
        if !found_sections.is_empty() {
            println!("✅ Selected sections: {}", found_sections.join(", "));
        }
        if !not_found.is_empty() {
            println!("❌ Sections not found: {}", not_found.join(", "));
        }
    
        // If any sections were changed, trigger the update logic
        if selection_changed {
            // Update shared selections for scanner
            {
                let mut shared = self.selected_sections_shared.lock();
                *shared = self.selected_sections.clone();
            }
    
            self.distribute_seats_to_users();
    
            let has_selected = self.selected_sections.values().any(|&selected| selected);
            if has_selected && !self.scanner_running && self.bot_running {
                self.start_scanner();
            } else if !has_selected && self.scanner_running {
                self.scanner_running = false;
            }
        }
    
        // Clear the input after applying
        self.section_input.clear();
    }
    fn render_user_table(&mut self, ui: &mut egui::Ui) {
        use egui_extras::{Column, TableBuilder};
    
        let mut reload_button_clicked_for: Option<usize> = None;
        let mut pay2_button_clicked_for: Option<usize> = None;
        let mut logs_button_clicked_for: Option<usize> = None;
        let mut toggle_selection_for: Option<usize> = None;
        let mut delete_button_clicked_for: Option<usize> = None; // Add this line
    
        let available_width = ui.available_width();
        let delete_width = available_width * 0.04; // Add this line
        let checkbox_width = available_width * 0.04;
        let email_width = available_width * 0.22;
        let type_width = available_width * 0.05;
        let proxy_width = available_width * 0.18;
        let status_width = available_width * 0.10;
        let expire_width = available_width * 0.07;
        let seats_width = available_width * 0.07;
        let pay_width = available_width * 0.07;
        let pay2_width = available_width * 0.07;
        let payment_status_width = available_width * 0.09;
        let table_height = ui.available_height();
    
        TableBuilder::new(ui)
            .striped(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .max_scroll_height(table_height)
            .column(Column::exact(delete_width)) // Add this column first
            .column(Column::exact(email_width))
            .column(Column::exact(type_width))
            .column(Column::exact(proxy_width))
            .column(Column::exact(status_width))
            .column(Column::exact(expire_width))
            .column(Column::exact(seats_width))
            .column(Column::exact(checkbox_width))
            .column(Column::exact(pay_width))
            .column(Column::exact(pay2_width))
            .column(Column::exact(payment_status_width))
            .header(25.0, |mut header| {
                header.col(|ui| {
                    ui.heading("🗑"); // Add a header for the delete column
                });
                // Header code remains the same
                header.col(|ui| {
                    ui.heading("Email");
                });
                header.col(|ui| {
                    ui.heading("Type");
                });
                header.col(|ui| {
                    ui.heading("Proxy");
                });
                header.col(|ui| {
                    ui.heading("Status");
                });
                header.col(|ui| {
                    ui.heading("Expire");
                });
                header.col(|ui| {
                    ui.heading("Seats");
                });
                header.col(|ui| {
                    let selectable_users: Vec<_> = self.users.iter()
                        .filter(|user| !self.has_user_paid_all_seats(user))
                        .collect();
                    
                    let all_selectable_selected = selectable_users.iter()
                        .all(|user| user.selected);
                    
                    let mut select_all_state = all_selectable_selected && !selectable_users.is_empty();
                    
                    if ui.checkbox(&mut select_all_state, "").changed() {
                        let users_paid_all: Vec<bool> = self.users.iter()
                            .map(|user| self.has_user_paid_all_seats(user))
                            .collect();
                        
                        for (user, &has_paid_all) in self.users.iter_mut().zip(users_paid_all.iter()) {
                            if !has_paid_all {
                                user.selected = select_all_state;
                            }
                        }
                        self.select_all_users = select_all_state;
                    }
                });
                header.col(|ui| {
                    ui.heading("Reload");
                });
                header.col(|ui| {
                    ui.heading("Pay2");
                });
                header.col(|ui| {
                    ui.heading("Payment");
                });
            })
            .body(|body| {
                let users = self.users.clone();
                body.rows(25.0, users.len(), |user_index, mut row| {
                    let user = &users[user_index];
                    row.col(|ui| {
                        if ui.button("🗑️").on_hover_text("Delete user").clicked() {
                            delete_button_clicked_for = Some(user_index);
                        }
                    });
                    // Email column
                    row.col(|ui| {
                        // Create a proper hover-sensitive area
                        let rect = ui.available_rect_before_wrap();
                        let response = ui.interact(rect, ui.id().with("hover_email").with(user_index), egui::Sense::hover());
                        
                        if response.hovered() {
                            ui.painter().rect_filled(
                                rect,
                                egui::Rounding::same(2.0),
                                egui::Color32::from_rgba_unmultiplied(255, 140, 0, 25), // Orange hover
                            );
                        }
                        
                        if ui.button(&user.email).clicked() {
                            logs_button_clicked_for = Some(user_index);
                        }
                    });
    
                    // Type column
                    row.col(|ui| {
                        let rect = ui.available_rect_before_wrap();
                        let response = ui.interact(rect, ui.id().with("hover_type").with(user_index), egui::Sense::hover());
                        
                        if response.hovered() {
                            ui.painter().rect_filled(
                                rect,
                                egui::Rounding::same(2.0),
                                egui::Color32::from_rgba_unmultiplied(255, 140, 0, 25), // Orange hover
                            );
                        }
                        
                        ui.label(&user.user_type);
                    });
    
                    // Proxy column
                    row.col(|ui| {
                        let rect = ui.available_rect_before_wrap();
                        let response = ui.interact(rect, ui.id().with("hover_proxy").with(user_index), egui::Sense::hover());
                        
                        if response.hovered() {
                            ui.painter().rect_filled(
                                rect,
                                egui::Rounding::same(2.0),
                                egui::Color32::from_rgba_unmultiplied(255, 140, 0, 25), // Orange hover
                            );
                        }
                        
                        if user.proxy == "No proxy" || user.proxy.is_empty() {
                            ui.colored_label(egui::Color32::LIGHT_GRAY, "No proxy");
                        } else {
                            ui.label(&user.proxy);
                        }
                    });
    
                    // Status column
                    row.col(|ui| {
                        let rect = ui.available_rect_before_wrap();
                        let response = ui.interact(rect, ui.id().with("hover_status").with(user_index), egui::Sense::hover());
                        
                        if response.hovered() {
                            ui.painter().rect_filled(
                                rect,
                                egui::Rounding::same(2.0),
                                egui::Color32::from_rgba_unmultiplied(255, 140, 0, 25), // Orange hover
                            );
                        }
                        
                        let color = match user.status.as_str() {
                            "Active" => egui::Color32::GREEN,
                            "Expired" => egui::Color32::RED,
                            "Waiting" => egui::Color32::KHAKI,
                            "Paid" => egui::Color32::BLUE,
                            _ => egui::Color32::GRAY,
                        };
                        ui.colored_label(color, &user.status);
                    });
    
                    // Expire column
                    row.col(|ui| {
                        let rect = ui.available_rect_before_wrap();
                        let response = ui.interact(rect, ui.id().with("hover_expire").with(user_index), egui::Sense::hover());
                        
                        if response.hovered() {
                            ui.painter().rect_filled(
                                rect,
                                egui::Rounding::same(2.0),
                                egui::Color32::from_rgba_unmultiplied(255, 140, 0, 25), // Orange hover
                            );
                        }
                        
                        ui.label(&user.expire);
                    });
    
                    // Seats column
                    row.col(|ui| {
                        let rect = ui.available_rect_before_wrap();
                        let response = ui.interact(rect, ui.id().with("hover_seats").with(user_index), egui::Sense::hover());
                        
                        if response.hovered() {
                            ui.painter().rect_filled(
                                rect,
                                egui::Rounding::same(2.0),
                                egui::Color32::from_rgba_unmultiplied(255, 140, 0, 25), // Orange hover
                            );
                        }
                        
                        ui.label(&user.held_seats.len().to_string());
                    });
    
                    // Checkbox column
                    row.col(|ui| {
                        let rect = ui.available_rect_before_wrap();
                        let response = ui.interact(rect, ui.id().with("hover_checkbox").with(user_index), egui::Sense::hover());
                        
                        if response.hovered() {
                            ui.painter().rect_filled(
                                rect,
                                egui::Rounding::same(2.0),
                                egui::Color32::from_rgba_unmultiplied(255, 140, 0, 25), // Orange hover
                            );
                        }
                        
                        let user = &users[user_index];
                        let has_paid_all = self.has_user_paid_all_seats(user);
                        let mut is_selected = user.selected;
                        
                        let checkbox_enabled = !has_paid_all;
                        
                        if checkbox_enabled {
                            if ui.checkbox(&mut is_selected, "").clicked() {
                                toggle_selection_for = Some(user_index);
                            }
                        } else {
                            ui.add_enabled(false, egui::Checkbox::new(&mut is_selected, ""))
                                .on_hover_text("User has paid for all allocated seats. Click 'Reload' to re-enable.");
                        }
                    });
    
                    // Reload button column
                    row.col(|ui| {
                        let rect = ui.available_rect_before_wrap();
                        let response = ui.interact(rect, ui.id().with("hover_reload").with(user_index), egui::Sense::hover());
                        
                        if response.hovered() {
                            ui.painter().rect_filled(
                                rect,
                                egui::Rounding::same(2.0),
                                egui::Color32::from_rgba_unmultiplied(255, 140, 0, 25), // Orange hover
                            );
                        }
                        
                        if ui.button("Reload").clicked() {
                            reload_button_clicked_for = Some(user_index);
                        }
                    });
    
                    // Pay2 button column
                    row.col(|ui| {
                        let rect = ui.available_rect_before_wrap();
                        let response = ui.interact(rect, ui.id().with("hover_pay2").with(user_index), egui::Sense::hover());
                        
                        if response.hovered() {
                            ui.painter().rect_filled(
                                rect,
                                egui::Rounding::same(2.0),
                                egui::Color32::from_rgba_unmultiplied(255, 140, 0, 25), // Orange hover
                            );
                        }
                        
                        let unpaid_seats_count = user.held_seats
                            .iter()
                            .filter(|seat| !user.paid_seats.contains(seat))
                            .count();
                        
                        let button_enabled = !user.payment_completed && unpaid_seats_count > 0;
                        let button_text = if unpaid_seats_count > 0 {
                            format!("Pay2 ({})", unpaid_seats_count)
                        } else {
                            "Pay2".to_string()
                        };
                        
                        if ui
                            .add_enabled(button_enabled, egui::Button::new(button_text))
                            .clicked()
                        {
                            pay2_button_clicked_for = Some(user_index);
                        }
                    });
    
                    // Payment status column
                    row.col(|ui| {
                        let rect = ui.available_rect_before_wrap();
                        let response = ui.interact(rect, ui.id().with("hover_payment").with(user_index), egui::Sense::hover());
                        
                        if response.hovered() {
                            ui.painter().rect_filled(
                                rect,
                                egui::Rounding::same(2.0),
                                egui::Color32::from_rgba_unmultiplied(255, 140, 0, 25), // Orange hover
                            );
                        }
                        
                        let user = &users[user_index];
                        if self.has_user_paid_all_seats(user) {
                            ui.colored_label(egui::Color32::BLUE, "✅ All Paid");
                        } else {
                            let (color, text) = match user.pay_status.as_str() {
                                status if status.contains("Processing") => (egui::Color32::YELLOW, &user.pay_status),
                                status if status.contains("Failed") => (egui::Color32::RED, &user.pay_status),
                                status if status.contains("Error") => (egui::Color32::RED, &user.pay_status),
                                status if status.contains("Reloading") => (egui::Color32::LIGHT_BLUE, &user.pay_status),
                                status if status.contains("Partial Paid") => (egui::Color32::GREEN, &user.pay_status),
                                status if status.contains("✅") => (egui::Color32::GREEN, &user.pay_status),
                                "Pay" => (egui::Color32::GRAY, &user.pay_status),
                                _ => (egui::Color32::WHITE, &user.pay_status),
                            };
                            
                            ui.colored_label(color, text);
                        }
                    });
                });
            });
    
        // Handle button clicks (rest of the code remains the same)
        if let Some(user_index) = toggle_selection_for {
            if let Some(user) = self.users.get_mut(user_index) {
                let has_paid_all = if user.max_seats == 0 {
                    false
                } else {
                    user.paid_seats.len() >= user.max_seats
                };
                
                if !has_paid_all {
                    user.selected = !user.selected;
                    let is_selected = user.selected;
                    
                    drop(user);
                    
                    let selectable_users: Vec<_> = self.users.iter()
                        .filter(|u| {
                            if u.max_seats == 0 {
                                true
                            } else {
                                u.paid_seats.len() < u.max_seats
                            }
                        })
                        .collect();
                    
                    if !is_selected {
                        self.select_all_users = false;
                    } else {
                        self.select_all_users = selectable_users.iter().all(|u| u.selected);
                    }
                }
            }
        }
    
        if let Some(user_index) = pay2_button_clicked_for {
            self.handle_payment_click2(user_index);
        }
        if let Some(user_index) = reload_button_clicked_for {
            self.handle_reload_click(user_index);
        }
        if let Some(user_index) = logs_button_clicked_for {
            self.selected_user_index = Some(user_index);
            self.show_logs_modal = true;
        }
        if let Some(user_index) = delete_button_clicked_for {
            if user_index < self.users.len() {
                self.users.remove(user_index);
            }
        }
    }
    fn handle_pay_selected_click(&mut self) {
        if self.bot_manager.is_none() {
            println!("Bot is not running or ready.");
            return;
        }

        let selected_user_indices: Vec<usize> = self
            .users
            .iter()
            .enumerate()
            .filter(|(_, u)| u.selected && !u.payment_completed)
            .map(|(i, _)| i)
            .collect();

        if selected_user_indices.is_empty() {
            println!("No users selected for payment.");
            return;
        }

        // This is a new function that will handle all selected users
        self.initiate_parallel_payment(selected_user_indices);
    }


    fn initiate_parallel_payment(&mut self, user_indices: Vec<usize>) {
        let mut payloads = Vec::new();
        if let Some(bot_manager) = &self.bot_manager {
            for &index in &user_indices {
                if let Some(bot_user) = bot_manager.users.get(index) {
                    if let Some(hold_token) = bot_user.webook_hold_token.lock().clone() {
                        if let Some(ui_user) = self.users.get_mut(index) {
                            ui_user.pay_status = "⏳ Processing...".to_string();
                            self.payment_in_progress.lock().insert(index);
                        }
                        let unpaid_seats: Vec<String> = self.users[index].held_seats
                            .iter()
                            .filter(|seat| !self.users[index].paid_seats.contains(seat))
                            .cloned()
                            .collect();

                        payloads.push(ParallelPaymentUserPayload {
                            user_index: index,
                            seats: unpaid_seats, // Use unpaid_seats instead of all held_seats
                            hold_token,
                        });
                    }
                }
            }
        }

        if payloads.is_empty() {
            println!("Could not prepare payload for any selected user.");
            return;
        }

        let request_payload = ParallelPaymentRequest { users: payloads };

        // --- FIX: Moved CAPTCHA logic inside the async task below ---

        if let (Some(rt), Some(log_sender)) = (&self.rt, &self.log_sender) {
            // --- Clone all necessary variables for the async task ---
            let bot_manager_clone = self.bot_manager.clone().unwrap();
            let log_sender_clone = log_sender.clone();
            let telegram_sender_clone = self.telegram_sender.clone().unwrap();
            let payment_sender_clone = self.payment_status_sender.clone().unwrap();
            let payment_in_progress_clone = self.payment_in_progress.clone();
            let d_seats_limit: usize = self.d_seats.parse().unwrap_or(0);

            let users_info: HashMap<usize, (String, String)> = self
                .users
                .iter()
                .enumerate()
                .map(|(i, u)| (i, (u.email.clone(), u.user_type.clone())))
                .collect();

            rt.spawn(async move {
                // --- FIX: CAPTCHA check is now the FIRST step inside the async task ---
                let site_key_exists = bot_manager_clone.shared_data.read().await.recaptcha_site_key.is_some();
                if !site_key_exists {
                    // Log a general message, as this is a one-time check for the batch
                    log_sender_clone.send((0, "🔑 Captcha site key not found, fetching...".to_string())).ok();
                    if let Err(e) = bot_manager_clone.extract_recaptcha_site_key().await {
                        log_sender_clone.send((0, format!("❌ Failed to get site key: {}. Aborting payment.", e))).ok();
                        // Clean up "in progress" status for all users in this batch
                        for user in &request_payload.users {
                            payment_in_progress_clone.lock().remove(&user.user_index);
                        }
                        return;
                    }
                    log_sender_clone.send((0, "✅ Captcha site key fetched successfully.".to_string())).ok();
                } else {
                    log_sender_clone.send((0, "✅ Using cached captcha site key.".to_string())).ok();
                }

                // --- The rest of the original async logic follows ---
                let client = reqwest::Client::new();
                match client
                    .post("http://localhost:3000/findSeatsForUsers")
                    .json(&request_payload)
                    .send()
                    .await
                {
                    Ok(response) => {
                        if response.status().is_success() {
                            match response.json::<Vec<ParallelPaymentResponseUser>>().await {
                                Ok(users_with_seats) => {
                                    let mut tasks = Vec::new();
                                    for user_data in users_with_seats {
                                        if let Some((email, user_type)) = users_info.get(&user_data.user_index) {
                                            let task = Self::process_payment_for_user(
                                                user_data.user_index,
                                                user_data.selectable,
                                                bot_manager_clone.clone(),
                                                log_sender_clone.clone(),
                                                telegram_sender_clone.clone(),
                                                payment_sender_clone.clone(),
                                                payment_in_progress_clone.clone(),
                                                email.clone(),
                                                user_type.clone(),
                                                d_seats_limit,
                                            );
                                            tasks.push(tokio::spawn(task));
                                        }
                                    }
                                    futures::future::join_all(tasks).await;
                                }
                                Err(e) => println!("Error parsing response from /findSeatsForUsers: {}", e),
                            }
                        } else {
                            println!("HTTP Error from /findSeatsForUsers: {}", response.status());
                        }
                    }
                    Err(e) => println!("Request to /findSeatsForUsers failed: {}", e),
                }
            });
        }
    }
    // In impl WebbookBot

    async fn process_payment_for_user(
        user_index: usize,
        selectable_seats: Vec<Value>,
        bot_manager: Arc<BotManager>,
        log_sender: mpsc::UnboundedSender<(usize, String)>,
        telegram_sender: mpsc::UnboundedSender<TelegramMessage>,
        payment_sender: mpsc::UnboundedSender<(usize, usize)>, // <-- Updated to match new channel type
        payment_in_progress: Arc<Mutex<HashSet<usize>>>,
        user_email: String,
        user_type: String,
        d_seats_limit: usize,
    ) {
        let bot_user = &bot_manager.users[user_index];
        let site_key = match bot_manager.shared_data.read().await.recaptcha_site_key.clone() {
            Some(key) => key,
            None => {
                log_sender.send((user_index, "❌ No reCAPTCHA site key available.".to_string())).ok();
                payment_in_progress.lock().remove(&user_index);
                return;
            }
        };
        //println!("{}",site_key);
        // --- 1. CAPTCHA Solving Section with Retry Loop ---
        let captcha_solution = {
            let mut solution = None;
            for attempt in 1..=3 {
                log_sender
                    .send((
                        user_index,
                        format!("🧩 Solving CAPTCHA (Attempt {}/3)...", attempt),
                    ))
                    .ok();

                let captcha_solver = CaptchaSolver::new();
                match captcha_solver
                    .solve(
                        "https://webook.com/",
                        &site_key,
                    )
                    .await
                {
                    Ok(s) => {
                        log_sender
                            .send((user_index, "✅ CAPTCHA solved successfully.".to_string()))
                            .ok();
                        solution = Some(s);
                        break; // --- Success, so we exit the CAPTCHA loop ---
                    }
                    Err(e) => {
                        log_sender
                            .send((
                                user_index,
                                format!("❌ CAPTCHA failed (attempt {}): {}", attempt, e),
                            ))
                            .ok();
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                        // Loop will continue to the next attempt
                    }
                }
            }
            solution
        };

        // --- 2. Check if CAPTCHA was solved ---
        // If 'captcha_solution' is None, it means all 3 attempts failed.
        let captcha_solution = match captcha_solution {
            Some(s) => s,
            None => {
                log_sender
                    .send((
                        user_index,
                        "❌ All CAPTCHA attempts failed. Aborting payment.".to_string(),
                    ))
                    .ok();
                payment_in_progress.lock().remove(&user_index); // Cleanup
                return; // Exit the function
            }
        };

        // --- 3. Payment Processing Section (runs only ONCE) ---
        // This part is the same as before, but it's no longer inside a loop.
        log_sender
            .send((user_index, "💳 Proceeding to payment...".to_string()))
            .ok();
        let total_seats = selectable_seats.len();
        let original_limit: usize = d_seats_limit; // or get from user.max_seats
        let seats_to_pay = std::cmp::min(total_seats, original_limit);
        
        // Only pay for the first N seats
        let seats_for_payment: Vec<Value> = selectable_seats.into_iter().take(seats_to_pay).collect();
        let selected_seats_value = match serde_json::to_value(seats_for_payment) {
            Ok(v) => v,
            Err(_) => {
                log_sender
                    .send((user_index, "❌ Failed to serialize seat data".to_string()))
                    .ok();
                payment_in_progress.lock().remove(&user_index);
                return;
            }
        };

        let selected_seats_json_string = selected_seats_value.to_string();

        let token_data = match bot_user.token_manager.get_valid_token(&bot_user.email) {
            Some(token) => token,
            None => {
                log_sender
                    .send((user_index, "❌ No valid token for payment.".to_string()))
                    .ok();
                payment_in_progress.lock().remove(&user_index);
                return;
            }
        };
        let event_id = match &bot_user.shared_data.read().await.event_id {
            Some(id) => id.clone(),
            None => {
                log_sender
                    .send((user_index, "❌ Event ID not found.".to_string()))
                    .ok();
                payment_in_progress.lock().remove(&user_index);
                return;
            }
        };

        let checkout_payload = json!({
            "event_id": event_id,
            "season_id": event_id,
            "redirect": "https://webook.com/en/payment-success",
            "redirect_failed": "https://webook.com/en/payment-failed",
            "booking_source": "rs-web",
            "lang": "en",
            "payment_method": "credit_card",
            "is_wallet": false,
            "saudi_redeem": null,
            "is_mada": false,
            "is_amex": false,
            "perks": [],
            "merchandise": [],
            "addons":[],
            "vouchers":[],
            "holdToken":"",
            "selectedSeats": selected_seats_json_string,
            "captcha": captcha_solution, // Use the successfully solved captcha
            "app_source": "rs",
            "tpp_cart_id": null,
            "utm_wbk_wa_session_id": token_data.session_id
        });

        let checkout_url = "https://api.webook.com/api/v2/event-seat/checkout?lang=en";
        let response = bot_user
            .client
            .post(checkout_url)
            .header(
                "Authorization",
                format!("Bearer {}", token_data.access_token),
            )
            .header("token", &token_data.token)
            .json(&checkout_payload)
            .send()
            .await;

        match response {
            Ok(resp) => {
                let status = resp.status();
                let body_text = resp.text().await.unwrap_or_default();
                if status.is_success() {
                    if let Ok(data) = serde_json::from_str::<Value>(&body_text) {
                        if let Some(link) = data
                            .get("data")
                            .and_then(|d| d.get("redirect_url"))
                            .and_then(|l| l.as_str())
                        {
                            log_sender
                                .send((user_index, format!("✅ Payment link: {}", link)))
                                .ok();
                            let bot_user = &bot_manager.users[user_index];
                            let currently_held = bot_user.held_seats.lock().clone();
                            let held_seats = bot_manager.users[user_index].held_seats.lock();
                            let seats_count = held_seats.len();
                            let seat_list: Vec<String> = held_seats.clone();
                            let paid_count = seats_to_pay; 
                            tokio::spawn({
                                let bot_user_clone = bot_user.clone();
                                let log_sender_clone = log_sender.clone();
                                let user_index = user_index;
                                async move {
                                    // Use the robust token refresh with retries
                                    bot_user_clone.refresh_token_with_retries(
                                        &log_sender_clone,
                                        user_index,
                                        vec![], // You may need to pass proper channel_keys here
                                    ).await;
                                }
                            });
                            
                            payment_sender.send((user_index, seats_to_pay)).ok(); // Send both values
                            let message = if user_type == "+"
                                && d_seats_limit > 0
                                && seats_count >= d_seats_limit
                            {
                                format!(
                                    "⚠️ PAYMENT REQUIRED (LIMIT REACHED)\n━━━━━━━━━━━━━━━━━\n🔧 User: {} (Type: +)\n🎫 Seats: {} (Limit reached)\n💰 Must pay now!\n🔗 Payment: {}\n━━━━━━━━━━━━━━━━━",
                                    user_email, seats_count, link
                                )
                            } else {
                                let seats_formatted = seat_list.join("\n");
                                format!(
                                    "💳 PAYMENT LINK GENERATED\n━━━━━━━━━━━━━━━━━\n🔧 User: {}\n🎫 Seats ({}):\n{}\n🔗 Link: {}\n━━━━━━━━━━━━━━━━━",
                                    user_email, seats_count, seats_formatted, link
                                )
                            };
                            telegram_sender
                                .send(TelegramMessage {
                                    text: message,
                                    chat_id: 4814580777,
                                })
                                .ok();
                        }
                    }
                } else {
                    log_sender
                        .send((
                            user_index,
                            format!("❌ Checkout failed with status {}: {}", status, body_text),
                        ))
                        .ok();
                    payment_sender.send((user_index, usize::MAX)).ok(); 
                }
            }
            Err(e) => {
                log_sender
                    .send((user_index, format!("❌ Checkout request error: {}", e)))
                    .ok();
                payment_sender.send((user_index, usize::MAX)).ok(); 
            }
        }

        // Final cleanup
        payment_in_progress.lock().remove(&user_index);
    }
    // This is the REFACTORED payment logic with a 3-attempt retry loop
    // MODIFIED to use the new process_payment_for_user function
    
    // Replace the entire handle_payment_click function with this:
fn handle_reload_click(&mut self, user_index: usize) {
    if let (Some(bot_manager), Some(rt), Some(log_sender)) = (
        &self.bot_manager,
        &self.rt,
        &self.log_sender,
    ) {
        // Reset user data immediately in UI
        if let Some(user) = self.users.get_mut(user_index) {
            user.held_seats.clear();
            user.paid_seats.clear();
            user.seats = "0".to_string();
            user.pay_status = "Reloading...".to_string();
            user.payment_completed = false;
            user.remaining_seat_limit = user.max_seats;
            user.logs.clear();
            user.selected = false; // Reset selection state
        }

        // Remove from paid users list
        self.paid_users.lock().remove(&user_index);
        self.payment_in_progress.lock().remove(&user_index);

        // Update UI immediately
        if let Some(sender) = &self.assigned_seats_sender {
            sender.send((user_index, "".to_string())).ok(); // Clear seats in UI
        }

        let bot_manager_clone = bot_manager.clone();
        let log_sender_clone = log_sender.clone();
        let channel_keys = self.build_channel_keys();
        let user_email = self.users[user_index].email.clone();

        rt.spawn(async move {
            if let Some(bot_user) = bot_manager_clone.users.get(user_index) {
                log_sender_clone
                    .send((user_index, "🔄 Starting user reload...".to_string()))
                    .ok();

                // Step 1: Clear bot user's held seats
                bot_user.held_seats.lock().clear();
                
                // Step 2: Reset expire time
                bot_user.expire_time.store(usize::MAX, Ordering::Relaxed);

                // Step 3: Generate new browser ID
                {
                    let mut browser_id = bot_user.browser_id.lock();
                    let new_id = format!("{:016x}", rand::random::<u64>());
                    *browser_id = Some(new_id);
                }

                log_sender_clone
                    .send((user_index, "🆔 Generated new browser ID".to_string()))
                    .ok();

                // Step 4: Use the robust token refresh system
                let success = bot_user.refresh_token_with_retries(
                    &log_sender_clone,
                    user_index,
                    channel_keys,
                ).await;

                if success {
                    log_sender_clone
                        .send((user_index, "✅ User reload completed successfully!".to_string()))
                        .ok();
                } else {
                    log_sender_clone
                        .send((user_index, "⚠️ User reload completed with warnings".to_string()))
                        .ok();
                }
            }
        });
    }
}
    fn handle_payment_click2(&mut self, user_index: usize) {
        if self.users[user_index].payment_completed {
            if let Some(log_sender) = &self.log_sender {
                log_sender
                    .send((
                        user_index,
                        "⚠️ Payment already completed for this user".to_string(),
                    ))
                    .ok();
            }
            return;
        }
    
        // Calculate unpaid seats count for Method 2
        let unpaid_seats_count = self.users[user_index].held_seats
            .iter()
            .filter(|seat| !self.users[user_index].paid_seats.contains(seat))
            .count();
    
        if unpaid_seats_count == 0 {
            if let Some(log_sender) = &self.log_sender {
                log_sender
                    .send((
                        user_index,
                        "ℹ️ No unpaid seats to process".to_string(),
                    ))
                    .ok();
            }
            return;
        }
    
        self.payment_in_progress.lock().insert(user_index);
        self.users[user_index].pay_status = "⏳ Processing...".to_string();
        
        if let (
            Some(_bot_manager),
            Some(rt),
            Some(log_sender),
            Some(telegram_sender),
            Some(payment_sender),
        ) = (
            &self.bot_manager,
            &self.rt,
            &self.log_sender,
            &self.telegram_sender,
            &self.payment_status_sender,
        ) {
            let log_sender_clone = log_sender.clone();
            let telegram_sender_clone = telegram_sender.clone();
            let payment_sender_clone = payment_sender.clone();
            let user_email = self.users[user_index].email.clone();
            let user_password = self.users[user_index].password.clone();
            let user_proxy = self.users[user_index].proxy.clone();
            let event_url = self.event_url.clone();
            let unpaid_count = unpaid_seats_count; // Store the count for the async task
    
            log_sender_clone
                .send((
                    user_index,
                    format!("💳 Generating payment link (Method 2) for {} unpaid seats...", unpaid_count),
                ))
                .ok();
                
            let payment_in_progress_clone = self.payment_in_progress.clone();
            rt.spawn(async move {
                match Self::get_payment_link_method2(
                    &user_email,
                    &user_password,
                    &user_proxy,
                    &event_url,
                )
                .await
                {
                    Ok(payment_link) => {
                        log_sender_clone
                            .send((
                                user_index,
                                format!("✅ Payment link (M2): {}", payment_link),
                            ))
                            .ok();
                        
                        // Send the actual count of unpaid seats that were processed
                        payment_sender_clone.send((user_index, unpaid_count)).ok();
    
                        let message = format!(
                            "💳 PAYMENT LINK GENERATED (M2)\n\
                            ▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬\n\
                            📧 User: {}\n\
                            🎫 Unpaid Seats: {}\n\
                            🔗 Link: {}\n\
                            ▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬▬",
                            user_email, unpaid_count, payment_link
                        );
                        telegram_sender_clone
                            .send(TelegramMessage {
                                text: message,
                                chat_id: 4683666687,
                            })
                            .ok();
                    }
                    Err(e) => {
                        log_sender_clone
                            .send((user_index, format!("❌ Payment link (M2) failed: {}", e)))
                            .ok();
                        payment_sender_clone.send((user_index, 0)).ok();
                    }
                }
                payment_in_progress_clone.lock().remove(&user_index);
            });
        }
    }
    async fn get_payment_link_method2(
        email: &str,
        password: &str,
        proxy: &str,
        event_url: &str,
    ) -> Result<String> {
        #[derive(Serialize)]
        struct PaymentRequest {
            email: String,
            password: String,
            #[serde(rename = "eventsUrl")]
            events_url: String,
            #[serde(rename = "proxyUrl")]
            proxy_url: String,
        }

        #[derive(Deserialize)]
        struct PaymentResponse {
            success: bool,
            #[serde(rename = "paymentLink")]
            payment_link: Option<String>,
            error: Option<String>,
        }

        let payload = PaymentRequest {
            email: email.to_string(),
            password: password.to_string(),
            events_url: event_url.to_string(),
            proxy_url: proxy.to_string(),
        };

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(600)) // 5 minutes
            .build()?;

        let response = client
            .post("http://localhost:3000/getPayment")
            .json(&payload)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!("HTTP error: {}", response.status()));
        }

        let result: PaymentResponse = response.json().await?;

        if result.success {
            result
                .payment_link
                .ok_or_else(|| anyhow::anyhow!("No payment link in response"))
        } else {
            Err(anyhow::anyhow!(
                "Payment generation failed: {}",
                result.error.unwrap_or_else(|| "Unknown error".to_string())
            ))
        }
    }
    fn render_status_bar(&mut self, ui: &mut egui::Ui) {
        ui.separator();

        // Collect all status messages first to avoid borrow conflicts
        let mut status_messages = Vec::new();
        if let Some(receiver) = &mut self.status_receiver {
            while let Ok(status) = receiver.try_recv() {
                status_messages.push(status);
            }
        }

        // Now process the collected messages without borrowing conflicts
        for status in status_messages {
            // Handle special ready messages
            if status.starts_with("USER_READY:") {
                if let Ok(user_index) = status[11..].parse::<usize>() {
                    if user_index < self.users.len() {
                        self.users[user_index].status = "Active".to_string();
                        
                        // If user was reloading, reset their pay status
                        if self.users[user_index].pay_status == "Reloading..." {
                            self.users[user_index].pay_status = "Pay".to_string();
                        }
                        
                        self.update_ready_scanner_users_atomic();
            
                        let has_selected_sections =
                            self.selected_sections.values().any(|&selected| selected);
                        if has_selected_sections && !self.scanner_running {
                            self.start_scanner();
                        }
                    }
                }
            }else {
                self.sections_status = status;
            }
        }

        ui.horizontal(|ui| {
            ui.label(format!("Users: {} |", self.users.len()));
            ui.label(format!("Proxies: {} |", self.proxies.len()));
            ui.label(format!(
                "Bot: {}",
                if self.bot_running {
                    "Running"
                } else {
                    "Stopped"
                }
            ));
        });
    }
    // In impl WebbookBot
    fn start_scanner(&mut self) {
        if self.scanner_running || !self.bot_running || self.shared_data.is_none() {
            return;
        }

        // Initialize shared selections with current state
        {
            let mut shared = self.selected_sections_shared.lock();
            *shared = self.selected_sections.clone();
        }

        // Check if we have any selected sections
        let has_selections = self.selected_sections.values().any(|&selected| selected);
        if !has_selections {
            return;
        }

        self.scanner_running = true;
        self.reserved_seats_map.lock().clear();
        self.update_ready_scanner_users_atomic();

        let assigned_seats_sender = self.assigned_seats_sender.as_ref().unwrap().clone();
        let log_sender = self.log_sender.as_ref().unwrap().clone();
        let telegram_sender = self.telegram_sender.as_ref().unwrap().clone();
        let reserved_seats_map_clone = self.reserved_seats_map.clone();
        let transfer_ignore_list_clone = self.transfer_ignore_list.clone();
        let pending_scanner_seats_clone = self.pending_scanner_seats.clone();
        let selected_sections_shared_clone = self.selected_sections_shared.clone(); // ADD THIS
        let payment_in_progress_clone = self.payment_in_progress.clone(); // <-- ADD THIS
        let next_user_index_clone = self.next_user_index.clone();
        let cooldown_until_clone = self.cooldown_until.clone();

        if let Some(rt) = &self.rt {
            let shared_data = self.shared_data.clone().unwrap();
            let bot_manager = self.bot_manager.clone();
            let users_data_clone = self.users.clone();
            let ready_scanner_users_clone = self.ready_scanner_users.clone();

            rt.spawn(async move {
                if let Err(e) = Self::run_websocket_scanner(
                    shared_data,
                    bot_manager,
                    selected_sections_shared_clone, // CHANGED
                    users_data_clone,
                    assigned_seats_sender,
                    log_sender,
                    &telegram_sender,
                    ready_scanner_users_clone,
                    next_user_index_clone,
                    reserved_seats_map_clone,
                    transfer_ignore_list_clone,
                    pending_scanner_seats_clone,
                    payment_in_progress_clone,
                    cooldown_until_clone,
                )
                .await
                {
                    println!("Scanner error: {}", e);
                }
            });
        }
    }

    // In impl WebbookBot
    async fn run_websocket_scanner(
        shared_data: Arc<RwLock<SharedData>>,
        bot_manager: Option<Arc<BotManager>>,
        selected_sections_shared: Arc<Mutex<HashMap<String, bool>>>, // CHANGED
        users_data: Vec<User>,
        assigned_seats_sender: mpsc::UnboundedSender<(usize, String)>,
        log_sender: mpsc::UnboundedSender<(usize, String)>,
        telegram_sender: &mpsc::UnboundedSender<TelegramMessage>,
        ready_scanner_users: Arc<Mutex<Vec<usize>>>,
        next_user_index: Arc<AtomicUsize>,
        reserved_seats_map: Arc<Mutex<HashMap<String, Vec<String>>>>,
        transfer_ignore_list: Arc<Mutex<HashSet<String>>>,
        pending_scanner_seats: Arc<Mutex<HashMap<usize, usize>>>,
        payment_in_progress_clone: Arc<Mutex<HashSet<usize>>>,
        cooldown_until: Arc<Mutex<HashMap<usize, std::time::Instant>>>, // <-- ADD THIS
    ) -> Result<()> {
        loop {
            // The reconnect loop
            match Self::connect_and_listen(
                &shared_data,
                &bot_manager,
                &selected_sections_shared, // CHANGED
                &users_data,
                &assigned_seats_sender,
                &log_sender,
                &telegram_sender,
                &ready_scanner_users,
                &next_user_index,
                &reserved_seats_map,
                &transfer_ignore_list,
                &pending_scanner_seats,
                &payment_in_progress_clone,
                &cooldown_until, // <-- ADD THIS (as a reference)
            )
            .await
            {
                Ok(_) => {
                    println!("WebSocket connection ended normally");
                    break;
                }
                Err(e) => {
                    println!("Scanner error: {}", e);
                    //tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                }
            }
        }
        Ok(())
    }
    async fn connect_and_listen(
        shared_data: &Arc<RwLock<SharedData>>,
        bot_manager: &Option<Arc<BotManager>>,
        selected_sections_shared: &Arc<Mutex<HashMap<String, bool>>>, // Changed parameter
        users_data: &Vec<User>,
        assigned_seats_sender: &mpsc::UnboundedSender<(usize, String)>,
        log_sender: &mpsc::UnboundedSender<(usize, String)>,
        telegram_sender: &mpsc::UnboundedSender<TelegramMessage>,
        ready_scanner_users: &Arc<Mutex<Vec<usize>>>,
        next_user_index: &Arc<AtomicUsize>,
        reserved_seats_map: &Arc<Mutex<HashMap<String, Vec<String>>>>,
        transfer_ignore_list: &Arc<Mutex<HashSet<String>>>,
        pending_scanner_seats: &Arc<Mutex<HashMap<usize, usize>>>,
        payment_in_progress_clone: &Arc<Mutex<HashSet<usize>>>,
        cooldown_until: &Arc<Mutex<HashMap<usize, std::time::Instant>>>, // <-- ADD THIS
    ) -> Result<()> {
        use futures_util::{SinkExt, StreamExt};
        use sha1::{Digest, Sha1};
        use tokio_tungstenite::{
            connect_async, connect_async_with_config, tungstenite::protocol::WebSocketConfig,
            tungstenite::Message,
        }; // MODIFIED: Added imports

        let ws_url = "wss://messaging-eu.seatsio.net/ws";
        let config = WebSocketConfig {
            max_send_queue: Some(1000),
            max_message_size: Some(64 << 20),
            max_frame_size: Some(16 << 20),
            // --- FIX: ADDED MISSING FIELDS ---
            write_buffer_size: 16 << 20,     // 16MB initial write buffer
            max_write_buffer_size: 64 << 20, // 64MB max write buffer
            // --- END FIX ---
            accept_unmasked_frames: false,
        };
        let (ws_stream, _) = connect_async_with_config(ws_url, Some(config), false).await?;
        let (mut write, mut read) = ws_stream.split();

        // Subscription logic remains the same
        let workspace_sub = json!({ "type": "SUBSCRIBE", "data": { "channel": WORKSPACE_KEY } });
        write.send(Message::Text(workspace_sub.to_string())).await?;
        let data = shared_data.read().await;
        if let Some(event_key) = &data.event_key {
            let event_channel = format!("{}-{}", WORKSPACE_KEY, event_key);
            let event_sub = json!({ "type": "SUBSCRIBE", "data": { "channel": event_channel } });
            write.send(Message::Text(event_sub.to_string())).await?;
        }
        if let Some(season_structure) = &data.season_structure {
            let season_channel = format!("{}-{}", WORKSPACE_KEY, season_structure);
            let season_sub = json!({ "type": "SUBSCRIBE", "data": { "channel": season_channel } });
            write.send(Message::Text(season_sub.to_string())).await?;
        }
        drop(data);
        println!("Scanner connected and subscribed to channels");

        while let Some(msg) = read.next().await {
            let msg = match msg {
                Ok(msg) => msg,
                Err(e) => {
                    return Err(anyhow::anyhow!("Read error: {}", e));
                }
            };

            if let Message::Text(text) = msg {
                if let Ok(messages) = serde_json::from_str::<Vec<ScannerMessage>>(&text) {
                    for scanner_msg in messages {
                        let body = scanner_msg.message.body;
                        println!("{:?}", body);
                        // --- 1. FREE SEAT (Standard Scanner) ---
                        if body.status.as_deref() == Some("free") {
                            if let Some(ref seat_id) = body.object_label {
                                // --- THIS IS THE FIX ---
                                // Check the ignore list before trying to take a seat.
                                let is_ignored = transfer_ignore_list.lock().contains(seat_id);
                                let is_in_selected_section = {
                                    let current_selections = selected_sections_shared.lock();
                                    current_selections.iter().any(|(section, &is_selected)| {
                                        is_selected
                                            && seat_id.starts_with(section)
                                            && (seat_id.len() == section.len()
                                                || seat_id
                                                    .chars()
                                                    .nth(section.len())
                                                    .map_or(false, |c| !c.is_ascii_digit()))
                                    })
                                };

                                if !is_ignored && is_in_selected_section {
                                    Self::dispatch_seat_take_task(
                                        seat_id,
                                        "SCANNER",
                                        bot_manager,
                                        ready_scanner_users,
                                        assigned_seats_sender,
                                        log_sender,
                                        telegram_sender,
                                        users_data,
                                        payment_in_progress_clone,
                                        next_user_index,
                                        cooldown_until, // Ensure this matches the function signature
                                    )
                                    .await;
                                } else if is_ignored {
                                    log_sender
                                        .send((
                                            0,
                                            format!(
                                                "🚫 SCANNER: Ignoring {} (transfer in progress)",
                                                seat_id
                                            ),
                                        ))
                                        .ok();
                                }
                            }
                        }

                        // --- 2. SPYING (Phase 1) ---
                        if body.status.as_deref() == Some("reservedByToken") {
                            // --- FIX: Add `ref` here as well to borrow the seat label ---
                            if let (Some(hash), Some(ref seat)) =
                                (body.hold_token_hash, body.object_label)
                            {
                                let mut map = reserved_seats_map.lock();
                                // We clone the borrowed `seat` so the map can own the String.
                                //println!("{} ==> {}", hash, seat);
                                map.entry(hash).or_default().push(seat.clone());
                            }
                        }

                        // --- 3. SNIPING (Phase 2) ---
                        // Located inside the `connect_and_listen` function's main loop
                        if body.message_type.as_deref() == Some("HOLD_TOKEN_EXPIRED") {
                            if let Some(token) = body.hold_token {
                                let mut hasher = Sha1::new();
                                hasher.update(token.as_bytes());
                                let hash_result = hasher.finalize();
                                let hash_hex = hex::encode(hash_result);

                                let seats_to_snipe = {
                                    let mut map = reserved_seats_map.lock();
                                    map.remove(&hash_hex)
                                };

                                if let Some(seats) = seats_to_snipe {
                                    // --- START: New pre-emptive check ---
                                    let is_there_capacity_among_ready_users = {
                                        let ready_users_indices = ready_scanner_users.lock();
                                        // Check if any of the ready users have space for at least one seat.
                                        ready_users_indices.iter().any(|&user_index| {
                                            if let (Some(ui_user), Some(bot_user)) = (
                                                users_data.get(user_index),
                                                bot_manager.as_ref().and_then(|m| m.users.get(user_index)),
                                            ) {
                                                let current_held = bot_user.held_seats.lock().len();
                                                let limit = ui_user.max_seats;
                                                // A user has capacity if their limit is 0 (unlimited) OR they are below their limit.
                                                limit == 0 || current_held < limit
                                            } else {
                                                false // User index is invalid, so no capacity.
                                            }
                                        })
                                    };
                                    // --- END: New pre-emptive check ---

                                    // Now, only proceed if there's at least one user who can receive a seat.
                                    if is_there_capacity_among_ready_users {
                                        log_sender
                                            .send((
                                                0,
                                                format!(
                                                    "🎯 SNIPER: Token expired, found {} seats to take.",
                                                    seats.len()
                                                ),
                                            ))
                                            .ok();
                                        for seat_id in seats {
                                            let is_ignored = transfer_ignore_list.lock().contains(&seat_id);

                                            let is_in_selected_section = {
                                                let current_selections = selected_sections_shared.lock();
                                                current_selections.iter().any(|(section, &is_selected)| {
                                                    is_selected
                                                        && seat_id.starts_with(section)
                                                        && (seat_id.len() == section.len()
                                                            || seat_id
                                                                .chars()
                                                                .nth(section.len())
                                                                .map_or(false, |c| !c.is_ascii_digit()))
                                                })
                                            };
                                            if !is_ignored && is_in_selected_section {
                                                Self::dispatch_seat_take_task(
                                                    &seat_id,
                                                    "SNIPER",
                                                    bot_manager,
                                                    ready_scanner_users,
                                                    assigned_seats_sender,
                                                    log_sender,
                                                    telegram_sender,
                                                    users_data,
                                                    payment_in_progress_clone,
                                                    next_user_index,
                                                    cooldown_until, // Ensure this matches the function signature
                                                )
                                                .await;
                                            } else if is_ignored {
                                                log_sender
                                                    .send((
                                                        0,
                                                        format!("🚫 SNIPER: Ignoring {} (transfer in progress)", seat_id),
                                                    ))
                                                    .ok();
                                            }
                                        }
                                    } else {
                                        // This log message is new, it explains why the sniper isn't taking the seats.
                                        log_sender
                                            .send((
                                                0,
                                                format!(
                                                    "🎯 SNIPER: Found {} seats, but all active users are at their limit. Skipping.",
                                                    seats.len()
                                                ),
                                            ))
                                            .ok();
                                    }
                                }
                            }
                        }
                    }
                }
            } else if let Message::Close(_) = msg {
                break;
            }
        }
        Ok(())
    }
    fn show_telegram_auth_dialog(&self, ctx: &egui::Context) -> Option<String> {
        let mut code = String::new();
        let mut dialog_open = true;
        let mut result = None;

        if dialog_open {
            egui::Window::new("Telegram Authentication")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.vertical(|ui| {
                        ui.label("Enter the 5-digit code from Telegram:");
                        ui.add(egui::TextEdit::singleline(&mut code).hint_text("12345"));

                        ui.horizontal(|ui| {
                            if ui.button("Submit").clicked() && !code.is_empty() {
                                result = Some(code.clone());
                                dialog_open = false;
                            }
                            if ui.button("Cancel").clicked() {
                                dialog_open = false;
                            }
                        });

                        ui.label("After submitting, confirm in Telegram app!");
                    });
                });
        }

        result
    }
}
// In main.rs

impl eframe::App for WebbookBot {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
                // --- START OF FIX ---
        // Add this line to constantly synchronize the scanner's ready list with the UI state.
        self.update_ready_scanner_users_atomic();
        // --- END OF FIX ---

        // Set theme
        ctx.set_visuals(egui::Visuals::light());
        self.ctx = Some(ctx.clone());


        // --- CHANGE 1: TOP PANEL for controls ---
        // This panel will stick to the top.
        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            ui.style_mut().spacing.item_spacing = egui::vec2(8.0, 8.0);
            self.render_top_panel(ui);
        });

        // --- CHANGE 2: BOTTOM PANEL for status bar ---
        // This panel will stick to the bottom.
        egui::TopBottomPanel::bottom("bottom_panel").show(ctx, |ui| {
            self.render_status_bar(ui);
        });

        // --- CHANGE 3: CENTRAL PANEL for the main content (the table) ---
        // This panel automatically fills all remaining space between the top and bottom panels.
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Pay Selected").clicked() {
                    self.handle_pay_selected_click();
                }
            });
            ui.separator();

            // The ScrollArea now lives inside the CentralPanel.
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    self.render_user_table(ui);
                });
        });

        // --- This logic remains the same ---
        self.handle_telegram_dialogs();
        if self.show_telegram_dialog {
            self.show_telegram_dialog_ui(ctx);
        }

        if self.bot_running {
            self.receive_bot_manager();
            self.update_countdowns();
            self.update_assigned_seats();
            self.update_logs();
            self.update_payment_status();
            self.handle_paid_seats_queries();

            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
        if self.show_transfer_modal {
            self.show_transfer_modal(ctx);
        }
        if self.show_logs_modal {
            self.show_logs_modal(ctx);
        }
    }
}

// ADD THE METHODS HERE - OUTSIDE THE APP IMPLEMENTATION
impl WebbookBot {
    fn handle_telegram_dialogs(&mut self) {
        if let Some(receiver) = &mut self.telegram_request_receiver {
            while let Ok(request) = receiver.try_recv() {
                self.telegram_dialog_type = Some(request);
                self.show_telegram_dialog = true;
                self.telegram_input.clear();
            }
        }
    }
    fn has_user_paid_all_seats(&self, user: &User) -> bool {
        if user.max_seats == 0 {
            return false; // Unlimited seats, never "paid all"
        }
        user.paid_seats.len() >= user.max_seats
    }

    fn update_payment_status(&mut self) {
        if let Some(receiver) = &mut self.payment_status_receiver {
            while let Ok((user_index, paid_count)) = receiver.try_recv() {
                if user_index < self.users.len() {
                    if paid_count == usize::MAX {
                        // Handle failure case
                        self.users[user_index].pay_status = "❌ Payment Failed".to_string();
                        self.payment_in_progress.lock().remove(&user_index);
                    } else if paid_count == 0 {
                        // Handle method2 failure or no seats to pay
                        if self.users[user_index].held_seats.is_empty() {
                            self.users[user_index].pay_status = "❌ No Seats to Pay".to_string();
                        } else {
                            self.users[user_index].pay_status = "❌ Payment Failed".to_string();
                        }
                        self.payment_in_progress.lock().remove(&user_index);
                    } else {
                    let original_limit = self.users[user_index].max_seats;
                    self.users[user_index].remaining_seat_limit = original_limit.saturating_sub(paid_count);
                    
                    // Track paid seats (add newly paid seats to existing paid seats)
                    let current_held = self.users[user_index].held_seats.clone();
                    let unpaid_seats: Vec<String> = current_held
                        .iter()
                        .filter(|seat| !self.users[user_index].paid_seats.contains(seat))
                        .cloned()
                        .collect();
                    
                    // Add the newly paid seats (first N unpaid seats)
                    let newly_paid: Vec<String> = unpaid_seats.into_iter().take(paid_count).collect();
                    self.users[user_index].paid_seats.extend(newly_paid);
                    
                    // Update status - never mark as completed, always allow more payments
                    let total_unpaid = self.users[user_index].held_seats
                        .iter()
                        .filter(|seat| !self.users[user_index].paid_seats.contains(seat))
                        .count();
                    
                    if total_unpaid == 0 {
                        self.users[user_index].pay_status = "✅ All Paid".to_string();
                    } else {
                        self.users[user_index].pay_status = "✅ Partial Paid".to_string();
                    }
                    
                    // Never mark payment_completed as true - always allow more payments
                    // self.users[user_index].payment_completed = false; // Keep this false
                    
                    // Auto-deselect
                    self.users[user_index].selected = false;
                    self.select_all_users = self.users.iter().all(|u| u.selected);
    
                    // Only add to paid_users if they reach their maximum limit AND have no unpaid seats
                    let at_limit = original_limit > 0 && self.users[user_index].held_seats.len() >= original_limit;
                    if at_limit && total_unpaid == 0 {
                        self.paid_users.lock().insert(user_index);
                    }
                }
                }
            }
        }
    }
    fn show_telegram_dialog_ui(&mut self, ctx: &egui::Context) {
        if let Some(request_type) = &self.telegram_dialog_type {
            let title = match request_type {
                TelegramRequest::RequestPhone => "Enter Phone Number",
                TelegramRequest::RequestCode => "Enter Telegram Code",
                TelegramRequest::RequestPassword => "Enter 2FA Password",
            };

            let mut dialog_open = true;
            egui::Window::new(title)
                .collapsible(false)
                .resizable(false)
                .open(&mut dialog_open)
                .show(ctx, |ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.telegram_input));
                    ui.horizontal(|ui| {
                        if ui.button("Submit").clicked() && !self.telegram_input.is_empty() {
                            if let Some(sender) = &self.telegram_response_sender {
                                sender.send(self.telegram_input.clone()).ok();
                            }
                            self.show_telegram_dialog = false;
                        }
                        if ui.button("Cancel").clicked() {
                            self.show_telegram_dialog = false;
                        }
                    });
                });

            if !dialog_open {
                self.show_telegram_dialog = false;
            }
        }
    }
}
#[cfg(unix)]
fn optimize_system_settings() {
    // Set file descriptor limits
    unsafe {
        // --- FIX: CORRECTED rlimit USAGE ---
        let mut limits = rlimit {
            rlim_cur: 65536,
            rlim_max: 65536,
        };
        if setrlimit(RLIMIT_NOFILE, &limits) != 0 {
            println!("Warning: Failed to set file descriptor limit. May encounter issues under heavy load.");
        }
        // --- END FIX ---
    }
}
fn main() -> Result<(), eframe::Error> {
    #[cfg(unix)]
    optimize_system_settings();
    env_logger::init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 700.0])
            .with_title("Webbook Bot - Rust Edition"),
        ..Default::default()
    };

    eframe::run_native(
        "Webbook Bot",
        options,
        Box::new(|_cc| Box::new(WebbookBot::new())),
    )
}

// he can't chaneg seat limitation we he distubuted reverse
