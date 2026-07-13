//! Real GitOps status + backup job status, via the `kube` crate against the
//! actual homelab cluster this laptop already has kubectl access to
//! (`Client::try_default()` — in-cluster service account first, falling
//! back to the local kubeconfig, same as the real site).
//!
//! Deliberately scoped down from the real site's `cluster_stats.rs`: the
//! CPU/mem/disk/network/Ceph panel there is Prometheus-backed, and
//! Prometheus's NodePort is bound to the home LAN interface only — this
//! machine reaches the cluster over Tailscale, not the LAN, so that data
//! genuinely isn't reachable from here without additional homelab network
//! exposure. GitOps (Flux) status and backup Job status both go through the
//! k8s API server directly, which *is* reachable, so those are real.

use k8s_openapi::api::batch::v1::Job;
use kube::api::{Api, ListParams};
use kube::core::DynamicObject;
use kube::discovery::ApiResource;
use kube::Client;
use serde_json::{json, Value};

const FLUX_TYPES: &[(&str, &str, &str, &str)] = &[
    ("Kustomization", "kustomize.toolkit.fluxcd.io", "kustomizations", "v1"),
    ("HelmRelease", "helm.toolkit.fluxcd.io", "helmreleases", "v2"),
    ("GitRepository", "source.toolkit.fluxcd.io", "gitrepositories", "v1"),
    ("HelmRepository", "source.toolkit.fluxcd.io", "helmrepositories", "v1"),
    ("HelmChart", "source.toolkit.fluxcd.io", "helmcharts", "v1"),
];

// The three CronJobs the real site's cluster panel knows about by name
// (`backup_display_name` in cluster_stats.rs) — real names on the real
// homelab cluster, confirmed via `kubectl get cronjobs -n media`.
const BACKUP_JOBS: &[(&str, &str)] = &[
    ("backup-immich-photos", "immich photos"),
    ("backup-media", "media"),
    ("backup-backup-vol", "backup vol"),
];

async fn fetch_flux_status(client: &Client) -> Vec<Value> {
    let mut resources = Vec::new();

    for &(kind, group, plural, version) in FLUX_TYPES {
        let ar = ApiResource {
            group: group.to_string(),
            version: version.to_string(),
            api_version: format!("{group}/{version}"),
            kind: kind.to_string(),
            plural: plural.to_string(),
        };
        let api: Api<DynamicObject> = Api::all_with(client.clone(), &ar);
        let list = match api.list(&ListParams::default()).await {
            Ok(l) => l,
            Err(_) => continue,
        };
        for obj in list.items {
            let name = obj.metadata.name.unwrap_or_default();
            let namespace = obj.metadata.namespace.unwrap_or_default();
            let ready = obj.data["status"]["conditions"]
                .as_array()
                .and_then(|conds| conds.iter().find(|c| c["type"].as_str() == Some("Ready")))
                .and_then(|c| c["status"].as_str())
                .map(|s| s == "True")
                .unwrap_or(false);
            resources.push(json!({
                "kind": kind,
                "namespace": namespace,
                "name": name,
                "ready_icon": if ready { "\u{2713}" } else { "\u{2717}" },
            }));
        }
    }

    resources.sort_by(|a, b| {
        let ra = a["ready_icon"].as_str().unwrap_or("");
        let rb = b["ready_icon"].as_str().unwrap_or("");
        ra.cmp(rb)
            .then(a["kind"].as_str().cmp(&b["kind"].as_str()))
            .then(a["name"].as_str().cmp(&b["name"].as_str()))
    });

    resources
}

async fn fetch_backup_status(client: &Client) -> Vec<Value> {
    let jobs: Api<Job> = Api::namespaced(client.clone(), "media");
    let list = match jobs.list(&ListParams::default()).await {
        Ok(l) => l,
        Err(_) => return Vec::new(),
    };

    BACKUP_JOBS
        .iter()
        .map(|&(cronjob_name, display_name)| {
            let mut owned: Vec<_> = list
                .items
                .iter()
                .filter(|job| {
                    job.metadata
                        .owner_references
                        .as_deref()
                        .unwrap_or_default()
                        .iter()
                        .any(|r| r.name == cronjob_name)
                })
                .collect();
            owned.sort_by_key(|job| {
                job.status
                    .as_ref()
                    .and_then(|s| s.start_time.as_ref())
                    .map(|t| t.0.as_second())
                    .unwrap_or(0)
            });

            match owned.last() {
                Some(job) => {
                    let status = job.status.as_ref();
                    let (icon, label) = if status.and_then(|s| s.succeeded).unwrap_or(0) > 0 {
                        ("\u{2713}", "complete")
                    } else if status.and_then(|s| s.failed).unwrap_or(0) > 0 {
                        ("\u{2717}", "failed")
                    } else if status.and_then(|s| s.active).unwrap_or(0) > 0 {
                        ("\u{25cf}", "running")
                    } else {
                        ("?", "unknown")
                    };
                    let job_name = job.metadata.name.clone().unwrap_or_default();
                    json!({
                        "name": display_name,
                        "job_name": job_name,
                        "status_icon": icon,
                        "status_label": label,
                    })
                }
                None => json!({
                    "name": display_name,
                    "job_name": "",
                    "status_icon": "?",
                    "status_label": "no runs found",
                }),
            }
        })
        .collect()
}

/// Runs the real kube queries. Called both to build the initial context at
/// startup and from the "refresh" transition's reducer — this is genuinely
/// async work, so the reducer calls it via `block_in_place` + `block_on`
/// (Foster's `ReducerFn` is a plain sync `Fn`, no async reducer support).
pub fn fetch_cluster_status() -> Value {
    let result = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            let client = Client::try_default().await?;
            let flux = fetch_flux_status(&client).await;
            let backups = fetch_backup_status(&client).await;
            Ok::<_, kube::Error>((flux, backups))
        })
    });

    match result {
        Ok((flux, backups)) => json!({
            "connected": true,
            "flux": flux,
            "backups": backups,
        }),
        Err(e) => json!({
            "connected": false,
            "error": e.to_string(),
            "flux": [],
            "backups": [],
        }),
    }
}
