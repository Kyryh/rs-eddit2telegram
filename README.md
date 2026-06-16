# reddit2telegram

Forwards Reddit posts to Telegram chats. Each run fetches recent submissions from configured subreddits and sends them to the chat.

## Prerequisites

- A **Telegram bot token** — create a bot via [@BotFather](https://t.me/BotFather) and copy the token it gives you.
- **Reddit API credentials** — go to [reddit.com/prefs/apps](https://www.reddit.com/prefs/apps), create a "script" type app, and copy the client ID and secret.

## Setup

### 1. Create the `.env` file

Copy `.env.template` to `.env` and fill in your credentials:

```
BOT_TOKEN=<your Telegram bot token>
REDDIT_CLIENT_ID=<your Reddit app client ID>
REDDIT_CLIENT_SECRET=<your Reddit app client secret>
```

### 2. Create at least one poster

Duplicate `posters/poster.rhai.template` as a `.rhai` file (any name works, e.g. `posters/memes.rhai`). You can create one file per channel.

Each poster file has four required constants:

```rust
const SUBREDDIT = "memes";       // subreddit(s) name (without r/) 
const CHAT = "r_Memes";     // Telegram chat ID (negative for channels/groups) or channel username
const SORT_BY = "hot";           // "hot", "new", "top", or "rising"
const LIMIT = 10;                // how many posts to fetch per run
```

## Running

Open the terminal and run:

```sh
./reddit2telegram        # Linux / macOS
reddit2telegram.exe      # Windows
```

The program runs once, sends any new posts, then exits.

To keep the channel updated, schedule the binary to run periodically, for example with cron:

```cron
30 * * * * /path/to/reddit2telegram >> /path/to/logging.log
```

Or a Windows scheduled task set to repeat every 30 minutes.
