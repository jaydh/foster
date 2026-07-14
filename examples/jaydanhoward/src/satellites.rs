//! Real TLE data from CelesTrak (confirmed publicly reachable — a plain
//! public GET, no auth, the "CI runner gets 403'd" issue in git history is
//! CI-IP-specific) + real orbital mechanics via the `sgp4` crate (the same
//! crate and propagation the real site's `satellite_calculations.rs` uses)
//! computed once at startup.
//!
//! Deliberately not a live 3D propagation loop: this is a 2D canvas, and
//! re-propagating on every animation frame would need either a request per
//! frame or an SGP4 implementation in JS. Instead the real angular position
//! *right now* and the real angular velocity (from the TLE's mean motion)
//! are computed once, real data, and `static/satellites.js` does simple
//! constant-angular-velocity extrapolation from there — same shape as
//! "life"/"conjunction": Foster/the server hands off real data once, a
//! hand-rolled client loop animates it continuously.

use serde_json::{json, Value};
use sgp4::{Constants, Elements};

const EARTH_RADIUS_KM: f64 = 6371.0;

struct RealSat {
    angle_rad: f64,
    angular_velocity_rad_per_sec: f64,
    altitude_km: f64,
    position_km: [f64; 3],
}

fn fetch_tle_group(group: &str) -> Result<Vec<(String, String, String)>, String> {
    let url = format!("https://celestrak.org/NORAD/elements/gp.php?GROUP={group}&FORMAT=tle");
    let body = reqwest::blocking::Client::builder()
        .user_agent("Mozilla/5.0 (compatible; foster-jaydanhoward-demo)")
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?
        .get(&url)
        .send()
        .map_err(|e| e.to_string())?
        .text()
        .map_err(|e| e.to_string())?;

    let lines: Vec<&str> = body.lines().collect();
    Ok(lines
        .chunks(3)
        .filter(|c| c.len() == 3)
        .map(|c| (c[0].trim().to_string(), c[1].to_string(), c[2].to_string()))
        .collect())
}

fn propagate_now(name: &str, line1: &str, line2: &str) -> Option<RealSat> {
    let elements = Elements::from_tle(Some(name.to_string()), line1.as_bytes(), line2.as_bytes()).ok()?;
    let mean_motion_revs_per_day = elements.mean_motion;
    let constants = Constants::from_elements(&elements).ok()?;
    let prediction = constants.propagate(sgp4::MinutesSinceEpoch(0.0)).ok()?;

    let x = prediction.position[0];
    let y = prediction.position[1];
    let z = prediction.position[2];
    let distance_from_center = (x * x + y * y + z * z).sqrt();

    Some(RealSat {
        angle_rad: y.atan2(x),
        angular_velocity_rad_per_sec: mean_motion_revs_per_day * std::f64::consts::TAU / 86_400.0,
        altitude_km: distance_from_center - EARTH_RADIUS_KM,
        position_km: [x, y, z],
    })
}

/// Real name + real ECI position (km, right now) for every satellite in a
/// CelesTrak group — reused by conjunction.rs for real pairwise-distance
/// screening instead of duplicating the TLE fetch + sgp4 propagation.
pub fn fetch_group_positions(group: &str) -> Vec<(String, [f64; 3])> {
    tokio::task::block_in_place(|| {
        let tles = fetch_tle_group(group).unwrap_or_default();
        tles.iter()
            .filter_map(|(name, l1, l2)| {
                let sat = propagate_now(name, l1, l2)?;
                Some((name.clone(), sat.position_km))
            })
            .collect()
    })
}

/// Fetches real TLEs for representative LEO/MEO/GEO groups and computes
/// each satellite's real angle-right-now + real angular velocity. Blocking
/// (network + CPU), called via `block_in_place` at startup and from the
/// "refresh" reducer, same pattern as cluster.rs/visitors.rs.
pub fn fetch_real_satellites() -> Value {
    tokio::task::block_in_place(fetch_real_satellites_blocking)
}

fn fetch_real_satellites_blocking() -> Value {
    let groups = [("stations", "leo"), ("gps-ops", "meo"), ("geo", "geo")];

    let mut by_ring: std::collections::HashMap<&str, Vec<Value>> =
        std::collections::HashMap::new();

    for (group, ring) in groups {
        let tles = match fetch_tle_group(group) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let sats: Vec<Value> = tles
            .iter()
            .filter_map(|(name, l1, l2)| {
                let sat = propagate_now(name, l1, l2)?;
                Some(json!({
                    "name": name,
                    "angle": sat.angle_rad,
                    "angular_velocity": sat.angular_velocity_rad_per_sec,
                    "altitude_km": sat.altitude_km,
                }))
            })
            .collect();
        by_ring.insert(ring, sats);
    }

    let leo = by_ring.remove("leo").unwrap_or_default();
    let meo = by_ring.remove("meo").unwrap_or_default();
    let geo = by_ring.remove("geo").unwrap_or_default();

    json!({
        "fetched": true,
        "leo_count": leo.len(),
        "meo_count": meo.len(),
        "geo_count": geo.len(),
        "leo": leo,
        "meo": meo,
        "geo": geo,
    })
}
