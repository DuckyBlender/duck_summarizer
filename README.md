# Ducky Summarizer

Ducky Summarizer is a Telegram bot that summarizes conversations using an in-memory message store and the Groq API.

## Features
- Summarizes the last n messages from a chat.
- Displays in-memory message statistics.
- Privacy-first approach: messages are not saved on disk.
- Open source: [GitHub](https://github.com/DuckyBlender/duck_summarizer).

## Installation
1. Clone the repository:
   ```
   git clone https://github.com/DuckyBlender/duck_summarizer
   ```
2. Create a `.env` file in the project root with:
   ```
   TELEGRAM_BOT_TOKEN=your_telegram_bot_token
   GROQ_API_KEY=your_groq_api_key
   ```
3. Build and run:
   ```
   cargo run --release
   ```

## Usage
- `/help` - Displays available commands.
- `/summarize <count>` - Summarizes the last messages. Defaults to 100 but can go up to 1000.
- `/memory` - Shows message and chat statistics.
- `/privacy` - Displays the privacy disclaimer.

## License
This project is open source. See the [LICENSE](LICENSE) file for details.
