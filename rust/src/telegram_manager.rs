use std::io::{self, BufRead, Write};
// --- ADD THIS TO YOUR `use` STATEMENTS ---
use std::path::PathBuf;
// -----------------------------------------
use grammers_client::{Client, Config, SignInError};
use grammers_session::Session;
use grammers_tl_types::enums::InputPeer;
use grammers_tl_types::functions::messages::SendMessage;
use grammers_tl_types::types::InputPeerChat;
use tokio::sync::mpsc;

const API_ID: i32 = 21955613;
const API_HASH: &str = "8cc201109a0697c1e42e70bc942931a7";
// We no longer use the SESSION_FILE constant here.

#[derive(Debug)]
pub struct TelegramMessage {
    pub text: String,
    pub chat_id: i64,
}

pub struct TelegramManager {
    client: Client,
    receiver: mpsc::UnboundedReceiver<TelegramMessage>,
    gui_request_sender: Option<mpsc::UnboundedSender<TelegramRequest>>,
    gui_response_receiver: Option<mpsc::UnboundedReceiver<String>>,
    // --- ADD THIS FIELD ---
    session_file_path: PathBuf,
}

#[derive(Debug)]
pub enum TelegramRequest {
    RequestPhone,
    RequestCode,
    RequestPassword,
}

// --- NEW HELPER FUNCTION ---
// This function creates a stable path like `C:\Users\YourUser\.my_telegram_bot\telegram.session`
fn get_session_path() -> Result<PathBuf, io::Error> {
    // Get the user's home directory. This works on both Windows and Linux.
    let mut path = dirs::home_dir().ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "Could not find home directory")
    })?;

    // Create a hidden folder for our app's data.
    path.push(".my_telegram_bot");
    
    // Create the directory if it doesn't exist.
    std::fs::create_dir_all(&path)?;

    // Add the session file name to the path.
    path.push("telegram.session");
    
    Ok(path)
}
// ----------------------------


impl TelegramManager {
    pub async fn new(
        receiver: mpsc::UnboundedReceiver<TelegramMessage>,
        gui_request_sender: mpsc::UnboundedSender<TelegramRequest>,
        gui_response_receiver: mpsc::UnboundedReceiver<String>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        // --- THE FIX IS HERE ---
        // 1. Get the stable, absolute path for the session file.
        let session_path = get_session_path()?;
        println!("Using session file at: {:?}", session_path);

        let client = Client::connect(Config {
            // 2. Load the session from that specific path.
            session: Session::load_file_or_create(&session_path)?,
            api_id: API_ID,
            api_hash: API_HASH.to_string(),
            params: Default::default(),
        }).await?;

        Ok(Self {
            client,
            receiver,
            gui_request_sender: Some(gui_request_sender),
            gui_response_receiver: Some(gui_response_receiver),
            // 3. Store the path so we can save to it later.
            session_file_path: session_path,
        })
    }

    async fn request_from_gui(
        &mut self,
        request_type: TelegramRequest,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
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

    pub async fn run(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if !self.client.is_authorized().await? {
            println!("Telegram: Not signed in. Please follow the prompts.");

            let phone = self.request_from_gui(TelegramRequest::RequestPhone).await?;
            let token = self.client.request_login_code(&phone).await?;
            let code = self.request_from_gui(TelegramRequest::RequestCode).await?;

            let signed_in = self.client.sign_in(&token, &code).await;
            match signed_in {
                Err(SignInError::PasswordRequired(password_token)) => {
                    let password = self.request_from_gui(TelegramRequest::RequestPassword).await?;
                    self.client.check_password(password_token, &password).await?;
                }
                Ok(_) => (),
                Err(e) => return Err(Box::new(e)),
            }
            
            println!("Telegram: Signed in successfully!");
            // --- FIX ---
            // Save the session to the stable path we stored earlier.
            self.client.session().save_to_file(&self.session_file_path)?;
        } else {
            println!("Telegram: Already signed in.");
        }
        
        // The rest of the function remains the same.
        while let Some(telegram_msg) = self.receiver.recv().await {
            let request = SendMessage {
                peer: InputPeer::Chat(InputPeerChat { chat_id: telegram_msg.chat_id }),
                message: telegram_msg.text,
                random_id: rand::random(),
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
                effect: None,
            };
            
            if let Err(e) = self.client.invoke(&request).await {
                 println!("Failed to send telegram message: {}", e);
            }
        }
        
        Ok(())
    }
}