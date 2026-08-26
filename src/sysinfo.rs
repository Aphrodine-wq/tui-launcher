//! System information for the About panel — read from /proc and
//! /etc/os-release, no external commands.

use std::fs;

pub fn gather() -> Vec<(String, String)> {
    let mut rows = Vec::new();
    let mut push = |label: &str, value: Option<String>| {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            rows.push((label.to_owned(), value));
        }
    };
    push(
        "Hostname",
        fs::read_to_string("/etc/hostname")
            .ok()
            .map(|value| value.trim().to_owned()),
    );
    push(
        "System",
        fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|contents| parse_os_release(&contents)),
    );
    push(
        "Kernel",
        fs::read_to_string("/proc/sys/kernel/osrelease")
            .ok()
            .map(|value| value.trim().to_owned()),
    );
    push(
        "Uptime",
        fs::read_to_string("/proc/uptime")
            .ok()
            .and_then(|contents| parse_uptime(&contents)),
    );
    push(
        "Processor",
        fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|contents| parse_cpu_model(&contents)),
    );
    push(
        "Memory",
        fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|contents| parse_memory(&contents)),
    );
    rows.push((
        "Launcher".to_owned(),
        format!("tui-launcher {}", env!("CARGO_PKG_VERSION")),
    ));
    rows
}

fn parse_os_release(contents: &str) -> Option<String> {
    contents
        .lines()
        .find_map(|line| line.strip_prefix("PRETTY_NAME="))
        .map(|value| value.trim_matches('"').to_owned())
}

fn parse_uptime(contents: &str) -> Option<String> {
    let seconds = contents.split_whitespace().next()?.parse::<f64>().ok()? as u64;
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    Some(if days > 0 {
        format!("{days}d {hours}h {minutes}m")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    })
}

fn parse_cpu_model(contents: &str) -> Option<String> {
    contents
        .lines()
        .find(|line| line.starts_with("model name"))
        .and_then(|line| line.split(':').nth(1))
        .map(|value| value.trim().to_owned())
}

fn parse_memory(contents: &str) -> Option<String> {
    let field = |name: &str| -> Option<u64> {
        contents
            .lines()
            .find(|line| line.starts_with(name))?
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()
    };
    let total = field("MemTotal:")?;
    let available = field("MemAvailable:")?;
    let gib = |kib: u64| kib as f64 / 1024.0 / 1024.0;
    Some(format!(
        "{:.1} GiB free of {:.1} GiB",
        gib(available),
        gib(total)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsers_handle_real_shapes() {
        assert_eq!(
            parse_os_release("NAME=\"Arch Linux\"\nPRETTY_NAME=\"Arch Linux\"\n"),
            Some("Arch Linux".to_owned())
        );
        assert_eq!(parse_uptime("93784.21 512341.11\n"), Some("1d 2h 3m".to_owned()));
        assert_eq!(parse_uptime("754.0 100.0\n"), Some("12m".to_owned()));
        assert_eq!(
            parse_cpu_model("processor\t: 0\nmodel name\t: AMD Ryzen 7 5800X\n"),
            Some("AMD Ryzen 7 5800X".to_owned())
        );
        assert_eq!(
            parse_memory("MemTotal:       32805912 kB\nMemFree:  1 kB\nMemAvailable:   16402956 kB\n"),
            Some("15.6 GiB free of 31.3 GiB".to_owned())
        );
    }
}
