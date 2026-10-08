use base64::{Engine, engine::general_purpose::STANDARD};
use opsssh_ssh_management::*;
use serde_json::json;

const KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEB";
fn registry(bytes: &[u8]) -> Registry {
    Registry::from_document("deploy", "/home/deploy/.ssh/authorized_keys", 1000, bytes).unwrap()
}

#[test]
fn restricted_edits_preserve_unrecognized_bytes_and_line_endings() {
    let source = format!("# keep this\r\ncommand=\"echo hello world\",restrict {KEY} old name\r\n");
    let mut bytes = source.as_bytes().to_vec();
    bytes.extend_from_slice(b"unknown \xff document\n\n");
    let document = registry(&bytes);
    assert_eq!(document.entries.len(), 1);
    assert_eq!(document.unrecognized, 1);
    let entry = &document.entries[0];
    assert_eq!(entry.restrictions, "command=\"echo hello world\",restrict");
    let edited = document.change(entry, Some("new name"), None).unwrap();
    assert_eq!(
        edited,
        [
            source.replace("old name", "new name").as_bytes(),
            b"unknown \xff document\n\n"
        ]
        .concat()
    );
    assert_eq!(
        document.change(entry, None, None).unwrap(),
        b"# keep this\r\nunknown \xff document\n\n"
    );
}

#[test]
fn duplicate_rows_are_removed_by_identity_not_fingerprint_alone() {
    let source = format!("{KEY} first\nrestrict {KEY} second");
    let document = registry(source.as_bytes());
    assert_eq!(
        document.change(&document.entries[1], None, None).unwrap(),
        format!("{KEY} first\n").as_bytes()
    );
    assert!(document.add(KEY).is_err());
}

#[test]
fn active_login_key_and_stale_rows_are_protected() {
    let document = registry(format!("{KEY} workstation\n").as_bytes());
    let key = &document.entries[0];
    assert!(document.change(key, None, Some(&key.fingerprint)).is_err());
    assert!(
        document
            .change(key, Some("name"), Some(&key.fingerprint))
            .is_err()
    );
    let mut stale = key.clone();
    stale.line = 5;
    assert!(document.change(&stale, None, None).is_err());
    assert!(document.change(key, Some("name\nnew line"), None).is_err());
}

#[test]
fn public_key_import_rejects_private_multiline_and_restricted_input() {
    assert!(public_key(KEY).is_ok());
    for bad in [
        "-----BEGIN OPENSSH PRIVATE KEY-----",
        "ssh-ed25519 invalid",
        &format!("{KEY}\n{KEY}"),
        &format!("restrict {KEY}"),
    ] {
        assert!(public_key(bad).is_err(), "{bad}");
    }
    let added = registry(b"# no final newline")
        .add(&format!("{KEY} workstation\n"))
        .unwrap();
    assert_eq!(
        added,
        format!("# no final newline\n{KEY} workstation\n").as_bytes()
    );
}

#[test]
fn revisions_and_document_limits_are_checked() {
    let bytes = format!("{KEY}\n").into_bytes();
    let bad = json!({"account":"deploy","path":"unused","uid":1000,"content":STANDARD.encode(&bytes),"revision":"stale"});
    assert!(Registry::from_reply(&bad).is_err());
    assert!(Registry::from_document("a", "b", 1, &vec![b'x'; MAX_DOCUMENT + 1]).is_err());
    let document = registry(&vec![b'#'; MAX_DOCUMENT]);
    assert!(document.add(KEY).is_err());
}

#[test]
fn command_separates_redacted_credentials_from_shell_source() {
    let request = request(
        json!({"op":"read","account":"deploy; touch bad"}),
        true,
        Some("secret-marker"),
    )
    .unwrap();
    assert!(request.command.starts_with("sudo -S -p '' -- python3 -c '"));
    assert!(!request.command.contains("secret-marker"));
    assert!(!request.command.contains("deploy; touch bad"));
    assert!(!format!("{request:?}").contains("secret-marker"));
    let stdin = request.stdin.unwrap();
    assert!(
        stdin
            .expose()
            .starts_with("secret-marker\nOPSSSH-MANAGEMENT-1\n")
    );
    assert!(opsssh_ssh_management::request(json!({}), true, Some("bad\npassword")).is_err());
    assert!(opsssh_ssh_management::request(json!({}), true, Some(&"x".repeat(8193))).is_err());
}

#[test]
fn conservative_usernames_reject_shell_flags_and_reserved_formats() {
    for name in ["deploy", "_service", "build-runner1"] {
        assert!(username(name));
    }
    for name in ["", "-root", "Root", "a:b", "a b", "$(id)", "a\n", "user$"] {
        assert!(!username(name));
    }
}
