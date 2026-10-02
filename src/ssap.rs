//! Client for SSAP, the websocket API on LG webOS TVs. Vendored from
//! [webos-ssap](https://github.com/cramt/webos-ssap) at 85efdec, cut down
//! to what emrakul uses, plus the signed manifest settings writes need.
//! Pairing, power, volume, toasts and buttons are there to bring over when
//! something here needs them.
//!
//! LG never documented SSAP. Payloads are checked against a real
//! OLED65B46LA (webOS 9.2.4).

mod tls;

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};

pub use tls::CertFingerprint;

/// TLS only. Plain ws on 3000 would put the client key on the LAN in clear.
const PORT: u16 = 3001;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// aiowebostv's permission list. Together with the signed half below it
/// registers ganymede's key, which bscpylgtv paired under its own manifest.
const PERMISSIONS: &[&str] = &[
    "APP_TO_APP",
    "CLOSE",
    "CONTROL_AUDIO",
    "CONTROL_DISPLAY",
    "CONTROL_INPUT_JOYSTICK",
    "CONTROL_INPUT_MEDIA_PLAYBACK",
    "CONTROL_INPUT_MEDIA_RECORDING",
    "CONTROL_INPUT_TEXT",
    "CONTROL_INPUT_TV",
    "CONTROL_MOUSE_AND_KEYBOARD",
    "CONTROL_POWER",
    "CONTROL_TV_SCREEN",
    "LAUNCH",
    "LAUNCH_WEBAPP",
    "READ_APP_STATUS",
    "READ_COUNTRY_INFO",
    "READ_CURRENT_CHANNEL",
    "READ_INPUT_DEVICE_LIST",
    "READ_INSTALLED_APPS",
    "READ_LGE_SDX",
    "READ_LGE_TV_INPUT_EVENTS",
    "READ_NETWORK_STATE",
    "READ_NOTIFICATIONS",
    "READ_POWER_STATE",
    "READ_RUNNING_APPS",
    "READ_SETTINGS",
    "READ_TV_CHANNEL_LIST",
    "READ_TV_CURRENT_TIME",
    "READ_UPDATE_INFO",
    "SEARCH",
    "TEST_OPEN",
    "TEST_PROTECTED",
    "TEST_SECURE",
    "UPDATE_FROM_REMOTE_APP",
    "WRITE_NOTIFICATION_ALERT",
    "WRITE_NOTIFICATION_TOAST",
    "WRITE_SETTINGS",
];

/// LG's own signature over [`signed_manifest`], from the LG Remote App and
/// shared by every SSAP client (lgtv2, pywebostv, bscpylgtv). The TV only
/// grants permissions from the signed half: `WRITE_SETTINGS` asked for
/// unsigned gets a 401 on every `setSystemSettings` (webOS 9.2.4).
const SIGNATURE: &str = concat!(
    "eyJhbGdvcml0aG0iOiJSU0EtU0hBMjU2Iiwia2V5SWQiOiJ0ZXN0LXNpZ25pbm",
    "ctY2VydCIsInNpZ25hdHVyZVZlcnNpb24iOjF9.hrVRgjCwXVvE2OOSpDZ58hR",
    "+59aFNwYDyjQgKk3auukd7pcegmE2CzPCa0bJ0ZsRAcKkCTJrWo5iDzNhMBWRy",
    "aMOv5zWSrthlf7G128qvIlpMT0YNY+n/FaOHE73uLrS/g7swl3/qH/BGFG2Hu4",
    "RlL48eb3lLKqTt2xKHdCs6Cd4RMfJPYnzgvI4BNrFUKsjkcu+WD4OO2A27Pq1n",
    "50cMchmcaXadJhGrOqH5YmHdOCj5NSHzJYrsW0HPlpuAx/ECMeIZYDh6RMqaFM",
    "2DXzdKX9NmmyqzJ3o/0lkk/N97gfVRLW5hA29yeAwaCViZNCP8iC9aO0q9fQoj",
    "oa7NQnAtw==",
);

/// Exactly what [`SIGNATURE`] signs. Change nothing in here.
fn signed_manifest() -> Value {
    json!({
        "appId": "com.lge.test",
        "created": "20140509",
        "localizedAppNames": {
            "": "LG Remote App",
            "ko-KR": "리모컨 앱",
            "zxx-XX": "ЛГ Rэмotэ AПП",
        },
        "localizedVendorNames": { "": "LG Electronics" },
        "permissions": [
            "TEST_SECURE",
            "CONTROL_INPUT_TEXT",
            "CONTROL_MOUSE_AND_KEYBOARD",
            "READ_INSTALLED_APPS",
            "READ_LGE_SDX",
            "READ_NOTIFICATIONS",
            "SEARCH",
            "WRITE_SETTINGS",
            "WRITE_NOTIFICATION_ALERT",
            "CONTROL_POWER",
            "READ_CURRENT_CHANNEL",
            "READ_RUNNING_APPS",
            "READ_UPDATE_INFO",
            "UPDATE_FROM_REMOTE_APP",
            "READ_LGE_TV_INPUT_EVENTS",
            "READ_TV_CURRENT_TIME",
        ],
        "serial": "2f930e2d2cfe083771f68e4fe7bb07",
        "vendorId": "com.lge",
    })
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("websocket: {0}")]
    Ws(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("TV closed the connection")]
    Closed,
    #[error("no reply from the TV within {0:?}")]
    Timeout(Duration),
    #[error("registering: {0}")]
    Rejected(String),
    /// The TV answered, and said no. The connection is fine.
    #[error("{uri}: {error}")]
    Tv { uri: String, error: String },
    #[error("unexpected payload from {uri}: {source}")]
    Payload {
        uri: String,
        source: serde_json::Error,
    },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// What the TV hands out once its pairing prompt is accepted. Whoever holds
/// it gets every permission in the manifest, so it's a secret.
#[derive(Clone, PartialEq, Eq)]
pub struct ClientKey(String);

#[derive(Debug, thiserror::Error)]
#[error("client key is empty")]
pub struct EmptyClientKey;

impl std::str::FromStr for ClientKey {
    type Err = EmptyClientKey;

    /// Trims, since keys usually come from a secret file with a newline.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "" => Err(EmptyClientKey),
            key => Ok(Self(key.to_owned())),
        }
    }
}

impl std::fmt::Debug for ClientKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ClientKey(..)")
    }
}

// Neither id can be built by hand: they only ever come from the TV, so a
// request can never name an app or input the TV doesn't have.

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct AppId(String);

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct InputId(String);

impl InputId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Input {
    id: InputId,
    /// Switching to an input launches this app, so it's also how the
    /// foreground app maps back to an input.
    app_id: AppId,
}

impl Input {
    pub fn id(&self) -> &InputId {
        &self.id
    }

    pub fn app_id(&self) -> &AppId {
        &self.app_id
    }
}

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Incoming {
    Response {
        id: String,
        #[serde(default)]
        payload: Value,
    },
    Registered {
        id: String,
    },
    Error {
        #[serde(default)]
        id: Option<String>,
        error: String,
    },
    #[serde(other)]
    Other,
}

pub struct Tv {
    ws: Socket,
    next_id: u64,
}

impl Tv {
    /// Every connection is held to the cert the TV showed when it was
    /// paired. Without the pin, anyone on the LAN could pose as the TV and
    /// collect the key.
    pub async fn connect(host: &str, key: &ClientKey, pin: CertFingerprint) -> Result<Self> {
        let (ws, _) = tokio::time::timeout(
            REQUEST_TIMEOUT,
            tokio_tungstenite::connect_async_tls_with_config(
                format!("wss://{host}:{PORT}"),
                None,
                true,
                Some(tls::connector(pin)),
            ),
        )
        .await
        .map_err(|_| Error::Timeout(REQUEST_TIMEOUT))??;
        let mut tv = Self { ws, next_id: 0 };
        tv.register(key).await?;
        Ok(tv)
    }

    async fn register(&mut self, key: &ClientKey) -> Result<()> {
        let payload = json!({
            "forcePairing": false,
            "pairingType": "PROMPT",
            "client-key": key.0,
            "manifest": {
                "appVersion": "1.1",
                "manifestVersion": 1,
                "permissions": PERMISSIONS,
                "signed": signed_manifest(),
                "signatures": [{ "signature": SIGNATURE, "signatureVersion": 1 }],
            },
        });
        self.send(json!({ "id": "register", "type": "register", "payload": payload }))
            .await?;
        // A known key comes straight back as `registered`. An unknown one
        // would put the pairing prompt up instead, and nobody is at the TV
        // to answer it, so that runs into the timeout.
        tokio::time::timeout(REQUEST_TIMEOUT, async {
            loop {
                match self.recv().await? {
                    Incoming::Registered { id } if id == "register" => return Ok(()),
                    Incoming::Error { id, error } if id.as_deref() == Some("register") => {
                        return Err(Error::Rejected(error));
                    }
                    _ => {}
                }
            }
        })
        .await
        .map_err(|_| Error::Timeout(REQUEST_TIMEOUT))?
    }

    async fn send(&mut self, message: Value) -> Result<()> {
        self.ws.send(Message::text(message.to_string())).await?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<Incoming> {
        loop {
            match self.ws.next().await.ok_or(Error::Closed)?? {
                // Anything we can't make sense of isn't a reply to us.
                Message::Text(text) => {
                    return Ok(serde_json::from_str(text.as_str()).unwrap_or(Incoming::Other));
                }
                Message::Close(_) => return Err(Error::Closed),
                _ => {}
            }
        }
    }

    /// One `ssap://` request, waiting for its reply.
    async fn request<T: DeserializeOwned>(&mut self, uri: &str, payload: Value) -> Result<T> {
        self.next_id += 1;
        let id = self.next_id.to_string();
        self.send(json!({ "id": id, "type": "request", "uri": format!("ssap://{uri}"), "payload": payload }))
            .await?;
        let payload = tokio::time::timeout(REQUEST_TIMEOUT, async {
            loop {
                match self.recv().await? {
                    Incoming::Response { id: got, payload } if got == id => return Ok(payload),
                    Incoming::Error {
                        id: Some(got),
                        error,
                    } if got == id => {
                        return Err(Error::Tv {
                            uri: uri.to_owned(),
                            error,
                        });
                    }
                    _ => {}
                }
            }
        })
        .await
        .map_err(|_| Error::Timeout(REQUEST_TIMEOUT))??;

        // Some services report failure inside a successful response.
        if payload.get("returnValue") == Some(&Value::Bool(false)) {
            let error = payload
                .get("errorText")
                .and_then(Value::as_str)
                .unwrap_or("returnValue false")
                .to_owned();
            return Err(Error::Tv {
                uri: uri.to_owned(),
                error,
            });
        }
        serde_json::from_value(payload).map_err(|source| Error::Payload {
            uri: uri.to_owned(),
            source,
        })
    }

    pub async fn foreground_app(&mut self) -> Result<AppId> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct R {
            app_id: AppId,
        }
        let r: R = self
            .request(
                "com.webos.applicationManager/getForegroundAppInfo",
                json!({}),
            )
            .await?;
        Ok(r.app_id)
    }

    pub async fn inputs(&mut self) -> Result<Vec<Input>> {
        #[derive(Deserialize)]
        struct R {
            devices: Vec<Input>,
        }
        let r: R = self.request("tv/getExternalInputList", json!({})).await?;
        Ok(r.devices)
    }

    /// One setting of the input on screen, e.g. `("picture", "pictureMode")`.
    /// Asking for several keys at once fails outright if any one of them is
    /// unknown, so this asks for one. Some can't be read at all:
    /// `aspectRatio` is always a 500.
    pub async fn system_setting(&mut self, category: &str, key: &str) -> Result<Value> {
        #[derive(Deserialize)]
        struct R {
            settings: Map<String, Value>,
        }
        let uri = "settings/getSystemSettings";
        let mut r: R = self
            .request(uri, json!({ "category": category, "keys": [key] }))
            .await?;
        r.settings.remove(key).ok_or_else(|| Error::Tv {
            uri: uri.to_owned(),
            error: format!("no {category}.{key} in the reply"),
        })
    }

    /// Writes settings of the input on screen. A value the TV doesn't know
    /// is a 500 and changes nothing.
    pub async fn set_system_settings(
        &mut self,
        category: &str,
        settings: Map<String, Value>,
    ) -> Result<()> {
        self.request::<Value>(
            "settings/setSystemSettings",
            json!({ "category": category, "settings": settings }),
        )
        .await
        .map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_parses_real_payload() {
        // Trimmed from an OLED65B46LA, webOS 9.2.4.
        let input: Input = serde_json::from_str(
            r#"{"id":"HDMI_1","label":"HDMI 1","port":1,"connected":true,
                "appId":"com.webos.app.hdmi1","hdmiPlugIn":true,"hdmiSignalExist":false}"#,
        )
        .unwrap();
        assert_eq!(input.id().as_str(), "HDMI_1");
        assert_eq!(input.app_id(), &AppId("com.webos.app.hdmi1".into()));
    }

    #[test]
    fn a_key_from_a_secret_file_is_trimmed() {
        let key: ClientKey = "abc123\n".parse().unwrap();
        assert_eq!(key.0, "abc123");
        assert!(" \n".parse::<ClientKey>().is_err());
    }
}
