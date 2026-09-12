//! Daemon-side scan admission: single-flight coalescing plus a short-lived
//! recent-result cache.
//!
//! `CWInterface::scanForNetworksWithName:error:` is synchronous and CoreWLAN
//! owns the channel traversal, so the physical scan duration is not ours to
//! shorten. What *is* ours is how many of them we ask for. A TUI reconnect, a
//! keypress, and a couple of CLI one-shots could previously turn into four
//! full channel sweeps queued end to end behind one another.
//!
//! This is a pure state machine — no CoreWLAN, no I/O, no clock of its own —
//! so the interesting cases are ordinary unit tests. The daemon owns one and
//! feeds it `Instant`s.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::corewlan::ScannedNetwork;

/// How long a successful result stays servable.
///
/// Long enough that a reconnecting TUI and the keypress behind it share one
/// sweep; short enough that a network you just walked away from disappears
/// while you are still looking at the list. Memory-only: a result that
/// survived a daemon restart would describe a different place entirely.
pub const CACHE_LIFETIME: Duration = Duration::from_secs(5);

/// A client waiting on a scan result, identified well enough to address the
/// reply back to exactly the request that asked for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanWaiter {
    pub client_id: u64,
    pub request_id: u64,
    pub purpose: ScanPurpose,
}

/// The response a request expects once the shared scan finishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanPurpose {
    Scan,
    Diagnose,
}

/// What the daemon should do about one incoming scan request.
#[derive(Debug)]
pub enum ScanDecision {
    /// Serve this result immediately; no worker involvement at all.
    Cached(Arc<Vec<ScannedNetwork>>),
    /// A scan is already running; this request will be answered by it.
    Coalesced,
    /// Nothing is running and nothing is fresh — start a physical scan.
    Start { operation_id: u64 },
}

/// The result of a finished physical scan, addressed to everyone who waited.
#[derive(Debug)]
pub struct ScanCompletion {
    pub waiters: Vec<ScanWaiter>,
    /// Coordinator start → completion. Covers worker queueing and CoreWLAN,
    /// but not the daemon's own send.
    pub elapsed: Duration,
    /// Shared with the cache, so one sweep is one allocation no matter how
    /// many clients it answers.
    pub result: Result<Arc<Vec<ScannedNetwork>>, String>,
}

struct InFlight {
    operation_id: u64,
    started_at: Instant,
}

struct CachedScan {
    networks: Arc<Vec<ScannedNetwork>>,
    completed_at: Instant,
}

pub struct ScanCoordinator {
    in_flight: Option<InFlight>,
    waiters: Vec<ScanWaiter>,
    last_success: Option<CachedScan>,
    next_operation_id: u64,
    lifetime: Duration,
}

impl Default for ScanCoordinator {
    fn default() -> Self {
        Self::new(CACHE_LIFETIME)
    }
}

impl ScanCoordinator {
    pub fn new(lifetime: Duration) -> Self {
        Self {
            in_flight: None,
            waiters: Vec::new(),
            last_success: None,
            next_operation_id: 1,
            lifetime,
        }
    }

    /// Admit one scan request.
    pub fn request(&mut self, waiter: ScanWaiter, now: Instant) -> ScanDecision {
        if let Some(cached) = &self.last_success
            && now.duration_since(cached.completed_at) < self.lifetime
        {
            return ScanDecision::Cached(cached.networks.clone());
        }
        self.waiters.push(waiter);
        if self.in_flight.is_some() {
            return ScanDecision::Coalesced;
        }
        let operation_id = self.next_operation_id;
        self.next_operation_id += 1;
        self.in_flight = Some(InFlight {
            operation_id,
            started_at: now,
        });
        ScanDecision::Start { operation_id }
    }

    /// Complete the operation `operation_id`, returning everyone owed a reply.
    ///
    /// Returns `None` for a stale operation — a result arriving after its
    /// operation was superseded must not clear the in-flight state of the scan
    /// that replaced it.
    pub fn finish(
        &mut self,
        operation_id: u64,
        result: Result<Vec<ScannedNetwork>, String>,
        now: Instant,
    ) -> Option<ScanCompletion> {
        let started_at = match &self.in_flight {
            Some(f) if f.operation_id == operation_id => f.started_at,
            _ => return None,
        };
        self.in_flight = None;
        // Only successes are cached. A failure leaves whatever was there
        // before intact: a stale-but-real list beats poisoning the cache with
        // an error, and the entry expires on its own soon enough.
        let result = match result {
            Ok(networks) => {
                let networks = Arc::new(networks);
                self.last_success = Some(CachedScan {
                    networks: networks.clone(),
                    completed_at: now,
                });
                Ok(networks)
            }
            Err(e) => Err(e),
        };
        Some(ScanCompletion {
            waiters: std::mem::take(&mut self.waiters),
            elapsed: now.saturating_duration_since(started_at),
            result,
        })
    }

    /// Drop a departed client's waiters. An in-flight scan still runs to
    /// completion — CoreWLAN offers no cancellation — and still fills the
    /// cache, which is exactly what the next client wants.
    pub fn forget_client(&mut self, client_id: u64) {
        self.waiters.retain(|w| w.client_id != client_id);
    }

    /// Abort the current operation when it could not be handed to the worker.
    pub fn abort(
        &mut self,
        operation_id: u64,
        error: String,
        now: Instant,
    ) -> Option<ScanCompletion> {
        self.finish(operation_id, Err(error), now)
    }
}

/// The diagnostic to send alongside a scan whose SSIDs all came back blank.
///
/// Shared by the coordinator and the direct worker path so every waiter on one
/// scan is told the same thing.
pub fn redaction_diagnostic(networks: &[ScannedNetwork]) -> Option<String> {
    let all_blank = !networks.is_empty()
        && networks
            .iter()
            .all(|n| n.ssid.as_deref().is_none_or(str::is_empty));
    if !all_blank {
        return None;
    }
    Some(crate::location::redaction_hint().map_or_else(
        // Location says we're authorized but SSIDs are still redacted —
        // almost always means the running executable isn't the bundled one
        // TCC granted.
        || {
            "SSIDs redacted despite Location auth — run via bundled .app \
             (scripts/bundle.sh) so TCC matches this binary"
                .to_string()
        },
        str::to_string,
    ))
}

/// How many of these networks came back with a blank SSID. Logged per scan:
/// a full list of blanks is the signature of a lost Location grant.
pub fn blank_ssids(networks: &[ScannedNetwork]) -> usize {
    networks
        .iter()
        .filter(|n| n.ssid.as_deref().is_none_or(str::is_empty))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn waiter(client_id: u64, request_id: u64) -> ScanWaiter {
        ScanWaiter {
            client_id,
            request_id,
            purpose: ScanPurpose::Scan,
        }
    }

    fn network(ssid: Option<&str>) -> ScannedNetwork {
        ScannedNetwork {
            ssid: ssid.map(str::to_string),
            bssid: None,
            rssi: -50,
            channel: None,
            security: crate::corewlan::Security::Open,
        }
    }

    #[test]
    fn first_request_starts_a_physical_scan() {
        let mut c = ScanCoordinator::default();
        assert!(matches!(
            c.request(waiter(1, 1), Instant::now()),
            ScanDecision::Start { operation_id: 1 }
        ));
    }

    #[test]
    fn concurrent_requests_share_one_physical_scan() {
        let mut c = ScanCoordinator::default();
        let now = Instant::now();
        assert!(matches!(
            c.request(waiter(1, 1), now),
            ScanDecision::Start { .. }
        ));
        for i in 2..=10 {
            assert!(matches!(
                c.request(waiter(i, i), now),
                ScanDecision::Coalesced
            ));
        }

        let completion = c.finish(1, Ok(vec![network(Some("net"))]), now).unwrap();
        assert_eq!(completion.waiters.len(), 10, "every request gets a reply");
        assert_eq!(c.next_operation_id, 2, "only one operation was started");
    }

    #[test]
    fn aborted_operation_releases_waiters_and_allows_retry() {
        let mut c = ScanCoordinator::default();
        let now = Instant::now();
        c.request(waiter(1, 1), now);
        c.request(waiter(2, 2), now);

        let completion = c.abort(1, "worker unavailable".into(), now).unwrap();
        assert_eq!(completion.waiters.len(), 2);
        assert!(matches!(completion.result, Err(ref e) if e == "worker unavailable"));
        assert!(matches!(
            c.request(waiter(3, 3), now),
            ScanDecision::Start { operation_id: 2 }
        ));
    }

    #[test]
    fn fresh_cache_answers_without_touching_the_worker() {
        let mut c = ScanCoordinator::default();
        let start = Instant::now();
        c.request(waiter(1, 1), start);
        c.finish(1, Ok(vec![network(Some("net"))]), start).unwrap();

        let hit = c.request(waiter(2, 2), start + Duration::from_secs(4));
        match hit {
            ScanDecision::Cached(networks) => assert_eq!(networks.len(), 1),
            other => panic!("expected a cache hit, got {other:?}"),
        }
        assert_eq!(c.next_operation_id, 2, "no second operation");
    }

    #[test]
    fn expired_cache_starts_a_new_scan() {
        let mut c = ScanCoordinator::default();
        let start = Instant::now();
        c.request(waiter(1, 1), start);
        c.finish(1, Ok(vec![network(Some("net"))]), start).unwrap();

        assert!(matches!(
            c.request(waiter(2, 2), start + CACHE_LIFETIME),
            ScanDecision::Start { operation_id: 2 }
        ));
    }

    #[test]
    fn successful_completion_returns_the_cached_arc() {
        let mut c = ScanCoordinator::default();
        let now = Instant::now();
        c.request(waiter(1, 1), now);
        let served = c
            .finish(1, Ok(vec![network(Some("net"))]), now)
            .unwrap()
            .result
            .expect("success");

        match c.request(waiter(2, 2), now) {
            // The waiters and the cache share one allocation rather than a
            // clone per client.
            ScanDecision::Cached(cached) => assert!(Arc::ptr_eq(&cached, &served)),
            other => panic!("expected a cache hit, got {other:?}"),
        }
    }

    #[test]
    fn failure_completes_every_waiter_and_does_not_poison_the_cache() {
        let mut c = ScanCoordinator::default();
        let start = Instant::now();
        c.request(waiter(1, 1), start);
        c.finish(1, Ok(vec![network(Some("net"))]), start).unwrap();

        // Force past the cache so a real scan is attempted, and fail it.
        let later = start + CACHE_LIFETIME;
        c.request(waiter(2, 2), later);
        c.request(waiter(3, 3), later);
        let completion = c.finish(2, Err("scan failed".into()), later).unwrap();
        assert_eq!(completion.waiters.len(), 2, "both waiters are released");
        assert!(completion.result.is_err());

        // In-flight state is cleared, so the next request is admitted...
        assert!(matches!(
            c.request(waiter(4, 4), later),
            ScanDecision::Start { .. }
        ));
        // ...and the previous good result was neither served as fresh nor
        // replaced by the error.
        assert!(c.last_success.is_some());
    }

    #[test]
    fn stale_completions_are_ignored() {
        let mut c = ScanCoordinator::default();
        let start = Instant::now();
        c.request(waiter(1, 1), start);
        c.finish(1, Err("boom".into()), start).unwrap();

        let later = start + Duration::from_millis(1);
        c.request(waiter(2, 2), later);
        // Operation 1's result arriving late must not steal operation 2's
        // waiters or clear its in-flight state.
        assert!(c.finish(1, Ok(vec![network(Some("net"))]), later).is_none());
        let completion = c.finish(2, Ok(vec![network(Some("net"))]), later).unwrap();
        assert_eq!(completion.waiters, vec![waiter(2, 2)]);
    }

    #[test]
    fn a_disconnected_waiter_does_not_block_the_others() {
        let mut c = ScanCoordinator::default();
        let now = Instant::now();
        c.request(waiter(1, 1), now);
        c.request(waiter(2, 2), now);
        c.forget_client(1);

        let completion = c.finish(1, Ok(vec![network(Some("net"))]), now).unwrap();
        assert_eq!(completion.waiters, vec![waiter(2, 2)]);
    }

    #[test]
    fn all_blank_ssids_produce_a_diagnostic() {
        assert_eq!(blank_ssids(&[network(None), network(Some(""))]), 2);
        assert!(redaction_diagnostic(&[network(None), network(Some(""))]).is_some());
    }

    #[test]
    fn a_single_named_network_suppresses_the_diagnostic() {
        assert!(redaction_diagnostic(&[network(None), network(Some("net"))]).is_none());
        assert!(redaction_diagnostic(&[]).is_none(), "empty is not redacted");
    }
}
