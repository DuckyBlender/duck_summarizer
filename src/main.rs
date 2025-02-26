use chrono::{DateTime, Utc};
use dotenvy::dotenv;
use fern::colors::{Color, ColoredLevelConfig};
use log::{LevelFilter, debug, error, info, trace, warn};
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use std::{
    collections::{HashMap, VecDeque},
    env, io,
    sync::Arc,
};
use teloxide::{
    dispatching::UpdateFilterExt,
    prelude::*,
    types::{ChatId, Message, MessageId, ParseMode, ReplyParameters, ThreadId, Update},
    utils::{command::BotCommands, markdown},
};
use tokio::sync::Mutex;

const MAX_MESSAGES: usize = 1000;

// Setup logger with fern
fn setup_logger() -> Result<(), fern::InitError> {
    let colors = ColoredLevelConfig::new()
        .trace(Color::Cyan)
        .debug(Color::Cyan)
        .error(Color::Red)
        .info(Color::Green)
        .warn(Color::Yellow);

    let log_level = LevelFilter::Debug;

    fern::Dispatch::new()
        .format(move |out, message, record| {
            out.finish(format_args!(
                "{timestamp} | {colored_level} | {target}: {message}",
                timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                colored_level = colors.color(record.level()),
                target = record.target(),
                message = message,
            ))
        })
        .level(log_level)
        // Set specific module log levels if needed
        // .level_for(env!("CARGO_PKG_NAME"), log_level)
        // Output to stdout and log file
        .chain(io::stdout())
        .chain(fern::log_file("duck_summarizer.log")?)
        .apply()?;

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ChatThreadId {
    chat_id: ChatId,
    thread_id: Option<ThreadId>,
}

#[derive(Debug, Clone)]
struct SavedMessage {
    message_id: MessageId,
    from_user: Option<String>, // Username or first_name
    reply_to_message_id: Option<MessageId>,
    text: String,
}

#[derive(Debug, Clone)]
struct MessageStore {
    // Map of chat_id+thread_id to message queue for that chat/thread
    chats: HashMap<ChatThreadId, VecDeque<SavedMessage>>,
    startup_time: DateTime<Utc>,
}

impl MessageStore {
    fn new() -> Self {
        Self {
            chats: HashMap::new(),
            startup_time: Utc::now(),
        }
    }

    fn add_message(&mut self, chat_id: ChatId, thread_id: Option<ThreadId>, message: SavedMessage) {
        let chat_thread_id = ChatThreadId { chat_id, thread_id };

        let chat_messages = self
            .chats
            .entry(chat_thread_id)
            .or_insert_with(|| VecDeque::with_capacity(MAX_MESSAGES));

        if chat_messages.len() >= MAX_MESSAGES {
            chat_messages.pop_front();
        }
        chat_messages.push_back(message);
    }

    fn get_last_n_messages(
        &self,
        chat_id: ChatId,
        thread_id: Option<ThreadId>,
        n: usize,
    ) -> Vec<SavedMessage> {
        let chat_thread_id = ChatThreadId { chat_id, thread_id };

        match self.chats.get(&chat_thread_id) {
            Some(messages) => {
                let count = n.min(messages.len());
                messages.iter().rev().take(count).rev().cloned().collect()
            }
            None => Vec::new(),
        }
    }

    fn get_uptime(&self) -> String {
        let now = Utc::now();
        let duration = now.signed_duration_since(self.startup_time);

        let days = duration.num_days();
        let hours = duration.num_hours() % 24;
        let minutes = duration.num_minutes() % 60;
        let seconds = duration.num_seconds() % 60;

        if days > 0 {
            format!("{}d {}h {}m {}s", days, hours, minutes, seconds)
        } else if hours > 0 {
            format!("{}h {}m {}s", hours, minutes, seconds)
        } else if minutes > 0 {
            format!("{}m {}s", minutes, seconds)
        } else {
            format!("{}s", seconds)
        }
    }
}

type MessageStoreType = Arc<Mutex<MessageStore>>;

#[derive(BotCommands, Clone, Debug)]
#[command(
    rename_rule = "lowercase",
    description = "These commands are supported:"
)]
enum Command {
    #[command(description = "info about the bot")]
    Start,
    #[command(description = "display this help message")]
    Help,
    #[command(description = "summarize the last n messages, defaults to 100")]
    Summarize(String),
    #[command(
        description = "show total messages and chat count in-memory",
        alias = "stats"
    )]
    Memory,
    #[command(description = "display privacy disclaimer")]
    Privacy,
}

#[derive(Serialize, Deserialize, Debug)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Serialize, Debug)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f32,
    max_tokens: u32,
}

#[derive(Deserialize, Debug)]
struct ChatCompletionResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize, Debug)]
struct Choice {
    message: ChatMessage,
}

async fn handle_message(msg: Message, message_store: MessageStoreType) -> ResponseResult<()> {
    let chat_id = msg.chat.id;
    let thread_id = msg.thread_id;

    if let Some(text) = msg.text() {
        let display_name = msg.from.as_ref().map(|user| {
            if let Some(last_name) = &user.last_name {
                format!("{} {}", user.first_name, last_name)
            } else {
                user.first_name.clone()
            }
        });

        trace!(target: "message_handler", "DisplayName: {}, FirstName: {}", 
            display_name.clone().unwrap_or_else(|| "None".to_string()), 
            msg.from.as_ref().map(|u| u.first_name.clone()).unwrap_or_else(|| "None".to_string()));

        let user_id = match msg.from.as_ref() {
            Some(user) => user.id,
            None => {
                debug!(target: "message_handler", "Received a message without a sender in chat {}, skipping", chat_id);
                return Ok(());
            }
        };

        trace!(target: "message_handler", "Received message from {} (ID: {}) in chat {} thread {:?}: {}", 
            display_name.clone().unwrap_or_else(|| "Unknown".to_string()), 
            user_id, 
            chat_id,
            thread_id,
            text);

        let saved_message = SavedMessage {
            message_id: msg.id,
            from_user: display_name,
            reply_to_message_id: msg.reply_to_message().map(|reply| reply.id),
            text: text.to_string(),
        };

        let mut store = message_store.lock().await;
        store.add_message(chat_id, thread_id, saved_message.clone());
    }
    Ok(())
}

async fn handle_command(
    bot: Bot,
    msg: Message,
    cmd: Command,
    message_store: MessageStoreType,
) -> ResponseResult<()> {
    let chat_id = msg.chat.id;
    let thread_id = msg.thread_id;
    let chat_type = format!("{:?}", msg.chat.kind);
    let display_name = msg
        .from
        .map(|user| {
            if let Some(last_name) = &user.last_name {
                format!("{} {}", user.first_name, last_name)
            } else if let Some(username) = &user.username {
                username.clone()
            } else {
                user.first_name.clone()
            }
        })
        .unwrap_or_else(|| "Unknown".to_string());

    // Helper function to add thread_id to message requests if present
    let send_message = |text: String| {
        let mut request = bot
            .send_message(msg.chat.id, text)
            .reply_parameters(ReplyParameters::new(msg.id));

        if let Some(thread) = thread_id {
            request = request.message_thread_id(thread);
        }

        request
    };

    match cmd {
        Command::Start => {
            info!(target: "command", "User {} requested /start in chat {} ({})", display_name, chat_id, chat_type);
            send_message("Hello!\n\n\
                I can summarize the last n messages in this chat or thread\\.\n\
                Use /summarize <n> to get started\\.\n\
                For more commands, use /help\\.".to_string())
            .await?;
        }
        Command::Help => {
            info!(target: "command", "User {} requested /help in chat {} ({})", display_name, chat_id, chat_type);
            send_message(Command::descriptions().to_string()).await?;
        }
        Command::Summarize(count_str) => {
            info!(target: "command", "User {} requested /summarize {} in chat {} thread {:?} ({})", 
                  display_name, count_str, chat_id, thread_id, chat_type);
            let trimmed = count_str.trim();
            let count = if trimmed.is_empty() {
                100
            } else {
                match usize::from_str(trimmed) {
                    Ok(n) if n > 0 && n <= MAX_MESSAGES => n,
                    _ => {
                        warn!(target: "command", "Invalid count '{}' provided for /summarize by {} in chat {}", count_str, display_name, chat_id);
                        send_message(format!(
                            "Please provide a valid number between 1 and {}",
                            MAX_MESSAGES
                        ))
                        .await?;
                        return Ok(());
                    }
                }
            };

            let store = message_store.lock().await;
            let messages = store.get_last_n_messages(msg.chat.id, thread_id, count);

            if messages.is_empty() {
                info!(target: "command", "No messages found to summarize in chat {} thread {:?} for user {}", chat_id, thread_id, display_name);
                send_message("No messages to summarize.".to_string()).await?;
                return Ok(());
            }

            debug!(target: "command", "Summarizing {} messages in chat {} thread {:?} for user {}", messages.len(), chat_id, thread_id, display_name);
            // Use actual number of messages retrieved in the summary message
            let bot_msg =
                send_message(format!("Summarizing {} messages...", messages.len())).await?;

            match summarize_conversation(&messages).await {
                Ok(summary) => {
                    info!(target: "summarization", "Successfully generated summary in chat {} thread {:?} for user {}", chat_id, thread_id, display_name);
                    let summary = format!("_{}_", markdown::escape(&summary));
                    bot.edit_message_text(bot_msg.chat.id, bot_msg.id, summary)
                        .parse_mode(ParseMode::MarkdownV2)
                        .await?;
                }
                Err(e) => {
                    error!(target: "summarization", "Failed to summarize conversation in chat {} thread {:?} for user {}: {}", chat_id, thread_id, display_name, e);
                    bot.edit_message_text(
                        bot_msg.chat.id,
                        bot_msg.id,
                        "Failed to summarize the conversation.",
                    )
                    .await?;
                }
            }
        }
        Command::Memory => {
            let store = message_store.lock().await;
            let total_chats = store.chats.len();
            let total_messages: usize = store.chats.values().map(|v| v.len()).sum();

            // Count messages for this chat/thread combination
            let current_chat_thread = ChatThreadId { chat_id, thread_id };
            let current_chat_messages = store
                .chats
                .get(&current_chat_thread)
                .map(|v| v.len())
                .unwrap_or(0);

            // Calculate uptime and format startup time
            let uptime = store.get_uptime();

            let thread_info = match thread_id {
                Some(_) => "thread",
                None => "chat",
            };

            send_message(format!(
                "There are *{}* messages in memory from *{}* different chats/threads\\.\n\
                 Messages in this {}: *{}*\n\
                 Uptime: *{}*\n\
                 _Messages are *only* saved in memory since bot startup\\._",
                total_messages,
                total_chats,
                thread_info,
                current_chat_messages,
                markdown::escape(&uptime)
            ))
            .parse_mode(ParseMode::MarkdownV2)
            .await?;
        }
        Command::Privacy => {
            info!(target: "command", "User {} requested /privacy in chat {} thread {:?} ({})", display_name, chat_id, thread_id, chat_type);
            send_message(
                "This bot stores all messages *only* in memory and *never* writes any data to disk\\.\n\n[Source Code](https://github.com/DuckyBlender/duck_summarizer)".to_string()
            )
            .parse_mode(ParseMode::MarkdownV2)
            .await?;
        }
    }

    Ok(())
}

async fn summarize_conversation(
    messages: &[SavedMessage],
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    debug!(target: "summarization", "Starting conversation summarization for {} messages", messages.len());

    let api_key = match env::var("GROQ_API_KEY") {
        Ok(key) => key,
        Err(e) => {
            error!(target: "summarization", "GROQ_API_KEY not set: {}", e);
            return Err("GROQ_API_KEY environment variable not set".into());
        }
    };

    let model = "llama-3.3-70b-versatile";
    let client = reqwest::Client::new();

    // Convert messages to conversation format
    let mut conversation_text = String::new();
    for message in messages {
        let username = message.from_user.as_deref().unwrap_or("Unknown");

        // Replace newlines with literals
        let text = message.text.replace('\n', "\\n");

        // Add reply information if available
        if let Some(reply_id) = message.reply_to_message_id {
            let replied_to = messages
                .iter()
                .find(|m| m.message_id == reply_id)
                .and_then(|m| m.from_user.as_ref())
                .map(|u| u.as_str())
                .unwrap_or("someone");

            conversation_text.push_str(&format!(
                "{} (replying to {}): {}\n",
                username, replied_to, text
            ));
        } else {
            conversation_text.push_str(&format!("{}: {}\n", username, text));
        }
    }

    trace!(target: "summarization", "Prepared conversation text for summarization: {} characters", conversation_text.len());

    let system_prompt = "You are a Telegram conversation summarizer. Your task is to create a concise, accurate, and well-structured summary of the conversation provided. Make it as short as possible while retaining all important information. Don't include any personal opinions or additional comments. Don't use markdown.";

    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    let request = ChatCompletionRequest {
        model: model.to_string(),
        messages: vec![
            ChatMessage {
                role: "system".to_string(),
                content: system_prompt.to_string(),
            },
            ChatMessage {
                role: "user".to_string(),
                content: conversation_text.to_string(),
            },
        ],
        temperature: 0.4,
        max_tokens: 2000,
    };

    debug!(target: "api", "Sending request to Groq API for summarization, model: {}", model);

    let response = match client
        .post("https://api.groq.com/openai/v1/chat/completions")
        .headers(headers)
        .bearer_auth(&api_key)
        .json(&request)
        .send()
        .await
    {
        Ok(resp) => {
            if !resp.status().is_success() {
                let status = resp.status();
                let error_text = resp
                    .text()
                    .await
                    .unwrap_or_else(|_| "Unable to read error response".to_string());
                error!(target: "api", "Groq API returned error status {}: {}", status, error_text);
                return Err(format!("API error: Status {}", status).into());
            }
            resp
        }
        Err(e) => {
            error!(target: "api", "Failed to send request to Groq API: {}", e);
            return Err(Box::new(e));
        }
    };

    match response.json::<ChatCompletionResponse>().await {
        Ok(parsed) => {
            if parsed.choices.is_empty() {
                error!(target: "api", "Groq API returned empty choices array");
                return Err("API returned no choices".into());
            }

            let summary = parsed.choices[0].message.content.clone();
            debug!(target: "summarization", "Successfully received summary from API: {} characters", summary.len());
            Ok(summary)
        }
        Err(e) => {
            error!(target: "api", "Failed to parse Groq API response: {}", e);
            Err(Box::new(e))
        }
    }
}

#[tokio::main]
async fn main() {
    dotenv().ok();

    // Initialize the logger with fern
    if let Err(e) = setup_logger() {
        eprintln!("Error setting up logger: {}", e);
        std::process::exit(1);
    }

    info!(target: "startup", "Ducky Summarizer starting up");

    let bot_token = match env::var("TELEGRAM_BOT_TOKEN") {
        Ok(token) => token,
        Err(e) => {
            error!(target: "startup", "Failed to get TELEGRAM_BOT_TOKEN: {}", e);
            std::process::exit(1);
        }
    };

    info!(target: "startup", "Initializing bot");
    let bot = Bot::new(bot_token);

    info!(target: "startup", "Setting bot commands");
    bot.set_my_commands(Command::bot_commands()).await.unwrap();

    let message_store = Arc::new(Mutex::new(MessageStore::new()));
    info!(target: "startup", "Message store initialized");

    let command_handler = teloxide::filter_command::<Command, _>().branch(dptree::endpoint(
        move |bot: Bot, msg: Message, cmd: Command, store: MessageStoreType| {
            handle_command(bot, msg, cmd, store)
        },
    ));

    let message_handler =
        Update::filter_message()
            .branch(command_handler)
            .branch(dptree::endpoint(
                move |_: Bot, msg: Message, store: MessageStoreType| handle_message(msg, store),
            ));

    info!(target: "startup", "Setting up dispatcher and starting bot");

    Dispatcher::builder(bot, message_handler)
        .dependencies(dptree::deps![message_store])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;

    info!(target: "shutdown", "Bot has been shut down");
}
