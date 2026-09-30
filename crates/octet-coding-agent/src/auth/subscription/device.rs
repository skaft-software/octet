#![allow(missing_docs)]

//! RFC 8628 device authorization grant.
//!
//! One shared poller implements the cadence rules every device-code provider
//! needs: honor the server's interval, apply the RFC 8628 §3.5 five-second
//! increment on `slow_down`, never poll faster than once per second, never sleep
//! past the code's own expiry, and report a clock-drift timeout distinctly from
//! an ordinary one.

use std::time::Duration;

use anyhow::{bail, Result};
use tokio::time::Instant;

/// RFC 8628 §3.2: an absent `interval` means the client must wait five seconds.
const DEFAULT_INTERVAL: Duration = Duration::from_secs(5);

/// RFC 8628 §3.5: `slow_down` raises the interval by five seconds.
const SLOW_DOWN_INCREMENT: Duration = Duration::from_secs(5);

/// Floor on the poll cadence, so a server cannot induce a hot loop.
const MINIMUM_INTERVAL: Duration = Duration::from_secs(1);

/// The result of one device-authorization poll.
pub(crate) enum PollOutcome<T> {
    /// The user has not finished authorizing yet.
    Pending,
    /// The server asked octet to poll less often.
    SlowDown {
        /// A new minimum interval the server supplied with `slow_down`.
        interval_seconds: Option<u64>,
    },
    /// The user authorized; the grant is complete.
    Complete(T),
}

/// Poll until the device authorization completes, is denied, or expires.
///
/// `expires_in_seconds` of `None` means the provider reported no lifetime, in
/// which case the flow is bounded only by its own cancellation.
pub(crate) async fn poll_device_authorization<T, Poll, Fut>(
    provider_label: &str,
    interval_seconds: Option<u64>,
    expires_in_seconds: Option<u64>,
    wait_before_first_poll: bool,
    mut poll: Poll,
) -> Result<T>
where
    Poll: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<PollOutcome<T>>>,
{
    let deadline = expires_in_seconds.map(|seconds| Instant::now() + Duration::from_secs(seconds));
    let mut interval = interval_seconds
        .map(Duration::from_secs)
        .filter(|interval| *interval >= MINIMUM_INTERVAL)
        .unwrap_or(DEFAULT_INTERVAL);
    let mut slow_down_responses = 0_u64;

    if wait_before_first_poll {
        sleep_capped(interval, deadline).await;
    }

    while deadline.is_none_or(|deadline| Instant::now() < deadline) {
        match poll().await? {
            PollOutcome::Complete(value) => return Ok(value),
            // A terminal denial or expiry is reported by the provider's own poll
            // closure, so reaching here means the authorization is still live.
            PollOutcome::Pending => {}
            PollOutcome::SlowDown { interval_seconds } => {
                slow_down_responses += 1;
                // Prefer the server's own minimum when it sends one; trusting
                // only a client-tracked value risks polling early forever under
                // a drifting clock.
                interval = interval_seconds
                    .map(Duration::from_secs)
                    .filter(|interval| *interval >= MINIMUM_INTERVAL)
                    .unwrap_or_else(|| interval.saturating_add(SLOW_DOWN_INCREMENT));
            }
        }
        sleep_capped(interval, deadline).await;
    }

    if slow_down_responses > 0 {
        bail!(
            "{provider_label} device authorization timed out after the server asked octet to \
             poll more slowly. This usually means the system clock drifted; sync it and try again"
        );
    }
    bail!("{provider_label} device authorization timed out; run the login again");
}

/// Sleep for `interval`, never overshooting the code's expiry.
async fn sleep_capped(interval: Duration, deadline: Option<Instant>) {
    let sleep_for = match deadline {
        Some(deadline) => interval.min(deadline.saturating_duration_since(Instant::now())),
        None => interval,
    };
    if !sleep_for.is_zero() {
        tokio::time::sleep(sleep_for).await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn completes_immediately_when_the_poll_succeeds() {
        let outcome =
            poll_device_authorization::<u8, _, _>("test", Some(5), Some(900), true, || async {
                Ok(PollOutcome::Complete(7))
            })
            .await;
        assert_eq!(outcome.unwrap(), 7);
    }

    #[tokio::test(start_paused = true)]
    async fn slow_down_applies_the_rfc_increment_and_then_succeeds() {
        let polls = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&polls);
        let outcome =
            poll_device_authorization::<u8, _, _>("test", Some(2), Some(900), false, move || {
                let counter = Arc::clone(&counter);
                async move {
                    if counter.fetch_add(1, Ordering::SeqCst) < 2 {
                        Ok(PollOutcome::SlowDown {
                            interval_seconds: None,
                        })
                    } else {
                        Ok(PollOutcome::Complete(9))
                    }
                }
            })
            .await;
        assert_eq!(outcome.unwrap(), 9);
        assert_eq!(polls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_server_supplied_interval_wins_over_the_local_increment() {
        let polls = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&polls);
        let outcome =
            poll_device_authorization::<u8, _, _>("test", Some(600), Some(900), false, move || {
                let counter = Arc::clone(&counter);
                async move {
                    if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                        Ok(PollOutcome::SlowDown {
                            interval_seconds: Some(2),
                        })
                    } else {
                        Ok(PollOutcome::Complete(1))
                    }
                }
            })
            .await;
        assert_eq!(outcome.unwrap(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn an_ignored_interval_is_never_shorter_than_one_second() {
        let polls = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&polls);
        let outcome =
            poll_device_authorization::<u8, _, _>("test", Some(0), Some(900), false, move || {
                let counter = Arc::clone(&counter);
                async move {
                    if counter.fetch_add(1, Ordering::SeqCst) < 1 {
                        Ok(PollOutcome::Pending)
                    } else {
                        Ok(PollOutcome::Complete(3))
                    }
                }
            })
            .await;
        assert_eq!(outcome.unwrap(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn expiry_after_slow_down_names_the_clock_drift() {
        let error =
            poll_device_authorization::<u8, _, _>("xAI", Some(2), Some(4), false, || async {
                Ok(PollOutcome::SlowDown {
                    interval_seconds: None,
                })
            })
            .await
            .expect_err("an unbounded slow_down must time out");
        let message = error.to_string();
        assert!(message.contains("poll more slowly"), "{message}");
        assert!(message.contains("xAI"), "{message}");

        let error =
            poll_device_authorization::<u8, _, _>("kimi", Some(2), Some(4), false, || async {
                Ok(PollOutcome::Pending)
            })
            .await
            .expect_err("a pending grant must time out");
        assert!(error.to_string().contains("run the login again"), "{error}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_provider_poll_error_propagates_untouched() {
        let error =
            poll_device_authorization::<u8, _, _>("meta", Some(1), Some(900), false, || async {
                bail!("meta device authorization was denied")
            })
            .await
            .expect_err("a denial must not be retried");
        assert_eq!(error.to_string(), "meta device authorization was denied");
    }
}
