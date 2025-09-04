use std::io::{self, BufRead, Write};
use grammers_client::{Client, Config, SignInError};
use grammers_session::Session;
use grammers_tl_types::enums::InputPeer;
use grammers_tl_types::functions::messages::SendMessage; // --- ADD THIS ---
use grammers_tl_types::types::InputPeerChat;
use tokio::sync::mpsc;

// Your constants remain the same
const API_ID: i32 = 29997839;
const API_HASH: &str = "8061d4958099b07974fae4498903f16f";
const SESSION_FILE: &str = "telegram.session";
const CHAT_ID: i64 = 4952333786;

pub struct TelegramManager {
    client: Client,
    receiver: mpsc::UnboundedReceiver<String>,
}

impl TelegramManager {
    pub async fn new(receiver: mpsc::UnboundedReceiver<String>) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let client = Client::connect(Config {
            session: Session::load_file_or_create(SESSION_FILE)?,
            api_id: API_ID,
            api_hash: API_HASH.to_string(),
            params: Default::default(),
        }).await?;

        Ok(Self { client, receiver })
    }

    // In impl TelegramManager
    // In impl TelegramManager
    // In impl TelegramManager
    pub async fn run(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if !self.client.is_authorized().await? {
            println!("Telegram: Not signed in. Please follow the prompts.");
            let phone = prompt("Enter your phone number (e.g., +1234567890): ")?;
            let token = self.client.request_login_code(&phone).await?;
            let code = prompt("Enter the code you received: ")?;
            
            let signed_in = self.client.sign_in(&token, &code).await;
            match signed_in {
                Err(SignInError::PasswordRequired(password_token)) => {
                    let password = prompt("Enter your 2FA password: ")?;
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