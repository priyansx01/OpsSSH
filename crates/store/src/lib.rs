//! Validated server profiles and settings. This format intentionally has no secret fields.
use serde::{Deserialize, Serialize};
use std::{fs, io, path::Path};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Auth {
    Password,
    Key,
    #[default]
    Agent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Profile {
    pub id: u64,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub environment: String,
    pub tags: Vec<String>,
    pub favorite: bool,
    pub auth: Auth,
    pub identity_file: String,
    pub certificate_file: String,
    pub proxy_jump: String,
    pub proxy_command: String,
    pub proxy_review_required: bool,
    pub tmux_session: Option<String>,
    pub keepalive_seconds: u64,
    pub forward_agent: bool,
    pub legacy: bool,
    pub known_hosts: String,
    pub identity_agent: String,
    pub identities_only: bool,
    pub strict_host_key: bool,
    pub ssh_alias: String,
}
impl Default for Profile {
    fn default() -> Self {
        Self {
            id: 0,
            name: String::new(),
            host: String::new(),
            port: 22,
            user: String::new(),
            environment: String::new(),
            tags: vec![],
            favorite: false,
            auth: Auth::Agent,
            identity_file: String::new(),
            certificate_file: String::new(),
            proxy_jump: String::new(),
            proxy_command: String::new(),
            proxy_review_required: true,
            tmux_session: None,
            keepalive_seconds: 30,
            forward_agent: false,
            legacy: false,
            known_hosts: String::new(),
            identity_agent: String::new(),
            identities_only: false,
            strict_host_key: false,
            ssh_alias: String::new(),
        }
    }
}
impl Profile {
    pub fn validate(&self) -> io::Result<()> {
        if self.host.is_empty()
            || self.host.starts_with('-')
            || self
                .host
                .chars()
                .any(|c| c.is_control() || c.is_whitespace())
            || self.port == 0
            || self.user.chars().any(|c| c.is_control())
        {
            return Err(invalid("invalid host, username or port"));
        }
        if self.name.len() > 256 || self.proxy_command.len() > 16384 {
            return Err(invalid("profile field exceeds size limit"));
        }
        if self.tmux_session.as_ref().is_some_and(|s| {
            s.is_empty()
                || !s
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        }) {
            return Err(invalid(
                "tmux session must contain letters, numbers, '-' or '_'",
            ));
        }
        Ok(())
    }
    pub fn matches(&self, query: &str) -> bool {
        let haystack = format!(
            "{} {} {} {} {}",
            self.name,
            self.host,
            self.user,
            self.environment,
            self.tags.join(" ")
        )
        .to_lowercase();
        let mut chars = haystack.chars();
        query
            .to_lowercase()
            .chars()
            .all(|needle| chars.by_ref().any(|c| c == needle))
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub theme: String,
    pub font_size: u16,
    pub update_check: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "dark".into(),
            font_size: 16,
            update_check: false,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Store {
    pub version: u32,
    pub servers: Vec<Profile>,
    pub settings: Settings,
}
impl Default for Store {
    fn default() -> Self {
        Self {
            version: 1,
            servers: vec![],
            settings: Settings::default(),
        }
    }
}
fn invalid(message: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}
impl Store {
    pub fn parse(text: &str) -> io::Result<Self> {
        if text.len() > 8 * 1024 * 1024 {
            return Err(invalid("server file exceeds 8 MiB"));
        }
        let store: Self = toml::from_str(text).map_err(invalid)?;
        store.validate()?;
        Ok(store)
    }
    pub fn load(path: &Path) -> io::Result<Self> {
        use std::io::Read;
        let read: io::Result<String> = (|| {
            let mut text = String::new();
            fs::File::open(path)?
                .take(8 * 1024 * 1024 + 1)
                .read_to_string(&mut text)?;
            Ok(text)
        })();
        match read {
            Ok(text) => Self::parse(&text),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }
    pub fn validate(&self) -> io::Result<()> {
        if self.version != 1 || self.servers.len() > 10000 {
            return Err(invalid("unsupported version or too many servers"));
        }
        let mut ids = std::collections::HashSet::new();
        for profile in &self.servers {
            profile.validate()?;
            if profile.id == 0 || !ids.insert(profile.id) {
                return Err(invalid("duplicate or missing server id"));
            }
        }
        if !(8..=48).contains(&self.settings.font_size) {
            return Err(invalid("font size must be between 8 and 48"));
        }
        Ok(())
    }
    pub fn export(&self) -> io::Result<String> {
        self.validate()?;
        let mut safe = self.clone();
        for profile in &mut safe.servers {
            profile.proxy_review_required = true;
            // A program argument can contain a token; never include commands in shared exports.
            profile.proxy_command.clear();
        }
        toml::to_string_pretty(&safe).map_err(invalid)
    }
    pub fn save(&self, path: &Path) -> io::Result<()> {
        self.validate()?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;
        let text = toml::to_string_pretty(self).map_err(invalid)?;
        use std::io::Write;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(text.as_bytes())?;
        temporary.as_file().sync_all()?;
        temporary.persist(path).map_err(|error| error.error)?;
        Ok(())
    }
    pub fn upsert(&mut self, mut profile: Profile) -> io::Result<u64> {
        profile.validate()?;
        if profile.name.trim().is_empty() {
            profile.name = profile.host.clone();
        }
        if profile.id == 0 {
            profile.id = self
                .servers
                .iter()
                .map(|p| p.id)
                .max()
                .unwrap_or(0)
                .checked_add(1)
                .ok_or_else(|| invalid("id exhausted"))?;
        }
        let id = profile.id;
        if let Some(existing) = self.servers.iter_mut().find(|p| p.id == id) {
            *existing = profile;
        } else {
            self.servers.push(profile);
        }
        Ok(id)
    }
    pub fn import(&mut self, text: &str) -> io::Result<usize> {
        let imported = Self::parse(text)?;
        let count = imported.servers.len();
        if self.servers.len() + count > 10000 {
            return Err(invalid("too many servers"));
        }
        let mut next = self.clone();
        for mut profile in imported.servers {
            profile.id = 0;
            profile.proxy_review_required = true;
            next.upsert(profile)?;
        }
        *self = next;
        Ok(count)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trip_and_import_reviews_commands() {
        let mut store = Store::default();
        store
            .upsert(Profile {
                host: "example.test".into(),
                proxy_command: "cloudflared access ssh".into(),
                proxy_review_required: false,
                ..Profile::default()
            })
            .unwrap();
        let text = store.export().unwrap();
        let mut imported = Store::default();
        imported.import(&text).unwrap();
        assert!(imported.servers[0].proxy_review_required);
        assert!(imported.servers[0].proxy_command.is_empty());
        assert_eq!(imported.servers[0].host, "example.test");
    }
    #[test]
    fn repeated_save_replaces_atomically_and_failed_save_keeps_data() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("servers.toml");
        let mut store = Store::default();
        store.save(&path).unwrap();
        store
            .upsert(Profile {
                host: "example.test".into(),
                ..Profile::default()
            })
            .unwrap();
        store.save(&path).unwrap();
        assert_eq!(Store::load(&path).unwrap().servers.len(), 1);
        assert!(store.save(directory.path()).is_err());
        assert_eq!(Store::load(&path).unwrap().servers.len(), 1);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }
    #[test]
    fn secrets_and_invalid_addresses_rejected() {
        assert!(Store::parse("password='secret'").is_err());
        assert!(
            Profile {
                host: "-evil".into(),
                ..Profile::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            Profile {
                host: "test".into(),
                tmux_session: Some("work;evil".into()),
                ..Profile::default()
            }
            .validate()
            .is_err()
        );
    }
    #[test]
    fn search_is_case_insensitive_subsequence() {
        assert!(
            Profile {
                host: "production.example".into(),
                ..Profile::default()
            }
            .matches("PRD")
        );
    }
}
