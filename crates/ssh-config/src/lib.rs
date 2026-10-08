//! Bounded OpenSSH config reader and non-executing quick-connect command parser.
use opsssh_store::{Auth, Profile};
use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Default)]
pub struct ParsedConfig {
    lines: Vec<(String, Vec<String>)>,
    pub warnings: Vec<String>,
    files_read: usize,
}
impl ParsedConfig {
    pub fn read(path: &Path, home: &Path) -> io::Result<Self> {
        let mut config = Self::default();
        let mut seen = HashSet::new();
        config.include(path, home, &mut seen, 0)?;
        Ok(config)
    }
    fn include(
        &mut self,
        path: &Path,
        home: &Path,
        seen: &mut HashSet<PathBuf>,
        depth: usize,
    ) -> io::Result<()> {
        self.files_read += 1;
        if depth > 16 || self.lines.len() > 100000 || self.files_read > 256 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "SSH config include limit exceeded",
            ));
        }
        let canonical = fs::canonicalize(path)?;
        if !seen.insert(canonical.clone()) {
            self.warnings
                .push(format!("Repeated Include skipped: {}", path.display()));
            return Ok(());
        }
        use std::io::Read;
        let mut text = String::new();
        fs::File::open(path)?
            .take(4 * 1024 * 1024 + 1)
            .read_to_string(&mut text)?;
        if text.len() > 4 * 1024 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "SSH config exceeds 4 MiB",
            ));
        }
        for (index, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some(split) = line.find(|c: char| c.is_whitespace() || c == '=') else {
                self.warnings
                    .push(format!("{}:{}: missing value", path.display(), index + 1));
                continue;
            };
            let key = line[..split].to_ascii_lowercase();
            let value = line[split..].trim_start_matches(|c: char| c.is_whitespace() || c == '=');
            let Some(values) = shlex::split(value) else {
                self.warnings
                    .push(format!("{}:{}: invalid quoting", path.display(), index + 1));
                continue;
            };
            if key == "include" {
                for pattern in values {
                    let pattern = expand_path(&pattern, home);
                    let pattern = if pattern.is_absolute() {
                        pattern
                    } else {
                        home.join(".ssh").join(pattern)
                    };
                    match glob::glob(&pattern.to_string_lossy()) {
                        Ok(paths) => {
                            for included in paths.flatten() {
                                self.include(&included, home, seen, depth + 1)?;
                            }
                        }
                        Err(e) => self.warnings.push(e.to_string()),
                    }
                }
            } else if key == "proxycommand" {
                // Preserve argument quoting; splitting happens only after explicit review.
                self.lines.push((key, vec![value.to_string()]));
            } else {
                self.lines.push((key, values));
            }
        }
        seen.remove(&canonical);
        Ok(())
    }
    pub fn aliases(&self) -> Vec<String> {
        let mut aliases = Vec::new();
        for (key, values) in &self.lines {
            if key == "host" {
                for value in values {
                    if !value.contains(['*', '?', '!']) && !aliases.contains(value) {
                        aliases.push(value.clone());
                    }
                }
            }
        }
        aliases
    }
    pub fn resolve(&self, alias: &str) -> QuickConnect {
        let mut result = QuickConnect {
            options: HashSet::new(),
            profile: Profile {
                host: alias.into(),
                name: alias.into(),
                ssh_alias: alias.into(),
                ..Profile::default()
            },
            warnings: self.warnings.clone(),
        };
        let mut active = true;
        let mut applied = HashSet::new();
        for (key, values) in &self.lines {
            if key == "host" {
                active = host_matches(values, alias);
                continue;
            }
            if key == "match" {
                active = false;
                result
                    .warnings
                    .push("Match blocks are unsupported and were not applied".into());
                continue;
            }
            if !active {
                continue;
            }
            if applied.contains(key) && key != "identityfile" {
                continue;
            }
            applied.insert(key.clone());
            let value = values.join(" ");
            apply(&mut result, key, &value);
        }
        result
    }
}
pub fn expand_path(value: &str, home: &Path) -> PathBuf {
    if value == "~" {
        home.to_path_buf()
    } else if let Some(rest) = value.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(value)
    }
}
fn host_matches(patterns: &[String], host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    let mut positive = false;
    for pattern in patterns {
        let (negative, p) = pattern
            .strip_prefix('!')
            .map_or((false, pattern.as_str()), |p| (true, p));
        if glob::Pattern::new(&p.to_ascii_lowercase()).is_ok_and(|p| p.matches(&host)) {
            if negative {
                return false;
            }
            positive = true;
        }
    }
    positive
}

#[derive(Debug, Clone)]
pub struct QuickConnect {
    /// Options explicitly supplied, so forms can retain unrelated draft values.
    pub options: HashSet<String>,
    pub profile: Profile,
    pub warnings: Vec<String>,
}
fn apply(result: &mut QuickConnect, key: &str, value: &str) {
    result.options.insert(key.into());
    let p = &mut result.profile;
    match key {
        "hostname" => p.host = value.into(),
        "user" => p.user = value.into(),
        "port" => match value.parse::<u16>() {
            Ok(port) if port > 0 => p.port = port,
            _ => result.warnings.push("Invalid Port".into()),
        },
        "identityfile" => {
            if p.identity_file.is_empty() {
                p.identity_file = value.into();
                p.auth = Auth::Key;
            } else {
                result
                    .warnings
                    .push("Multiple IdentityFile entries require selecting a key".into());
            }
        }
        "certificatefile" => p.certificate_file = value.into(),
        "identityagent" => p.identity_agent = value.into(),
        "identitiesonly" => match value.to_ascii_lowercase().as_str() {
            "yes" => p.identities_only = true,
            "no" => p.identities_only = false,
            _ => result.warnings.push("Invalid IdentitiesOnly".into()),
        },
        "userknownhostsfile" => {
            if value == "none" {
                result
                    .warnings
                    .push("Disabling known_hosts is not supported".into());
            } else {
                p.known_hosts = value.into();
            }
        }
        "stricthostkeychecking" => match value.to_ascii_lowercase().as_str() {
            "yes" => p.strict_host_key = true,
            "ask" => p.strict_host_key = false,
            _ => result
                .warnings
                .push("Only StrictHostKeyChecking yes or ask is supported".into()),
        },
        "proxyjump" => {
            if value != "none" {
                p.proxy_jump = value.into()
            }
        }
        "proxycommand" => {
            if value != "none" {
                p.proxy_command = value.into();
                p.proxy_review_required = true
            }
        }
        "serveraliveinterval" => match value.parse() {
            Ok(seconds) => p.keepalive_seconds = seconds,
            Err(_) => result.warnings.push("Invalid ServerAliveInterval".into()),
        },
        "forwardagent" => {
            p.forward_agent = value.eq_ignore_ascii_case("yes");
            if p.forward_agent {
                result
                    .warnings
                    .push("Agent forwarding is not yet supported".into());
            }
        }
        _ => result.warnings.push(format!("Unsupported option: {key}")),
    }
}

pub fn parse_command(command: &str) -> Result<QuickConnect, String> {
    if command.len() > 16384 {
        return Err("Command exceeds 16 KiB".into());
    }
    let words = shlex::split(command).ok_or("Unclosed quote or escape")?;
    let mut iter = words.into_iter().peekable();
    if iter.peek().is_some_and(|v| v == "ssh") {
        iter.next();
    }
    let mut result = QuickConnect {
        options: HashSet::new(),
        profile: Profile::default(),
        warnings: vec![],
    };
    let mut destination = false;
    while let Some(word) = iter.next() {
        if word == "--" {
            continue;
        }
        if word.starts_with('-') {
            if !word.is_char_boundary(2) {
                return Err(format!("Unsupported SSH flag: {word}"));
            }
            let (flag, inline) = if word.len() > 2 {
                (&word[..2], Some(word[2..].to_string()))
            } else {
                (word.as_str(), None)
            };
            let key = match flag {
                "-p" => "port",
                "-l" => "user",
                "-i" => "identityfile",
                "-J" => "proxyjump",
                "-o" => "option",
                "-A" => {
                    apply(&mut result, "forwardagent", "yes");
                    continue;
                }
                "-a" => {
                    apply(&mut result, "forwardagent", "no");
                    continue;
                }
                _ => return Err(format!("Unsupported SSH flag: {word}")),
            };
            let value = inline
                .or_else(|| iter.next())
                .ok_or_else(|| format!("Missing value for {flag}"))?;
            if key == "option" {
                let (name, value) = value
                    .split_once('=')
                    .or_else(|| value.split_once(' '))
                    .ok_or("-o requires Option=value")?;
                apply(&mut result, &name.to_ascii_lowercase(), value);
            } else {
                apply(&mut result, key, &value);
            }
            continue;
        }
        if destination {
            result
                .warnings
                .push("Remote commands are unsupported; a shell will be opened".into());
            break;
        }
        let (user, address) = word
            .rsplit_once('@')
            .map_or((None, word.as_str()), |(u, h)| (Some(u), h));
        if let Some(user) = user {
            result.profile.user = user.into();
        }
        let (host, port) = if let Some(rest) = address.strip_prefix('[') {
            let end = rest.find(']').ok_or("Unclosed IPv6 brackets")?;
            let suffix = &rest[end + 1..];
            (
                &rest[..end],
                if suffix.is_empty() {
                    None
                } else {
                    Some(suffix.strip_prefix(':').ok_or("Invalid address suffix")?)
                },
            )
        } else if address.matches(':').count() == 1 {
            let (host, port) = address.split_once(':').unwrap();
            (host, Some(port))
        } else {
            (address, None)
        };
        result.profile.host = host.into();
        if let Some(port) = port {
            result.profile.port = port.parse().map_err(|_| "Invalid port")?;
        }
        destination = true;
    }
    result.profile.name = result.profile.host.clone();
    result.profile.validate().map_err(|e| e.to_string())?;
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_parses_quoted_key_jump_and_ipv6() {
        let p = parse_command("ssh -i 'my key' -p 2222 -J admin@bastion user@[::1]")
            .unwrap()
            .profile;
        assert_eq!(p.host, "::1");
        assert_eq!(p.port, 2222);
        assert_eq!(p.identity_file, "my key");
        assert_eq!(p.proxy_jump, "admin@bastion");
    }
    #[test]
    fn command_options_after_destination_and_remote_commands() {
        let parsed = parse_command("ssh -i ~/.ssh/mykey.pem user@192.168.1.1 -p 2222").unwrap();
        assert_eq!(parsed.profile.port, 2222);
        assert_eq!(parsed.profile.user, "user");
        assert_eq!(parsed.profile.host, "192.168.1.1");
        assert_eq!(parsed.profile.identity_file, "~/.ssh/mykey.pem");
        assert!(parsed.options.contains("identityfile") && parsed.options.contains("port"));
        assert!(parsed.warnings.is_empty());
        let remote = parse_command("ssh user@host echo -p 9999").unwrap();
        assert_eq!(remote.profile.port, 22);
        assert_eq!(remote.warnings.len(), 1);
    }

    #[test]
    fn unsupported_and_malformed_commands_never_silently_execute() {
        assert!(parse_command("ssh -é host").is_err());
        assert!(parse_command("ssh -L 80:localhost:80 host").is_err());
        assert!(parse_command("ssh 'broken").is_err());
        assert!(parse_command("host:0").is_err());
        let p = parse_command("ssh -o 'ProxyCommand=cloudflared access ssh' host")
            .unwrap()
            .profile;
        assert!(p.proxy_review_required);
    }
    #[test]
    fn includes_keep_quoted_proxy_arguments_and_report_unsafe_policy() {
        let directory = tempfile::tempdir().unwrap();
        let ssh = directory.path().join(".ssh");
        fs::create_dir(&ssh).unwrap();
        fs::write(ssh.join("included.conf"),"Host work\n User alice\n ProxyCommand helper 'argument with spaces' %h\n StrictHostKeyChecking no\n").unwrap();
        fs::write(
            ssh.join("config"),
            "Include included.conf\nHost *\n User default\n",
        )
        .unwrap();
        let config = ParsedConfig::read(&ssh.join("config"), directory.path()).unwrap();
        let resolved = config.resolve("work");
        assert_eq!(resolved.profile.user, "alice");
        assert_eq!(
            shlex::split(&resolved.profile.proxy_command).unwrap()[1],
            "argument with spaces"
        );
        assert!(!resolved.warnings.is_empty());
    }
    #[test]
    fn first_value_wins_and_negative_host_patterns() {
        let config = ParsedConfig {
            lines: vec![
                ("host".into(), vec!["work".into()]),
                ("user".into(), vec!["specific".into()]),
                ("host".into(), vec!["*".into(), "!excluded".into()]),
                ("user".into(), vec!["default".into()]),
            ],
            warnings: vec![],
            files_read: 0,
        };
        assert_eq!(config.resolve("work").profile.user, "specific");
        assert_eq!(config.resolve("other").profile.user, "default");
        assert_eq!(config.resolve("excluded").profile.user, "");
    }
}
