//! Update checking against the project's GitHub releases.
//!
//! This is the only network access the app ever makes, it is off unless the
//! user leaves it on, and it never installs anything by itself: the most it
//! does is download the installer and hand it to the user to run.

use anyhow::{bail, Context, Result};
use std::path::PathBuf;

/// Overridable so a fork, or a test, can point somewhere else.
pub const DEFAULT_REPO: &str = "wlwatkins/rosetta-desktop";
/// GitHub rejects API requests without one.
const USER_AGENT: &str = concat!("rosetta-desktop/", env!("CARGO_PKG_VERSION"));

pub fn repo() -> String {
    std::env::var("ROSETTA_REPO").unwrap_or_else(|_| DEFAULT_REPO.to_string())
}

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub tag: String,
    /// The release page, for "what changed".
    pub page_url: String,
    /// The installer asset, when the release has one.
    pub installer_url: Option<String>,
    pub installer_name: Option<String>,
    pub notes: String,
}

/// A semantic version, compared numerically rather than as text so that
/// 0.10.0 sorts above 0.9.0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    major: u32,
    minor: u32,
    patch: u32,
    /// 1 for a final release, 0 for a pre-release, so 1.0.0-rc1 < 1.0.0.
    release_rank: u8,
}

impl Version {
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim().trim_start_matches(['v', 'V']);
        let (core, pre) = match text.split_once(['-', '+']) {
            Some((core, rest)) => (core, !rest.is_empty()),
            None => (text, false),
        };

        let mut parts = core.split('.');
        let major = parts.next()?.parse().ok()?;
        // A missing minor or patch reads as zero, so "1" means 1.0.0.
        let minor = parts.next().unwrap_or("0").parse().ok()?;
        let patch = parts.next().unwrap_or("0").parse().ok()?;
        if parts.next().is_some() {
            return None;
        }

        Some(Self { major, minor, patch, release_rank: if pre { 0 } else { 1 } })
    }
}

/// `Ok(Some(release))` only when the published version is strictly newer.
pub fn check(current: &str) -> Result<Option<Release>> {
    let latest = fetch_latest()?;
    Ok(newer_of(current, &latest))
}

/// The comparison, split out so it can be tested without the network.
pub fn newer_of(current: &str, latest: &Release) -> Option<Release> {
    let here = Version::parse(current)?;
    let there = Version::parse(&latest.version)?;
    (there > here).then(|| latest.clone())
}

fn fetch_latest() -> Result<Release> {
    let url = format!("https://api.github.com/repos/{}/releases/latest", repo());
    let body = get_text(&url)?;
    parse_release(&body)
}

fn get_text(url: &str) -> Result<String> {
    let mut response = ureq::get(url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .call()
        .with_context(|| format!("requesting {url}"))?;
    Ok(response.body_mut().read_to_string()?)
}

/// Pulls what we need out of a GitHub release payload.
pub fn parse_release(json: &str) -> Result<Release> {
    let v: serde_json::Value = serde_json::from_str(json).context("parsing the release JSON")?;

    let tag = v["tag_name"].as_str().unwrap_or_default().to_string();
    if tag.is_empty() {
        bail!("the release has no tag_name");
    }
    let version = tag.trim_start_matches(['v', 'V']).to_string();

    // Prefer the installer; a release might also carry a portable zip.
    let mut installer_url = None;
    let mut installer_name = None;
    if let Some(assets) = v["assets"].as_array() {
        for asset in assets {
            let name = asset["name"].as_str().unwrap_or_default();
            let lower = name.to_ascii_lowercase();
            if lower.ends_with(".exe") && lower.contains("setup") {
                installer_url = asset["browser_download_url"].as_str().map(str::to_string);
                installer_name = Some(name.to_string());
                break;
            }
        }
    }

    Ok(Release {
        version,
        tag,
        page_url: v["html_url"].as_str().unwrap_or_default().to_string(),
        installer_url,
        installer_name,
        notes: v["body"].as_str().unwrap_or_default().to_string(),
    })
}

/// Downloads the installer to the temp directory and returns its path.
///
/// Nothing is executed here: the caller shows the user where it went and lets
/// them start it.
pub fn download_installer(release: &Release) -> Result<PathBuf> {
    let (Some(url), Some(name)) = (&release.installer_url, &release.installer_name) else {
        bail!("release {} has no installer attached", release.tag);
    };

    let dest = std::env::temp_dir().join(name);
    let mut response = ureq::get(url)
        .header("User-Agent", USER_AGENT)
        .call()
        .with_context(|| format!("downloading {url}"))?;

    let mut file = std::fs::File::create(&dest)
        .with_context(|| format!("creating {}", dest.display()))?;
    std::io::copy(&mut response.body_mut().as_reader(), &mut file)
        .with_context(|| format!("writing {}", dest.display()))?;

    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_versions() {
        assert_eq!(Version::parse("1.2.3"), Version::parse("v1.2.3"));
        assert!(Version::parse("1.2.3").is_some());
        assert!(Version::parse("V0.1.0").is_some());
    }

    #[test]
    fn missing_components_read_as_zero() {
        assert_eq!(Version::parse("1"), Version::parse("1.0.0"));
        assert_eq!(Version::parse("1.2"), Version::parse("1.2.0"));
    }

    #[test]
    fn rejects_rubbish() {
        assert!(Version::parse("").is_none());
        assert!(Version::parse("banana").is_none());
        assert!(Version::parse("1.2.3.4").is_none());
        assert!(Version::parse("1.x.3").is_none());
    }

    #[test]
    fn compares_numerically_not_lexically() {
        // The whole reason for parsing rather than comparing strings.
        assert!(Version::parse("0.10.0") > Version::parse("0.9.0"));
        assert!(Version::parse("1.0.0") > Version::parse("0.99.99"));
        assert!(Version::parse("0.1.10") > Version::parse("0.1.9"));
    }

    #[test]
    fn a_prerelease_sorts_below_its_final() {
        assert!(Version::parse("1.0.0") > Version::parse("1.0.0-rc.1"));
        assert!(Version::parse("1.0.0-rc.1") > Version::parse("0.9.9"));
    }

    fn release(version: &str) -> Release {
        Release {
            version: version.into(),
            tag: format!("v{version}"),
            page_url: String::new(),
            installer_url: None,
            installer_name: None,
            notes: String::new(),
        }
    }

    #[test]
    fn only_a_strictly_newer_release_counts() {
        assert!(newer_of("0.1.0", &release("0.2.0")).is_some());
        assert!(newer_of("0.1.0", &release("0.1.0")).is_none(), "same version");
        assert!(newer_of("0.2.0", &release("0.1.0")).is_none(), "older version");
    }

    #[test]
    fn an_unparseable_version_never_prompts_an_update() {
        assert!(newer_of("banana", &release("9.9.9")).is_none());
        assert!(newer_of("0.1.0", &release("banana")).is_none());
    }

    const PAYLOAD: &str = r#"{
        "tag_name": "v0.3.1",
        "html_url": "https://github.com/o/r/releases/tag/v0.3.1",
        "body": "Fixed the thing.",
        "assets": [
            {"name": "notes.txt", "browser_download_url": "https://example/notes.txt"},
            {"name": "Rosetta-Setup-0.3.1.exe", "browser_download_url": "https://example/setup.exe"}
        ]
    }"#;

    #[test]
    fn reads_a_github_release_payload() {
        let r = parse_release(PAYLOAD).unwrap();
        assert_eq!(r.version, "0.3.1");
        assert_eq!(r.tag, "v0.3.1");
        assert_eq!(r.notes, "Fixed the thing.");
        assert_eq!(r.installer_name.as_deref(), Some("Rosetta-Setup-0.3.1.exe"));
        assert_eq!(r.installer_url.as_deref(), Some("https://example/setup.exe"));
    }

    #[test]
    fn picks_the_installer_out_of_several_assets() {
        // The .txt comes first in the list and must not be chosen.
        let r = parse_release(PAYLOAD).unwrap();
        assert!(r.installer_name.as_deref().unwrap().ends_with(".exe"));
    }

    #[test]
    fn a_release_without_an_installer_is_still_readable() {
        let r = parse_release(r#"{"tag_name": "v1.0.0", "assets": []}"#).unwrap();
        assert_eq!(r.version, "1.0.0");
        assert!(r.installer_url.is_none());
    }

    #[test]
    fn a_payload_without_a_tag_is_an_error() {
        assert!(parse_release(r#"{"assets": []}"#).is_err());
        assert!(parse_release("not json").is_err());
    }

    #[test]
    fn the_crate_version_is_parseable() {
        // Guards against someone putting something exotic in Cargo.toml that
        // would silently disable update checks.
        assert!(Version::parse(current_version()).is_some(), "{}", current_version());
    }
}
