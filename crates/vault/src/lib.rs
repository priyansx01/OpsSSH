//! Opt-in OS credential storage. OTP responses must never be passed to this API.
use zeroize::Zeroizing;
/// Explicit user consent is required for each credential save.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveConsent {
    Granted,
    Declined,
}
#[derive(Debug, Default)]
pub struct Vault;
impl Vault {
    pub fn read(&self, profile_id: &str) -> keyring::Result<Zeroizing<String>> {
        keyring::Entry::new("OpsSSH", profile_id)?
            .get_password()
            .map(Zeroizing::new)
    }
    pub fn save(
        &self,
        profile_id: &str,
        secret: &str,
        consent: SaveConsent,
    ) -> keyring::Result<bool> {
        if consent == SaveConsent::Declined {
            return Ok(false);
        }
        keyring::Entry::new("OpsSSH", profile_id)?.set_password(secret)?;
        Ok(true)
    }
    pub fn delete(&self, profile_id: &str) -> keyring::Result<()> {
        keyring::Entry::new("OpsSSH", profile_id)?.delete_credential()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn declined_save_never_opens_os_store() {
        assert!(
            !Vault
                .save("test-no-os-side-effects", "secret", SaveConsent::Declined)
                .unwrap()
        );
    }
}
