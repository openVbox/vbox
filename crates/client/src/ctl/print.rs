//! Human-readable rendering of successful RPC replies.

use vbox_proto::RpcOk;

pub(super) fn print_ok(ok: &RpcOk) {
    for line in format_ok(ok) {
        println!("{line}");
    }
}

/// Pure formatter: turn an `RpcOk` into the user-facing lines `print_ok`
/// would emit. Splitting this out lets tests assert the exact wording an
/// operator sees in their shell without redirecting stdout.
fn format_ok(ok: &RpcOk) -> Vec<String> {
    match ok {
        RpcOk::Status(s) => {
            let mut lines = Vec::with_capacity(2 + s.instances.len());
            lines.push(format!("daemon_pid={}", s.daemon_pid));
            lines.push(format!("instances={}", s.instances.len()));
            for inst in &s.instances {
                let apps = if inst.app_pids.is_empty() {
                    "-".to_owned()
                } else {
                    inst.app_pids
                        .iter()
                        .map(u32::to_string)
                        .collect::<Vec<_>>()
                        .join(",")
                };
                lines.push(format!(
                    "  {} port={} server_pid={} apps=[{}]",
                    inst.instance, inst.port, inst.server_pid, apps
                ));
            }
            lines
        }
        RpcOk::InstanceStarted { pid } => vec![format!("instance_started pid={pid}")],
        RpcOk::InstanceStopped => vec!["instance_stopped".to_owned()],
        RpcOk::AppLaunched { pid } => vec![format!("app_launched pid={pid}")],
        RpcOk::SocketReady => vec!["socket_ready".to_owned()],
        RpcOk::DataPlanePrepared(bundle) => {
            let mut lines = vec![
                format!("tcp_addr={}", bundle.tcp_addr),
                format!(
                    "quic_addr={}",
                    bundle
                        .quic_addr
                        .map(|addr| addr.to_string())
                        .unwrap_or_else(|| "-".to_owned())
                ),
                format!("session_token={}", bundle.session_token),
                format!(
                    "quic_server_cert_sha256={}",
                    bundle.quic_server_cert_sha256.as_deref().unwrap_or("-")
                ),
            ];
            lines.push(format!(
                "capabilities=reliable_streams:{} datagrams:{} multi_app_channels:{}",
                bundle.capabilities.reliable_streams,
                bundle.capabilities.datagrams,
                bundle.capabilities.multi_app_channels
            ));
            lines
        }
        // Authenticated never reaches the user-visible call path; round-trip
        // hides it inside call(). Keeps the match exhaustive.
        RpcOk::Authenticated => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vbox_proto::{InstanceSummary, StatusReply};

    // Story: each successful RPC reply renders into a specific shape the
    // operator depends on when running `vbox ctl status` and friends. The
    // tests pin one line of output per variant, including the multi-line
    // Status block.

    #[test]
    fn instance_started_renders_pid() {
        let lines = format_ok(&RpcOk::InstanceStarted { pid: 4242 });
        assert_eq!(lines, vec!["instance_started pid=4242".to_owned()]);
    }

    #[test]
    fn instance_stopped_is_a_single_token_line() {
        let lines = format_ok(&RpcOk::InstanceStopped);
        assert_eq!(lines, vec!["instance_stopped".to_owned()]);
    }

    #[test]
    fn app_launched_renders_pid() {
        let lines = format_ok(&RpcOk::AppLaunched { pid: 100_000 });
        assert_eq!(lines, vec!["app_launched pid=100000".to_owned()]);
    }

    #[test]
    fn socket_ready_renders_one_marker_line() {
        assert_eq!(
            format_ok(&RpcOk::SocketReady),
            vec!["socket_ready".to_owned()]
        );
    }

    #[test]
    fn authenticated_produces_no_output() {
        // Authenticated is an internal handshake step the user shouldn't
        // see — print_ok must stay silent for it.
        assert!(format_ok(&RpcOk::Authenticated).is_empty());
    }

    #[test]
    fn status_with_no_instances_shows_zero_count() {
        let lines = format_ok(&RpcOk::Status(StatusReply {
            daemon_pid: 1234,
            instances: vec![],
        }));
        assert_eq!(
            lines,
            vec!["daemon_pid=1234".to_owned(), "instances=0".to_owned()]
        );
    }

    #[test]
    fn status_with_instances_lists_each_with_apps_or_dash() {
        // One instance with apps + one with none. The dash placeholder is
        // a deliberate UI choice — never print an empty `apps=[]`.
        let lines = format_ok(&RpcOk::Status(StatusReply {
            daemon_pid: 1234,
            instances: vec![
                InstanceSummary {
                    instance: "dev".into(),
                    port: 5710,
                    server_pid: 5000,
                    app_pids: vec![5001, 5002],
                },
                InstanceSummary {
                    instance: "scratch".into(),
                    port: 5712,
                    server_pid: 6000,
                    app_pids: vec![],
                },
            ],
        }));
        assert_eq!(
            lines,
            vec![
                "daemon_pid=1234".to_owned(),
                "instances=2".to_owned(),
                "  dev port=5710 server_pid=5000 apps=[5001,5002]".to_owned(),
                "  scratch port=5712 server_pid=6000 apps=[-]".to_owned(),
            ]
        );
    }
}
