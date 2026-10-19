//! GitHub supplies bounded discovery hints. Only the separately signed release
//! manifest may authorize an update or advance the local trust floor.
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use reqwest::blocking::Client;
use semver::Version;
use serde::Deserialize;

pub(super) const RELEASES_API: &str =
    "https://api.github.com/repos/Setmaster/Video_For_Lazies/releases";
pub(super) const RELEASES_WEB: &str = "https://github.com/Setmaster/Video_For_Lazies/releases";
pub(super) const MANIFEST_NAME: &str = "vfl-update-manifest-v1.json";
pub(super) const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(45);
const MAX_PAGES: usize = 10;
const PAGE_SIZE: usize = 100;
const PAGE_BYTES: usize = 5 * 1024 * 1024;
const INVENTORY_BYTES: usize = 20 * 1024 * 1024;
const MAX_ASSETS: usize = 256;
static RATE_LIMIT_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(super) struct Release {
    pub id: u64,
    pub tag_name: String,
    pub draft: bool,
    pub prerelease: bool,
    pub published_at: Option<String>,
    pub assets: Vec<Asset>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(super) struct Asset {
    pub id: u64,
    pub name: String,
    pub state: String,
    pub size: u64,
    pub browser_download_url: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Candidate {
    pub version: Version,
    pub archive_name: String,
    pub archive_size: u64,
}

impl Candidate {
    pub fn tag(&self) -> String {
        format!("v{}", self.version)
    }

    pub fn manifest_url(&self) -> String {
        manifest_url(&self.version)
    }
}

pub(super) fn stable_version(tag: &str) -> Option<Version> {
    let version = Version::parse(tag.strip_prefix('v')?).ok()?;
    (version.pre.is_empty() && version.build.is_empty() && tag == format!("v{version}"))
        .then_some(version)
}

pub(super) fn manifest_url(version: &Version) -> String {
    format!("{RELEASES_WEB}/download/v{version}/{MANIFEST_NAME}")
}

pub(super) fn archive_name(version: &Version, target: &str) -> Result<String, String> {
    let platform = match target {
        "windows-x64" => "win-x64",
        "linux-x64" => "linux-x64",
        _ => return Err("Unsupported update platform.".to_string()),
    };
    Ok(format!("Video_For_Lazies-v{version}-{platform}.zip"))
}

pub(super) fn select(releases: &[Release], target: &str) -> Result<Candidate, String> {
    let mut selected: Option<(&Release, Version, String)> = None;
    let mut ids = HashMap::new();
    let mut tags = HashSet::new();
    for release in releases {
        if let Some(previous) = ids.insert(release.id, release) {
            if previous == release {
                continue;
            }
            return Err("Conflicting update release identities.".to_string());
        }
        if release.id == 0 || !tags.insert(&release.tag_name) {
            return Err("Conflicting or repeated update release identities.".to_string());
        }
        if release.assets.len() > MAX_ASSETS {
            return Err("Update release has too many assets.".to_string());
        }
        if release.draft
            || release.prerelease
            || release.published_at.as_deref().is_none_or(str::is_empty)
        {
            continue;
        }
        let Some(version) = stable_version(&release.tag_name) else {
            continue;
        };
        let name = archive_name(&version, target)?;
        // Asset presence advertises this platform, even if its upload or
        // metadata is broken. Never prefilter a broken newer target release
        // into an apparently successful check of an older release.
        if !release.assets.iter().any(|asset| asset.name == name) {
            continue;
        }
        if selected.as_ref().is_none_or(|(_, old, _)| version > *old) {
            selected = Some((release, version, name));
        }
    }
    let (release, version, name) = selected
        .ok_or_else(|| "No stable update release is available for this platform.".to_string())?;
    let archive = unique_uploaded_asset(release, &name)?;
    unique_uploaded_asset(release, MANIFEST_NAME)?;
    unique_uploaded_asset(release, &format!("{MANIFEST_NAME}.sig"))?;
    let mut asset_ids = HashSet::new();
    for name in [
        &name,
        &MANIFEST_NAME.to_string(),
        &format!("{MANIFEST_NAME}.sig"),
    ] {
        let asset = unique_uploaded_asset(release, name)?;
        if !asset_ids.insert(asset.id)
            || asset.browser_download_url
                != format!("{RELEASES_WEB}/download/{}/{name}", release.tag_name)
        {
            return Err(
                "The selected update release has inconsistent asset identity or URLs.".to_string(),
            );
        }
    }
    Ok(Candidate {
        version,
        archive_name: name,
        archive_size: archive.size,
    })
}

fn unique_uploaded_asset<'a>(release: &'a Release, name: &str) -> Result<&'a Asset, String> {
    let mut matches = release.assets.iter().filter(|asset| asset.name == name);
    let asset = matches.next().ok_or_else(|| {
        "The selected update release is missing required metadata or archive.".to_string()
    })?;
    if matches.next().is_some() || asset.id == 0 || asset.state != "uploaded" || asset.size == 0 {
        return Err("The selected update release has ambiguous or incomplete assets.".to_string());
    }
    Ok(asset)
}

pub(super) fn client() -> Result<Client, String> {
    Client::builder()
        .user_agent("Video For Lazies updater")
        // The repository API never needs a CDN redirect. Reject before sending
        // a second request; checking response.url() afterward would be too late.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("Failed to create update discovery client: {e}"))
}

pub(super) fn remaining(deadline: Instant) -> Result<Duration, String> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| "Update discovery exceeded its time limit. Try again later.".to_string())
}

fn page_url(base: &str, page: usize) -> String {
    format!("{base}?per_page={PAGE_SIZE}&page={page}")
}

// Parse Link only to validate pagination completeness. Requests always use our
// generated URL. No server-provided URL is ever requested.
fn next_page(link: Option<&str>, base: &str, page: usize) -> Result<bool, String> {
    let Some(link) = link else { return Ok(false) };
    let mut seen = HashSet::new();
    let mut next = false;
    for part in link.split(',') {
        let mut fields = part.trim().split(';').map(str::trim);
        let raw = fields.next().unwrap_or_default();
        let url = raw
            .strip_prefix('<')
            .and_then(|s| s.strip_suffix('>'))
            .ok_or_else(|| "Invalid update discovery pagination.".to_string())?;
        let relation = fields
            .next()
            .and_then(|s| s.strip_prefix("rel=\""))
            .and_then(|s| s.strip_suffix('"'))
            .ok_or_else(|| "Invalid update discovery pagination relation.".to_string())?;
        if fields.next().is_some() || !seen.insert(relation) {
            return Err("Ambiguous update discovery pagination.".to_string());
        }
        let parsed = reqwest::Url::parse(url)
            .map_err(|_| "Invalid update discovery page URL.".to_string())?;
        let mut parameters = parsed.query_pairs().collect::<HashMap<_, _>>();
        let linked_page = parameters.remove("page").and_then(|s| s.parse::<usize>().ok())
            .filter(|n| *n > 0 && *n <= MAX_PAGES)
            .ok_or_else(|| "Update history exceeds this app's discovery limit. Download a newer portable app manually.".to_string())?;
        if url != page_url(base, linked_page) {
            return Err("Update discovery pagination left the expected repository.".to_string());
        }
        match relation {
            "next" if linked_page == page + 1 => next = true,
            "prev" if page > 1 && linked_page == page - 1 => {}
            "first" if linked_page == 1 => {}
            "last" if linked_page >= page => {}
            _ => return Err("Inconsistent update discovery pagination.".to_string()),
        }
    }
    if !next && seen.contains("last") {
        // A distant last link without next must not truncate the inventory.
        let last = format!("<{}>; rel=\"last\"", page_url(base, page));
        if !link.split(',').any(|part| part.trim() == last) {
            return Err("Incomplete update discovery pagination.".to_string());
        }
    }
    Ok(next)
}

fn rate_limit_error(headers: &reqwest::header::HeaderMap) -> String {
    let seconds = headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .or_else(|| {
            headers
                .get("x-ratelimit-reset")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .map(|reset| reset.saturating_sub(super::now_ms() / 1000))
        })
        .unwrap_or(60)
        .clamp(1, 3600);
    if let Ok(mut until) = RATE_LIMIT_UNTIL.lock() {
        *until = Some(Instant::now() + Duration::from_secs(seconds));
    }
    rate_limit_message(seconds)
}

fn rate_limit_message(seconds: u64) -> String {
    format!("Update checks are temporarily limited by GitHub. Try again in {seconds} seconds.")
}

// This process-local cooldown does not write successful-check preferences.
// Forced checks must honor GitHub's backoff too. A fresh process can retry;
// there is deliberately no persistent unsigned trust/preferences mutation.
pub(super) fn check_cooldown() -> Result<(), String> {
    let until = RATE_LIMIT_UNTIL
        .lock()
        .map_err(|_| "Update discovery is temporarily unavailable.".to_string())?;
    if let Some(duration) = until.and_then(|until| until.checked_duration_since(Instant::now())) {
        return Err(rate_limit_message(
            duration.as_secs().saturating_add(1).min(3600),
        ));
    }
    Ok(())
}

fn fetch_page(
    client: &Client,
    base: &str,
    page: usize,
    deadline: Instant,
    bytes: &mut usize,
) -> Result<(Vec<Release>, bool), String> {
    let mut response = client
        .get(page_url(base, page))
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .timeout(remaining(deadline)?)
        .send()
        .map_err(|e| format!("Failed to discover platform updates: {e}"))?;
    if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS
        || response.status() == reqwest::StatusCode::FORBIDDEN
    {
        return Err(rate_limit_error(response.headers()));
    }
    if !response.status().is_success() {
        return Err(format!(
            "Update discovery failed with HTTP {}. Try again later.",
            response.status()
        ));
    }
    let mut links = response.headers().get_all(reqwest::header::LINK).iter();
    let link = links
        .next()
        .map(|value| {
            value
                .to_str()
                .map_err(|_| "Invalid update discovery pagination header.".to_string())
        })
        .transpose()?;
    if links.next().is_some() {
        return Err("Ambiguous update discovery pagination headers.".to_string());
    }
    let next = next_page(link, base, page)?;
    let limit = PAGE_BYTES.min(INVENTORY_BYTES.saturating_sub(*bytes));
    if response
        .content_length()
        .is_some_and(|size| size > limit as u64)
    {
        return Err("Update discovery metadata exceeds its size limit.".to_string());
    }
    let mut body = Vec::new();
    response
        .by_ref()
        .take(limit as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|e| format!("Failed to read update discovery: {e}"))?;
    remaining(deadline)?;
    if body.len() > limit {
        return Err("Update discovery metadata exceeds its size limit.".to_string());
    }
    *bytes += body.len();
    let releases: Vec<Release> = serde_json::from_slice(&body)
        .map_err(|e| format!("Invalid update release inventory: {e}"))?;
    if releases.len() > PAGE_SIZE || releases.iter().any(|r| r.assets.len() > MAX_ASSETS) {
        return Err("Update discovery contains too many records or assets.".to_string());
    }
    // A previous page advertised this continuation. An empty tail contradicts
    // that inventory even when this response omits Link entirely.
    if releases.is_empty() && (page > 1 || next) {
        return Err("Incomplete update discovery pagination.".to_string());
    }
    Ok((releases, next))
}

pub(super) fn discover(
    client: &Client,
    base: &str,
    target: &str,
    deadline: Instant,
) -> Result<Candidate, String> {
    let mut bytes = 0;
    let mut inventory = Vec::new();
    let mut first = None;
    for page in 1..=MAX_PAGES {
        let (releases, next) = fetch_page(client, base, page, deadline, &mut bytes)?;
        if page == 1 {
            first = Some(releases.clone());
        }
        inventory.extend(releases);
        if inventory.len() >= MAX_PAGES * PAGE_SIZE {
            return Err("Update history exceeds this app's discovery limit. Download a newer portable app manually.".to_string());
        }
        if !next {
            if page > 1 {
                // Pagination is not a snapshot. Detect normal publication/page
                // movement and ask for a new check instead of guessing latest.
                let (head, head_next) = fetch_page(client, base, 1, deadline, &mut bytes)?;
                if Some(head) != first || !head_next {
                    return Err("Update releases changed during discovery. Try again.".to_string());
                }
            }
            remaining(deadline)?;
            let candidate = select(&inventory, target)?;
            remaining(deadline)?;
            return Ok(candidate);
        }
    }
    Err("Update history exceeds this app's discovery limit. Download a newer portable app manually.".to_string())
}

#[cfg(test)]
mod tests;
