use std::{collections::HashMap, fmt, num::NonZeroU8, path::PathBuf, str::FromStr, time::Duration};

use anyhow::{Context, bail};
use facet::Facet;

use crate::{
    apps::Argv,
    tv::{Layers, LinkConfig, Setting, SettingValue, Settings, TvConfig},
};

const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// config.toml as written. Only [`Config`] leaves this module.
#[derive(Facet)]
struct RawConfig {
    device: String,
    connector: String,
    mode: Option<String>,
    #[facet(default)]
    launch: Vec<String>,
    /// Seconds.
    idle_timeout: Option<u64>,
    /// Whole multiples only: fractional scaling isn't needed on a 4K TV.
    scale: Option<u8>,
    tv: Option<RawTv>,
}

/// `[tv]`. Settings are `[tv.settings.<category>]`, `[tv.home.<category>]`,
/// `[tv.app.<category>]` and `[tv.profiles.<name>.<category>]`.
#[derive(Facet)]
struct RawTv {
    host: String,
    key_file: String,
    cert_fingerprint: String,
    input: String,
    #[facet(default)]
    settings: RawSettings,
    #[facet(default)]
    home: RawSettings,
    #[facet(default)]
    app: RawSettings,
    #[facet(default)]
    profiles: HashMap<String, RawSettings>,
}

/// Category, then key.
type RawSettings = HashMap<String, HashMap<String, SettingValue>>;

pub struct Config {
    pub device: PathBuf,
    pub connector: String,
    /// `None` takes whatever the display marks as preferred.
    pub mode: Option<Mode>,
    pub launch: Option<Argv>,
    /// How long without activity until the screen blanks.
    pub idle_timeout: Duration,
    /// How many physical pixels a client's logical pixel takes, each way.
    /// Home, the on-screen keyboard and the cursor are drawn in physical
    /// pixels and ignore it.
    pub scale: NonZeroU8,
    /// `None` leaves the TV's own settings alone.
    pub tv: Option<TvConfig>,
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
        let idle_timeout = match raw.idle_timeout {
            None => DEFAULT_IDLE_TIMEOUT,
            Some(0) => bail!("idle_timeout must be at least 1 second"),
            Some(secs) => Duration::from_secs(secs),
        };
        Ok(Self {
            device: raw.device.into(),
            connector: raw.connector,
            mode,
            launch: Argv::from_vec(raw.launch),
            idle_timeout,
            scale: NonZeroU8::new(raw.scale.unwrap_or(1)).context("scale must be at least 1")?,
            tv: raw.tv.map(RawTv::parse).transpose()?,
        })
    }
}

impl RawTv {
    fn parse(self) -> anyhow::Result<TvConfig> {
        let flatten = |raw: RawSettings| -> Settings {
            raw.into_iter()
                .flat_map(|(category, keys)| {
                    keys.into_iter().map(move |(key, value)| {
                        let category = category.clone();
                        (Setting { category, key }, value)
                    })
                })
                .collect()
        };
        Ok(TvConfig {
            link: LinkConfig {
                cert_fingerprint: self
                    .cert_fingerprint
                    .parse()
                    .context("tv.cert_fingerprint")?,
                host: self.host,
                key_file: self.key_file.into(),
                input: self.input,
            },
            layers: Layers {
                always: flatten(self.settings),
                home: flatten(self.home),
                app: flatten(self.app),
                profiles: self
                    .profiles
                    .into_iter()
                    .map(|(name, raw)| (name, flatten(raw)))
                    .collect(),
            },
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
            idle_timeout = 30
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
        assert_eq!(config.idle_timeout, Duration::from_secs(30));
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
        assert_eq!(config.idle_timeout, Duration::from_secs(600));
        assert!(config.tv.is_none());
    }

    #[test]
    fn scale_defaults_to_1() {
        let config = Config::parse(
            r#"
            device = "/dev/dri/card0"
            connector = "HDMI-A-1"
            "#,
        )
        .unwrap();
        assert_eq!(config.scale.get(), 1);
    }

    #[test]
    fn scale_is_read() {
        let config = Config::parse(
            r#"
            device = "/dev/dri/card0"
            connector = "HDMI-A-1"
            scale = 2
            "#,
        )
        .unwrap();
        assert_eq!(config.scale.get(), 2);
    }

    #[test]
    fn a_zero_scale_is_refused() {
        let config = Config::parse(
            r#"
            device = "/dev/dri/card0"
            connector = "HDMI-A-1"
            scale = 0
            "#,
        );
        assert!(config.is_err());
    }

    #[test]
    fn a_zero_idle_timeout_is_refused() {
        let config = Config::parse(
            r#"
            device = "/dev/dri/card0"
            connector = "HDMI-A-1"
            idle_timeout = 0
            "#,
        );
        assert!(config.is_err());
    }

    #[test]
    fn tv_settings_layer_by_section() {
        let config = Config::parse(
            r#"
            device = "/dev/dri/card0"
            connector = "HDMI-A-1"

            [tv]
            host = "192.168.178.36"
            key_file = "/run/secrets/tv-key"
            cert_fingerprint = "11:C5:B1:C5:90:77:50:AB:B9:DA:2A:66:65:CC:CE:2B:B2:88:A5:83:F4:5A:33:39:E7:1F:87:BF:2F:80:85:52"
            input = "HDMI_1"

            [tv.settings.aspectRatio]
            justScan = "on"

            [tv.app.picture]
            pictureMode = "filmMaker"
            backlight = 80

            [tv.profiles.game.picture]
            pictureMode = "game"
            "#,
        )
        .unwrap();
        let tv = config.tv.unwrap();
        assert_eq!(tv.link.input, "HDMI_1");
        let setting = |category: &str, key: &str| Setting {
            category: category.into(),
            key: key.into(),
        };
        let text = |s: &str| SettingValue::Text(s.into());
        assert_eq!(
            tv.layers.always,
            Settings::from([(setting("aspectRatio", "justScan"), text("on"))])
        );
        assert!(tv.layers.home.is_empty());
        assert_eq!(
            tv.layers.app,
            Settings::from([
                (setting("picture", "pictureMode"), text("filmMaker")),
                (setting("picture", "backlight"), SettingValue::Number(80)),
            ])
        );
        assert_eq!(
            tv.layers.profiles["game"],
            Settings::from([(setting("picture", "pictureMode"), text("game"))])
        );
    }

    #[test]
    fn tv_needs_a_valid_fingerprint() {
        let config = Config::parse(
            r#"
            device = "/dev/dri/card0"
            connector = "HDMI-A-1"

            [tv]
            host = "192.168.178.36"
            key_file = "/run/secrets/tv-key"
            cert_fingerprint = "11:C5"
            input = "HDMI_1"
            "#,
        );
        assert!(config.is_err());
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
