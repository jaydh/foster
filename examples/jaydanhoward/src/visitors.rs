//! Real visitor logging against a real (local, throwaway) Postgres —
//! `sqlx::PgPool::connect` + embedded migrations, same schema shape and
//! connection setup as the real site's `db.rs`/`0001_create_visitors.sql`.
//!
//! One deliberate adaptation from production: the real site skips logging
//! (and skips the geo lookup) for private/loopback IPs, since in production
//! every real hit is already a public IP and internal traffic is noise. This
//! demo is *only* ever hit from localhost, so keeping that same gate would
//! mean the table never accumulates a single row — nothing to verify. So
//! this always logs a real row (real IP, real path, real timestamp); it
//! only skips the *geo* lookup for private IPs, matching the real site's
//! actual reason for the check (ip-api.com can't geolocate a private IP
//! anyway) without losing the ability to prove real accumulation locally.

use axum::extract::{Request, State};
use axum::http::HeaderMap;
use axum::middleware::Next;
use axum::response::Response;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::PgPool;
use std::net::IpAddr;

pub async fn create_pool(database_url: &str) -> Result<PgPool, sqlx::Error> {
    let pool = PgPool::connect(database_url).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}

fn should_log(path: &str) -> bool {
    if path == "/health_check" || path.starts_with("/pkg/") {
        return false;
    }
    if let Some(ext) = path.rsplit('.').next() {
        if path.contains('.') {
            return !matches!(
                ext,
                "wasm" | "js" | "css" | "woff2" | "woff" | "ttf" | "eot" | "otf" | "png" | "jpg"
                    | "jpeg" | "gif" | "svg" | "ico" | "webp" | "map"
            );
        }
    }
    true
}

fn is_private_ip(ip_str: &str) -> bool {
    match ip_str.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        Ok(IpAddr::V6(ip)) => ip.is_loopback(),
        Err(_) => true,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeoResponse {
    status: String,
    country: Option<String>,
    country_code: Option<String>,
    city: Option<String>,
    isp: Option<String>,
}

async fn record_visit(pool: PgPool, ip: String, path: String) {
    let is_private = is_private_ip(&ip);

    let (country, country_code, city, isp) = if !is_private {
        let geo: Option<GeoResponse> = async {
            reqwest::Client::new()
                .get(format!(
                    "http://ip-api.com/json/{ip}?fields=status,country,countryCode,city,isp"
                ))
                .timeout(std::time::Duration::from_secs(3))
                .send()
                .await
                .ok()?
                .json::<GeoResponse>()
                .await
                .ok()
        }
        .await
        .filter(|g| g.status == "success");

        match geo {
            Some(g) => (g.country, g.country_code, g.city, g.isp),
            None => (None, None, None, None),
        }
    } else {
        (None, None, None, None)
    };

    let _ = sqlx::query(
        "INSERT INTO visitors (ip, country, country_code, city, isp, path) VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(&ip)
    .bind(&country)
    .bind(&country_code)
    .bind(&city)
    .bind(&isp)
    .bind(&path)
    .execute(&pool)
    .await;
}

pub async fn visitor_logger(
    State(pool): State<PgPool>,
    headers: HeaderMap,
    req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path().to_string();

    if should_log(&path) {
        let ip = headers
            .get("x-real-ip")
            .or_else(|| headers.get("x-forwarded-for"))
            .and_then(|v| v.to_str().ok())
            .map(|s| s.split(',').next().unwrap_or(s).trim().to_string())
            .unwrap_or_else(|| "127.0.0.1".to_string());

        let pool = pool.clone();
        tokio::spawn(async move {
            record_visit(pool, ip, path).await;
        });
    }

    next.run(req).await
}

pub fn fetch_visitor_stats(pool: &PgPool) -> Value {
    let result: Result<(i64, i64, i64, Vec<Value>), sqlx::Error> =
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                let totals: (i64, i64) = sqlx::query_as(
                    "SELECT COUNT(*)::bigint, COUNT(DISTINCT ip)::bigint FROM visitors",
                )
                .fetch_one(pool)
                .await?;

                let unique_countries: (i64,) = sqlx::query_as(
                    "SELECT COUNT(DISTINCT country_code)::bigint FROM visitors WHERE country_code IS NOT NULL",
                )
                .fetch_one(pool)
                .await?;

                let rows: Vec<(String, Option<String>, Option<String>, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
                    "SELECT path, country, city, visited_at FROM visitors ORDER BY visited_at DESC LIMIT 10",
                )
                .fetch_all(pool)
                .await?;

                let recent: Vec<Value> = rows
                    .into_iter()
                    .map(|(path, country, city, visited_at)| {
                        let mins_ago = (chrono::Utc::now() - visited_at).num_minutes();
                        json!({
                            "path": path,
                            "location": match (city, country) {
                                (Some(c), Some(co)) => format!("{c}, {co}"),
                                (None, Some(co)) => co,
                                (Some(c), None) => c,
                                (None, None) => "local".to_string(),
                            },
                            "minutes_ago": mins_ago,
                        })
                    })
                    .collect();

                Ok((totals.0, totals.1, unique_countries.0, recent))
            })
        });

    match result {
        Ok((total_visits, unique_ips, unique_countries, recent_visits)) => json!({
            "connected": true,
            "total_visits": total_visits,
            "unique_ips": unique_ips,
            "unique_countries": unique_countries,
            "recent_visits": recent_visits,
        }),
        Err(e) => json!({
            "connected": false,
            "error": e.to_string(),
            "total_visits": 0,
            "unique_ips": 0,
            "unique_countries": 0,
            "recent_visits": [],
        }),
    }
}
