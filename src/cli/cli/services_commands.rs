use super::{AnyhowResult, ServiceAction};

// `print_services_table`, `print_service_detail` and `service_row_state` live
// here. `cli.rs` imports only `cmd_services_impl`, and does so privately — there
// is no `pub(crate) use` re-export. `cli_test.rs` reaches `service_row_state`
// through an explicit `use super::services_commands::service_row_state;`, so a
// re-export is neither present nor needed.

/// Only successful HTTP readiness is "up"; TCP alone may be another process
/// or a model server that is still loading.
pub(super) fn service_row_state(st: &crate::services::ServiceStatus) -> &'static str {
    match (st.http_ready, st.pid.is_some()) {
        (true, _) => "up",
        (false, true) => "starting",
        (false, false) => "down",
    }
}

/// Print the `services list` table (or the empty-config hint).
pub(super) fn print_services_table(
    services: &std::collections::BTreeMap<String, crate::config::ServiceConfig>,
) {
    if services.is_empty() {
        println!("No [services] sections configured.");
        println!("Add one to ~/.leindex/config/leindex.toml, e.g.:");
        println!();
        println!("[services.laya]");
        println!("model = \"/path/to/model.gguf\"");
        println!("port = 8181");
        return;
    }
    println!(
        "{:<14} {:<7} {:<6} {:<24} {:<10} MODEL",
        "SERVICE", "STATE", "PID", "ENDPOINT", "AUTO-START"
    );
    for (name, cfg) in services {
        let st = crate::services::status(name, cfg).ok();
        let st_ref = st.as_ref();
        println!(
            "{:<14} {:<7} {:<6} {:<24} {:<10} {}",
            name,
            st_ref.map(service_row_state).unwrap_or("error"),
            st_ref
                .and_then(|s| s.pid)
                .map(|p| p.to_string())
                .unwrap_or_else(|| "-".into()),
            st_ref.map(|s| s.endpoint.as_str()).unwrap_or("-"),
            st_ref
                .map(|s| if s.auto_start { "yes" } else { "no" })
                .unwrap_or("-"),
            st_ref
                .and_then(|s| s.served_model.clone())
                .unwrap_or_else(|| "-".into())
        );
    }
}

/// Print the `services status <name>` detail block.
pub(super) fn print_service_detail(name: &str, st: &crate::services::ServiceStatus) {
    println!("service:     {}", st.name);
    println!(
        "state:       {}",
        match service_row_state(st) {
            "up" => "up (HTTP health ready)",
            "starting" => "starting (process live, HTTP health not ready)",
            _ => "down",
        }
    );
    println!(
        "pid:         {}",
        st.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into())
    );
    println!("endpoint:    {}", st.endpoint);
    println!(
        "model:       {}",
        st.model.as_deref().unwrap_or("(none — cannot start)")
    );
    println!("served name: {}", st.served_model.as_deref().unwrap_or("-"));
    println!("auto-start:  {}", if st.auto_start { "yes" } else { "no" });
    println!("log:         ~/.leindex/logs/services/{name}.log");
}

/// `leindex services` — manage `[services.<name>]` model servers.
pub(super) async fn cmd_services_impl(action: Option<ServiceAction>) -> AnyhowResult<()> {
    use crate::services as svc;
    let config = crate::config::LeIndexConfig::load_cached();

    let resolve = |name: &str| -> AnyhowResult<&crate::config::ServiceConfig> {
        config
            .services
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("no [services.{name}] section in the config"))
    };

    match action {
        None | Some(ServiceAction::List) => {
            print_services_table(&config.services);
            Ok(())
        }
        Some(ServiceAction::Status { name }) => {
            let cfg = resolve(&name)?;
            print_service_detail(&name, &svc::status(&name, cfg)?);
            Ok(())
        }
        Some(ServiceAction::Start { name }) => {
            let cfg = resolve(&name)?;
            if svc::start(&name, cfg)? {
                println!("started {name} ({} ready)", cfg.base_url());
            } else {
                println!("{name} already up ({})", cfg.base_url());
            }
            Ok(())
        }
        Some(ServiceAction::Stop { name }) => {
            let cfg = resolve(&name)?;
            match svc::stop(&name, cfg)? {
                svc::StopOutcome::Stopped(pid) => println!("stopped {name} (pid {pid})"),
                svc::StopOutcome::AlreadyDown => println!("{name} already down"),
                svc::StopOutcome::StalePidfileRemoved => {
                    println!("{name}: stale pidfile removed (was not running)")
                }
            }
            Ok(())
        }
        Some(ServiceAction::Restart { name }) => {
            let cfg = resolve(&name)?;
            svc::restart(&name, cfg)?;
            println!("restarted {name} ({})", cfg.base_url());
            Ok(())
        }
    }
}
