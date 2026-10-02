use std::{fmt, path::PathBuf, str::FromStr};

use anyhow::{Context, bail};
use facet::Facet;

use crate::apps::Argv;

/// config.toml as written. Only [`Config`] leaves this module.
#[derive(Facet)]
struct RawConfig {
    device: String,
    connector: String,
    mode: Option<String>,
    #[facet(default)]
    launch: Vec<String>,
}

pub struct Config {
    pub device: PathBuf,
    pub connector: String,
    /// `None` takes whatever the display marks as preferred.
    pub mode: Option<Mode>,
    pub launch: Option<Argv>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    pub width: u16,
    pub height: u16,
    /// Whole hertz, matched against the mode's nominal rate. 59.94 and 60 both
    /// answer to 60, and the exact one wins (see `drm::pick_mode`).
    pub refresh: u32,
}

impl Config {
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("in {}", path.display()))
    }

    fn parse(text: &str) -> anyhow::Result<Self> {
        let raw: RawConfig = facet_toml::from_str(text).map_err(|e| anyhow::anyhow!("{e}"))?;
        let mode = raw.mode.as_deref().map(str::parse).transpose()?;
        Ok(Self {
            device: raw.device.into(),
            connector: raw.connector,
            mode,
            launch: Argv::from_vec(raw.launch),
        })
    }
}

impl FromStr for Mode {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        let parse = || -> Option<Self> {
            let (size, refresh) = s.split_once('@')?;
            let (width, height) = size.split_once('x')?;
            Some(Self {
                width: width.parse().ok()?,
                height: height.parse().ok()?,
                refresh: refresh.parse().ok()?,
            })
        };
        match parse() {
            Some(mode) if mode.width > 0 && mode.height > 0 && mode.refresh > 0 => Ok(mode),
            _ => bail!("mode {s:?} is not WIDTHxHEIGHT@HZ, e.g. 3840x2160@60"),
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}@{}", self.width, self.height, self.refresh)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_config() {
        let config = Config::parse(
            r#"
            device = "/dev/dri/by-path/pci-0000:01:00.0-card"
            connector = "HDMI-A-1"
            mode = "3840x2160@60"
            launch = ["foot", "--fullscreen"]
            "#,
        )
        .unwrap();
        assert_eq!(config.connector, "HDMI-A-1");
        assert_eq!(
            config.mode,
            Some(Mode {
                width: 3840,
                height: 2160,
                refresh: 60
            })
        );
        let launch = config.launch.unwrap();
        assert_eq!(launch.program, "foot");
        assert_eq!(launch.args, ["--fullscreen"]);
    }

    #[test]
    fn mode_and_launch_are_optional() {
        let config = Config::parse(
            r#"
            device = "/dev/dri/card0"
            connector = "HDMI-A-1"
            "#,
        )
        .unwrap();
        assert!(config.mode.is_none());
        assert!(config.launch.is_none());
    }

    #[test]
    fn rejects_malformed_modes() {
        for bad in [
            "3840x2160",
            "3840@60",
            "0x2160@60",
            "3840x2160@0",
            "4kx2160@60",
            "",
        ] {
            assert!(bad.parse::<Mode>().is_err(), "{bad:?} parsed");
        }
    }

    #[test]
    fn mode_round_trips() {
        let mode: Mode = "1920x1080@120".parse().unwrap();
        assert_eq!(mode.to_string(), "1920x1080@120");
    }
}
