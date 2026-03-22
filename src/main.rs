use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, NaiveDate, SecondsFormat, TimeZone, Utc};
use clap::{ArgGroup, Parser};
use regex::Regex;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT_LANGUAGE, CONNECTION, COOKIE};
use roxmltree::Document;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::env;
use std::fs;
use std::process::ExitCode;
use std::time::Duration;
use url::Url;

const DEFAULT_RESPONSE_LIMIT: i64 = 50_000;
const HTTP_TIMEOUT_SECONDS: u64 = 60;
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/137.0.0.0 Safari/537.36";
const FALLBACK_TITLE: &str = "Transcript";
const YOUTUBE_INNERTUBE_PLAYER_API_URL: &str = "https://www.youtube.com/youtubei/v1/player";
const YOUTUBE_INNERTUBE_CLIENT_NAME: &str = "ANDROID";
const YOUTUBE_INNERTUBE_CLIENT_VERSION: &str = "20.10.38";

#[derive(Debug, Parser)]
#[command(name = "youtube-transcript", version)]
#[command(group(
	ArgGroup::new("operation")
		.required(true)
		.multiple(false)
		.args(["get_transcript", "get_timed_transcript", "get_video_info"])
))]
struct Cli {
    #[arg(long = "get_transcript")]
    get_transcript: bool,

    #[arg(long = "get_timed_transcript")]
    get_timed_transcript: bool,

    #[arg(long = "get_video_info")]
    get_video_info: bool,

    #[arg(long = "url", required = true)]
    url: String,

    #[arg(long = "lang")]
    lang: Option<String>,

    #[arg(long = "next_cursor")]
    next_cursor: Option<String>,

    #[arg(
		long = "response-limit",
		default_value_t = DEFAULT_RESPONSE_LIMIT,
		allow_hyphen_values = true,
		allow_negative_numbers = true
	)]
    response_limit: i64,

    #[arg(long = "webshare-proxy-username", env = "WEBSHARE_PROXY_USERNAME")]
    webshare_proxy_username: Option<String>,

    #[arg(long = "webshare-proxy-password", env = "WEBSHARE_PROXY_PASSWORD")]
    webshare_proxy_password: Option<String>,

    #[arg(long = "http-proxy", env = "HTTP_PROXY")]
    http_proxy: Option<String>,

    #[arg(long = "https-proxy", env = "HTTPS_PROXY")]
    https_proxy: Option<String>,
}

#[derive(Debug, Clone)]
enum Operation {
    Transcript,
    TimedTranscript,
    VideoInfo,
}

#[derive(Debug, Clone)]
enum ResolvedProxy {
    None,
    Webshare {
        username: String,
        password: String,
    },
    Generic {
        http: Option<String>,
        https: Option<String>,
    },
}

#[derive(Debug, Serialize, PartialEq)]
struct TranscriptResponse {
    title: String,
    transcript: String,
    next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct TranscriptSnippetResponse {
    text: String,
    start: f64,
    duration: f64,
}

#[derive(Debug, Serialize, PartialEq)]
struct TimedTranscriptResponse {
    title: String,
    snippets: Vec<TranscriptSnippetResponse>,
    next_cursor: Option<String>,
}

#[derive(Debug, Serialize, PartialEq)]
struct VideoInfoResponse {
    title: String,
    description: String,
    uploader: String,
    upload_date: String,
    duration: String,
}

#[derive(Debug, Clone)]
struct InternalTranscriptSnippet {
    text: String,
    start: f64,
    duration: f64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct YouTubePlayerResponse {
    video_details: Option<YouTubeVideoDetails>,
    microformat: Option<YouTubeMicroformat>,
    captions: Option<YouTubeCaptions>,
    playability_status: Option<YouTubePlayabilityStatus>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct YouTubeVideoDetails {
    title: Option<String>,
    short_description: Option<String>,
    author: Option<String>,
    length_seconds: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct YouTubeMicroformat {
    player_microformat_renderer: Option<YouTubePlayerMicroformatRenderer>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct YouTubePlayerMicroformatRenderer {
    upload_date: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct YouTubeCaptions {
    player_captions_tracklist_renderer: Option<YouTubeCaptionTracklistRenderer>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct YouTubeCaptionTracklistRenderer {
    #[serde(default)]
    caption_tracks: Vec<CaptionTrack>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CaptionTrack {
    base_url: String,
    language_code: String,
    kind: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct YouTubePlayabilityStatus {
    status: Option<String>,
    reason: Option<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    if let Err(error) = load_proxy_env_from_binary_dir() {
        eprintln!("{error:#}");
        return ExitCode::FAILURE;
    }

    let cli = Cli::parse();

    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}

fn load_proxy_env_from_binary_dir() -> Result<()> {
    let binary_path = env::current_exe().context("failed to resolve current executable path")?;
    let binary_dir = binary_path.parent().ok_or_else(|| {
        anyhow!(
            "current executable path `{}` has no parent directory",
            binary_path.display()
        )
    })?;
    let dotenv_path = binary_dir.join(".env");

    if !dotenv_path.exists() {
        return Ok(());
    }

    let dotenv_contents = fs::read_to_string(&dotenv_path)
        .with_context(|| format!("failed to read `{}`", dotenv_path.display()))?;
    let proxy_env = parse_proxy_env_from_dotenv(&dotenv_contents)?;

    if env::var_os("HTTP_PROXY").is_none() {
        if let Some(http_proxy) = proxy_env.http_proxy {
            env::set_var("HTTP_PROXY", http_proxy);
        }
    }

    if env::var_os("HTTPS_PROXY").is_none() {
        if let Some(https_proxy) = proxy_env.https_proxy {
            env::set_var("HTTPS_PROXY", https_proxy);
        }
    }

    Ok(())
}

#[derive(Debug, Default, PartialEq)]
struct ProxyEnvFile {
    http_proxy: Option<String>,
    https_proxy: Option<String>,
}

fn parse_proxy_env_from_dotenv(contents: &str) -> Result<ProxyEnvFile> {
    let mut proxy_env = ProxyEnvFile::default();

    for (index, raw_line) in contents.lines().enumerate() {
        let line_number = index + 1;
        let line = raw_line.trim();

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let assignment = line.strip_prefix("export ").unwrap_or(line).trim_start();

        let (raw_key, raw_value) = assignment.split_once('=').ok_or_else(|| {
            anyhow!(
                "invalid .env assignment on line {}: expected KEY=VALUE",
                line_number
            )
        })?;

        let Some(key) = normalize_proxy_env_key(raw_key.trim()) else {
            continue;
        };

        let value = parse_shell_env_value(raw_value.trim())
            .with_context(|| format!("failed to parse `{key}` from .env line {line_number}"))?;

        if value.is_empty() {
            bail!("proxy variable `{key}` in .env line {line_number} is empty");
        }

        match key {
            "HTTP_PROXY" => proxy_env.http_proxy = Some(value),
            "HTTPS_PROXY" => proxy_env.https_proxy = Some(value),
            _ => unreachable!("normalized proxy env key must be HTTP_PROXY or HTTPS_PROXY"),
        }
    }

    Ok(proxy_env)
}

fn normalize_proxy_env_key(key: &str) -> Option<&'static str> {
    match key {
        "HTTP_PROXY" | "http_proxy" => Some("HTTP_PROXY"),
        "HTTPS_PROXY" | "https_proxy" => Some("HTTPS_PROXY"),
        _ => None,
    }
}

fn parse_shell_env_value(raw_value: &str) -> Result<String> {
    if raw_value.is_empty() {
        return Ok(String::new());
    }

    let first_char = raw_value.chars().next().expect("raw_value is not empty");
    if matches!(first_char, '\'' | '"') {
        return parse_quoted_shell_env_value(raw_value, first_char);
    }

    if let Some((value, _comment)) = raw_value.split_once(" #") {
        return Ok(value.trim_end().to_string());
    }

    Ok(raw_value.to_string())
}

fn parse_quoted_shell_env_value(raw_value: &str, quote: char) -> Result<String> {
    let mut escaped = false;
    let mut parsed = String::new();
    let mut closing_index = None;

    for (index, ch) in raw_value.char_indices().skip(1) {
        if quote == '"' && escaped {
            parsed.push(ch);
            escaped = false;
            continue;
        }

        if quote == '"' && ch == '\\' {
            escaped = true;
            continue;
        }

        if ch == quote {
            closing_index = Some(index);
            break;
        }

        parsed.push(ch);
    }

    if escaped {
        bail!("unterminated escape sequence in quoted value");
    }

    let closing_index = closing_index.ok_or_else(|| anyhow!("missing closing quote"))?;
    let trailing = raw_value[closing_index + quote.len_utf8()..].trim();
    if !trailing.is_empty() && !trailing.starts_with('#') {
        bail!("unexpected trailing content after quoted value");
    }

    Ok(parsed)
}

async fn run(cli: Cli) -> Result<()> {
    let operation = operation_from_cli(&cli)?;
    validate_operation_arguments(&cli, &operation)?;

    let proxy = ResolvedProxy::from_cli(&cli);

    match operation {
        Operation::Transcript => write_json(&build_transcript_response(&cli, &proxy).await?),
        Operation::TimedTranscript => {
            write_json(&build_timed_transcript_response(&cli, &proxy).await?)
        }
        Operation::VideoInfo => write_json(&build_video_info_response(&cli, &proxy).await?),
    }
}

fn operation_from_cli(cli: &Cli) -> Result<Operation> {
    if cli.get_transcript {
        return Ok(Operation::Transcript);
    }

    if cli.get_timed_transcript {
        return Ok(Operation::TimedTranscript);
    }

    if cli.get_video_info {
        return Ok(Operation::VideoInfo);
    }

    bail!("exactly one operation flag is required");
}

fn validate_operation_arguments(cli: &Cli, operation: &Operation) -> Result<()> {
    if matches!(operation, Operation::VideoInfo) {
        if cli.lang.is_some() {
            bail!("`--lang` is only valid with `--get_transcript` or `--get_timed_transcript`");
        }

        if cli.next_cursor.is_some() {
            bail!(
                "`--next_cursor` is only valid with `--get_transcript` or `--get_timed_transcript`"
            );
        }
    }

    Ok(())
}

impl ResolvedProxy {
    fn from_cli(cli: &Cli) -> Self {
        match (
            cli.webshare_proxy_username.as_ref(),
            cli.webshare_proxy_password.as_ref(),
        ) {
            (Some(username), Some(password)) => Self::Webshare {
                username: username.clone(),
                password: password.clone(),
            },
            _ if cli.http_proxy.is_some() || cli.https_proxy.is_some() => Self::Generic {
                http: cli.http_proxy.clone(),
                https: cli.https_proxy.clone(),
            },
            _ => Self::None,
        }
    }

    fn preferred_http_proxy(&self) -> Option<String> {
        match self {
            Self::None => None,
            Self::Webshare { username, password } => Some(webshare_proxy_url(username, password)),
            Self::Generic { http, https } => http.clone().or_else(|| https.clone()),
        }
    }

    fn preferred_https_proxy(&self) -> Option<String> {
        match self {
            Self::None => None,
            Self::Webshare { username, password } => Some(webshare_proxy_url(username, password)),
            Self::Generic { http, https } => https.clone().or_else(|| http.clone()),
        }
    }

    fn http_client(&self, accept_language: Option<&str>) -> Result<reqwest::Client> {
        let mut headers = HeaderMap::new();
        if let Some(accept_language) = accept_language {
            headers.insert(
                ACCEPT_LANGUAGE,
                HeaderValue::from_str(accept_language).with_context(|| {
                    format!("invalid Accept-Language header: {accept_language}")
                })?,
            );
        }

        if matches!(self, Self::Webshare { .. }) {
            headers.insert(CONNECTION, HeaderValue::from_static("close"));
        }

        let mut builder = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .default_headers(headers)
            .timeout(Duration::from_secs(HTTP_TIMEOUT_SECONDS));

        if let Some(http_proxy) = self.preferred_http_proxy() {
            builder = builder.proxy(reqwest::Proxy::http(&http_proxy).with_context(|| {
                format!("invalid HTTP proxy URL for title request: {http_proxy}")
            })?);
        }

        if let Some(https_proxy) = self.preferred_https_proxy() {
            builder = builder.proxy(reqwest::Proxy::https(&https_proxy).with_context(|| {
                format!("invalid HTTPS proxy URL for title request: {https_proxy}")
            })?);
        }

        if matches!(self, Self::Webshare { .. }) {
            builder = builder.tcp_keepalive(None);
        }

        builder.build().context("failed to build HTTP client")
    }
}

async fn build_transcript_response(cli: &Cli, proxy: &ResolvedProxy) -> Result<TranscriptResponse> {
    let requested_lang = cli.lang.as_deref().unwrap_or("en");
    let language_codes = preferred_language_codes(cli.lang.as_deref().unwrap_or("en"));
    let language_refs = language_codes
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let title = fetch_video_title(&cli.url, &language_refs, proxy).await?;
    let snippets = fetch_transcript_snippets(&cli.url, requested_lang, proxy).await?;

    if cli.response_limit <= 0 {
        return Ok(TranscriptResponse {
            title,
            transcript: snippets
                .iter()
                .map(|snippet| snippet.text.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            next_cursor: None,
        });
    }

    let start_index = parse_next_cursor(cli.next_cursor.as_deref())?;
    let (transcript, next_cursor) = paginate_transcript(&snippets, cli.response_limit, start_index);

    Ok(TranscriptResponse {
        title,
        transcript,
        next_cursor,
    })
}

async fn build_timed_transcript_response(
    cli: &Cli,
    proxy: &ResolvedProxy,
) -> Result<TimedTranscriptResponse> {
    let requested_lang = cli.lang.as_deref().unwrap_or("en");
    let language_codes = preferred_language_codes(cli.lang.as_deref().unwrap_or("en"));
    let language_refs = language_codes
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let title = fetch_video_title(&cli.url, &language_refs, proxy).await?;
    let snippets = fetch_transcript_snippets(&cli.url, requested_lang, proxy).await?;

    if cli.response_limit <= 0 {
        return Ok(TimedTranscriptResponse {
            title,
            snippets: snippets
                .iter()
                .map(TranscriptSnippetResponse::from)
                .collect(),
            next_cursor: None,
        });
    }

    let start_index = parse_next_cursor(cli.next_cursor.as_deref())?;
    let (snippets, next_cursor) =
        paginate_timed_transcript(&title, &snippets, cli.response_limit, start_index)?;

    Ok(TimedTranscriptResponse {
        title,
        snippets,
        next_cursor,
    })
}

async fn build_video_info_response(cli: &Cli, proxy: &ResolvedProxy) -> Result<VideoInfoResponse> {
    let player_response = fetch_html_player_response(&cli.url, proxy).await?;
    let video_details = player_response
        .video_details
        .as_ref()
        .context("missing `videoDetails` in YouTube player response")?;
    let microformat = player_response
        .microformat
        .as_ref()
        .and_then(|microformat| microformat.player_microformat_renderer.as_ref())
        .context("missing `playerMicroformatRenderer` in YouTube player response")?;

    let title = video_details
        .title
        .clone()
        .context("missing `title` in YouTube player response")?;
    let description = video_details
        .short_description
        .clone()
        .context("missing `shortDescription` in YouTube player response")?;
    let uploader = video_details
        .author
        .clone()
        .context("missing `author` in YouTube player response")?;
    let upload_date_raw = microformat
        .upload_date
        .as_deref()
        .context("missing `uploadDate` in YouTube player response")?;
    let duration_seconds = video_details
        .length_seconds
        .as_deref()
        .context("missing `lengthSeconds` in YouTube player response")?
        .parse::<i64>()
        .context("failed to parse `lengthSeconds` from YouTube player response")?;

    Ok(VideoInfoResponse {
        title,
        description,
        uploader,
        upload_date: parse_upload_datetime(upload_date_raw)?,
        duration: naturaldelta(duration_seconds),
    })
}

async fn fetch_transcript_snippets(
    url: &str,
    requested_lang: &str,
    proxy: &ResolvedProxy,
) -> Result<Vec<InternalTranscriptSnippet>> {
    let player_response = fetch_player_response(url, proxy).await?;
    let track = select_caption_track(&player_response, requested_lang).with_context(|| {
        format!(
            "no transcript track found for preferred languages: {}",
            preferred_language_codes(requested_lang).join(", ")
        )
    })?;

    let client = proxy.http_client(None)?;
    let response = client
        .get(&track.base_url)
        .send()
        .await
        .context("failed to download transcript track")?
        .error_for_status()
        .context("transcript track returned an error status")?;

    let transcript_xml = response
        .text()
        .await
        .context("failed to read transcript track body")?;

    parse_transcript_xml(&transcript_xml)
}

fn select_caption_track(
    player_response: &YouTubePlayerResponse,
    requested_lang: &str,
) -> Option<CaptionTrack> {
    let tracks = player_response
        .captions
        .as_ref()?
        .player_captions_tracklist_renderer
        .as_ref()?
        .caption_tracks
        .as_slice();

    for language in preferred_language_codes(requested_lang) {
        if let Some(track) = tracks
            .iter()
            .find(|track| track.language_code == language && track.kind.as_deref() != Some("asr"))
        {
            return Some(track.clone());
        }

        if let Some(track) = tracks.iter().find(|track| track.language_code == language) {
            return Some(track.clone());
        }
    }

    None
}

async fn fetch_player_response(url: &str, proxy: &ResolvedProxy) -> Result<YouTubePlayerResponse> {
    let video_id = parse_video_id(url)?;
    let html = fetch_video_html(url, None, proxy).await?;
    let api_key = extract_innertube_api_key(&html)?;
    let client = proxy.http_client(None)?;
    let response = client
        .post(format!("{YOUTUBE_INNERTUBE_PLAYER_API_URL}?key={api_key}"))
        .json(&json!({
            "context": {
                "client": {
                    "clientName": YOUTUBE_INNERTUBE_CLIENT_NAME,
                    "clientVersion": YOUTUBE_INNERTUBE_CLIENT_VERSION,
                }
            },
            "videoId": video_id,
        }))
        .send()
        .await
        .context("failed to fetch YouTube player response")?
        .error_for_status()
        .context("YouTube player response returned an error status")?;

    let player_response = response
        .json::<YouTubePlayerResponse>()
        .await
        .context("failed to parse YouTube player response JSON")?;

    if player_response
        .playability_status
        .as_ref()
        .and_then(|status| status.status.as_deref())
        .is_some_and(|status| status != "OK")
    {
        let reason = player_response
            .playability_status
            .as_ref()
            .and_then(|status| status.reason.as_deref())
            .unwrap_or("unknown YouTube playability error");
        bail!("YouTube returned non-playable status for transcript retrieval: {reason}");
    }

    Ok(player_response)
}

async fn fetch_html_player_response(
    url: &str,
    proxy: &ResolvedProxy,
) -> Result<YouTubePlayerResponse> {
    let html = fetch_video_html(url, None, proxy).await?;
    let raw_player_response =
        extract_json_object_after_marker(&html, "var ytInitialPlayerResponse = ")
            .or_else(|| extract_json_object_after_marker(&html, "ytInitialPlayerResponse = "))
            .context("failed to extract `ytInitialPlayerResponse` from YouTube watch page")?;

    serde_json::from_str::<YouTubePlayerResponse>(&raw_player_response)
        .context("failed to parse `ytInitialPlayerResponse` JSON")
}

async fn fetch_video_title(url: &str, languages: &[&str], proxy: &ResolvedProxy) -> Result<String> {
    let accept_language = languages.join(",");
    let body = fetch_video_html(url, Some(&accept_language), proxy).await?;
    let document = Html::parse_document(&body);
    let selector = Selector::parse("title").map_err(|error| anyhow!(error.to_string()))?;

    let title = document
        .select(&selector)
        .next()
        .map(|element| element.text().collect::<String>())
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| FALLBACK_TITLE.to_string());

    Ok(title)
}

fn parse_video_id(url: &str) -> Result<String> {
    let parsed_url = Url::parse(url).with_context(|| format!("invalid URL: {url}"))?;

    if parsed_url.host_str() == Some("youtu.be") {
        let video_id = parsed_url.path().trim_start_matches('/').to_string();
        if video_id.is_empty() {
            bail!("couldn't find a video ID from the provided URL: {url}.");
        }
        return Ok(video_id);
    }

    let video_id = parsed_url
        .query_pairs()
        .find_map(|(key, value)| (key == "v").then(|| value.into_owned()))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("couldn't find a video ID from the provided URL: {url}."))?;

    Ok(video_id)
}

async fn fetch_video_html(
    url: &str,
    accept_language: Option<&str>,
    proxy: &ResolvedProxy,
) -> Result<String> {
    let client = proxy.http_client(accept_language)?;
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("failed to fetch video page for `{url}`"))?
        .error_for_status()
        .with_context(|| format!("video page returned an error status for `{url}`"))?;

    let body = response
        .text()
        .await
        .context("failed to read video page body")?;

    if !body.contains("action=\"https://consent.youtube.com/s\"") {
        return Ok(body);
    }

    let consent_value = extract_consent_value(&body)?;
    let response = client
        .get(url)
        .header(COOKIE, format!("CONSENT=YES+{consent_value}"))
        .send()
        .await
        .with_context(|| format!("failed to refetch consented video page for `{url}`"))?
        .error_for_status()
        .with_context(|| format!("consented video page returned an error status for `{url}`"))?;

    let body = response
        .text()
        .await
        .context("failed to read consented video page body")?;

    if body.contains("action=\"https://consent.youtube.com/s\"") {
        bail!("failed to bypass YouTube consent page for `{url}`");
    }

    Ok(body)
}

fn extract_innertube_api_key(html: &str) -> Result<String> {
    for pattern in [
        r#"\"INNERTUBE_API_KEY\":\s*\"([a-zA-Z0-9_-]+)\""#,
        r#""INNERTUBE_API_KEY":\s*"([a-zA-Z0-9_-]+)""#,
    ] {
        let regex = Regex::new(pattern).context("failed to compile INNERTUBE API key regex")?;
        if let Some(captures) = regex.captures(html) {
            if let Some(api_key) = captures.get(1) {
                return Ok(api_key.as_str().to_string());
            }
        }
    }

    bail!("failed to extract `INNERTUBE_API_KEY` from YouTube watch page")
}

fn extract_consent_value(html: &str) -> Result<String> {
    let regex = Regex::new(r#"name=\"v\" value=\"([^\"]+)\""#)
        .context("failed to compile YouTube consent regex")?;
    let captures = regex
        .captures(html)
        .context("failed to extract YouTube consent value from watch page")?;
    let value = captures
        .get(1)
        .map(|value| value.as_str().to_string())
        .context("missing YouTube consent value capture")?;
    Ok(value)
}

fn extract_json_object_after_marker(html: &str, marker: &str) -> Option<String> {
    let start = html.find(marker)? + marker.len();
    let json_start = html[start..].find('{')? + start;
    let mut depth = 0_u32;
    let mut in_string = false;
    let mut escaped = false;

    for (offset, ch) in html[json_start..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
                continue;
            }

            if ch == '\\' {
                escaped = true;
                continue;
            }

            if ch == '"' {
                in_string = false;
            }

            continue;
        }

        if ch == '"' {
            in_string = true;
            continue;
        }

        if ch == '{' {
            depth += 1;
            continue;
        }

        if ch == '}' {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                let end = json_start + offset + ch.len_utf8();
                return Some(html[json_start..end].to_string());
            }
        }
    }

    None
}

fn parse_transcript_xml(xml: &str) -> Result<Vec<InternalTranscriptSnippet>> {
    let document = Document::parse(xml).context("failed to parse transcript XML")?;
    let mut snippets = Vec::new();

    for node in document.descendants().filter(|node| node.has_tag_name("p")) {
        let text = node
            .children()
            .filter_map(|child| child.text())
            .collect::<String>();

        if text.is_empty() {
            continue;
        }

        let start_ms = node
            .attribute("t")
            .context("missing `t` attribute in transcript XML")?
            .parse::<f64>()
            .context("failed to parse `t` attribute in transcript XML")?;
        let duration_ms = match node.attribute("d") {
            Some(value) => value
                .parse::<f64>()
                .context("failed to parse `d` attribute in transcript XML")?,
            None => 0.0,
        };

        snippets.push(InternalTranscriptSnippet {
            text,
            start: start_ms / 1000.0,
            duration: duration_ms / 1000.0,
        });
    }

    Ok(snippets)
}

fn preferred_language_codes(lang: &str) -> Vec<String> {
    if lang == "en" {
        return vec!["en".to_string()];
    }

    vec![lang.to_string(), "en".to_string()]
}

fn parse_next_cursor(next_cursor: Option<&str>) -> Result<usize> {
    match next_cursor {
        Some(value) => value
            .parse::<usize>()
            .with_context(|| format!("invalid `next_cursor` value: {value}")),
        None => Ok(0),
    }
}

fn paginate_transcript(
    snippets: &[InternalTranscriptSnippet],
    response_limit: i64,
    start_index: usize,
) -> (String, Option<String>) {
    let mut transcript = String::new();
    let mut next_cursor = None;
    let limit = response_limit as usize;

    for (index, snippet) in snippets.iter().enumerate().skip(start_index) {
        let line_len = snippet.text.chars().count();
        let current_len = transcript.chars().count();

        if current_len + line_len + 1 > limit {
            next_cursor = Some(index.to_string());
            break;
        }

        transcript.push_str(&snippet.text);
        transcript.push('\n');
    }

    if transcript.ends_with('\n') {
        transcript.pop();
    }

    (transcript, next_cursor)
}

fn paginate_timed_transcript(
    title: &str,
    snippets: &[InternalTranscriptSnippet],
    response_limit: i64,
    start_index: usize,
) -> Result<(Vec<TranscriptSnippetResponse>, Option<String>)> {
    let mut response_snippets = Vec::new();
    let mut next_cursor = None;
    let limit = response_limit as usize;
    let size = title.chars().count() + 1;

    for (index, snippet) in snippets.iter().enumerate().skip(start_index) {
        let response_snippet = TranscriptSnippetResponse::from(snippet);
        if size + json_char_count(&response_snippet)? + 1 > limit {
            next_cursor = Some(index.to_string());
            break;
        }

        response_snippets.push(response_snippet);
    }

    Ok((response_snippets, next_cursor))
}

fn parse_upload_datetime(upload_date: &str) -> Result<String> {
    if let Ok(parsed) = DateTime::parse_from_rfc3339(upload_date) {
        let upload_datetime = parsed.with_timezone(&Utc);
        if upload_datetime.timestamp_subsec_micros() == 0 {
            return Ok(upload_datetime.format("%Y-%m-%dT%H:%M:%SZ").to_string());
        }

        return Ok(upload_datetime.to_rfc3339_opts(SecondsFormat::Micros, true));
    }

    let parsed_date = NaiveDate::parse_from_str(upload_date, "%Y-%m-%d")
        .with_context(|| format!("invalid upload date from YouTube microformat: {upload_date}"))?;
    let upload_datetime =
        Utc.from_utc_datetime(&parsed_date.and_hms_opt(0, 0, 0).ok_or_else(|| {
            anyhow!("invalid upload date from YouTube microformat: {upload_date}")
        })?);

    Ok(upload_datetime.format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

fn naturaldelta(seconds: i64) -> String {
    let seconds = seconds.unsigned_abs();
    let total_days = seconds / 86_400;
    let day_remainder = seconds % 86_400;
    let years = total_days / 365;
    let days = total_days % 365;
    let num_months = round_ties_even(days as f64 / 30.5) as u64;

    if years == 0 && days < 1 {
        if day_remainder == 0 {
            return "a moment".to_string();
        }

        if day_remainder == 1 {
            return "a second".to_string();
        }

        if day_remainder < 60 {
            return format!("{} seconds", day_remainder);
        }

        if day_remainder < 3_600 {
            let minutes = round_ties_even(day_remainder as f64 / 60.0) as u64;
            if minutes == 1 {
                return "a minute".to_string();
            }

            if minutes == 60 {
                return "an hour".to_string();
            }

            return format!("{} minutes", minutes);
        }

        let hours = round_ties_even(day_remainder as f64 / 3_600.0) as u64;
        if hours == 1 {
            return "an hour".to_string();
        }

        if hours == 24 {
            return "a day".to_string();
        }

        return format!("{} hours", hours);
    }

    if years == 0 {
        if days == 1 {
            return "a day".to_string();
        }

        if num_months == 0 {
            return pluralize(days, "day");
        }

        if num_months == 1 {
            return "a month".to_string();
        }

        if num_months == 12 {
            return "a year".to_string();
        }

        return pluralize(num_months, "month");
    }

    if years == 1 {
        if num_months == 0 && days == 0 {
            return "a year".to_string();
        }

        if num_months == 0 {
            return format!("1 year, {}", pluralize(days, "day"));
        }

        if num_months == 1 {
            return "1 year, 1 month".to_string();
        }

        if num_months == 12 {
            return "2 years".to_string();
        }

        return format!("1 year, {}", pluralize(num_months, "month"));
    }

    format!("{} years", with_commas(years))
}

fn round_ties_even(value: f64) -> f64 {
    if !value.is_finite() {
        return value;
    }

    let floor = value.floor();
    let fraction = value - floor;
    let epsilon = 1e-12;

    if fraction < 0.5 - epsilon {
        return floor;
    }

    if fraction > 0.5 + epsilon {
        return floor + 1.0;
    }

    if (floor as i64) % 2 == 0 {
        return floor;
    }

    floor + 1.0
}

fn pluralize(value: u64, noun: &str) -> String {
    if value == 1 {
        return format!("1 {noun}");
    }

    format!("{value} {noun}s")
}

fn with_commas(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);

    for (index, ch) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }

    grouped.chars().rev().collect()
}

fn json_char_count<T: Serialize>(value: &T) -> Result<usize> {
    Ok(serde_json::to_string(value)
        .context("failed to serialize JSON while computing response size")?
        .chars()
        .count())
}

fn webshare_proxy_url(username: &str, password: &str) -> String {
    format!("http://{username}-rotate:{password}@p.webshare.io:80/")
}

fn write_json<T: Serialize>(value: &T) -> Result<()> {
    let json = serde_json::to_string(value).context("failed to serialize response to JSON")?;
    println!("{json}");
    Ok(())
}

impl From<&InternalTranscriptSnippet> for TranscriptSnippetResponse {
    fn from(value: &InternalTranscriptSnippet) -> Self {
        Self {
            text: value.text.clone(),
            start: value.start,
            duration: value.duration,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snippet(text: &str, start: f64, duration: f64) -> InternalTranscriptSnippet {
        InternalTranscriptSnippet {
            text: text.to_string(),
            start,
            duration,
        }
    }

    #[test]
    fn parses_watch_video_id() {
        let url = "https://www.youtube.com/watch?v=LPZh9BOjkQs";
        assert_eq!(parse_video_id(url).expect("video id"), "LPZh9BOjkQs");
    }

    #[test]
    fn parses_short_video_id() {
        let url = "https://youtu.be/LPZh9BOjkQs";
        assert_eq!(parse_video_id(url).expect("video id"), "LPZh9BOjkQs");
    }

    #[test]
    fn rejects_invalid_video_id_url() {
        let url = "https://www.youtube.com/watch?vv=abcdefg";
        let error = parse_video_id(url).expect_err("invalid URL should fail");
        assert!(error.to_string().contains("couldn't find a video ID"));
    }

    #[test]
    fn parses_rfc3339_upload_datetime() {
        let upload_date = parse_upload_datetime("2024-11-20T07:07:15-08:00").expect("upload date");
        assert_eq!(upload_date, "2024-11-20T15:07:15Z");
    }

    #[test]
    fn parses_date_only_upload_datetime() {
        let upload_date = parse_upload_datetime("2025-09-21").expect("upload date");
        assert_eq!(upload_date, "2025-09-21T00:00:00Z");
    }

    #[test]
    fn naturaldelta_matches_documented_examples() {
        assert_eq!(naturaldelta(0), "a moment");
        assert_eq!(naturaldelta(1), "a second");
        assert_eq!(naturaldelta(30 * 60), "30 minutes");
        assert_eq!(naturaldelta(24 * 60 * 60), "a day");
        assert_eq!(naturaldelta(1_234_567), "14 days");
    }

    #[test]
    fn transcript_pagination_uses_string_index_cursor() {
        let snippets = vec![
            snippet("first", 0.0, 1.0),
            snippet("second", 1.0, 1.0),
            snippet("third", 2.0, 1.0),
        ];

        let (transcript, next_cursor) = paginate_transcript(&snippets, 12, 0);

        assert_eq!(transcript, "first");
        assert_eq!(next_cursor, Some("1".to_string()));
    }

    #[test]
    fn timed_transcript_pagination_matches_reference_logic() {
        let snippets = vec![
            snippet("a", 0.0, 1.0),
            snippet("b", 1.0, 1.0),
            snippet("c", 2.0, 1.0),
        ];

        let (page, next_cursor) =
            paginate_timed_transcript("Title", &snippets, 1_000, 0).expect("page");

        assert_eq!(page.len(), 3);
        assert_eq!(next_cursor, None);
    }

    #[test]
    fn parses_transcript_xml_snippets() {
        let xml = r#"<?xml version="1.0" encoding="utf-8" ?><timedtext format="3"><body><p t="1140" d="2836">Hello <s>world</s></p><p t="4000">Bye</p></body></timedtext>"#;

        let snippets = parse_transcript_xml(xml).expect("snippets");

        assert_eq!(snippets.len(), 2);
        assert_eq!(snippets[0].text, "Hello world");
        assert_eq!(snippets[0].start, 1.14);
        assert_eq!(snippets[0].duration, 2.836);
        assert_eq!(snippets[1].text, "Bye");
        assert_eq!(snippets[1].duration, 0.0);
    }

    #[test]
    fn parses_proxy_env_file_in_shell_format() {
        let dotenv = r#"
			# comment
			export HTTPS_PROXY="http://authkey:secret@10.222.6.1:65531"
			HTTP_PROXY='http://127.0.0.1:8080'
			IGNORED_VALUE="nope"
		"#;

        let parsed = parse_proxy_env_from_dotenv(dotenv).expect("proxy env");

        assert_eq!(
            parsed,
            ProxyEnvFile {
                http_proxy: Some("http://127.0.0.1:8080".to_string()),
                https_proxy: Some("http://authkey:secret@10.222.6.1:65531".to_string()),
            }
        );
    }

    #[test]
    fn rejects_invalid_quoted_proxy_env_value() {
        let dotenv = "HTTPS_PROXY=\"http://proxy.example.com\" trailing";
        let error =
            parse_proxy_env_from_dotenv(dotenv).expect_err("invalid quoted value must fail");

        assert!(format!("{error:#}").contains("unexpected trailing content after quoted value"));
    }
}
