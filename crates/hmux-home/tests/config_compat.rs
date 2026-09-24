use hmux_home::config::{load_home, load_inventory, HomeConfig};
use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct SyntheticDir(PathBuf);
impl SyntheticDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "hmux-rust-config-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn file(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, contents).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        path
    }
}
impl Drop for SyntheticDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/config-v1"
);

#[test]
fn go_read_only_config_oracle_matches() {
    let oracle: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/config-v1/go-oracle.json"
    ))
    .unwrap();
    let home = Path::new("/synthetic/hmux-user");
    let default = load_home(Some(&home.join("missing-home.toml")), home).unwrap();
    assert_eq!(
        serde_json::json!({
            "schema_version": default.schema_version,
            "role": default.role,
            "inventory_path": default.inventory_path.to_string_lossy().replace(home.to_str().unwrap(), "{HOME}"),
            "state_dir": default.state_dir.to_string_lossy().replace(home.to_str().unwrap(), "{HOME}"),
        }),
        oracle["home-default"]
    );
    for name in ["home-current", "home-legacy"] {
        let config = load_home(
            Some(&Path::new(FIXTURES).join(format!("{name}.toml"))),
            home,
        )
        .unwrap();
        let value = json!({
            "schema_version": config.schema_version,
            "role": config.role,
            "inventory_path": config.inventory_path.to_string_lossy().replace(home.to_str().unwrap(), "{HOME}"),
            "state_dir": config.state_dir.to_string_lossy().replace(home.to_str().unwrap(), "{HOME}"),
        });
        assert_eq!(value, oracle[name]);
    }
    let inventory = load_inventory(&Path::new(FIXTURES).join("inventory.toml")).unwrap();
    assert_eq!(
        serde_json::to_value(inventory).unwrap(),
        oracle["inventory"]
    );
}

#[test]
fn fallback_and_invalid_current_file_behavior() {
    let temp = SyntheticDir::new();
    let home = &temp.0;
    let config_dir = home.join(".config/hmux");
    fs::create_dir_all(&config_dir).unwrap();
    assert_eq!(
        load_home(None, home).unwrap(),
        HomeConfig::default_for(home)
    );
    fs::write(
        config_dir.join("client.toml"),
        "schema_version = 1\nstate_dir = \"~/legacy\"\n",
    )
    .unwrap();
    let config = load_home(None, home).unwrap();
    assert_eq!(config.state_dir, home.join("legacy"));
    fs::write(
        config_dir.join("home.toml"),
        "schema_version = 1\nstate_dir = \"~/current\"\n",
    )
    .unwrap();
    assert_eq!(
        load_home(None, home).unwrap().state_dir,
        home.join("current")
    );
    fs::write(
        config_dir.join("home.toml"),
        "schema_version = 1\nstate_dir = \"/\"\n",
    )
    .unwrap();
    assert!(load_home(None, home).is_err());
}

#[test]
fn strict_fields_types_paths_and_file_safety() {
    let temp = SyntheticDir::new();
    let home = Path::new("/synthetic/hmux-user");
    for raw in [
        "schema_version = 1\nunknown = true\n",
        "schema_version = 1\nrole = \"remote\"\n",
        "schema_version = 2\n",
        "schema_version = 1\ntimeout_seconds = \"10\"\n",
        "schema_version = 1\nstate_dir = \"relative\"\n",
        "schema_version = 1\ninventory_path = \"/\"\n",
    ] {
        let path = temp.file("bad.toml", raw);
        assert!(load_home(Some(&path), home).is_err(), "accepted {raw}");
    }
    let target = temp.file("target.toml", "schema_version = 1\n");
    let link = temp.0.join("link.toml");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(load_home(Some(&link), home).is_err());
    fs::set_permissions(&target, fs::Permissions::from_mode(0o622)).unwrap();
    assert!(load_home(Some(&target), home).is_err());
    let empty = temp.file("empty.toml", "");
    assert!(load_home(Some(&empty), home).is_err());
    let large = temp.file("large.toml", &"a".repeat(1024 * 1024 + 1));
    assert!(load_home(Some(&large), home).is_err());
}

#[test]
fn inventory_rejects_unknown_legacy_fields_and_invalid_profiles() {
    let temp = SyntheticDir::new();
    let fixture = fs::read_to_string(Path::new(FIXTURES).join("inventory.toml")).unwrap();
    for raw in [
        format!("{fixture}\ninvented_option = true\n"),
        fixture.replace("port = 22", "port = \"22\""),
        fixture.replace("id = \"shell\"", "id = \"codex\""),
        fixture.replace("command = [\"sh\", \"-l\"]", "command = []"),
        fixture.replace(
            "revision = \"synthetic-v1\"",
            "revision = \"bad\\nrevision\"",
        ),
    ] {
        let path = temp.file("inventory.toml", &raw);
        assert!(load_inventory(&path).is_err(), "accepted invalid inventory");
    }
}
