//! The notebook under `analytics/` and `docs/telemetry.md` describe the same telemetry: a
//! metric or event the notebook queries must be one the docs list.

use std::path::PathBuf;

use serde_json::Value;

fn repo_file(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

fn sql_of(cells: &[Value], out: &mut Vec<String>) {
    for cell in cells {
        if let Some(sql) = cell.get("sql").and_then(Value::as_str) {
            out.push(sql.to_string());
        }
        if let Some(children) = cell.get("children").and_then(Value::as_array) {
            sql_of(children, out);
        }
    }
}

/// The text between each `opening` and the next single quote.
fn quoted_after<'a>(text: &'a str, opening: &str) -> Vec<&'a str> {
    text.match_indices(opening)
        .filter_map(|(at, _)| {
            let rest = &text[at + opening.len()..];
            rest.find('\'').map(|end| &rest[..end])
        })
        .collect()
}

#[test]
fn the_notebook_is_a_screen_file_whose_queries_name_only_documented_telemetry() {
    let path = repo_file("analytics/notebooks/fontaine-overview.json");
    let notebook: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(notebook["name"], "fontaine-overview");
    assert_eq!(notebook["screen_type"], "notebook");
    assert_eq!(notebook["config"]["timeRangeFrom"], "now-7d");
    assert_eq!(notebook["config"]["timeRangeTo"], "now");

    let mut queries = Vec::new();
    sql_of(
        notebook["config"]["cells"].as_array().unwrap(),
        &mut queries,
    );
    assert!(!queries.is_empty());
    let docs = std::fs::read_to_string(repo_file("docs/telemetry.md")).unwrap();

    let mut metrics = 0;
    let mut events = 0;
    for sql in &queries {
        for name in quoted_after(sql, "'fontaine.") {
            metrics += 1;
            let full = format!("fontaine.{name}");
            assert!(
                docs.contains(&format!("`{full}`")),
                "{full} is not in docs/telemetry.md"
            );
        }
        for name in quoted_after(sql, "split_part(msg, ' ', 1) = '") {
            events += 1;
            assert!(
                docs.contains(&format!("`{name}`")),
                "{name} is not in docs/telemetry.md"
            );
        }
        assert!(
            !sql.contains("otel_spans") && !sql.contains("async_events"),
            "span tables differ between the backends: {sql}"
        );
    }
    assert!(
        metrics > 0 && events > 0,
        "the checks found nothing to check"
    );
}
