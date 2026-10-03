//! Client for the DayZ-VR debug plugin (`dayz_openxr_debug.dll`, `[debug]` in the ini).
//!
//! One command per line, one JSON line back, on 127.0.0.1:`port`. Only the three verbs
//! the editor needs are wrapped: `tunables` (which keys are live and their values),
//! `set` and `recenter`.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use serde_json::Value;

const TIMEOUT: Duration = Duration::from_millis(1500);

/// Why a plugin request failed.
#[derive(Debug)]
pub enum LiveError {
    Io(std::io::Error),
    Json(serde_json::Error),
    /// The plugin answered `{"ok":false,"error":...}` or an unexpected shape.
    Rejected(String),
}

impl std::fmt::Display for LiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "connection: {error}"),
            Self::Json(error) => write!(f, "bad reply: {error}"),
            Self::Rejected(reason) => f.write_str(reason),
        }
    }
}

impl std::error::Error for LiveError {}

impl From<std::io::Error> for LiveError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for LiveError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// A connected plugin session.
pub struct LiveClient {
    writer: TcpStream,
    reader: BufReader<TcpStream>,
}

impl LiveClient {
    /// Connects to the plugin on the local loopback.
    ///
    /// # Errors
    /// Fails when the game or the plugin is not running.
    pub fn connect(port: u16) -> Result<Self, LiveError> {
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let stream = TcpStream::connect_timeout(&address, TIMEOUT)?;
        stream.set_read_timeout(Some(TIMEOUT))?;
        stream.set_write_timeout(Some(TIMEOUT))?;
        let reader = BufReader::new(stream.try_clone()?);
        Ok(Self {
            writer: stream,
            reader,
        })
    }

    fn request(&mut self, line: &str) -> Result<Value, LiveError> {
        self.writer.write_all(line.as_bytes())?;
        self.writer.write_all(b"\n")?;
        let mut reply = String::new();
        if self.reader.read_line(&mut reply)? == 0 {
            return Err(LiveError::Rejected(
                "the plugin closed the connection".to_owned(),
            ));
        }
        Ok(serde_json::from_str(&reply)?)
    }

    /// Live tunables as `section.key` → current value.
    ///
    /// # Errors
    /// Fails on connection loss or a reply that is not a JSON object.
    pub fn tunables(&mut self) -> Result<BTreeMap<String, Value>, LiveError> {
        match self.request("tunables")? {
            Value::Object(map) => Ok(map.into_iter().collect()),
            other => Err(LiveError::Rejected(format!(
                "tunables reply is not an object: {other}"
            ))),
        }
    }

    /// Sets one tunable for the running game (lost when the game exits).
    ///
    /// # Errors
    /// Fails on connection loss or when the plugin rejects the name or value.
    pub fn set(&mut self, name: &str, value: &str) -> Result<(), LiveError> {
        let reply = self.request(&format!("set {name} {value}"))?;
        expect_ok(&reply)
    }

    /// Recaptures the HMD yaw and position centre.
    ///
    /// # Errors
    /// Fails on connection loss or a rejected command.
    pub fn recenter(&mut self) -> Result<(), LiveError> {
        let reply = self.request("recenter")?;
        expect_ok(&reply)
    }
}

fn expect_ok(reply: &Value) -> Result<(), LiveError> {
    if reply.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(());
    }
    let reason = reply
        .get("error")
        .map_or_else(|| reply.to_string(), Value::to_string);
    Err(LiveError::Rejected(reason))
}

/// Converts ini text to what `set` accepts: the plugin parses every value as a number,
/// so booleans go as 1/0; other text is passed through unchanged.
#[must_use]
pub fn set_text(ini_value: &str) -> String {
    match ini_value.trim() {
        "true" | "yes" | "on" => "1".to_owned(),
        "false" | "no" | "off" => "0".to_owned(),
        other => other.to_owned(),
    }
}

/// Formats a tunable's live value the way the ini writes it.
#[must_use]
pub fn ini_text(value: &Value) -> String {
    match value {
        Value::Bool(flag) => flag.to_string(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;

    /// A fake plugin answering a fixed script of replies.
    fn serve(replies: &'static [&'static str]) -> (u16, std::thread::JoinHandle<Vec<String>>) {
        let listener =
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap_or_else(|e| panic!("{e}"));
        let port = listener
            .local_addr()
            .map_or_else(|e| panic!("{e}"), |a| a.port());
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap_or_else(|e| panic!("{e}"));
            let mut received = Vec::new();
            let mut byte = [0_u8; 1];
            for reply in replies {
                let mut line = Vec::new();
                while stream.read(&mut byte).unwrap_or(0) == 1 && byte[0] != b'\n' {
                    line.push(byte[0]);
                }
                received.push(String::from_utf8_lossy(&line).into_owned());
                let _ = stream.write_all(reply.as_bytes());
                let _ = stream.write_all(b"\n");
            }
            received
        });
        (port, handle)
    }

    #[test]
    fn tunables_set_and_errors() {
        let (port, handle) = serve(&[
            r#"{"stereo.lock_yaw":false,"hud.ammo_quad_width_meters":0.07}"#,
            r#"{"ok":true}"#,
            r#"{"ok":false,"error":"unknown tunable"}"#,
        ]);
        let mut client = LiveClient::connect(port).unwrap_or_else(|e| panic!("{e}"));
        let tunables = client.tunables().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(ini_text(&tunables["stereo.lock_yaw"]), "false");
        assert_eq!(ini_text(&tunables["hud.ammo_quad_width_meters"]), "0.07");
        client
            .set("stereo.lock_yaw", "true")
            .unwrap_or_else(|e| panic!("{e}"));
        let rejected = client.set("nope", "1");
        assert!(matches!(rejected, Err(LiveError::Rejected(ref r)) if r.contains("unknown")));
        drop(client);
        let received = handle.join().unwrap_or_else(|_| panic!("server thread"));
        assert_eq!(
            received,
            ["tunables", "set stereo.lock_yaw true", "set nope 1"]
        );
    }

    #[test]
    fn set_text_maps_booleans_to_numbers() {
        assert_eq!(set_text("true"), "1");
        assert_eq!(set_text(" false "), "0");
        assert_eq!(set_text("-600"), "-600");
        assert_eq!(set_text("0.07"), "0.07");
    }

    #[test]
    fn connect_fails_without_plugin() {
        let listener =
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap_or_else(|e| panic!("{e}"));
        let port = listener
            .local_addr()
            .map_or_else(|e| panic!("{e}"), |a| a.port());
        drop(listener);
        assert!(matches!(LiveClient::connect(port), Err(LiveError::Io(_))));
    }
}
