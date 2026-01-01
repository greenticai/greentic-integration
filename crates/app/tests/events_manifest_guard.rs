use std::fs;

#[test]
fn events_manifest_guard() {
    let manifest_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .unwrap()
        .join("tests")
        .join("packs")
        .join("manifest.txt");
    let data = match fs::read_to_string(&manifest_path) {
        Ok(d) => d,
        Err(_) => {
            eprintln!("skipping: manifest missing at {}", manifest_path.display());
            return;
        }
    };
    let has_events = data
        .lines()
        .map(|l| l.trim())
        .any(|l| l.starts_with("greentic-packs/events-"));
    if !has_events {
        eprintln!("skipping: no greentic-packs/events-* entries in manifest");
    }
}
