# youtube-summary

`youtube-summary` is a reusable agent skill for generating clean, structured summaries of YouTube videos. The repository ships the skill definition together with a small Rust CLI that fetches video metadata and transcript data used by the skill.

## Installation

### macOS (Apple Silicon)

```shell
cd .agents/skills
mkdir youtube-summary
cd youtube-summary
curl -L -o SKILL.md https://github.com/agentics-skills/youtube-summary/releases/latest/download/SKILL.md
curl -L -o youtube-transcript-aarch64-apple-darwin https://github.com/agentics-skills/youtube-summary/releases/latest/download/youtube-transcript-aarch64-apple-darwin
chmod +x youtube-transcript-aarch64-apple-darwin
xattr -d com.apple.quarantine youtube-transcript-aarch64-apple-darwin || true
```

### macOS (Intel)

```shell
cd .agents/skills
mkdir youtube-summary
cd youtube-summary
curl -L -o SKILL.md https://github.com/agentics-skills/youtube-summary/releases/latest/download/SKILL.md
curl -L -o youtube-transcript-x86_64-apple-darwin https://github.com/agentics-skills/youtube-summary/releases/latest/download/youtube-transcript-x86_64-apple-darwin
chmod +x youtube-transcript-x86_64-apple-darwin
xattr -d com.apple.quarantine youtube-transcript-x86_64-apple-darwin || true
```

### Linux (x86_64 musl)

```shell
cd .agents/skills
mkdir youtube-summary
cd youtube-summary
curl -L -o SKILL.md https://github.com/agentics-skills/youtube-summary/releases/latest/download/SKILL.md
curl -L -o youtube-transcript-x86_64-unknown-linux-musl https://github.com/agentics-skills/youtube-summary/releases/latest/download/youtube-transcript-x86_64-unknown-linux-musl
chmod +x youtube-transcript-x86_64-unknown-linux-musl
```

### Windows (x86_64)

```shell
cd .agents/skills
mkdir youtube-summary
cd youtube-summary
curl -L -o SKILL.md https://github.com/agentics-skills/youtube-summary/releases/latest/download/SKILL.md
curl -L -o youtube-transcript-x86_64-pc-windows-msvc.exe https://github.com/agentics-skills/youtube-summary/releases/latest/download/youtube-transcript-x86_64-pc-windows-msvc.exe
```

## CLI

The release binaries expose a single CLI and always print JSON to stdout.

The launch arguments below are intended for **manual testing and debugging**. For agent-driven usage, the binary should be executed from the skill directory and proxy configuration should come from a colocated `.env` file.

### Arguments

| Argument | Required | Description |
| --- | --- | --- |
| `--get_transcript` | one operation flag is required | Returns the plain transcript text. |
| `--get_timed_transcript` | one operation flag is required | Returns transcript snippets with `text`, `start`, and `duration`. |
| `--get_video_info` | one operation flag is required | Returns title, description, uploader, upload date, and duration. |
| `--url <URL>` | yes | Accepts a YouTube watch URL or `youtu.be` URL. |
| `--lang <CODE>` | no | Preferred transcript language. If the requested language is unavailable, the CLI falls back to English when possible. Only valid with transcript operations. |
| `--next_cursor <CURSOR>` | no | Continues paginated transcript output from a previous response. Only valid with transcript operations. |
| `--response-limit <N>` | no | Maximum response size used for transcript pagination. Use `0` or a negative value to return the full payload in one response. |

### How it works

1. The CLI extracts the video ID from a standard YouTube URL.
2. It downloads the YouTube watch page and resolves the internal player data needed for transcripts and metadata.
3. For transcript requests, it selects the best matching caption track, downloads the transcript XML, and converts it into plain text or timed snippets.
4. For video info requests, it extracts title, description, uploader, upload date, and a human-readable duration.
5. When transcript output reaches the configured `--response-limit`, the CLI returns a `next_cursor` so the caller can continue pagination.
6. The CLI prints the final response as JSON to stdout.

## Proxy configuration

### Agent usage

For agent-driven usage, configure proxy settings through a `.env` file placed **next to the binary** in the skill folder. This is the expected setup for the skill.

Supported `.env` environment variables:

- `HTTP_PROXY`
- `HTTPS_PROXY`

Example:

```shell
HTTP_PROXY=http://127.0.0.1:8080
HTTPS_PROXY=http://127.0.0.1:8080
```

### Manual testing

For manual local testing, proxy settings can also be passed through launch arguments or environment variables.

Launch arguments:

- `--webshare-proxy-username <USERNAME>`
- `--webshare-proxy-password <PASSWORD>`
- `--http-proxy <URL>`
- `--https-proxy <URL>`

Environment variables:

- `WEBSHARE_PROXY_USERNAME`
- `WEBSHARE_PROXY_PASSWORD`
- `HTTP_PROXY`
- `HTTPS_PROXY`

## License

MIT. See `LICENSE` for details.
