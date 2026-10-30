//! A token bucket that bounds how fast we call one host (spec section 5.6).
//!
//! `calls` tokens per `window`, refilled continuously. `acquire` sleeps for exactly the
//! time until the next token, so callers never busy-wait.

use std::sync::Mutex;
use std::time::Duration;

use tokio::time::Instant;

pub struct RateLimiter {
    capacity: f64,
    per_token: Duration,
    bucket: Mutex<Bucket>,
}

struct Bucket {
    tokens: f64,
    refilled_at: Instant,
}

impl RateLimiter {
    pub fn new(calls: u32, window: Duration) -> Self {
        let calls = calls.max(1);
        RateLimiter {
            capacity: f64::from(calls),
            per_token: window / calls,
            bucket: Mutex::new(Bucket { tokens: f64::from(calls), refilled_at: Instant::now() }),
        }
    }

    /// Take one token, sleeping until one is available.
    pub async fn acquire(&self) {
        loop {
            let wait = {
                let mut bucket = self.bucket.lock().expect("rate limiter lock");
                self.refill(&mut bucket);
                if bucket.tokens >= 1.0 {
                    bucket.tokens -= 1.0;
                    return;
                }
                self.per_token.mul_f64(1.0 - bucket.tokens)
            };
            tokio::time::sleep(wait).await;
        }
    }

    /// Take one token if one is available right now.
    pub fn try_acquire(&self) -> bool {
        let mut bucket = self.bucket.lock().expect("rate limiter lock");
        self.refill(&mut bucket);
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    fn refill(&self, bucket: &mut Bucket) {
        let now = Instant::now();
        let elapsed = now.duration_since(bucket.refilled_at);
        if elapsed > Duration::ZERO {
            let refilled = elapsed.as_secs_f64() / self.per_token.as_secs_f64();
            bucket.tokens = (bucket.tokens + refilled).min(self.capacity);
            bucket.refilled_at = now;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn allows_a_full_burst_then_waits_one_token_interval() {
        let limiter = RateLimiter::new(3, Duration::from_secs(30));
        for _ in 0..3 {
            assert!(limiter.try_acquire());
        }
        assert!(!limiter.try_acquire());

        let started = Instant::now();
        limiter.acquire().await;
        let waited = started.elapsed();
        assert!(waited >= Duration::from_secs(10) && waited < Duration::from_secs(11), "waited {waited:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn refills_continuously_and_caps_at_capacity() {
        let limiter = RateLimiter::new(2, Duration::from_secs(2));
        assert!(limiter.try_acquire());
        assert!(limiter.try_acquire());
        assert!(!limiter.try_acquire());

        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(limiter.try_acquire(), "one token per second refills");
        assert!(!limiter.try_acquire());

        tokio::time::advance(Duration::from_secs(60)).await;
        assert!(limiter.try_acquire());
        assert!(limiter.try_acquire());
        assert!(!limiter.try_acquire(), "the bucket never holds more than its capacity");
    }

    #[tokio::test(start_paused = true)]
    async fn acquire_returns_immediately_while_tokens_remain() {
        let limiter = RateLimiter::new(300, Duration::from_secs(30));
        let started = Instant::now();
        for _ in 0..300 {
            limiter.acquire().await;
        }
        assert_eq!(started.elapsed(), Duration::ZERO);
    }
}
