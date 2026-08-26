//! Wi-Fi scanning and connection control backed by iwd's `iwctl`.
//!
//! iwctl emits human tables with ANSI color even when piped; signal strength
//! is encoded in the colors (bright stars are filled bars, dim stars empty),
//! so parsing keeps the escape codes around until the bars are counted.

use std::{process::Command, thread, time::Duration};

use anyhow::{Result, anyhow};

use crate::sources::command_exists;

const SECURITY_TOKENS: [&str; 6] = ["psk", "open", "8021x", "wep", "owe", "sae"];

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WifiNetwork {
    pub ssid: String,
    pub security: String,
    pub bars: u8,
    pub connected: bool,
    pub known: bool,
}

impl WifiNetwork {
    pub fn secured(&self) -> bool {
        self.security != "open"
    }
}

#[derive(Clone, Debug, Default)]
pub struct WifiSnapshot {
    pub device: Option<String>,
    pub networks: Vec<WifiNetwork>,
    pub error: Option<String>,
}

pub fn available() -> bool {
    command_exists("iwctl")
}

pub fn snapshot(rescan: bool) -> WifiSnapshot {
    let Some(device) = station_device() else {
        return WifiSnapshot {
            device: None,
            networks: Vec::new(),
            error: Some("no wireless station device was found".to_owned()),
        };
    };
    if rescan {
        let _ = iwctl(&["station", &device, "scan"]);
        thread::sleep(Duration::from_millis(2300));
    }
    let known = known_networks().unwrap_or_default();
    match iwctl(&["station", &device, "get-networks"]) {
        Ok(raw) => WifiSnapshot {
            device: Some(device),
            networks: parse_networks(&raw, &known),
            error: None,
        },
        Err(error) => WifiSnapshot {
            device: Some(device),
            networks: Vec::new(),
            error: Some(error.to_string()),
        },
    }
}

pub fn connect(device: &str, ssid: &str, passphrase: Option<&str>) -> Result<String> {
    match passphrase {
        Some(passphrase) => iwctl(&["--passphrase", passphrase, "station", device, "connect", ssid])?,
        None => iwctl(&["station", device, "connect", ssid])?,
    };
    Ok(format!("Connected to {ssid}"))
}

pub fn disconnect(device: &str) -> Result<String> {
    iwctl(&["station", device, "disconnect"])?;
    Ok("Disconnected".to_owned())
}

pub fn station_device() -> Option<String> {
    let output = iwctl(&["device", "list"]).ok()?;
    for line in output.lines().skip(4) {
        let clean = strip_ansi(line);
        let fields: Vec<&str> = clean.split_whitespace().collect();
        if fields.len() >= 5 && fields[4] == "station" {
            return Some(fields[0].to_owned());
        }
    }
    None
}

fn known_networks() -> Result<Vec<String>> {
    Ok(parse_known(&iwctl(&["known-networks", "list"])?))
}

fn iwctl(args: &[&str]) -> Result<String> {
    let output = Command::new("iwctl").args(args).output()?;
    if !output.status.success() {
        let stderr = strip_ansi(&String::from_utf8_lossy(&output.stderr));
        let stdout = strip_ansi(&String::from_utf8_lossy(&output.stdout));
        let message = if stderr.trim().is_empty() { stdout } else { stderr };
        return Err(anyhow!("{}", message.trim().to_owned()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Count filled signal bars: stars rendered outside a dim (`90`) SGR span.
fn signal_bars(raw_line: &str) -> u8 {
    let mut dim = false;
    let mut bars = 0u8;
    let mut chars = raw_line.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            let mut code = String::new();
            for escape in chars.by_ref() {
                if escape.is_ascii_alphabetic() {
                    if escape == 'm' {
                        dim = code.split(';').any(|part| part == "90");
                    }
                    break;
                }
                code.push(escape);
            }
            continue;
        }
        if character == '*' && !dim {
            bars += 1;
        }
    }
    bars
}

fn strip_ansi(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for escape in chars.by_ref() {
                    if escape.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(character);
    }
    out
}

fn parse_networks(raw: &str, known: &[String]) -> Vec<WifiNetwork> {
    let mut networks: Vec<WifiNetwork> = Vec::new();
    for line in raw.lines().skip(4) {
        let bars = signal_bars(line);
        let clean = strip_ansi(line);
        let trimmed = clean.trim();
        if trimmed.is_empty() {
            continue;
        }
        let (connected, rest) = match trimmed.strip_prefix('>') {
            Some(rest) => (true, rest.trim_start()),
            None => (false, trimmed),
        };
        let mut fields: Vec<&str> = rest.split_whitespace().collect();
        if fields.len() < 3 {
            continue;
        }
        fields.pop();
        let security = fields.pop().unwrap_or_default().to_owned();
        let ssid = fields.join(" ");
        if ssid.is_empty() || networks.iter().any(|network| network.ssid == ssid) {
            continue;
        }
        let known = connected || known.iter().any(|name| name == &ssid);
        networks.push(WifiNetwork {
            ssid,
            security,
            bars,
            connected,
            known,
        });
    }
    networks.sort_by(|left, right| {
        right
            .connected
            .cmp(&left.connected)
            .then_with(|| right.known.cmp(&left.known))
            .then_with(|| right.bars.cmp(&left.bars))
            .then_with(|| left.ssid.cmp(&right.ssid))
    });
    networks
}

fn parse_known(raw: &str) -> Vec<String> {
    raw.lines()
        .skip(4)
        .filter_map(|line| {
            let clean = strip_ansi(line);
            let fields: Vec<&str> = clean.split_whitespace().collect();
            let security = fields
                .iter()
                .position(|field| SECURITY_TOKENS.contains(field))?;
            if security == 0 {
                return None;
            }
            Some(fields[..security].join(" "))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Captured from `iwctl station wlan0 get-networks` on iwd 3.x.
    const NETWORKS: &str = "                               Available networks\u{1b}[1;90m                              \u{1b}[0m\n\u{1b}[90m--------------------------------------------------------------------------------\n\u{1b}[0m\u{1b}[1;90m      Network name                      Security            Signal\n\u{1b}[0m\u{1b}[90m--------------------------------------------------------------------------------\n\u{1b}[0m  \u{1b}[1;90m> \u{1b}[0m  Burge                             psk                 ****    \n      Swanger Guest Net                 psk                 **\u{1b}[1;90m**\u{1b}[0m    \n      Open Cafe                         open                *\u{1b}[1;90m***\u{1b}[0m    \n";

    const KNOWN: &str = "                                 Known Networks\u{1b}[1;90m                                \u{1b}[0m\n\u{1b}[90m--------------------------------------------------------------------------------\n\u{1b}[0m\u{1b}[1;90m  Name                              Security     Hidden     Last connected     \n\u{1b}[0m\u{1b}[90m--------------------------------------------------------------------------------\n\u{1b}[0m  Burge                             psk                     Aug 21,  9:15 PM     \n";

    const DEVICES: &str = "                                    Devices\u{1b}[1;90m                                    \u{1b}[0m\n\u{1b}[90m--------------------------------------------------------------------------------\n\u{1b}[0m\u{1b}[1;90m  Name                  Address               Powered     Adapter     Mode      \n\u{1b}[0m\u{1b}[90m--------------------------------------------------------------------------------\n\u{1b}[0m  wlan0                 10:6f:d9:cf:6b:49     on          phy0        station     \n";

    #[test]
    fn parses_networks_with_signal_from_color_spans() {
        let known = parse_known(KNOWN);
        assert_eq!(known, vec!["Burge".to_owned()]);
        let networks = parse_networks(NETWORKS, &known);
        assert_eq!(networks.len(), 3);
        assert_eq!(networks[0].ssid, "Burge");
        assert!(networks[0].connected && networks[0].known);
        assert_eq!(networks[0].bars, 4);
        let guest = networks
            .iter()
            .find(|network| network.ssid == "Swanger Guest Net")
            .unwrap();
        assert_eq!(guest.bars, 2);
        assert!(guest.secured() && !guest.known);
        let cafe = networks
            .iter()
            .find(|network| network.ssid == "Open Cafe")
            .unwrap();
        assert_eq!(cafe.bars, 1);
        assert!(!cafe.secured());
    }

    #[test]
    fn finds_the_station_device() {
        let clean = strip_ansi(DEVICES);
        assert!(clean.contains("wlan0"));
        // parse path mirrors station_device(): row 5, mode column "station"
        let row = strip_ansi(DEVICES.lines().nth(4).unwrap());
        let fields: Vec<&str> = row.split_whitespace().collect();
        assert_eq!(fields[0], "wlan0");
        assert_eq!(fields[4], "station");
    }
}
