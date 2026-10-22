mod common;

use std::ffi::OsString;

use common::World;
use fontaine_core::tools::scrubbed_env;

fn names(list: &[OsString]) -> Vec<String> {
    list.iter()
        .map(|n| n.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn the_shell_drops_telemetry_credentials_account_keys_and_the_scanner_token() {
    let w = World::new();
    w.user_file(
        "[accounts.work]\nkind = \"anthropic\"\napi_key_env = \"WORK_KEY\"\n\n[accounts.home]\nkind = \"openai\"\napi_key_env = \"HOME_KEY\"\n\n[accounts.local]\nkind = \"ollama\"\n\n[telemetry]\napi_key_env = \"TELEMETRY_KEY\"\n\n[guardian]\nscanner = \"pg2\"\nscanner_hf_token_env = \"HF_TOKEN_NAME\"\n",
    );
    let config = w.load().unwrap();
    let present = [
        "PATH",
        "HOME",
        "MICROMEGAS_INGESTION_API_KEY",
        "MICROMEGAS_TELEMETRY_URL",
        "OTEL_EXPORTER_OTLP_HEADERS",
        "OTEL_EXPORTER_OTLP_TRACES_HEADERS",
        "OTEL_SERVICE_NAME",
    ]
    .map(OsString::from);
    let scrubbed = names(&scrubbed_env(&config, present));
    for name in [
        "MICROMEGAS_INGESTION_API_KEY",
        "MICROMEGAS_TELEMETRY_URL",
        "OTEL_EXPORTER_OTLP_HEADERS",
        "OTEL_EXPORTER_OTLP_TRACES_HEADERS",
        "WORK_KEY",
        "HOME_KEY",
        "TELEMETRY_KEY",
        "HF_TOKEN_NAME",
    ] {
        assert!(
            scrubbed.contains(&name.to_string()),
            "{name} in {scrubbed:?}"
        );
    }
    for name in ["PATH", "HOME", "OTEL_SERVICE_NAME"] {
        assert!(
            !scrubbed.contains(&name.to_string()),
            "{name} in {scrubbed:?}"
        );
    }
}

#[test]
fn the_otlp_header_variable_is_always_dropped_even_when_unset() {
    let w = World::new();
    let config = w.load().unwrap();
    let scrubbed = names(&scrubbed_env(&config, Vec::<OsString>::new()));
    assert!(scrubbed.contains(&"OTEL_EXPORTER_OTLP_HEADERS".to_string()));
}
