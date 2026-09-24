use hmux_core::token;
use std::{
    fs,
    os::unix::fs::{symlink, DirBuilderExt, PermissionsExt},
    path::PathBuf,
};

const TOKEN: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn bounded_private_connector_token_read_rejects_aliases_and_invalid_bytes() {
    let root = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join(format!("hmux-token-{}", std::process::id()));
    fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    let fixture = Fixture(root);
    let path = fixture.0.join("connector.token");
    fs::write(&path, format!(" \n{TOKEN}\r\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(token::load(&path).unwrap(), TOKEN);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(token::load(&path).is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let alias = fixture.0.join("alias");
    symlink(&path, &alias).unwrap();
    assert!(token::load(&alias).is_err());
    fs::remove_file(&alias).unwrap();
    fs::hard_link(&path, &alias).unwrap();
    assert!(token::load(&path).is_err());
    fs::remove_file(&alias).unwrap();
    let linked_parent = fixture.0.join("parent-alias");
    symlink(&fixture.0, &linked_parent).unwrap();
    assert!(token::load(&linked_parent.join("connector.token")).is_err());
    for invalid in [
        Vec::new(),
        b"secret-MUST-NOT-LEAK".to_vec(),
        vec![0xff; 43],
        vec![b'A'; 257],
        format!("{TOKEN}=").into_bytes(),
        format!("{}B", &TOKEN[..42]).into_bytes(),
    ] {
        fs::write(&path, invalid).unwrap();
        assert_eq!(
            token::load(&path).unwrap_err().to_string(),
            "connector token unavailable or invalid"
        );
    }
    assert!(token::load(&fixture.0).is_err());
    assert!(token::load(&fixture.0.join("missing")).is_err());
}
