use std::io::{self, BufRead, Write};
use grammers_client::{Client, Config, SignInError};
use grammers_session::Session;
use grammers_tl_types::enums::InputPeer;
use grammers_tl_types::functions::messages::SendMessage; // --- ADD THIS ---
use grammers_tl_types::types::InputPeerChat;
use tokio::sync::mpsc;

// Your constants remain the same
const API_ID: i32 = 21955613;
const API_HASH: &str = "8cc201109a0697c1e42e70bc942931a7";
const SESSION_FILE: &str = "telegram.session";
const CHAT_ID: i64 = 4785839500;

pub struct TelegramManager {
    client: Client,
    receiver: mpsc::UnboundedReceiver<String>,
    gui_request_sender: Option<mpsc::UnboundedSender<TelegramRequest>>,
    gui_response_receiver: Option<mpsc::UnboundedReceiver<String>>,
}
#[derive(Debug)]
pub enum TelegramRequest {
    RequestPhone,
    RequestCode,
    RequestPassword,
}

impl TelegramManager {
    pub async fn new(
        receiver: mpsc::UnboundedReceiver<String>,
        gui_request_sender: mpsc::UnboundedSender<TelegramRequest>,
        gui_response_receiver: mpsc::UnboundedReceiver<String>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let client = Client::connect(Config {
            session: Session::load_file_or_create(SESSION_FILE)?,
            api_id: API_ID,
            api_hash: API_HASH.to_string(),
            params: Default::default(),
        }).await?;

        Ok(Self { 
            client, 
            receiver,
            gui_request_sender: Some(gui_request_sender),
            gui_response_receiver: Some(gui_response_receiver),
        })
    }

    async fn request_from_gui(&mut self, request_type: TelegramRequest) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        if let Some(sender) = &self.gui_request_sender {
            sender.send(request_type)?;
        }
        
        if let Some(receiver) = &mut self.gui_response_receiver {
            if let Some(response) = receiver.recv().await {
                return Ok(response);
            }
        }
        
        Err("Failed to get response from GUI".into())
    }
    // In impl TelegramManager
    pub async fn run(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if !self.client.is_authorized().await? {
            println!("Telegram: Not signed in. Please follow the prompts.");
            
            // REPLACE: let phone = prompt("Enter your phone number (e.g., +1234567890): ")?;
            let phone = self.request_from_gui(TelegramRequest::RequestPhone).await?;
            
            let token = self.client.request_login_code(&phone).await?;
            
            // REPLACE: let code = prompt("Enter the code you received: ")?;
            let code = self.request_from_gui(TelegramRequest::RequestCode).await?;
            
            let signed_in = self.client.sign_in(&token, &code).await;
            match signed_in {
                Err(SignInError::PasswordRequired(password_token)) => {
                    // REPLACE: let password = prompt("Enter your 2FA password: ")?;
                    let password = self.request_from_gui(TelegramRequest::RequestPassword).await?;
                    self.client.check_password(password_token, &password).await?;
                }
                Ok(_) => (),
                Err(e) => return Err(Box::new(e)),
            }
            
            println!("Telegram: Signed in successfully!");
            self.client.session().save_to_file(SESSION_FILE)?;
        } else {
            println!("Telegram: Already signed in.");
        }
        
        while let Some(message) = self.receiver.recv().await {
            let request = SendMessage {
                peer: InputPeer::Chat(InputPeerChat { chat_id: CHAT_ID }),
                message,
                random_id: rand::random(),
                
                // Manually set all other fields to their default values
                no_webpage: false,
                silent: false,
                background: false,
                clear_draft: false,
                noforwards: false,
                update_stickersets_order: false,
                invert_media: false,
                reply_to: None,
                entities: None,
                schedule_date: None,
                reply_markup: None,
                send_as: None,
                quick_reply_shortcut: None,
                effect: None, // --- ADD THIS MISSING LINE ---
            };
            
            if let Err(e) = self.client.invoke(&request).await {
                 println!("Failed to send telegram message: {}", e);
            }
        }
        
        Ok(())
    }
}

fn prompt(message: &str) -> io::Result<String> {
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    stdout.write_all(message.as_bytes())?;
    stdout.flush()?;

    let stdin = io::stdin();
    let mut stdin = stdin.lock();
    let mut line = String::new();
    stdin.read_line(&mut line)?;
    Ok(line.trim().to_string())
}