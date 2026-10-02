//! The TV's own settings (picture mode, Just Scan, energy saving), held to
//! whatever is on screen. emrakul says what they should be for Home, for
//! apps, and for the profile an app's entry names, and keeps checking until
//! the TV agrees: switched on later, set back with its remote, or back from
//! another input, it gets put right again.
//!
//! It talks to the TV over SSAP ([`crate::ssap`]) from a thread of its own, so a
//! TV that is off or slow never holds up a frame.

use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    path::PathBuf,
    time::Duration,
};

use crate::ssap::{self, CertFingerprint, ClientKey, Input};
use anyhow::Context;
use facet::Facet;
use serde_json::{Map, Value};
use tokio::sync::watch;

/// How often the TV is checked when nothing on screen has changed: it may
/// have been switched on, come back to this machine's input, or been changed
/// with its remote.
const RECHECK: Duration = Duration::from_secs(15);

/// One of the TV's system settings, such as `picture.pictureMode`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Setting {
    pub category: String,
    pub key: String,
}

impl Setting {
    /// Picture settings like backlight belong to a picture mode, so the mode
    /// has to be in place before they are checked.
    fn is_picture_mode(&self) -> bool {
        self.category == "picture" && self.key == "pictureMode"
    }
}

impl fmt::Display for Setting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.category, self.key)
    }
}

/// What the TV takes: a string (`"filmMaker"`, `"on"`) or a number.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[repr(u8)]
#[facet(untagged)]
pub enum SettingValue {
    Text(String),
    Number(i64),
}

impl SettingValue {
    fn to_json(&self) -> Value {
        match self {
            Self::Text(s) => Value::from(s.as_str()),
            Self::Number(n) => Value::from(*n),
        }
    }

    /// The TV reports some numbers as strings (`"brightness": "50"`) and
    /// others as numbers (`"backlight": 80`), so which one doesn't matter.
    fn matches(&self, reported: &Value) -> bool {
        let reported = match reported {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => return false,
        };
        match self {
            Self::Text(s) => *s == reported,
            Self::Number(n) => n.to_string() == reported,
        }
    }
}

impl fmt::Display for SettingValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text(s) => write!(f, "{s:?}"),
            Self::Number(n) => n.fmt(f),
        }
    }
}

pub type Settings = BTreeMap<Setting, SettingValue>;

pub struct TvConfig {
    pub link: LinkConfig,
    pub layers: Layers,
}

#[derive(Clone)]
pub struct LinkConfig {
    pub host: String,
    /// Holds the client key from `tv pair`. Read on every connect, so a
    /// secret that lands after emrakul starts is still picked up.
    pub key_file: PathBuf,
    pub cert_fingerprint: CertFingerprint,
    /// The TV input this machine is plugged into, e.g. `HDMI_1`. Settings
    /// belong to an input, so nothing is touched while the TV shows another.
    pub input: String,
}

/// The settings wanted for each thing that can be on screen, each layered
/// over `always`.
#[derive(Default)]
pub struct Layers {
    pub always: Settings,
    pub home: Settings,
    /// Apps whose entry names no profile.
    pub app: Settings,
    /// Picked by an entry's `X-Emrakul-Tv=<name>`, in place of `app`.
    pub profiles: HashMap<String, Settings>,
}

/// What is on screen, as far as the TV's settings go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Showing<'a> {
    Home,
    App { profile: Option<&'a str> },
}

impl Layers {
    pub fn knows(&self, profile: &str) -> bool {
        self.profiles.contains_key(profile)
    }

    /// An unknown profile gets `app`; launching warns about it.
    pub fn wanted(&self, showing: Showing) -> Settings {
        let layer = match showing {
            Showing::Home => &self.home,
            Showing::App { profile } => profile
                .and_then(|name| self.profiles.get(name))
                .unwrap_or(&self.app),
        };
        let mut wanted = self.always.clone();
        wanted.extend(layer.iter().map(|(s, v)| (s.clone(), v.clone())));
        wanted
    }
}

/// The order settings are checked in: the picture mode first, since the
/// picture settings checked after it belong to whichever mode is set.
fn in_order(wanted: &Settings) -> impl Iterator<Item = (&Setting, &SettingValue)> {
    let (mode, rest): (Vec<_>, Vec<_>) = wanted.iter().partition(|(s, _)| s.is_picture_mode());
    mode.into_iter().chain(rest)
}

/// The handle the compositor keeps. Dropping it ends the TV thread.
pub struct Tv {
    layers: Layers,
    wanted: watch::Sender<Settings>,
}

impl Tv {
    pub fn spawn(config: TvConfig) -> anyhow::Result<Self> {
        let (wanted, receiver) = watch::channel(Settings::new());
        let link = config.link;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("starting the TV thread's runtime")?;
        std::thread::Builder::new()
            .name("tv".into())
            .spawn(move || runtime.block_on(hold(link, receiver)))
            .context("starting the TV thread")?;
        Ok(Self {
            layers: config.layers,
            wanted,
        })
    }

    pub fn layers(&self) -> &Layers {
        &self.layers
    }

    pub fn show(&self, showing: Showing) {
        let settings = self.layers.wanted(showing);
        self.wanted.send_if_modified(|wanted| {
            let changed = *wanted != settings;
            *wanted = settings;
            changed
        });
    }
}

/// The TV thread: on every change of what's wanted, and every [`RECHECK`],
/// brings the TV's settings in line.
async fn hold(config: LinkConfig, mut wanted: watch::Receiver<Settings>) {
    let mut link = None;
    loop {
        let target = wanted.borrow_and_update().clone();
        if let Err(err) = converge(&config, &mut link, &target).await {
            // An off TV doesn't answer at all, every RECHECK, all night.
            if link.take().is_some() {
                tracing::info!("lost the TV: {err:#}");
            } else {
                tracing::debug!("TV unreachable: {err:#}");
            }
        }
        tokio::select! {
            changed = wanted.changed() => if changed.is_err() { return },
            () = tokio::time::sleep(RECHECK) => {}
        }
    }
}

struct Link {
    tv: ssap::Tv,
    input: Input,
    /// Settings the TV can be told but can't report back (`aspectRatio`),
    /// as last set over this link while on our input. Setting them again
    /// every [`RECHECK`] could make the picture blink.
    set_blind: Settings,
    /// Values the TV refused. Not tried again until the wanted value
    /// changes or the link drops, so a typo in the config warns once.
    refused: Settings,
}

impl Link {
    async fn open(config: &LinkConfig) -> anyhow::Result<Self> {
        let key: ClientKey = std::fs::read_to_string(&config.key_file)
            .with_context(|| format!("reading {}", config.key_file.display()))?
            .parse()?;
        let mut tv = ssap::Tv::connect(&config.host, &key, config.cert_fingerprint).await?;
        let inputs = tv.inputs().await?;
        let Some(input) = inputs
            .into_iter()
            .find(|i| i.id().as_str().eq_ignore_ascii_case(&config.input))
        else {
            tracing::warn!(input = config.input, "the TV has no such input");
            anyhow::bail!("no input {}", config.input);
        };
        tracing::info!(
            host = config.host,
            input = input.id().as_str(),
            "reached the TV"
        );
        Ok(Self {
            tv,
            input,
            set_blind: Settings::new(),
            refused: Settings::new(),
        })
    }
}

async fn converge(
    config: &LinkConfig,
    link: &mut Option<Link>,
    wanted: &Settings,
) -> anyhow::Result<()> {
    let link = match link {
        Some(link) => link,
        None => link.insert(Link::open(config).await?),
    };
    if link.tv.foreground_app().await? != *link.input.app_id() {
        // Whatever we set blind belongs to our input, and may have been
        // changed while we weren't looking.
        link.set_blind.clear();
        return Ok(());
    }
    for (setting, value) in in_order(wanted) {
        if link.refused.get(setting) == Some(value) || link.set_blind.get(setting) == Some(value) {
            continue;
        }
        let blind = match link
            .tv
            .system_setting(&setting.category, &setting.key)
            .await
        {
            Ok(reported) if value.matches(&reported) => continue,
            Ok(_) => false,
            Err(ssap::Error::Tv { .. }) => true,
            Err(err) => return Err(err.into()),
        };
        let settings = Map::from_iter([(setting.key.clone(), value.to_json())]);
        match link
            .tv
            .set_system_settings(&setting.category, settings)
            .await
        {
            Ok(()) => {
                tracing::info!(%setting, %value, "set on the TV");
                if blind {
                    link.set_blind.insert(setting.clone(), value.clone());
                }
            }
            Err(ssap::Error::Tv { error, .. }) => {
                tracing::warn!(%setting, %value, "the TV refused it: {error}");
                link.refused.insert(setting.clone(), value.clone());
            }
            Err(err) => return Err(err.into()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setting(category: &str, key: &str) -> Setting {
        Setting {
            category: category.into(),
            key: key.into(),
        }
    }

    fn text(s: &str) -> SettingValue {
        SettingValue::Text(s.into())
    }

    fn settings(entries: &[(&str, &str, SettingValue)]) -> Settings {
        entries
            .iter()
            .map(|(c, k, v)| (setting(c, k), v.clone()))
            .collect()
    }

    fn layers() -> Layers {
        Layers {
            always: settings(&[
                ("aspectRatio", "justScan", text("on")),
                ("picture", "pictureMode", text("filmMaker")),
            ]),
            home: Settings::new(),
            app: settings(&[("picture", "energySaving", text("off"))]),
            profiles: HashMap::from([(
                "game".to_owned(),
                settings(&[("picture", "pictureMode", text("game"))]),
            )]),
        }
    }

    #[test]
    fn home_gets_only_what_always_holds() {
        assert_eq!(layers().wanted(Showing::Home), layers().always);
    }

    #[test]
    fn an_app_layers_over_always() {
        let wanted = layers().wanted(Showing::App { profile: None });
        assert_eq!(
            wanted,
            settings(&[
                ("aspectRatio", "justScan", text("on")),
                ("picture", "energySaving", text("off")),
                ("picture", "pictureMode", text("filmMaker")),
            ])
        );
    }

    #[test]
    fn a_profile_replaces_app_and_overrides_always() {
        let wanted = layers().wanted(Showing::App {
            profile: Some("game"),
        });
        assert_eq!(
            wanted,
            settings(&[
                ("aspectRatio", "justScan", text("on")),
                ("picture", "pictureMode", text("game")),
            ])
        );
    }

    #[test]
    fn an_unknown_profile_is_an_app() {
        let layers = layers();
        assert!(!layers.knows("cinema"));
        assert_eq!(
            layers.wanted(Showing::App {
                profile: Some("cinema")
            }),
            layers.wanted(Showing::App { profile: None })
        );
    }

    #[test]
    fn picture_mode_goes_first() {
        let wanted = settings(&[
            ("aspectRatio", "justScan", text("on")),
            ("picture", "backlight", SettingValue::Number(80)),
            ("picture", "pictureMode", text("game")),
        ]);
        let order: Vec<_> = in_order(&wanted).map(|(s, _)| s.to_string()).collect();
        assert_eq!(
            order,
            [
                "picture.pictureMode",
                "aspectRatio.justScan",
                "picture.backlight"
            ]
        );
    }

    #[test]
    fn values_match_whether_reported_as_strings_or_numbers() {
        assert!(SettingValue::Number(80).matches(&serde_json::json!(80)));
        assert!(SettingValue::Number(50).matches(&serde_json::json!("50")));
        assert!(text("50").matches(&serde_json::json!(50)));
        assert!(text("eco").matches(&serde_json::json!("eco")));
        assert!(!text("game").matches(&serde_json::json!("eco")));
        assert!(!text("auto").matches(&serde_json::json!({"ntsc": "auto"})));
    }

    /// Against a paired TV showing `EMRAKUL_TV_INPUT`: flips the picture
    /// mode to game and back, reading each one back, with Just Scan held
    /// alongside.
    ///
    /// `EMRAKUL_TV_HOST=… EMRAKUL_TV_KEY_FILE=… EMRAKUL_TV_FINGERPRINT=…
    /// EMRAKUL_TV_INPUT=HDMI_1 cargo test real_tv -- --ignored`
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "needs a paired TV, switched on and showing this machine"]
    async fn converges_a_real_tv() {
        let env = |var| std::env::var(var).unwrap_or_else(|_| panic!("{var} unset"));
        let config = LinkConfig {
            host: env("EMRAKUL_TV_HOST"),
            key_file: env("EMRAKUL_TV_KEY_FILE").into(),
            cert_fingerprint: env("EMRAKUL_TV_FINGERPRINT").parse().unwrap(),
            input: env("EMRAKUL_TV_INPUT"),
        };
        let mode = setting("picture", "pictureMode");
        let mut link = None;
        converge(&config, &mut link, &Settings::new())
            .await
            .unwrap();
        let tv = &mut link.as_mut().unwrap().tv;
        let before = tv.system_setting("picture", "pictureMode").await.unwrap();
        let before = SettingValue::Text(before.as_str().unwrap().to_owned());

        for value in [text("game"), before] {
            let wanted = Settings::from([
                (mode.clone(), value.clone()),
                (setting("aspectRatio", "justScan"), text("on")),
            ]);
            converge(&config, &mut link, &wanted).await.unwrap();
            let link = link.as_mut().unwrap();
            let reported = link.tv.system_setting("picture", "pictureMode").await;
            assert!(value.matches(&reported.unwrap()), "wanted {value}");
            assert_eq!(link.set_blind.len(), 1, "justScan is set blind");
            assert!(link.refused.is_empty());
        }
    }
}
