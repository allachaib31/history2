#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use anyhow::Result;
use eframe::egui;
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


#[derive(Default)]
pub struct WebbookBot {
    // Form fields
    event_url: String,
    seats: String,
    d_seats: String,
    favorite_team: String,
    sections_status: String,

    // User data
    users: Vec<User>,
    proxies: Vec<String>,

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
    ready_scanner_users: Arc<Mutex<Vec<usize>>>, // Indices of ready users  
    next_user_index: Arc<AtomicUsize>,
    ws_connection: Option<Arc<Mutex<Option<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>>>>,
    show_transfer_modal: bool,
    transfer_from_user: String,
    transfer_to_user: String,
    show_logs_modal: bool,
    // In struct WebbookBot
    selected_user_index: Option<usize>,
    ready_receiver: Option<mpsc::UnboundedReceiver<usize>>, // ADD THIS LINE
    log_sender: Option<mpsc::UnboundedSender<(usize, String)>>,
    log_receiver: Option<mpsc::UnboundedReceiver<(usize, String)>>,
    telegram_sender: Option<mpsc::UnboundedSender<String>>, // --- ADD THIS ---
    transfer_ignore_list: Arc<Mutex<HashSet<String>>>,
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
    // In impl BotUser
    // In impl BotUser
    // In impl BotUser
    // In impl BotUser
    fn new(
        email: String,
        proxy_url: String,
        shared_data: Arc<RwLock<SharedData>>,
        token_manager: Arc<TokenManager>,
    ) -> Result<Self> {
        // --- START OF FINAL FIX ---
        // Use the constructor suggested by the compiler to avoid name collisions.
        let no_proxy = reqwest::NoProxy::from_string("localhost,127.0.0.1");
        
        let proxy = Proxy::all(&proxy_url)?
            .no_proxy(no_proxy);
        // --- END OF FINAL FIX ---

        let client = Client::builder()
            .proxy(proxy)
            .pool_idle_timeout(Some(std::time::Duration::from_secs(90)))
            .pool_max_idle_per_host(10) 
            .connect_timeout(std::time::Duration::from_secs(20)) 
            .timeout(std::time::Duration::from_secs(17))
            .tcp_keepalive(std::time::Duration::from_secs(60))
            .build()?;

        Ok(Self {
            email,
            proxy_url,
            client,
            shared_data,
            token_manager,
            webook_hold_token: Arc::new(Mutex::new(None)),
            expire_time: Arc::new(AtomicUsize::new(0)),
            browser_id: Arc::new(Mutex::new(None)),
            held_seats: Arc::new(Mutex::new(Vec::new())),
            prepared_request: Arc::new(Mutex::new(None)),
        })
    }
    // In impl BotUser
    async fn find_seats(&self) -> Result<Value> {
        let hold_token = self.webook_hold_token.lock().clone()
            .ok_or_else(|| anyhow::anyhow!("Hold token not available for findSeats"))?;
        
        let held_seats = self.held_seats.lock().clone();
        
        let payload = json!({
            "SeatsTaking": held_seats,
            "holdToken": hold_token,
        });

        let resp = self.client.post("http://localhost:3000/findSeats")
            .json(&payload)
            .send().await?;

        // The response from /findSeats is an array of objects, each with a "seat" key.
        // We need to extract the value of each "seat" key into a new array.
        let seat_info_array: Vec<Value> = resp.json().await?;
        let selected_seats: Vec<Value> = seat_info_array.into_iter()
            .filter_map(|v| v.get("seat").cloned())
            .collect();

        // Convert the final array back to a serde_json::Value
        Ok(serde_json::to_value(selected_seats)?)
    }
    async fn prepare_take_request(
        &self,
        seat: &str,
        base_headers: &reqwest::header::HeaderMap,
    ) -> Result<(String, reqwest::header::HeaderMap, String)> {
        let hold_token = self.webook_hold_token.lock().clone()
            .ok_or_else(|| anyhow::anyhow!("No hold token available"))?;

        let shared_data = self.shared_data.read().await;
        let event_keys = if let Some(key) = &shared_data.event_key {
            vec![key.clone()]
        } else {
            return Err(anyhow::anyhow!("Event key not available"));
        };
        
        // This channel key logic is duplicated from take_single_seat for correctness.
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
                        if let Some(team_keys) = channel_key_obj.get(team_id).and_then(|v| v.as_array()) {
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
            "channelKeys": final_channel_keys,
            "validateEventsLinkedToSameChart": true
        });

        let request_body_str = request_body.to_string();
        let signature = self.generate_signature(&request_body_str).await?;

        let mut headers = base_headers.clone();
        headers.insert("x-signature", signature.parse()?);

        let url = format!(
            "https://cdn-eu.seatsio.net/system/public/{}/events/groups/actions/hold-objects",
            WORKSPACE_KEY
        );

        Ok((url, headers, request_body_str))
    }

    // NEW FUNCTION: Pre-builds everything except the seat ID.
    // Call this function once after the user's hold token is acquired.
    async fn prepare_request_template(&self, channel_keys: Vec<String>) -> Result<PreparedSeatRequest> {
        let hold_token = self.webook_hold_token.lock().clone()
        .ok_or_else(|| anyhow::anyhow!("No hold token"))?;

        let shared_data = self.shared_data.read().await;
        
        // BUILD CHANNEL KEYS PROPERLY HERE
        let mut final_channel_keys = vec!["NO_CHANNEL".to_string()];
        
        // Add common channel keys
        if let Some(common_keys) = &shared_data.channel_key_common {
            final_channel_keys.extend(common_keys.clone());
        }
        
        // Add team-specific keys based on favorite team
        if let Some(favorite_team) = &shared_data.favorite_team {
            if let Some(channel_key_obj) = &shared_data.channel_key {
                let team_to_use = match favorite_team.as_str() {
                    "home" => &shared_data.home_team,
                    "away" => &shared_data.away_team,
                    _ => &shared_data.home_team,
                };
                
                if let Some(team) = team_to_use {
                    if let Some(team_id) = team.get("_id").and_then(|v| v.as_str()) {
                        if let Some(team_keys) = channel_key_obj.get(team_id).and_then(|v| v.as_array()) {
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
        
        // Use final_channel_keys instead of the parameter
        let request_body_template = json!({
            "events": if let Some(event_key) = &shared_data.event_key {
                vec![event_key.clone()]
            } else {
                return Err(anyhow::anyhow!("No event key available"));
            },
            "holdToken": hold_token,
            "objects": [{"objectId": "__SEAT_ID_PLACEHOLDER__"}],
            "channelKeys": final_channel_keys,  // USE THE PROPERLY BUILT KEYS
            "validateEventsLinkedToSameChart": true
        });
    
        let json_payload_template = request_body_template.to_string();
        println!("{:?}",json_payload_template);
        
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("accept", "*/*".parse()?);
        headers.insert("content-type", "application/json".parse()?);
        headers.insert("x-client-tool", "Renderer".parse()?);
        if let Some(browser_id) = self.browser_id.lock().as_ref() {
            headers.insert("x-browser-id", browser_id.parse()?);
        }
    
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

    // ULTRA-FAST execution - no building, just fire!
    

    async fn release_seat(&self, seat_id: &str, event_keys: &[String]) -> Result<bool> {
        let hold_token = self.webook_hold_token.lock().clone()
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
        headers.insert("user-agent", "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36".parse()?);
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
    
        let response = self.client
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
    ) -> Result<()> {
        if seats_to_retake.is_empty() {
            return Ok(());
        }

        println!("User {}: Starting ULTRA-FAST retake for {} seats", self.email, seats_to_retake.len());

        // Create a collection of tasks, one for each seat.
        let tasks = seats_to_retake
            .into_iter()
            .map(|seat| {
                // Clone all necessary data for the new concurrent task.
                let bot_user_clone = self.clone();
                let assigned_seats_sender_clone = assigned_seats_sender.clone();
                let seat_id_clone = seat.clone();

                // Spawn a new task for this single seat.
                tokio::spawn(async move {
                    // Use the same ultra-fast path as the scanner
                    match take_seat_direct_final(&bot_user_clone, &seat_id_clone).await {
                        Ok(_) => {
                            // Send successful seat to UI thread
                            assigned_seats_sender_clone.send((user_index, seat_id_clone)).ok();
                            println!("🚀 RETAKE SUCCESS: {} by {}", seat, bot_user_clone.email);
                        }
                        Err(e) => {
                            println!("✗ RETAKE FAILED for {}: {}", seat, e);
                        }
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

        log_sender.send((user_index, format!("🎯 Starting to take {} seats manually...", target_seats.len()))).ok();


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
                    match bot_user_clone.take_single_seat(&seat, &channel_keys_clone, &event_keys_clone, &base_headers_clone).await {
                        Ok(true) => {
                            assigned_seats_sender_clone.send((user_index, seat.clone())).ok();
                            log_sender_clone.send((user_index, format!("✅ Manual Take: {}", seat))).ok(); // ADD THIS
                        }
                        Ok(false) => {
                            log_sender_clone.send((user_index, format!("✗ Seat Not Available: {}", seat))).ok(); // ADD THIS
                        }
                        Err(e) => {
                            log_sender_clone.send((user_index, format!("❌ Error taking {}: {}", seat, e))).ok(); // ADD THIS
                        }
                    }
                })
            }).collect::<Vec<_>>();// Collect all the spawned tasks into a vector.


            futures::future::join_all(tasks).await;
            log_sender.send((user_index, "🏁 All manual seat attempts completed.".to_string())).ok();
            Ok(())
    }

    async fn take_single_seat(
        &self,
        seat: &str,
        channel_keys: &[String],  // This parameter can be ignored now
        event_keys: &[String],
        base_headers: &reqwest::header::HeaderMap,
    ) -> Result<bool> {
        let hold_token = self.webook_hold_token.lock().clone()
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
                        if let Some(team_keys) = channel_key_obj.get(team_id).and_then(|v| v.as_array()) {
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
}

async fn take_seat_direct_final(bot_user: &BotUser, seat_id: &str) -> Result<()> {
    // 1. Get the pre-built template. This lock is extremely short.
    let prepared = bot_user.prepared_request.lock().clone()
        .ok_or_else(|| anyhow::anyhow!("Request template not ready"))?;

    // 2. Create the final payload with a fast string replacement. NO JSON serialization.
    let final_payload = prepared.json_payload_template.replace("__SEAT_ID_PLACEHOLDER__", seat_id);
    //println!("{:?}",final_payload);
    // 3. Generate signature (very fast CPU-bound task).
    let signature = bot_user.generate_signature(&final_payload).await?;
    //println!("{:?}",signature);
    // 4. Clone headers and add the final signature.
    let mut headers = prepared.headers.clone();
    headers.insert("x-signature", signature.parse()?);

    // 5. FIRE THE REQUEST!
    let response = prepared.client
        .post(&prepared.url)
        .headers(headers)
        .body(final_payload) // Use the final string payload
        .send()
        .await?;

    // 6. Check status ONLY. Do not wait for `.json()`.
    let status = response.status();
    //let data: Value = response.json().await?;
    //println!("{:?}",data);

    if status == 200 || status == 204 {
        // Success! Don't parse the body, just update state.
        bot_user.held_seats.lock().push(seat_id.to_string());
        Ok(())
    } else {
        // The request failed. Now it's safe to read the body for error details.
        let error_body = response.text().await.unwrap_or_else(|_| "Could not read error body".to_string());
        println!("Error Body: {}", error_body);
        Err(anyhow::anyhow!("Seat take failed with status: {}. Body: {}", status, error_body))
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

    Ok(segments[segments.len() - 2].to_string())
}

impl BotManager {
    async fn new(
        users_data: Vec<(String, String)>,
        event_url: String,
        shared_data: Arc<RwLock<SharedData>>,
    ) -> Result<Self> {
        let mut token_manager = TokenManager::new();
        if let Err(e) = token_manager.load_from_file("tokens.json") {
            println!("Warning: Could not load tokens.json: {}", e);
            // Continue without tokens file
        }
        let token_manager = Arc::new(token_manager);

        let mut users = Vec::new();
        for (email, proxy) in users_data {
            let user = BotUser::new(email, proxy, shared_data.clone(), token_manager.clone())?;
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
            .timeout(std::time::Duration::from_secs(90)) // Generous 90-second timeout
            .build()?;
        // --- END OF FIX ---

        // Use the new local_client for this request instead of the user's client
        let resp = local_client.get("http://localhost:3000/extractcaptcha").send().await?;
        
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
            Err(anyhow::anyhow!("Failed to get site key: {}", data.error.unwrap_or_default()))
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
        println!("send_event_detail_request");

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

        //println!("Got response with status: {}", response.status());
        let data: Value = response.json().await?;

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
        shared_data.channels = Some(all_objects);
        //println!("Rendering info: {:?}", shared_data);
        Ok(())
    }
    // In impl BotManager
    // In impl BotManager
    async fn start_bot(
        &self,
        status_sender: mpsc::UnboundedSender<String>,
        log_sender: mpsc::UnboundedSender<(usize, String)>,
        channel_keys: Vec<String>,
    ) -> Result<()> {
        let mut user_tasks = Vec::new();

        for (user_index, user) in self.users.iter().enumerate() {
            user.generate_browser_id();
            
            let user_clone = user.clone();
            let log_sender_clone = log_sender.clone();
            let channel_keys_clone = channel_keys.clone();
            let status_sender_clone = status_sender.clone(); // --- THIS IS THE FIX ---

            let task = tokio::spawn(async move {
                log_sender_clone.send((user_index, "🚀 Bot started".to_string())).ok();

                for attempt in 1..=3 {
                    log_sender_clone.send((user_index, format!("Attempt {} - Getting hold token", attempt))).ok();
                        
                    match user_clone.get_webook_hold_token().await {
                        Ok(_) => {
                            log_sender_clone.send((user_index, "✅ Hold token received.".to_string())).ok();
                            
                            if user_clone.get_expire_time().await.is_err() {
                                log_sender_clone.send((user_index, "❌ Error getting expire time.".to_string())).ok();
                            } else {
                                log_sender_clone.send((user_index, "✅ Session verified.".to_string())).ok();
                                
                                match user_clone.prepare_request_template(channel_keys_clone.clone()).await {
                                    Ok(template) => {
                                        *user_clone.prepared_request.lock() = Some(template);
                                        log_sender_clone.send((user_index, "✅ Request template prepared.".to_string())).ok();
                                    },
                                    Err(e) => {
                                        log_sender_clone.send((user_index, format!("❌ FAILED to prepare template: {}", e))).ok();
                                    }
                                }
                                
                                // Use the cloned sender here
                                status_sender_clone.send(format!("USER_READY:{}", user_index)).ok();
                                return; // Exit loop on success
                            }
                        }
                        Err(e) => {
                            log_sender_clone.send((user_index, format!("❌ Attempt {} failed: {}", attempt, e))).ok();
                        }
                    }
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
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
        let (telegram_sender, telegram_receiver) = mpsc::unbounded_channel(); // --- ADD THIS ---
    
        // --- ADD THIS BLOCK to start the Telegram client ---
        rt.spawn(async move {
            match TelegramManager::new(telegram_receiver).await {
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
            event_url: "https://webook.com/en/events/spl-25-26-al-fateh".to_string(),
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
            users: vec![User {
                email: "alzeen.42@outlook.com".to_string(),
                user_type: "*".to_string(),
                password: "Bb654321@".to_string(),
                proxy: "http://spurs72ctg:5kolK...".to_string(),
                ..Default::default()
            }],
            proxies: vec![],
            rt: Some(rt),
            bot_manager: None,
            status_receiver: None,
            selected_sections: HashMap::new(),
            shared_data: None, // ADD THIS LINE
            bot_manager_receiver: None,
            expire_receiver: None,         // This exists
            assigned_seats_receiver: None, // This exists
            assigned_seats_sender: None,
            ready_receiver: None,
            ctx: None,           
            scanner_running: false,
            reserved_seats_map: Arc::new(Mutex::new(HashMap::new())),
            ready_scanner_users: Arc::new(Mutex::new(Vec::new())), // <-- Corrected line
            next_user_index: Arc::new(AtomicUsize::new(0)),
            ws_connection: None,
            show_transfer_modal: false,
            selected_user_index: None,
            transfer_from_user: String::new(),
            transfer_to_user: String::new(),
            show_logs_modal: false,
            log_sender: Some(log_sender),
            log_receiver: Some(log_receiver),
            telegram_sender: Some(telegram_sender),
            transfer_ignore_list: Arc::new(Mutex::new(HashSet::new())), 
        };
        app
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
            // Get current time in HH:MM:SS format
            let now = std::time::SystemTime::now();
            let since_epoch = now.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
            let secs = since_epoch.as_secs();
            let timestamp_str = format!(
                "{:02}:{:02}:{:02}",
                (secs / 3600) % 24,
                (secs / 60) % 60,
                secs % 60
            );

            let log_entry = format!("[{}] {}", timestamp_str, message);
            user.logs.push(log_entry);
        }
    }

    
    // In impl WebbookBot
    // This is a new helper function to avoid duplicating code.
    async fn dispatch_seat_take_task(
        seat_id: &str,
        log_prefix: &str, // e.g., "SNIPER" or "SCANNER"
        bot_manager: &Option<Arc<BotManager>>,
        ready_scanner_users: &Arc<Mutex<Vec<usize>>>,
        next_user_index: &Arc<AtomicUsize>,
        assigned_seats_sender: &mpsc::UnboundedSender<(usize, String)>,
        log_sender: &mpsc::UnboundedSender<(usize, String)>,
        telegram_sender: &mpsc::UnboundedSender<String>,
        ui_users: &Vec<User>,
    ) {
        if let Some(manager) = bot_manager {
            let ready_users_indices = ready_scanner_users.lock();
            let num_ready = ready_users_indices.len();
            if num_ready == 0 { return; }

            for _ in 0..num_ready {
                let current_atomic_index = next_user_index.fetch_add(1, Ordering::Relaxed);
                let user_pool_index = current_atomic_index % num_ready;
                let actual_user_index = ready_users_indices[user_pool_index];

                if let Some(ui_user) = ui_users.get(actual_user_index) {
                    let held_count = manager.users[actual_user_index].held_seats.lock().len();
                    
                    let is_eligible = if ui_user.user_type == "*" {
                        ui_user.max_seats == 0 || held_count < ui_user.max_seats
                    } else if ui_user.user_type == "+" {
                        ui_user.max_seats > 0 && held_count < ui_user.max_seats
                    } else { false };
                    
                    if is_eligible {
                        let bot_user = manager.users[actual_user_index].clone();
                        let sender_clone = assigned_seats_sender.clone();
                        let log_sender_clone = log_sender.clone();
                        let telegram_sender_clone = telegram_sender.clone();
                        let seat_id_clone = seat_id.to_string();
                        let log_prefix_clone = log_prefix.to_string();
                        
                        tokio::spawn(async move {
                            match take_seat_direct_final(&bot_user, &seat_id_clone).await {
                                Ok(_) => {
                                    sender_clone.send((actual_user_index, seat_id_clone.clone())).ok();
                                    let log_msg = format!("🚀 {}: Booked {}", log_prefix_clone, seat_id_clone);
                                    log_sender_clone.send((actual_user_index, log_msg.clone())).ok();
                                    let notification = format!("✅ Seat Taken! ({})\nAccount: {}\nSeat: {}", log_prefix_clone, bot_user.email, seat_id_clone);
                                    telegram_sender_clone.send(notification).ok();
                                }
                                Err(e) => {
                                    log_sender_clone.send((actual_user_index, format!("✗ {} FAILED for {}: {}", log_prefix_clone, seat_id_clone, e))).ok();
                                }
                            }
                        });
                        return;
                    }
                }
            }
        }
    }
    
    // In impl WebbookBot
    // In impl WebbookBot
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
                                        .desired_width(f32::INFINITY)
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

    fn show_transfer_modal(&mut self, ctx: &egui::Context) {
        let mut close_modal = false;
        let mut transfer_details: Option<(usize, usize)> = None;

        egui::Window::new("Transfer Seats")
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                // User selection logic...
                let star_users: Vec<(usize, String)> = self.users.iter().enumerate()
                    .filter(|(_, u)| u.user_type == "*")
                    .map(|(i, u)| (i, u.email.clone()))
                    .collect();
                
                let plus_users: Vec<(usize, String)> = self.users.iter().enumerate()
                    .filter(|(_, u)| u.user_type == "+")
                    .map(|(i, u)| (i, u.email.clone()))
                    .collect();

                // Dropdowns for selection
                ui.horizontal(|ui| {
                    ui.label("From (*) user:");
                    egui::ComboBox::from_id_source("from_user")
                        .selected_text(self.transfer_from_user.clone())
                        .show_ui(ui, |ui| {
                            for (idx, email) in &star_users {
                                if ui.selectable_label(self.transfer_from_user == *email, email).clicked() {
                                    self.transfer_from_user = email.clone();
                                }
                            }
                        });
                });

                ui.horizontal(|ui| {
                    ui.label("To (+) user:");
                    egui::ComboBox::from_id_source("to_user")
                        .selected_text(self.transfer_to_user.clone())
                        .show_ui(ui, |ui| {
                            for (idx, email) in &plus_users {
                                if ui.selectable_label(self.transfer_to_user == *email, email).clicked() {
                                    self.transfer_to_user = email.clone();
                                }
                            }
                        });
                });

                ui.separator();
                
                // Button logic
                ui.horizontal(|ui| {
                    if ui.button("Transfer").clicked() {
                         let from_index = self.users.iter().position(|u| u.email == self.transfer_from_user);
                         let to_index = self.users.iter().position(|u| u.email == self.transfer_to_user);
            
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
            self.execute_transfer(from_idx, to_idx);
        }
    
        if close_modal {
            self.show_transfer_modal = false;
            self.transfer_from_user.clear();
            self.transfer_to_user.clear();
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
                } else { return; }
            } else { return; };

            let assigned_seats_sender = match &self.assigned_seats_sender {
                Some(sender) => sender.clone(),
                None => { println!("Error: assigned_seats_sender not initialized."); return; }
            };
            let log_sender = match &self.log_sender {
                Some(sender) => sender.clone(),
                None => return,
            };

            if let Some(rt) = &self.rt {
                for i in 0..self.users.len() {
                    let user = &self.users[i];
                    if user.user_type == "*" && !user.target_seats.is_empty() {
                        if let Some(bot_user) = bot_manager.users.get(i) {
                            let bot_user_clone = bot_user.clone();
                            let target_seats = user.target_seats.clone();
                            let channel_keys_clone = channel_keys.clone();
                            let event_keys_clone = event_keys.clone();
                            let assigned_sender = assigned_seats_sender.clone();
                            let log_sender_clone = log_sender.clone();
                            
                            rt.spawn(async move {
                                // --- THIS IS THE FIX ---
                                // We handle the Result from the function inside the task
                                if let Err(e) = bot_user_clone.fill_assigned_seats(
                                    target_seats,
                                    channel_keys_clone,
                                    event_keys_clone,
                                    mpsc::unbounded_channel().0,
                                    assigned_sender,
                                    log_sender_clone,
                                    i,
                                ).await {
                                    println!("Error in fill_assigned_seats for user {}: {}", bot_user_clone.email, e);
                                }
                            });
                        }
                    }
                }
            }
        }
    }
    
    fn get_seats_from_selected_sections(&self) -> Vec<String> {
        let mut all_seats = Vec::new();

        if let Some(shared_data_arc) = &self.shared_data {
            if let Ok(shared_data) = shared_data_arc.try_read() {
                if let Some(channels) = &shared_data.channels {
                    // Get selected section prefixes
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

                    // Find all seats that start with selected sections
                    for seat in channels {
                        for selected_section in &selected_sections {
                            if seat.starts_with(selected_section) {
                                all_seats.push(seat.clone());
                            }
                        }
                    }
                }
            }
        }

        all_seats
    }

    fn distribute_seats_to_users(&mut self) {
        let available_seats = self.get_seats_from_selected_sections();
        let total_seats = available_seats.len();

        let star_users: Vec<(usize, &mut User)> = self
            .users
            .iter_mut()
            .enumerate()
            .filter(|(_, user)| user.user_type == "*")
            .collect();

        let user_count = star_users.len();

        if total_seats == 0 || user_count == 0 {
            println!("No seats available or no users with type '*'");
            return;
        }

        let seats_per_user: usize = self.seats.parse().unwrap_or(1);
        let actual_seats_per_user = if total_seats >= user_count * seats_per_user {
            seats_per_user
        } else {
            total_seats / user_count
        };

        println!(
            "Available seats: {}, Users with '*' type: {}, Each user gets: {}",
            total_seats, user_count, actual_seats_per_user
        );

        // Set target seats (what they should attempt to take)
        for (user_index, (_, user)) in star_users.into_iter().enumerate() {
            let start_index = user_index * actual_seats_per_user;
            let end_index =
                std::cmp::min(start_index + actual_seats_per_user, available_seats.len());

            if start_index < available_seats.len() {
                let target_seats: Vec<String> = available_seats[start_index..end_index].to_vec();
                user.target_seats = target_seats.clone();

                println!(
                    "User {}: will attempt {} seats",
                    user.email,
                    target_seats.len()
                );
            } else {
                user.target_seats.clear();
            }
        }

        // Reset for "+" type users
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

                    // Update status based on expire time
                    if expire_time == 0 {
                        self.users[user_index].status = "Expired".to_string();
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
                    // --- START OF FIX ---
                    if seat_msg.is_empty() {
                        // Clear all seats (for token expiration)
                        self.users[user_index].held_seats.clear();
                    } else if let Some(seat_to_remove) = seat_msg.strip_prefix("REMOVE:") {
                        // Remove a single seat (for transfers)
                        self.users[user_index].held_seats.retain(|s| s != seat_to_remove);
                    } else {
                        // Add a single seat (for normal takes)
                        self.users[user_index].held_seats.push(seat_msg);
                    }
                    // Update the display count string
                    self.users[user_index].seats = self.users[user_index].held_seats.len().to_string();
                    // --- END OF FIX ---
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
                                user.proxy = if !self.proxies.is_empty() {
                                    self.proxies[new_users.len() % self.proxies.len()].clone()
                                } else {
                                    "No proxy".to_string()
                                };
                                user.status = "Ready".to_string();
                                user.expire = "0".to_string();
                                user.seats = "0".to_string();
                                user.last_update = "10:37:47".to_string();
                                user.pay_status = "Pay".to_string();
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

                    // Update user proxies
                    for (i, user) in self.users.iter_mut().enumerate() {
                        if !self.proxies.is_empty() {
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
    
        // Set seat limits for each user type
        let star_user_limit: usize = self.seats.parse().unwrap_or(0);
        let plus_user_limit: usize = self.d_seats.parse().unwrap_or(0);
        for user in self.users.iter_mut() {
            if user.user_type == "*" {
                user.max_seats = star_user_limit;
            } else if user.user_type == "+" {
                user.max_seats = plus_user_limit;
            }
        }
    
        // Prepare data and channels for the background tasks
        let users_data: Vec<(String, String)> =
            self.users.iter().map(|u| (u.email.clone(), u.proxy.clone())).collect();
        let event_url = self.event_url.clone();
    
        // Create the single, shared channel for all seat updates
        let (assigned_seats_sender, assigned_seats_receiver) = mpsc::unbounded_channel();
        self.assigned_seats_sender = Some(assigned_seats_sender);
        self.assigned_seats_receiver = Some(assigned_seats_receiver);
    
        // Create other communication channels
        let (status_sender, status_receiver) = mpsc::unbounded_channel();
        let (expire_sender, expire_receiver) = mpsc::unbounded_channel::<(usize, usize)>();
        self.status_receiver = Some(status_receiver);
        self.expire_receiver = Some(expire_receiver);
    
        if let Some(rt) = &self.rt {
            // --- Clone all necessary data BEFORE the main async block ---
            let assigned_seats_sender_for_task = self.assigned_seats_sender.as_ref().unwrap().clone();
            let log_sender_for_task = self.log_sender.as_ref().unwrap().clone();
            let shared_data = Arc::new(RwLock::new(SharedData::default()));
            self.shared_data = Some(shared_data.clone());
            let favorite_team = self.favorite_team.clone();
    
            // Prepare shared data in a separate, non-blocking task
            rt.spawn({
                let shared_data = shared_data.clone();
                async move {
                    shared_data.write().await.favorite_team = Some(favorite_team);
                }
            });
    
            let (bot_sender, bot_receiver) = mpsc::unbounded_channel();
            self.bot_manager_receiver = Some(bot_receiver);
            let channel_keys = self.build_channel_keys();
    
            // This is the main background task for the bot
            rt.spawn(async move {
                match BotManager::new(users_data, event_url.clone(), shared_data).await {
                    Ok(bot_manager) => {
                        let bot_manager_arc = Arc::new(bot_manager);
                        bot_sender.send(bot_manager_arc.clone()).ok();
    
                        // --- SEQUENTIAL STARTUP LOGIC ---
                        log_sender_for_task.send((0, "📊 Extracting chart data...".to_string())).ok();
                        if let Err(e) = bot_manager_arc.extract_seatsio_chart_data().await {
                            log_sender_for_task.send((0, format!("❌ Error extracting chart data: {}", e))).ok();
                            return;
                        }
    
                        if let Ok(event_id) = extract_event_key(&event_url) {
                            log_sender_for_task.send((0, format!("🗓️ Getting details for event: {}...", event_id))).ok();
                            if let Err(e) = bot_manager_arc.send_event_detail_request(&event_id).await {
                                log_sender_for_task.send((0, format!("❌ Error getting event details: {}", e))).ok();
                                return;
                            }
                        } else {
                            log_sender_for_task.send((0, "❌ Could not extract event key from URL.".to_string())).ok();
                            return;
                        }
    
                        log_sender_for_task.send((0, "ℹ️ Getting rendering info...".to_string())).ok();
                        if let Err(e) = bot_manager_arc.get_rendering_info().await {
                            log_sender_for_task.send((0, format!("❌ Error getting rendering info: {}", e))).ok();
                            return;
                        }
                        log_sender_for_task.send((0, "✅ All event data loaded successfully.".to_string())).ok();
                        // --- ADD THIS BLOCK ---
                        log_sender_for_task.send((0, "🔑 Attempting to get reCAPTCHA site key...".to_string())).ok();
                        if let Err(e) = bot_manager_arc.extract_recaptcha_site_key().await {
                            // Log a WARNING instead of an error, and DO NOT return.
                            log_sender_for_task.send((0, format!("⚠️ Could not get site key: {}. Will try again on payment.", e))).ok();
                        } else {
                            log_sender_for_task.send((0, "✅ Site key received and cached.".to_string())).ok();
                        }
                        // --- END ---
                        // --- Start initializing users now that data is ready ---
                        let status_sender_clone = status_sender.clone();
                        let log_sender_clone = log_sender_for_task.clone();
                        if let Err(e) = bot_manager_arc.start_bot(status_sender, log_sender_clone, channel_keys.clone()).await {
                            println!("Bot error: {}", e);
                        }
                        
                        // --- CONCURRENT EXPIRATION LOOP ---
                        loop {
                            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                            for (i, user) in bot_manager_arc.users.iter().enumerate() {
                                let expire_time = user.expire_time.load(Ordering::Relaxed);
    
                                if expire_time > 0 {
                                    let remaining = expire_time.saturating_sub(1);
                                    user.expire_time.store(remaining, Ordering::Relaxed);
                                    expire_sender.send((i, remaining)).ok();
                                } else if expire_time == 0 {
                                    // Set placeholder to prevent re-triggering while task runs
                                    user.expire_time.store(600, Ordering::Relaxed);
    
                                    // Clone all data needed for the independent background task
                                    let user_clone = user.clone();
                                    let assigned_seats_sender_clone = assigned_seats_sender_for_task.clone();
                                    let status_sender_clone_task = status_sender_clone.clone();
                                    let log_sender_clone_task = log_sender_for_task.clone();
                                    let bot_manager_clone = bot_manager_arc.clone();
                                    let channel_keys_clone = channel_keys.clone();
                                    let user_index = i;
    
                                    // Launch the entire refresh-and-retake process in the background
                                    tokio::spawn(async move {
                                        let previously_held = {
                                            let mut held = user_clone.held_seats.lock();
                                            let seats = held.clone();
                                            held.clear();
                                            seats
                                        };
    
                                        log_sender_clone_task.send((user_index, format!("⏰ Token expired. Refreshing for {} seats...", previously_held.len()))).ok();
                                        assigned_seats_sender_clone.send((user_index, "".to_string())).ok();
    
                                        if user_clone.get_webook_hold_token().await.is_ok() {
                                            user_clone.expire_time.store(600, Ordering::Relaxed);
                                            log_sender_clone_task.send((user_index, "✅ New token acquired.".to_string())).ok();
    
                                            if let Ok(template) = user_clone.prepare_request_template(channel_keys_clone.clone()).await {
                                                *user_clone.prepared_request.lock() = Some(template);
                                            }
    
                                            if !previously_held.is_empty() {
                                                let event_keys = if let Some(key) = bot_manager_clone.shared_data.read().await.event_key.as_ref() {
                                                    vec![key.clone()]
                                                } else { return; };
                                                
                                                user_clone.retake_seats(
                                                    previously_held,
                                                    channel_keys_clone,
                                                    event_keys,
                                                    assigned_seats_sender_clone,
                                                    user_index,
                                                ).await.ok();
                                            }
                                        } else {
                                            log_sender_clone_task.send((user_index, "❌ FAILED to get new token.".to_string())).ok();
                                            user_clone.expire_time.store(0, Ordering::Relaxed); // Allow re-try
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

    fn render_top_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            // File loading buttons
            if ui.button("Load Users CSV").clicked() {
                self.load_users_csv();
            }

            if ui.button("Load Proxies").clicked() {
                self.load_proxies();
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

            // Display checkboxes - 10 per line
            // Display checkboxes in scrollable area
            
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
            .id_source("sections_scroll")  // ADD THIS LINE - unique ID
            .max_height(200.0)
            .show(ui, |ui| {
                // Use a grid for better layout control
                egui::Grid::new("sections_grid")
                    .num_columns(10)
                    .spacing([5.0, 5.0])
                    .show(ui, |ui| {
                        for (index, section) in sections_to_display.iter().enumerate() {
                            if let Some(is_selected) = self.selected_sections.get_mut(section) {
                                if ui.checkbox(is_selected, section).changed() {
                                    selection_changed = true;
                                }
                            }
                            
                            // End row after 10 items
                            if (index + 1) % 10 == 0 {
                                ui.end_row();
                            }
                        }
                    });
            });

            // If selection changed, redistribute seats
            // If selection changed, redistribute seats AND start scanner
            if selection_changed {
                println!("selection succed");
                self.distribute_seats_to_users();
                
                // Start scanner when first checkbox is selected
                let has_selected = self.selected_sections.values().any(|&selected| selected);
                if has_selected && !self.scanner_running && self.bot_running {
                    self.start_scanner();
                }else if !has_selected && self.scanner_running {
                    self.scanner_running = false; // Stop if no sections selected
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
            // Updated condition to include both '*' and '+' users
            if (user.user_type == "*" || user.user_type == "+") && user.status == "Active" {
                ready_users.push(i);
            }
        }
    }
    // In impl WebbookBot
    // In impl WebbookBot
    // In impl WebbookBot
    fn render_user_table(&mut self, ui: &mut egui::Ui) {
        use egui_extras::{Column, TableBuilder};

        let mut pay_button_clicked_for: Option<usize> = None;
        let mut logs_button_clicked_for: Option<usize> = None;

        TableBuilder::new(ui)
            .striped(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::initial(200.0).resizable(true)) // Email
            .column(Column::initial(50.0).resizable(true))  // Type
            .column(Column::initial(180.0).resizable(true)) // Proxy
            .column(Column::initial(80.0).resizable(true))  // Status
            .column(Column::initial(60.0).resizable(true))  // Expire
            .column(Column::initial(60.0).resizable(true))  // Seats
            .column(Column::initial(90.0).resizable(true))  // Last Update
            .column(Column::initial(60.0).resizable(true))  // Pay
            .header(25.0, |mut header| {
                header.col(|ui| { ui.heading("Email"); });
                header.col(|ui| { ui.heading("Type"); });
                header.col(|ui| { ui.heading("Proxy"); });
                header.col(|ui| { ui.heading("Status"); });
                header.col(|ui| { ui.heading("Expire"); });
                header.col(|ui| { ui.heading("Seats"); });
                header.col(|ui| { ui.heading("Last Update"); });
                header.col(|ui| { ui.heading("Pay"); });
            })
            .body(|body| {
                let users = self.users.clone();
                body.rows(25.0, users.len(), |user_index, mut row| {
                    let user = &users[user_index];
                    
                    row.col(|ui| {
                        if ui.button(&user.email).clicked() {
                            logs_button_clicked_for = Some(user_index);
                        }
                    });
                    row.col(|ui| { ui.label(&user.user_type); });
                    row.col(|ui| { ui.label(&user.proxy); });
                    row.col(|ui| {
                        let color = match user.status.as_str() {
                            "Ready" => egui::Color32::GREEN,
                            "Error" => egui::Color32::RED,
                            "Active" => egui::Color32::BLUE,
                            _ => egui::Color32::GRAY,
                        };
                        ui.colored_label(color, &user.status);
                    });
                    row.col(|ui| { ui.label(&user.expire); });
                    row.col(|ui| { ui.label(&user.seats); });
                    row.col(|ui| { ui.label(&user.last_update); });
                    row.col(|ui| {
                        if ui.button("Pay").clicked() {
                            pay_button_clicked_for = Some(user_index);
                        }
                    });
                });
            });

        if let Some(user_index) = pay_button_clicked_for {
            self.handle_payment_click(user_index);
        }

        // --- START OF FIX ---
        if let Some(user_index) = logs_button_clicked_for {
            self.selected_user_index = Some(user_index); // Save the index
            self.show_logs_modal = true;                 // Show the modal
        }
        // --- END OF FIX ---
    }
    // In impl WebbookBot
    // In impl WebbookBot
    // In impl WebbookBot
    fn handle_payment_click(&mut self, user_index: usize) {
        if let (Some(bot_manager), Some(rt), Some(log_sender), Some(telegram_sender)) = 
            (&self.bot_manager, &self.rt, &self.log_sender, &self.telegram_sender) 
        {
            let bot_manager_clone = bot_manager.clone();
            let log_sender_clone = log_sender.clone();
            let telegram_sender_clone = telegram_sender.clone();

            log_sender_clone.send((user_index, "💳 Generating payment link...".to_string())).ok();

            rt.spawn(async move {
                // --- START OF NEW "CHECK-THEN-FETCH" LOGIC ---
                let site_key: String;

                // First, check if we already have a key cached in our shared data.
                let maybe_key = bot_manager_clone.shared_data.read().await.recaptcha_site_key.clone();

                if let Some(existing_key) = maybe_key {
                    // If the key exists, use it instantly.
                    log_sender_clone.send((user_index, "✅ Using cached reCAPTCHA site key.".to_string())).ok();
                    site_key = existing_key;
                } else {
                    // If no key is cached, ONLY NOW do we try to fetch it from localhost.
                    log_sender_clone.send((user_index, "🔑 No cached key. Getting reCAPTCHA site key from localhost...".to_string())).ok();
                    if let Err(e) = bot_manager_clone.extract_recaptcha_site_key().await {
                        // If localhost is down or fails, cancel the payment operation.
                        log_sender_clone.send((user_index, format!("❌ Could not get site key from localhost: {}", e))).ok();
                        telegram_sender_clone.send(format!("⚠️ Payment for {} failed. Could not connect to the local Node.js server.", bot_manager_clone.users[user_index].email)).ok();
                        return;
                    }
                    
                    // After a successful fetch, read the newly cached key.
                    site_key = bot_manager_clone.shared_data.read().await.recaptcha_site_key.clone().unwrap();
                    log_sender_clone.send((user_index, "✅ Site key received and cached for future use.".to_string())).ok();
                }
                // --- END OF NEW LOGIC ---

                // The rest of the payment logic continues from here, using the `site_key` variable.
                let bot_user = &bot_manager_clone.users[user_index];

                let captcha_solver = CaptchaSolver::new();
                let captcha_solution = match captcha_solver.solve("https://webook.com/", &site_key).await {
                    Ok(solution) => {
                        log_sender_clone.send((user_index, "✅ CAPTCHA solved.".to_string())).ok();
                        solution
                    },
                    Err(e) => {
                        log_sender_clone.send((user_index, format!("❌ CAPTCHA failed: {}", e))).ok();
                        return;
                    }
                };

                let selected_seats_value = match bot_user.find_seats().await {
                    Ok(seats) => {
                        log_sender_clone.send((user_index, "✅ Received seat data from local server.".to_string())).ok();
                        seats
                    },
                    Err(e) => {
                        log_sender_clone.send((user_index, format!("❌ /findSeats failed: {}", e))).ok();
                        return;
                    }
                };
                let selected_seats_json_string = selected_seats_value.to_string();

                let token_data = match bot_user.token_manager.get_valid_token(&bot_user.email) {
                    Some(token) => token,
                    None => { log_sender_clone.send((user_index, "❌ No valid token for payment.".to_string())).ok(); return; }
                };
                let event_id = match &bot_user.shared_data.read().await.event_id {
                    Some(id) => id.clone(),
                    None => { log_sender_clone.send((user_index, "❌ Event ID not found.".to_string())).ok(); return; }
                };

                let checkout_payload = json!({
                    "event_id": event_id,
                    "redirect": "https://webook.com/en/payment-success",
                    "redirect_failed": "https://webook.com/en/payment-failed",
                    "booking_source": "rs-web",
                    "lang": "en",
                    "payment_method": "credit_card",
                    "selectedSeats": selected_seats_json_string,
                    "captcha": captcha_solution,
                    "app_source": "rs"
                });

                let checkout_url = "https://api.webook.com/api/v2/event-seat/checkout?lang=en";
                let response = bot_user.client.post(checkout_url)
                    .header("Authorization", format!("Bearer {}", token_data.access_token))
                    .header("token", &token_data.token)
                    .json(&checkout_payload)
                    .send().await;

                match response {
                    Ok(resp) => {
                        let status = resp.status();
                        let body_text = resp.text().await.unwrap_or_default();
                        if status.is_success() {
                            if let Ok(data) = serde_json::from_str::<Value>(&body_text) {
                                if let Some(link) = data.get("data").and_then(|d| d.get("redirect_url")).and_then(|l| l.as_str()) {
                                    log_sender_clone.send((user_index, format!("✅ Payment link: {}", link))).ok();
                                    telegram_sender_clone.send(format!("💳 Payment Link for {}:\n{}", bot_user.email, link)).ok();
                                } else {
                                    log_sender_clone.send((user_index, format!("❌ Link not found in response: {}", body_text))).ok();
                                }
                            }
                        } else {
                            log_sender_clone.send((user_index, format!("❌ Checkout failed ({}): {}", status, body_text))).ok();
                        }
                    }
                    Err(e) => {
                        log_sender_clone.send((user_index, format!("❌ Checkout request error: {}", e))).ok();
                    }
                }
            });
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
                        self.update_ready_scanner_users_atomic();
                        
                        // Start scanner if conditions met
                        let has_selected_sections = self.selected_sections.values().any(|&selected| selected);
                        if has_selected_sections && !self.scanner_running {
                            self.start_scanner();
                        }
                    }
                }
            } else {
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
        if self.scanner_running || !self.bot_running || self.shared_data.is_none() { return; }
        let selected_channels = self.get_selected_channels();
        if selected_channels.is_empty() { return; }

        self.scanner_running = true;
        self.reserved_seats_map.lock().clear();
        self.update_ready_scanner_users_atomic();

        let assigned_seats_sender = self.assigned_seats_sender.as_ref().unwrap().clone();
        let log_sender = self.log_sender.as_ref().unwrap().clone();
        let telegram_sender = self.telegram_sender.as_ref().unwrap().clone();
        let reserved_seats_map_clone = self.reserved_seats_map.clone();
        let transfer_ignore_list_clone = self.transfer_ignore_list.clone(); // --- ADD THIS ---

        if let Some(rt) = &self.rt {
            let shared_data = self.shared_data.clone().unwrap();
            let bot_manager = self.bot_manager.clone();
            let users_data_clone = self.users.clone();
            let ready_scanner_users_clone = self.ready_scanner_users.clone();
            let next_user_index_clone = self.next_user_index.clone();
            
            rt.spawn(async move {
                if let Err(e) = Self::run_websocket_scanner(
                    shared_data, bot_manager, selected_channels, users_data_clone,
                    assigned_seats_sender, log_sender, telegram_sender,
                    ready_scanner_users_clone, next_user_index_clone,
                    reserved_seats_map_clone,
                    transfer_ignore_list_clone, // --- PASS IT HERE ---
                ).await {
                    println!("Scanner error: {}", e);
                }
            });
        }
    }

    fn get_selected_channels(&self) -> Vec<String> {
        self.selected_sections
            .iter()
            .filter_map(|(section, &is_selected)| {
                if is_selected {
                    Some(section.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    fn update_ready_star_users(&mut self) {
        let new_users: Vec<usize> = self.users
        .iter()
        .enumerate()
        .filter(|(_, user)| user.user_type == "*" && user.status == "Active")
        .map(|(i, _)| i)  // This is the actual index in self.users
        .collect();
        
        let mut ready_users = self.ready_scanner_users.lock();
        ready_users.clear();
        ready_users.extend(new_users);
    }

    // In impl WebbookBot
    async fn run_websocket_scanner(
        shared_data: Arc<RwLock<SharedData>>,
        bot_manager: Option<Arc<BotManager>>,
        selected_channels: Vec<String>,
        users_data: Vec<User>,
        assigned_seats_sender: mpsc::UnboundedSender<(usize, String)>,
        log_sender: mpsc::UnboundedSender<(usize, String)>,
        telegram_sender: mpsc::UnboundedSender<String>,
        ready_scanner_users: Arc<Mutex<Vec<usize>>>,
        next_user_index: Arc<AtomicUsize>,
        reserved_seats_map: Arc<Mutex<HashMap<String, Vec<String>>>>,
        transfer_ignore_list: Arc<Mutex<HashSet<String>>>, // Add this parameter
    ) -> Result<()> {
        loop { // The reconnect loop
            match Self::connect_and_listen(
                &shared_data, &bot_manager, &selected_channels, &users_data,
                &assigned_seats_sender, &log_sender, &telegram_sender,
                &ready_scanner_users, &next_user_index, &reserved_seats_map,
                &transfer_ignore_list, // Pass it down
            ).await {
                Ok(_) => { println!("WebSocket connection ended normally"); break; }
                Err(e) => {
                    println!("Scanner error: {}", e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                }
            }
        }
        Ok(())
    }
    // In impl WebbookBot
    // In impl WebbookBot
    async fn connect_and_listen(
        shared_data: &Arc<RwLock<SharedData>>,
        bot_manager: &Option<Arc<BotManager>>,
        selected_channels: &[String],
        users_data: &Vec<User>,
        assigned_seats_sender: &mpsc::UnboundedSender<(usize, String)>,
        log_sender: &mpsc::UnboundedSender<(usize, String)>,
        telegram_sender: &mpsc::UnboundedSender<String>,
        ready_scanner_users: &Arc<Mutex<Vec<usize>>>,
        next_user_index: &Arc<AtomicUsize>,
        reserved_seats_map: &Arc<Mutex<HashMap<String, Vec<String>>>>,
        transfer_ignore_list: &Arc<Mutex<HashSet<String>>>, // Add this parameter
    ) -> Result<()> {
        use sha1::{Sha1, Digest};
        use tokio_tungstenite::{connect_async, tungstenite::Message};
        use futures_util::{SinkExt, StreamExt};
    
        let ws_url = "wss://messaging-eu.seatsio.net/ws";
        let (ws_stream, _) = connect_async(ws_url).await?;
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
                Err(e) => { return Err(anyhow::anyhow!("Read error: {}", e)); }
            };

            if let Message::Text(text) = msg {
                if let Ok(messages) = serde_json::from_str::<Vec<ScannerMessage>>(&text) {
                    for scanner_msg in messages {
                        let body = scanner_msg.message.body;

                        // --- 1. FREE SEAT (Standard Scanner) ---
                        if body.status.as_deref() == Some("free") {
                            if let Some(ref seat_id) = body.object_label {
                                // --- THIS IS THE FIX ---
                                // Check the ignore list before trying to take a seat.
                                let is_ignored = transfer_ignore_list.lock().contains(seat_id);
                                if !is_ignored && selected_channels.iter().any(|prefix| seat_id.starts_with(prefix)) {
                                    Self::dispatch_seat_take_task(
                                        seat_id, "SCANNER", bot_manager, ready_scanner_users, 
                                        next_user_index, assigned_seats_sender, log_sender, telegram_sender, users_data
                                    ).await;
                                }
                            }
                        }

                        // --- 2. SPYING (Phase 1) ---
                        if body.status.as_deref() == Some("reservedByToken") {
                            // --- FIX: Add `ref` here as well to borrow the seat label ---
                            if let (Some(hash), Some(ref seat)) = (body.hold_token_hash, body.object_label) {
                                let mut map = reserved_seats_map.lock();
                                // We clone the borrowed `seat` so the map can own the String.
                                //println!("{} ==> {}", hash, seat);
                                map.entry(hash).or_default().push(seat.clone());
                            }
                        }

                        // --- 3. SNIPING (Phase 2) ---
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
                                    log_sender.send((0, format!("🎯 SNIPER: Token expired, found {} seats to take.", seats.len()))).ok();
                                    for seat_id in seats {
                                        print!("{}", seat_id);
                                        if selected_channels.iter().any(|prefix| seat_id.starts_with(prefix)) {
                                            Self::dispatch_seat_take_task(
                                                &seat_id, "SNIPER", bot_manager, ready_scanner_users, 
                                                next_user_index, assigned_seats_sender, log_sender, telegram_sender, users_data
                                            ).await;
                                        }
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
    

    // In impl WebbookBot
    async fn atomic_transfer_seat(
        from_user: &BotUser,
        to_user: &BotUser,
        seat_id: &str,
        prepared_request: (String, reqwest::header::HeaderMap, String),
        assigned_seats_sender: &mpsc::UnboundedSender<(usize, String)>,
        from_user_index: usize,
        to_user_index: usize,
    ) -> Result<bool> {
        let event_keys = if let Ok(data) = from_user.shared_data.try_read() {
            if let Some(event_key) = &data.event_key { vec![event_key.clone()] } 
            else { return Err(anyhow::anyhow!("Event key not found")); }
        } else { return Err(anyhow::anyhow!("Could not read shared data")); };

        // --- START OF NEW SEQUENTIAL LOGIC ---

        // Step 1: Send the release request and WAIT for the server to confirm it succeeded.
        let release_succeeded = from_user.release_seat(seat_id, &event_keys).await?;
        
        if !release_succeeded {
            println!("❌ Transfer FAILED for {}. Seat could not be released.", seat_id);
            return Ok(false); // Halt the entire transfer operation.
        }

        // Step 2: Only if the release was successful, immediately fire the 5 concurrent take requests.
        let mut take_handles = Vec::new();
        for i in 0..5 {
            let to_user_client = to_user.client.clone();
            let (url, headers, body) = (prepared_request.0.clone(), prepared_request.1.clone(), prepared_request.2.clone());
            take_handles.push(tokio::spawn(async move {
                to_user_client.post(&url).headers(headers).body(body).send().await
            }));
        }
        
        let take_results = futures::future::join_all(take_handles).await;
        
        // Step 3: Analyze the results of the take requests.
        let mut take_succeeded = false;
        for (i, res) in take_results.into_iter().enumerate() {
            match res {
                Ok(Ok(resp)) => {
                    if resp.status().is_success() {
                        if !take_succeeded {
                            println!("✅ Transfer take request #{} SUCCEEDED", i + 1);
                            take_succeeded = true;
                        }
                    } else {
                        let status = resp.status();
                        let body = resp.text().await.unwrap_or_default();
                        println!("❌ Transfer take request #{} FAILED with status {}: {}", i + 1, status, body);
                    }
                }
                Ok(Err(e)) => { println!("❌ Transfer take request #{} FAILED with network error: {}", i + 1, e); }
                Err(e) => { println!("❌ Transfer take request #{} FAILED with task error: {}", i + 1, e); }
            }
        }

        // Step 4: Update the UI and state based on the final outcome.
        if take_succeeded {
            to_user.held_seats.lock().push(seat_id.to_string());
            println!("✅ Safe Transfer SUCCESS for {}", seat_id);
            assigned_seats_sender.send((from_user_index, format!("REMOVE:{}", seat_id))).ok();
            assigned_seats_sender.send((to_user_index, seat_id.to_string())).ok();
            return Ok(true);
        } else {
            // Since the release succeeded but the take failed, the seat is lost.
            println!("❌ Safe Transfer FAILED for {}. Seat released but not recaptured.", seat_id);
            assigned_seats_sender.send((from_user_index, format!("REMOVE:{}", seat_id))).ok();
            return Ok(false);
        }
        // --- END OF NEW LOGIC ---
    }
    fn execute_transfer(&mut self, from_user_index: usize, to_user_index: usize) {
        let assigned_seats_sender = match &self.assigned_seats_sender {
            Some(sender) => sender.clone(),
            None => { println!("Cannot transfer: assigned_seats_sender is not available."); return; }
        };
        let ignore_list = self.transfer_ignore_list.clone();

        if let (Some(bot_manager), Some(rt)) = (&self.bot_manager, &self.rt) {
            let from_user = bot_manager.users[from_user_index].clone();
            let to_user = bot_manager.users[to_user_index].clone();
            let seats_to_transfer = from_user.held_seats.lock().clone();

            if seats_to_transfer.is_empty() {
                println!("No seats to transfer for user {}", from_user.email);
                return;
            }
            {
                let mut list = ignore_list.lock();
                for seat in &seats_to_transfer {
                    list.insert(seat.clone());
                }
                println!("Added {} seats to transfer ignore list.", seats_to_transfer.len());
            }
            let d_seats_limit: usize = self.d_seats.parse().unwrap_or(0);
            println!("Starting ULTRA-FAST transfer: {} seats from {} to {}", seats_to_transfer.len(), from_user.email, to_user.email);

            rt.spawn(async move {
                let mut prepared_requests = HashMap::new();
                let mut base_headers = reqwest::header::HeaderMap::new();
                base_headers.insert("content-type", "application/json".parse().unwrap());
                if let Some(browser_id) = to_user.browser_id.lock().as_ref() {
                    base_headers.insert("x-browser-id", browser_id.parse().unwrap());
                }

                for seat_id in &seats_to_transfer {
                    if let Ok(req) = to_user.prepare_take_request(seat_id, &base_headers).await {
                        prepared_requests.insert(seat_id.clone(), req);
                    }
                }
                
                for seat_id in seats_to_transfer {
                    // This check correctly respects the target's seat limit.
                    let to_user_seat_count = to_user.held_seats.lock().len();
                    if d_seats_limit > 0 && to_user_seat_count >= d_seats_limit {
                        println!("Target user {} reached seat limit of {}. Halting transfer.", to_user.email, d_seats_limit);
                        break;
                    }
                    if let Some(prepared) = prepared_requests.remove(&seat_id) {
                        // --- START OF FIX ---
                        // This `match` block now handles all types of failures and stops the loop.
                        match Self::atomic_transfer_seat(
                            &from_user, &to_user, &seat_id, prepared, 
                            &assigned_seats_sender, from_user_index, to_user_index
                        ).await {
                            Ok(true) => {
                                // Success, continue to the next seat.
                            }
                            Ok(false) => {
                                // Soft failure (e.g., restore failed), stop the entire transfer.
                                println!("Atomic transfer failed for seat {}. Halting.", seat_id);
                                break;
                            }
                            Err(e) => {
                                // Hard failure (e.g., network error), stop the entire transfer.
                                println!("Error during atomic transfer for seat {}: {}. Halting.", seat_id, e);
                                break;
                            }
                        }
                        // --- END OF FIX ---
                    }
                }
                println!("Transfer process complete for {} -> {}", from_user.email, to_user.email);
                ignore_list.lock().clear();
                println!("Transfer ignore list cleared.");
            });
        }
    }
}

impl eframe::App for WebbookBot {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Set dark theme
        ctx.set_visuals(egui::Visuals::light());
        self.ctx = Some(ctx.clone());
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.style_mut().spacing.item_spacing = egui::vec2(8.0, 8.0);
    
            // Top panel with controls
            self.render_top_panel(ui);
    
            ui.add_space(10.0);
    
            // Main table
            egui::ScrollArea::both()
                .auto_shrink([false, true])
                .id_source("table_scroll")  // ADD THIS LINE - unique ID
                .max_height(400.0)
                .max_width(1000.0)
                .show(ui, |ui| {
                    self.render_user_table(ui);
                });
    
            // Status bar at bottom
            ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                self.render_status_bar(ui);
            });
        });
    
        // ADD THIS BLOCK HERE - RIGHT AFTER THE EGUI PANEL: ⬇️⬇️⬇️
        // Handle individual user ready signals
        // Handle individual user ready signals - collect first, then process
        /*let mut ready_user_indices = Vec::new();
        if let Some(receiver) = &mut self.ready_receiver {
            while let Ok(user_index) = receiver.try_recv() {
                ready_user_indices.push(user_index);
            }
        }

        // Now process the collected indices without borrow conflicts
        for user_index in ready_user_indices {
            if user_index < self.users.len() {
                self.users[user_index].status = "Active".to_string();
                self.update_ready_scanner_users_atomic(); // ✅ No borrow conflict
                
                // Start scanner when FIRST user becomes ready
                let has_ready_users = self.users.iter().any(|u| u.status == "Active");
                let has_selected_sections = self.selected_sections.values().any(|&selected| selected);
                
                if has_ready_users && has_selected_sections && !self.scanner_running {
                    self.start_scanner(); // ✅ No borrow conflict
                }
            }
        }*/
        // ⬆️⬆️⬆️ ADD THIS BLOCK RIGHT HERE
    
        // ADD THIS BLOCK HERE:
        if self.bot_running {
            self.receive_bot_manager(); // ADD THIS LINE
            self.update_countdowns();
            self.update_assigned_seats();
            self.update_logs();
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
        if self.show_transfer_modal {
            self.show_transfer_modal(ctx);  // Pass ctx, not ui
        }
        // Show logs modal if requested
        if self.show_logs_modal {
            self.show_logs_modal(ctx);
        }
    }


}

fn main() -> Result<(), eframe::Error> {
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