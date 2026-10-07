//! Conversion of reviewed profiles into transport options; no credentials are serialized.
use opsssh_ssh_core::{ApprovedProxyCommand, AuthMethod, ConnectionOptions, TmuxOptions};
use opsssh_store::{Auth, Profile, Store};

pub fn options(profile: &Profile, store: &Store) -> Result<ConnectionOptions, String> {
    let mut profile = profile.clone();
    if profile.user.is_empty() {
        profile.user = opsssh_platform::default_username().unwrap_or_default();
    }
    build(&profile, store, 0)
}
fn build(profile: &Profile, store: &Store, depth: usize) -> Result<ConnectionOptions, String> {
    profile.validate().map_err(|e| e.to_string())?;
    if depth > 1 {
        return Err("Jump profiles cannot themselves contain jumps".into());
    }
    if profile.user.trim().is_empty() {
        return Err("Enter an SSH username".into());
    }
    if profile.forward_agent || profile.legacy {
        return Err(
            "Agent forwarding and legacy algorithms are not implemented; disable them to connect"
                .into(),
        );
    }
    let home = opsssh_platform::home_dir().map_err(|e| e.to_string())?;
    let path = |value: &str| opsssh_ssh_config::expand_path(value, &home);
    let known = if profile.known_hosts.is_empty() {
        opsssh_platform::default_known_hosts().map_err(|e| e.to_string())?
    } else {
        path(&profile.known_hosts)
    };
    let mut options = ConnectionOptions::new(&profile.host, &profile.user, known);
    options.port = profile.port;
    options.credential_id = (profile.id != 0).then(|| {
        format!(
            "{}:{}:{}:{}",
            profile.id, profile.host, profile.port, profile.user
        )
    });
    options.strict_host_key = profile.strict_host_key;
    options.keepalive = std::time::Duration::from_secs(profile.keepalive_seconds);
    options.auth = match profile.auth {
        Auth::Password => vec![AuthMethod::PasswordPrompt, AuthMethod::KeyboardInteractive],
        Auth::Agent => vec![
            AuthMethod::Agent {
                identity_agent: (!profile.identity_agent.is_empty())
                    .then(|| profile.identity_agent.clone()),
            },
            AuthMethod::KeyboardInteractive,
            AuthMethod::PasswordPrompt,
        ],
        Auth::Key => {
            if profile.identity_file.is_empty() {
                return Err("Select a private key file".into());
            }
            let automatic_certificate = path(&format!("{}-cert.pub", profile.identity_file));
            let certificate = if !profile.certificate_file.is_empty() {
                Some(path(&profile.certificate_file))
            } else if automatic_certificate.is_file() {
                Some(automatic_certificate)
            } else {
                None
            };
            vec![
                AuthMethod::PrivateKey {
                    path: path(&profile.identity_file),
                    passphrase: None,
                    certificate,
                },
                AuthMethod::KeyboardInteractive,
            ]
        }
    };
    if profile.identity_agent == "none" {
        options
            .auth
            .retain(|method| !matches!(method, AuthMethod::Agent { .. }));
    }
    if profile.identities_only && profile.auth != Auth::Key {
        return Err("IdentitiesOnly requires an explicitly selected key file".into());
    }
    if let Some(session) = &profile.tmux_session {
        options.tmux = Some(TmuxOptions {
            session_name: session.clone(),
        });
    }
    if !profile.proxy_jump.is_empty() && !profile.proxy_command.is_empty() {
        return Err("Choose either jump hosts or a proxy command".into());
    }
    for jump in profile
        .proxy_jump
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let hop = if let Some(saved) = store.servers.iter().find(|p| p.name == jump) {
            saved.clone()
        } else {
            let config_path = home.join(".ssh/config");
            let mut parsed = if config_path.exists() && !jump.contains(['@', ':']) {
                let config = opsssh_ssh_config::ParsedConfig::read(&config_path, &home)
                    .map_err(|e| e.to_string())?;
                let resolved = config.resolve(jump);
                if !resolved.warnings.is_empty() {
                    return Err(format!(
                        "Unsupported jump host options: {}",
                        resolved.warnings.join("; ")
                    ));
                }
                resolved.profile
            } else {
                opsssh_ssh_config::parse_command(jump)?.profile
            };
            if parsed.user.is_empty() {
                parsed.user = profile.user.clone();
            }
            parsed
        };
        options.jumps.push(build(&hop, store, depth + 1)?);
    }
    if !profile.proxy_command.is_empty() {
        if profile.proxy_review_required {
            return Err(format!(
                "Review the exact proxy command in Edit before connecting: {}",
                profile.proxy_command
            ));
        }
        let mut words = shlex::split(&profile.proxy_command)
            .ok_or("Invalid proxy command quoting")?
            .into_iter();
        let program = expand_proxy(&words.next().ok_or("Empty proxy command")?, profile)?;
        options.proxy = Some(ApprovedProxyCommand {
            program,
            arguments: words
                .map(|arg| expand_proxy(&arg, profile))
                .collect::<Result<Vec<_>, _>>()?,
            approved: true,
        });
    }
    options.validate().map_err(|e| e.to_string())?;
    Ok(options)
}

fn expand_proxy(argument: &str, profile: &Profile) -> Result<String, String> {
    let mut result = String::new();
    let mut chars = argument.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            result.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => result.push('%'),
            Some('h') => result.push_str(&profile.host),
            Some('p') => result.push_str(&profile.port.to_string()),
            Some('r') => result.push_str(&profile.user),
            Some('n') => result.push_str(if profile.ssh_alias.is_empty() {
                &profile.host
            } else {
                &profile.ssh_alias
            }),
            _ => return Err("Unsupported or incomplete ProxyCommand percent token".into()),
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refuses_unreviewed_proxy_and_unsupported_options() {
        let mut p = Profile {
            host: "example.test".into(),
            user: "user".into(),
            proxy_command: "cloudflared access ssh".into(),
            ..Profile::default()
        };
        assert!(options(&p, &Store::default()).is_err());
        p.proxy_review_required = false;
        assert!(
            options(&p, &Store::default())
                .unwrap()
                .proxy
                .unwrap()
                .approved
        );
        p.forward_agent = true;
        assert!(options(&p, &Store::default()).is_err());
    }
}
