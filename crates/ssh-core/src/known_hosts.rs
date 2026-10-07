use anyhow::{Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use hmac::{Hmac, Mac};
use russh::keys::{HashAlg, PublicKey, PublicKeyOrCertificate};
use sha1::Sha1;
use std::{fs, io::Write, path::Path};
#[derive(Debug, PartialEq, Eq)]
pub enum HostStatus {
    Trusted,
    Unknown,
}
fn wildcard(pattern: &str, value: &str) -> bool {
    let (p, v) = (pattern.as_bytes(), value.as_bytes());
    let (mut i, mut j, mut star, mut mark) = (0, 0, None, 0);
    while j < v.len() {
        if i < p.len() && (p[i] == b'?' || p[i].eq_ignore_ascii_case(&v[j])) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == b'*' {
            star = Some(i);
            i += 1;
            mark = j;
        } else if let Some(s) = star {
            i = s + 1;
            mark += 1;
            j = mark;
        } else {
            return false;
        }
    }
    while i < p.len() && p[i] == b'*' {
        i += 1;
    }
    i == p.len()
}
fn matches(patterns: &str, host: &str) -> bool {
    let mut matched = false;
    for pattern in patterns.split(',') {
        let (negated, pattern) = pattern
            .strip_prefix('!')
            .map_or((false, pattern), |p| (true, p));
        let hit = if let Some(hashed) = pattern.strip_prefix("|1|") {
            let mut parts = hashed.split('|');
            match (
                parts.next().and_then(|p| STANDARD.decode(p).ok()),
                parts.next().and_then(|p| STANDARD.decode(p).ok()),
            ) {
                (Some(salt), Some(hash)) => {
                    Hmac::<Sha1>::new_from_slice(&salt).is_ok_and(|mut mac| {
                        mac.update(host.as_bytes());
                        mac.verify_slice(&hash).is_ok()
                    })
                }
                _ => false,
            }
        } else {
            wildcard(pattern, host)
        };
        if hit && negated {
            return false;
        }
        matched |= hit;
    }
    matched
}
pub fn check(
    path: &Path,
    host: &str,
    port: u16,
    presented: &PublicKeyOrCertificate,
) -> Result<HostStatus> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HostStatus::Unknown),
        Err(e) => return Err(e.into()),
    };
    let host_port = if port == 22 {
        host.to_owned()
    } else {
        format!("[{host}]:{port}")
    };
    let key = presented.public_key();
    let mut known = false;
    let mut trusted = false;
    for (index, line) in text.lines().enumerate() {
        let mut words = line.split_whitespace();
        let Some(mut hosts) = words.next() else {
            continue;
        };
        if hosts.starts_with('#') {
            continue;
        }
        let marker = if hosts.starts_with('@') {
            let marker = hosts;
            hosts = words
                .next()
                .ok_or_else(|| anyhow::anyhow!("Malformed known_hosts line {}", index + 1))?;
            Some(marker)
        } else {
            None
        };
        if !matches(hosts, &host_port) {
            continue;
        }
        let algorithm = words
            .next()
            .ok_or_else(|| anyhow::anyhow!("Missing host key algorithm"))?;
        let data = words
            .next()
            .ok_or_else(|| anyhow::anyhow!("Missing host key data"))?;
        let recorded = PublicKey::from_openssh(&format!("{algorithm} {data}"))?;
        match marker {
            Some("@revoked") => {
                if recorded.key_data() == key.key_data()
                    || presented
                        .certificate()
                        .is_some_and(|c| c.signature_key() == recorded.key_data())
                {
                    bail!("Host key is revoked (known_hosts line {})", index + 1);
                }
            }
            Some("@cert-authority") => {
                known = true;
                if let Some(cert) = presented.certificate() {
                    anyhow::ensure!(
                        cert.cert_type().is_host(),
                        "Server supplied a non-host certificate"
                    );
                    anyhow::ensure!(
                        cert.valid_principals().iter().any(|p| p == host),
                        "Host certificate principal does not match hostname"
                    );
                    anyhow::ensure!(
                        cert.critical_options().is_empty(),
                        "Unsupported critical host certificate option"
                    );
                    if cert.signature_key() == recorded.key_data() {
                        cert.validate(&[recorded.fingerprint(HashAlg::Sha256)])?;
                        trusted = true;
                    }
                }
            }
            Some(other) => bail!("Unsupported known_hosts marker {other}"),
            None => {
                known = true;
                if presented.certificate().is_none() && recorded.key_data() == key.key_data() {
                    trusted = true;
                }
            }
        }
    }
    if trusted {
        Ok(HostStatus::Trusted)
    } else if known {
        bail!("Host key changed or host certificate is not trusted; connection blocked")
    } else {
        Ok(HostStatus::Unknown)
    }
}
pub fn remember(path: &Path, host: &str, port: u16, key: &PublicKey) -> Result<()> {
    anyhow::ensure!(
        !host
            .chars()
            .any(|c: char| c.is_whitespace() || c.is_control() || c == ','),
        "Invalid known_hosts hostname"
    );
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let prefix = if port == 22 {
        host.to_owned()
    } else {
        format!("[{host}]:{port}")
    };
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "\n{prefix} {}", key.to_openssh()?)?;
    file.sync_all()?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn patterns_respect_negations() {
        assert!(matches("*.example,!bad.example", "good.example"));
        assert!(!matches("*.example,!bad.example", "bad.example"));
        assert!(!matches("[host]:2222", "host"));
    }
    #[test]
    fn hashed_hosts() {
        let salt = b"fixture-salt";
        let mut mac = Hmac::<Sha1>::new_from_slice(salt).unwrap();
        mac.update(b"[host]:2222");
        let pattern = format!(
            "|1|{}|{}",
            STANDARD.encode(salt),
            STANDARD.encode(mac.finalize().into_bytes())
        );
        assert!(matches(&pattern, "[host]:2222"));
        assert!(!matches(&pattern, "host"));
    }
}
#[cfg(test)]
mod certificate_tests {
    use super::*;
    use russh::keys::{
        Algorithm, PrivateKey,
        ssh_key::certificate::{Builder, CertType},
    };
    fn certificate(ca: &PrivateKey, principal: &str, expires: u64) -> PublicKeyOrCertificate {
        let subject = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        let mut builder = Builder::new(
            vec![42; 32],
            subject.public_key().key_data().clone(),
            0,
            expires,
        )
        .unwrap();
        builder
            .cert_type(CertType::Host)
            .unwrap()
            .valid_principal(principal)
            .unwrap();
        PublicKeyOrCertificate::Certificate(builder.sign(ca).unwrap())
    }
    #[test]
    fn trusted_ca_checks_signature_principal_expiry_and_revocation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        let ca = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        let authority = format!(
            "@cert-authority *.example {}\n",
            ca.public_key().to_openssh().unwrap()
        );
        std::fs::write(&path, &authority).unwrap();
        assert_eq!(
            check(
                &path,
                "good.example",
                22,
                &certificate(&ca, "good.example", u64::MAX)
            )
            .unwrap(),
            HostStatus::Trusted
        );
        assert!(
            check(
                &path,
                "good.example",
                22,
                &certificate(&ca, "other.example", u64::MAX)
            )
            .is_err()
        );
        assert!(
            check(
                &path,
                "good.example",
                22,
                &certificate(&ca, "good.example", 1)
            )
            .is_err()
        );
        let other = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        assert!(
            check(
                &path,
                "good.example",
                22,
                &certificate(&other, "good.example", u64::MAX)
            )
            .is_err()
        );
        std::fs::write(
            &path,
            format!(
                "{authority}@revoked *.example {}\n",
                ca.public_key().to_openssh().unwrap()
            ),
        )
        .unwrap();
        assert!(
            check(
                &path,
                "good.example",
                22,
                &certificate(&ca, "good.example", u64::MAX)
            )
            .is_err()
        );
    }
}
