//! `fontaine models`: prints the models each configured account offers. Nothing here starts a
//! session; it lists accounts the way the `/model` picker does.

use std::process::ExitCode;

use fontaine_core::config::Config;
use fontaine_core::dirs::Dirs;
use fontaine_core::model_report::ModelReport;
use fontaine_core::models::{
    self, LISTING_TIMEOUT, ModelRef, ResolveParams, Switcher, known_accounts, validated_models,
};
use fontaine_tools::error_chain::chain;

use crate::cli::{Cli, ModelsArgs, ReportFormat};
use crate::{Host, print_warnings, shared_env, timeout, usage};

pub fn run(cli: &Cli, args: &ModelsArgs, config: &Config, dirs: &Dirs, host: &Host) -> ExitCode {
    if config.accounts.is_empty() {
        return usage(known_accounts(config));
    }
    if let Some(name) = &args.account
        && !config.accounts.contains_key(name)
    {
        return usage(format!(
            "unknown account `{name}`; {}",
            known_accounts(config)
        ));
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("fontaine: cannot start the async runtime: {e}");
            return ExitCode::from(1);
        }
    };
    let report = runtime.block_on(build_report(cli, args, config, dirs, host));
    match args.format.format {
        ReportFormat::Text => print!("{}", report.render_text()),
        ReportFormat::Json => println!("{}", report.to_json()),
    }
    if report.all_failed() {
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

async fn build_report(
    cli: &Cli,
    args: &ModelsArgs,
    config: &Config,
    dirs: &Dirs,
    host: &Host,
) -> ModelReport {
    let resolved = timeout(
        LISTING_TIMEOUT,
        models::resolve(&ResolveParams {
            config,
            data_dir: &dirs.data,
            env: shared_env(host),
            model_override: cli.model.as_deref(),
            session_id: None,
        }),
    )
    .await;
    let switcher = Switcher::new(config.clone(), dirs.data.clone(), shared_env(host), None);
    let only = args.account.clone().map(|name| vec![name]);
    let (listings, main, guardian) = match &resolved {
        Ok(resolved) => {
            print_warnings(&resolved.notices);
            let listings = switcher
                .listings(
                    Some((&resolved.main.reference, &resolved.main_listing)),
                    only.as_deref(),
                )
                .await;
            let guardian: Option<ModelRef> =
                resolved.guardian.as_ref().map(|g| g.reference().clone());
            (listings, Some(resolved.main.reference.clone()), guardian)
        }
        Err(e) => {
            eprintln!(
                "fontaine: warning: no roles are marked, the main model did not resolve: {}",
                chain(&**e)
            );
            (switcher.listings(None, only.as_deref()).await, None, None)
        }
    };
    ModelReport::build(
        &listings,
        main.as_ref(),
        guardian.as_ref(),
        &validated_models(),
    )
}
