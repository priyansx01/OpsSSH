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
    pub terminal_upload_directory: Option<String>,
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
            terminal_upload_directory: None,
        }
    }
}
impl Profile {
    pub fn validate(&self) -> io::Result<()> {
        if self.terminal_upload_directory.as_ref().is_some_and(|path| {
            path.is_empty() || path.len() > 4096 || path.chars().any(char::is_control)
        }) {
            return Err(invalid("invalid terminal upload directory"));
        }
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
    pub server_view: String,
    pub sidebar_collapsed: bool,
    pub reduced_motion: bool,
    pub sort: String,
    #[serde(with = "connection_times")]
    pub last_connected: std::collections::BTreeMap<u64, u64>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "dark".into(),
            font_size: 16,
            update_check: false,
            server_view: "cards".into(),
            sidebar_collapsed: false,
            reduced_motion: false,
            sort: "name".into(),
            last_connected: std::collections::BTreeMap::new(),
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
        if !matches!(self.settings.theme.as_str(), "dark" | "light" | "system") {
            return Err(invalid("theme must be dark, light or system"));
        }
        if !matches!(
            self.settings.server_view.as_str(),
            "cards" | "list" | "compact"
        ) {
            return Err(invalid("server view must be cards, list or compact"));
        }
        if !matches!(
            self.settings.sort.as_str(),
            "name" | "last_connected" | "environment"
        ) {
            return Err(invalid("sort must be name, last_connected or environment"));
        }
        if self.settings.last_connected.len() > 10000
            || self
                .settings
                .last_connected
                .iter()
                .any(|(id, time)| *id == 0 || *time == 0)
        {
            return Err(invalid("invalid last-connected timestamps"));
        }
        Ok(())
    }
    pub fn export(&self) -> io::Result<String> {
        self.validate()?;
        let mut safe = self.clone();
        safe.settings.last_connected.clear();
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
        if self
            .servers
            .iter()
            .find(|old| old.id == profile.id)
            .is_some_and(|old| {
                old.host != profile.host || old.port != profile.port || old.user != profile.user
            })
        {
            profile.terminal_upload_directory = None;
        }
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
    /// Call only after SSH authentication succeeds; attempts do not update recency.
    pub fn record_connection(&mut self, server_id: u64, unix_seconds: u64) -> io::Result<()> {
        if unix_seconds == 0 || !self.servers.iter().any(|server| server.id == server_id) {
            return Err(invalid("a saved server and valid timestamp are required"));
        }
        self.settings.last_connected.insert(server_id, unix_seconds);
        Ok(())
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
mod connection_times {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;
    pub fn serialize<S: Serializer>(
        values: &BTreeMap<u64, u64>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        values
            .iter()
            .map(|(id, time)| (id.to_string(), *time))
            .collect::<BTreeMap<_, _>>()
            .serialize(serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<BTreeMap<u64, u64>, D::Error> {
        BTreeMap::<String, u64>::deserialize(deserializer)?
            .into_iter()
            .map(|(id, time)| {
                id.parse()
                    .map(|id| (id, time))
                    .map_err(serde::de::Error::custom)
            })
            .collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn upload_destination_persists_and_is_reset_when_endpoint_changes() {
        let mut store = Store::default();
        let id = store
            .upsert(Profile {
                host: "server.test".into(),
                user: "deploy".into(),
                terminal_upload_directory: Some("/srv/uploads".into()),
                ..Default::default()
            })
            .unwrap();
        let serialized = store.export().unwrap();
        let restored = Store::parse(&serialized).unwrap();
        assert_eq!(
            restored.servers[0].terminal_upload_directory.as_deref(),
            Some("/srv/uploads")
        );
        let mut renamed = store.servers[0].clone();
        renamed.name = "Renamed".into();
        store.upsert(renamed).unwrap();
        assert!(store.servers[0].terminal_upload_directory.is_some());
        for change in 0..3 {
            let mut changed = store.servers[0].clone();
            changed.terminal_upload_directory = Some("/srv/uploads".into());
            match change {
                0 => changed.host = "other.test".into(),
                1 => changed.port = 2222,
                _ => changed.user = "other".into(),
            }
            store.upsert(changed).unwrap();
            assert_eq!(store.servers[0].id, id);
            assert_eq!(store.servers[0].terminal_upload_directory, None);
        }
        let legacy = Store::parse("version=1\n[[servers]]\nid=1\nhost='legacy.test'\n").unwrap();
        assert_eq!(legacy.servers[0].terminal_upload_directory, None);
    }
    #[test]
    fn older_settings_receive_new_defaults() {
        let store = Store::parse(
            "version = 1\n[settings]\ntheme = 'light'\nfont_size = 18\nupdate_check = false\n",
        )
        .unwrap();
        assert_eq!(store.settings.server_view, "cards");
        assert_eq!(store.settings.sort, "name");
        assert!(!store.settings.sidebar_collapsed);
        assert!(!store.settings.reduced_motion);
        assert!(store.settings.last_connected.is_empty());
    }
    #[test]
    fn settings_round_trip_recency_is_local_and_values_are_validated() {
        let mut store = Store::default();
        let id = store
            .upsert(Profile {
                host: "example.test".into(),
                ..Profile::default()
            })
            .unwrap();
        store.settings.server_view = "compact".into();
        store.settings.sort = "last_connected".into();
        store.settings.sidebar_collapsed = true;
        store.settings.reduced_motion = true;
        store.record_connection(id, 1720000000).unwrap();
        let text = toml::to_string(&store).unwrap();
        let read = Store::parse(&text).unwrap();
        assert_eq!(read.settings.last_connected[&id], 1720000000);
        assert!(read.settings.sidebar_collapsed);
        assert!(
            Store::parse(&store.export().unwrap())
                .unwrap()
                .settings
                .last_connected
                .is_empty()
        );
        assert!(store.record_connection(id + 1, 1720000000).is_err());
        store.settings.theme = "unknown".into();
        assert!(store.validate().is_err());
        store.settings.theme = "system".into();
        store.settings.server_view = "unknown".into();
        assert!(store.validate().is_err());
    }
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
