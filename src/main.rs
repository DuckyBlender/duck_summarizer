use chrono::Utc;
use dotenv::dotenv;
use log::{error, info};
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use std::{collections::{HashMap, VecDeque}, env, sync::Arc};
use teloxide::{
    dispatching::UpdateFilterExt,
    prelude::*,
    types::{ChatId, Message, MessageId, Update, UserId},
    utils::command::BotCommands,
};
use tokio::sync::Mutex;
use std::str::FromStr;

const MAX_MESSAGES: usize = 1000;

#[derive(Debug, Clone)]
struct SavedMessage {
    message_id: MessageId,
    date: i64,
    from_user: Option<String>,  // Username or first_name
    from_id: UserId,
    reply_to_message_id: Option<MessageId>,
    text: String,
}

#[derive(Debug, Clone)]
struct MessageStore {
    // Map of chat_id to message queue for that chat
    chats: HashMap<ChatId, VecDeque<SavedMessage>>,
}

impl MessageStore {
    fn new() -> Self {
        Self {
            chats: HashMap::new(),
        }
    }

    fn add_message(&mut self, chat_id: ChatId, message: SavedMessage) {
        let chat_messages = self.chats.entry(chat_id).or_insert_with(|| {
            VecDeque::with_capacity(MAX_MESSAGES)
        });
        
        if chat_messages.len() >= MAX_MESSAGES {
            chat_messages.pop_front();
        }
        chat_messages.push_back(message);
    }

    fn get_last_n_messages(&self, chat_id: ChatId, n: usize) -> Vec<SavedMessage> {
        match self.chats.get(&chat_id) {
            Some(messages) => {
                let count = n.min(messages.len());
                messages
                    .iter()
                    .rev()
                    .take(count)
                    .rev()
                    .cloned()
                    .collect()
            },
            None => Vec::new(),
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
    #[command(description = "summarize the last n messages. Usage: /summarize <count>")]
    Summarize(String),
    #[command(description = "display this help message.")]
    Help,
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

async fn handle_message(bot: Bot, msg: Message, message_store: MessageStoreType) -> ResponseResult<()> {
    if let Some(text) = msg.text() {
        let user_name = msg
            .from()
            .and_then(|user| user.username.clone().or_else(|| Some(user.first_name.clone())));
        
        let user_id = match msg.from() {
            Some(user) => user.id,
            None => return Ok(()),  // Skip messages without a sender
        };

        let saved_message = SavedMessage {
            message_id: msg.id,
            date: Utc::now().timestamp(),
            from_user: user_name,
            from_id: user_id,
            reply_to_message_id: msg.reply_to_message().map(|reply| reply.id),
            text: text.to_string(),
        };

        let mut store = message_store.lock().await;
        store.add_message(msg.chat.id, saved_message);
    }
    Ok(())
}

async fn handle_command(
    bot: Bot,
    msg: Message,
    cmd: Command,
    message_store: MessageStoreType,
) -> ResponseResult<()> {
    match cmd {
        Command::Help => {
            bot.send_message(msg.chat.id, Command::descriptions().to_string())
                .await?;
        }
        Command::Summarize(count_str) => {
            let count = match usize::from_str(count_str.trim()) {
                Ok(n) if n > 0 && n <= MAX_MESSAGES => n,
                _ => {
                    bot.send_message(
                        msg.chat.id,
                        format!("Please provide a valid number between 1 and {}", MAX_MESSAGES),
                    )
                    .await?;
                    return Ok(());
                }
            };

            let store = message_store.lock().await;
            let messages = store.get_last_n_messages(msg.chat.id, count);

            if messages.is_empty() {
                bot.send_message(msg.chat.id, "No messages to summarize.").await?;
                return Ok(());
            }

            bot.send_message(msg.chat.id, "I'm summarizing your conversation...").await?;
            
            match summarize_conversation(&messages).await {
                Ok(summary) => {
                    bot.send_message(msg.chat.id, summary).await?;
                },
                Err(e) => {
                    error!("Failed to summarize: {}", e);
                    bot.send_message(
                        msg.chat.id,
                        "Sorry, I couldn't generate a summary. Please try again later.",
                    )
                    .await?;
                }
            }
        }
    }

    Ok(())
}

async fn summarize_conversation(messages: &[SavedMessage]) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let api_key = env::var("GROQ_API_KEY").expect("GROQ_API_KEY is not set");
    let model = "llama-3.3-70b-versatile";
    let client = reqwest::Client::new();
    
    // Convert messages to conversation format
    let mut conversation_text = String::new();
    for message in messages {
        let username = message.from_user.as_deref().unwrap_or("Unknown");
        
        // Add reply information if available
        if let Some(reply_id) = message.reply_to_message_id {
            let replied_to = messages.iter()
                .find(|m| m.message_id == reply_id)
                .and_then(|m| m.from_user.as_ref())
                .map(|u| u.as_str())
                .unwrap_or("someone");
            
            conversation_text.push_str(&format!("{} (replying to {}): {}\n", username, replied_to, message.text));
        } else {
            conversation_text.push_str(&format!("{}: {}\n", username, message.text));
        }
    }

    let system_prompt = "You are a Telegram conversation summarizer. Your task is to create a concise, accurate, and well-structured summary of the conversation provided. Follow these guidelines:
1. Identify the main participants and their key points
2. Highlight important topics discussed in the conversation
3. Note any decisions, actions, or conclusions reached
4. Maintain a neutral tone and avoid adding information not present in the original conversation
5. Group related points together thematically
6. Present the summary in clear paragraphs with proper formatting
7. If the conversation contains questions that were answered, include both the questions and their answers
8. Format the summary to be easily readable in Telegram";

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
                content: format!("Please summarize this Telegram conversation:\n\n{}", conversation_text),
            },
        ],
        temperature: 0.7,
        max_tokens: 2000,
    };

    let response = client
        .post("https://api.groq.com/openai/v1/chat/completions")
        .headers(headers)
        .bearer_auth(api_key)
        .json(&request)
        .send()
        .await?
        .json::<ChatCompletionResponse>()
        .await?;

    Ok(response.choices[0].message.content.clone())
}

#[tokio::main]
async fn main() {
    dotenv().ok();
    pretty_env_logger::init();

    let bot_token = env::var("TELEGRAM_BOT_TOKEN").expect("TELEGRAM_BOT_TOKEN is not set");
    let bot = Bot::new(bot_token);

    let message_store = Arc::new(Mutex::new(MessageStore::new()));

    let command_handler = teloxide::filter_command::<Command, _>()
        .branch(dptree::endpoint(move |bot: Bot, msg: Message, cmd: Command, store: MessageStoreType| {
            handle_command(bot, msg, cmd, store)
        }));

    let message_handler = Update::filter_message()
        .branch(command_handler)
        .branch(dptree::endpoint(move |bot: Bot, msg: Message, store: MessageStoreType| {
            handle_message(bot, msg, store)
        }));

    info!("Starting bot...");
    
    Dispatcher::builder(bot, message_handler)
        .dependencies(dptree::deps![message_store])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
}